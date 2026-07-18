//! The served session content, owned by the network runtime.
//!
//! Each session serves a plain [`gantz_ca::Registry`] over [`RawGraph`]
//! payloads: graphs sit at rest as application-serialized blobs under the
//! addresses the application validated them against, keeping the crate
//! agnostic of the node type. The application mirrors each session's scoped
//! closure (commits, graphs, heads) into the store via
//! [`Command::Register`](crate::Command::Register) and
//! [`Command::Update`](crate::Command::Update); the runtime's request
//! handler answers peers from it synchronously. Content-addressed keys make
//! every insert idempotent, so updates may be re-sent freely.
//!
//! The store is a relay: it holds what the local application (a decoding
//! peer) verified, under the claimed addresses. Receiving peers re-verify
//! everything through the typed `gantz_ca::sync::Staged` path on their own
//! side, which is where the security boundary lives.

use crate::{
    proto::{Object, ObjectRef, Objects, Want},
    session::{Access, PeerId, Session, SessionId},
};
use gantz_ca::{Commit, CommitAddr, GraphHash, MergeReport, Name, RawGraph, Registry};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

/// One session's served content. See the module docs.
pub type SessionRegistry = Registry<RawGraph>;

/// A session's configuration plus its served content.
#[derive(Debug)]
pub struct SessionEntry {
    pub session: Session,
    pub store: SessionRegistry,
}

/// The state shared between the runtime's driver and its request-serving
/// tasks. Internal: the application mutates it only through the ordered,
/// non-blocking command channel, so it never takes (or waits on) this lock.
#[derive(Debug, Default)]
pub(crate) struct SharedState {
    pub sessions: HashMap<SessionId, SessionEntry>,
}

/// A cheaply clonable handle to the [`SharedState`].
///
/// Lock hold times must stay short (lookups, inserts and response clones
/// only), and every holder runs on the runtime's own thread (native) or the
/// single browser thread (wasm) - contention never involves the
/// application's frame loop.
#[derive(Clone, Debug, Default)]
pub(crate) struct Shared(Arc<Mutex<SharedState>>);

impl SessionEntry {
    /// Whether `peer` may read this session's content.
    pub fn allows(&self, peer: PeerId) -> bool {
        match &self.session.access {
            Access::Public => true,
            Access::Restricted(allowed) => allowed.contains(&peer),
        }
    }
}

