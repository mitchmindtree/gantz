//! The served session content, shared between the application and the
//! network runtime.
//!
//! The application mirrors each session's scoped closure (commits, graphs as
//! serialized blobs, and the scoped `name -> tip` map) into its
//! [`SessionStore`]; the runtime's request handler answers peers from it
//! synchronously. Content-addressed keys make every insert idempotent.

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

/// The state shared between the application and the network runtime.
#[derive(Debug, Default)]
pub struct SharedState {
    pub sessions: HashMap<SessionId, SessionEntry>,
}

/// A cheaply clonable handle to the [`SharedState`].
///
/// Lock hold times must stay short (lookups and inserts only): the runtime
/// answers peer requests under this lock.
#[derive(Clone, Debug, Default)]
pub struct Shared(Arc<Mutex<SharedState>>);

impl SessionStore {
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
    pub fn lock(&self) -> MutexGuard<'_, SharedState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
