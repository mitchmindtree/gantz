//! The served session content, owned by the network runtime.
//!
//! The application mirrors each session's scoped closure (commits, graphs as
//! serialized blobs, and the scoped `name -> tip` map) into its
//! [`SessionStore`] via [`Command::Register`](crate::Command::Register) and
//! [`Command::Update`](crate::Command::Update); the runtime's request
//! handler answers peers from it synchronously. Content-addressed keys make
//! every insert idempotent, so updates may be re-sent freely.

use crate::{
    proto::{Objects, Want},
    session::{Access, PeerId, Session, SessionId},
};
use gantz_ca::{Commit, CommitAddr, GraphAddr};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

/// One session's served content.
#[derive(Debug, Default)]
pub struct SessionStore {
    /// The scoped commit closure.
    pub commits: HashMap<CommitAddr, Commit>,
    /// The scoped graphs, as application-serialized (self-describing) blobs.
    pub graphs: HashMap<GraphAddr, Vec<u8>>,
    /// The scoped `name -> tip` map.
    pub heads: BTreeMap<String, CommitAddr>,
}

/// A session's configuration plus its served content.
#[derive(Debug)]
pub struct SessionEntry {
    pub session: Session,
    pub store: SessionStore,
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

impl SessionStore {
    /// Merge served content into the store: content-addressed commit/graph
    /// inserts (idempotent) and per-name head upserts.
    pub fn merge(
        &mut self,
        heads: impl IntoIterator<Item = (String, CommitAddr)>,
        commits: impl IntoIterator<Item = (CommitAddr, Commit)>,
        graphs: impl IntoIterator<Item = (GraphAddr, Vec<u8>)>,
    ) {
        self.commits.extend(commits);
        self.graphs.extend(graphs);
        self.heads.extend(heads);
    }

    /// The requested objects, where present.
    pub fn objects(&self, want: &Want) -> Objects {
        let commits = want
            .commits
            .iter()
            .filter_map(|ca| self.commits.get(ca).map(|c| (*ca, c.clone().into())))
            .collect();
        let graphs = want
            .graphs
            .iter()
            .filter_map(|ga| self.graphs.get(ga).map(|g| (*ga, g.clone())))
            .collect();
        Objects { commits, graphs }
    }

    /// The whole store as a join snapshot.
    pub fn snapshot(&self) -> (Vec<(String, CommitAddr)>, Objects) {
        let heads = self.heads.iter().map(|(n, ca)| (n.clone(), *ca)).collect();
        let objects = Objects {
            commits: self
                .commits
                .iter()
                .map(|(ca, c)| (*ca, c.clone().into()))
                .collect(),
            graphs: self.graphs.iter().map(|(ga, g)| (*ga, g.clone())).collect(),
        };
        (heads, objects)
    }
}

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