impl Shared {
    /// Lock the shared state. A poisoned lock (a panicked peer thread) still
    /// yields the data: content-addressed state cannot be half-written into
    /// an invalid shape.
    pub(crate) fn lock(&self) -> MutexGuard<'_, SharedState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Merge served content into the store: content-addressed commit/graph
/// inserts (idempotent) and per-name head upserts (an incoming tip wins,
/// reported).
///
/// The content is trusted as-is under its claimed addresses (see the module
/// docs): nothing is re-hashed, no parents are detached, so the store serves
/// content byte-identical to what the application validated.
pub fn merge(
    store: &mut SessionRegistry,
    heads: impl IntoIterator<Item = (Name, CommitAddr)>,
    commits: impl IntoIterator<Item = (CommitAddr, Commit)>,
    graphs: impl IntoIterator<Item = RawGraph>,
) -> MergeReport {
    let graphs = graphs.into_iter().map(|g| (g.graph_addr(), g)).collect();
    let commits = commits.into_iter().collect();
    let heads = heads.into_iter().collect();
    store.merge(Registry::from_parts(graphs, commits, heads))
}

/// The requested objects, where present. Absent objects are skipped: the
/// requester re-requests from another peer or re-heals on the next announce.
pub fn objects(store: &SessionRegistry, want: &Want) -> Objects {
    let objects = want
        .refs
        .iter()
        .filter_map(|r| match r {
            ObjectRef::Commit(ca) => store
                .commits()
                .get(ca)
                .map(|c| Object::Commit(*ca, c.clone().into())),
            ObjectRef::Graph(ga) => store
                .graph(ga)
                .map(|g| Object::Graph(*ga, g.bytes.to_vec())),
            ObjectRef::Blob { section, addr } => store.blobs().get(section).and_then(|blobs| {
                blobs.get(addr).map(|bytes| Object::Blob {
                    section: section.clone(),
                    liveness: blobs.liveness,
                    addr: *addr,
                    bytes: bytes.to_vec(),
                })
            }),
        })
        .collect();
    Objects { objects }
}

/// The whole store as a join snapshot: every head, commit, graph and blob.
pub fn snapshot(store: &SessionRegistry) -> (Vec<(Name, CommitAddr)>, Objects) {
    let heads = store.heads().map(|(n, ca)| (n.clone(), ca)).collect();
    let mut objects = Vec::new();
    for (ca, c) in store.commits() {
        objects.push(Object::Commit(*ca, c.clone().into()));
    }
    for (ga, g) in store.graphs() {
        objects.push(Object::Graph(*ga, g.bytes.to_vec()));
    }
    for (section, blobs) in store.blobs() {
        for (addr, bytes) in &blobs.entries {
            objects.push(Object::Blob {
                section: section.clone(),
                liveness: blobs.liveness,
                addr: *addr,
                bytes: bytes.to_vec(),
            });
        }
    }
    (heads, Objects { objects })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gantz_ca::{BlobLiveness, ContentAddr, GraphAddr, commit_addr};
    use std::time::Duration;

    fn name(s: &str) -> Name {
        s.parse().unwrap()
    }

    fn graph_addr(n: u8) -> GraphAddr {
        GraphAddr::from(ContentAddr::from([n; 32]))
    }

    /// A store with one head over a two-commit chain and its graphs.
    fn test_store() -> (SessionRegistry, CommitAddr, CommitAddr) {
        let mut store = SessionRegistry::default();
        let root = Commit::new(Duration::from_secs(1), None, graph_addr(1));
        let root_ca = commit_addr(&root);
        let tip = Commit::new(Duration::from_secs(2), Some(root_ca), graph_addr(2));
        let tip_ca = commit_addr(&tip);
        merge(
            &mut store,
            [(name("jam"), tip_ca)],
            [(root_ca, root), (tip_ca, tip)],
            [
                RawGraph::new(graph_addr(1), &b"g1"[..]),
                RawGraph::new(graph_addr(2), &b"g2"[..]),
            ],
        );
        (store, root_ca, tip_ca)
    }

    #[test]
    fn merge_is_idempotent() {
        let (mut store, root_ca, tip_ca) = test_store();
        let root = store.commits()[&root_ca].clone();
        let tip = store.commits()[&tip_ca].clone();
        let report = merge(
            &mut store,
            [(name("jam"), tip_ca)],
            [(root_ca, root), (tip_ca, tip)],
            [RawGraph::new(graph_addr(1), &b"g1"[..])],
        );
        assert!(report.heads_added.is_empty());
        assert!(report.heads_replaced.is_empty());
        assert_eq!(store.commits().len(), 2);
        assert_eq!(store.graphs().len(), 2);
    }

    #[test]
    fn merge_repoints_heads() {
        let (mut store, root_ca, tip_ca) = test_store();
        let report = merge(&mut store, [(name("jam"), root_ca)], [], []);
        assert_eq!(report.heads_replaced, vec![(name("jam"), tip_ca, root_ca)]);
        assert_eq!(store.head(&name("jam")), Some(root_ca));
    }

    #[test]
    fn objects_filters_to_present() {
        let (store, root_ca, _tip_ca) = test_store();
        let absent_ca = CommitAddr::from(ContentAddr::from([9; 32]));
        let want = Want {
            refs: vec![
                ObjectRef::Commit(root_ca),
                ObjectRef::Commit(absent_ca),
                ObjectRef::Graph(graph_addr(1)),
                ObjectRef::Graph(graph_addr(9)),
                ObjectRef::Blob {
                    section: "dsp.buffer".to_string(),
                    addr: ContentAddr::from([9; 32]),
                },
            ],
        };
        let objects = objects(&store, &want).objects;
        assert_eq!(objects.len(), 2);
        assert!(matches!(objects[0], Object::Commit(ca, _) if ca == root_ca));
        assert!(
            matches!(&objects[1], Object::Graph(ga, bytes) if *ga == graph_addr(1) && bytes == b"g1")
        );
    }

    #[test]
    fn objects_serves_blobs() {
        let (mut store, _root_ca, _tip_ca) = test_store();
        let addr = store.add_blob("dsp.buffer", BlobLiveness::ContentReferenced, &b"pcm"[..]);
        let want = Want {
            refs: vec![ObjectRef::Blob {
                section: "dsp.buffer".to_string(),
                addr,
            }],
        };
        let objects = objects(&store, &want).objects;
        assert_eq!(
            objects,
            vec![Object::Blob {
                section: "dsp.buffer".to_string(),
                liveness: BlobLiveness::ContentReferenced,
                addr,
                bytes: b"pcm".to_vec(),
            }]
        );
    }

    #[test]
    fn snapshot_round_trips() {
        let (mut store, _root_ca, tip_ca) = test_store();
        store.add_blob("dsp.buffer", BlobLiveness::ContentReferenced, &b"pcm"[..]);
        let (heads, objects) = snapshot(&store);
        assert_eq!(heads, vec![(name("jam"), tip_ca)]);
        // Rebuild a store from the snapshot's wire objects.
        let mut rebuilt = SessionRegistry::default();
        let mut commits = Vec::new();
        let mut graphs = Vec::new();
        for object in objects.objects {
            match object {
                Object::Commit(ca, c) => commits.push((ca, c.into())),
                Object::Graph(ga, bytes) => graphs.push(RawGraph::new(ga, bytes)),
                Object::Blob {
                    section,
                    liveness,
                    bytes,
                    ..
                } => {
                    rebuilt.add_blob(section, liveness, bytes);
                }
            }
        }
        merge(&mut rebuilt, heads, commits, graphs);
        assert_eq!(rebuilt.commits(), store.commits());
        assert_eq!(rebuilt.graphs(), store.graphs());
        assert_eq!(rebuilt.blobs(), store.blobs());
        assert_eq!(
            rebuilt.heads().collect::<Vec<_>>(),
            store.heads().collect::<Vec<_>>()
        );
    }
}
