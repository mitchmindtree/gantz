//! The wire protocol: gossip messages and request/response types.
//!
//! Everything here is plain serde encoded with [postcard] (compact,
//! non-self-describing; `gantz_ca` addresses serialize as raw bytes).
//! Graphs are the exception: the application's node type needs a
//! self-describing format, so graphs travel inside [`Objects`] as opaque
//! pre-serialized blobs the application encodes/decodes. The blob format is
//! entirely the application's choice; it must be a *faithful* serde codec,
//! since a received graph only applies if its deserialized content address
//! verifies against the announced one. The gantz app uses the same RON
//! encoding as its persisted registry (`bevy_gantz::storage`), so wire and
//! persistence cannot drift. The human-facing `.gantz` text format is
//! deliberately not used here: it is a name-resolving projection for
//! import/export (its round-trip re-seeds names and re-roots commits),
//! while sync ships bare address-keyed graphs and moves names only through
//! the convergence rules.
//!
//! [postcard]: https://docs.rs/postcard

use crate::session::{PeerId, SessionId};
use gantz_ca::{Commit, CommitAddr, GraphAddr};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;

/// A message broadcast on a session's gossip topic.
///
/// Must stay well under iroh-gossip's message-size limit (4 KiB by default):
/// anything bulky moves over the request plane instead.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum GossipMsg {
    /// Scoped names whose tips changed on the announcing peer, with the tips'
    /// graph addresses (so receivers can pre-check twin adoptions cheaply).
    Tips {
        origin: PeerId,
        /// Per-origin sequence number, for stale-drop only: convergence
        /// never depends on delivery order.
        seq: u64,
        changed: Vec<(String, CommitAddr, GraphAddr)>,
    },
    /// Anti-entropy: a digest of the announcing peer's scoped heads (see
    /// [`heads_digest`]).
    ///
    /// Reserved: nothing broadcasts digests yet, and receivers do not pull
    /// [`SyncRequest::Heads`] on mismatch (the server already answers it).
    /// Today a peer that misses a `Tips` broadcast re-heals on the next
    /// announcement; this variant is the wire slot for the planned
    /// digest-triggered pull.
    Digest {
        origin: PeerId,
        seq: u64,
        n_names: u32,
        digest: [u8; 32],
    },
    /// Presence and self-reported username.
    Presence {
        origin: PeerId,
        name: Option<String>,
    },
}

/// Objects a peer is missing (mirrors `gantz_ca::sync::Missing`).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Want {
    pub commits: Vec<CommitAddr>,
    pub graphs: Vec<GraphAddr>,
}

/// Fetched session content.
///
/// Order carries no meaning: receivers validate and topologically apply via
/// `gantz_ca::sync::Staged`.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Objects {
    pub commits: Vec<(CommitAddr, WireCommit)>,
    /// Graphs as application-serialized blobs (self-describing, e.g. RON).
    pub graphs: Vec<(GraphAddr, Vec<u8>)>,
}

/// [`Commit`] mirrored without serde field-skipping.
///
/// `Commit` deliberately omits an empty `merge_parents` from its serialized
/// form (persisted-registry compatibility), which desynchronises
/// non-self-describing readers like postcard - the reader cannot tell the
/// field is absent. The wire carries this faithful mirror instead.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct WireCommit {
    pub timestamp: gantz_ca::Timestamp,
    pub parent: Option<CommitAddr>,
    pub graph: GraphAddr,
    pub merge_parents: Vec<CommitAddr>,
}

impl From<Commit> for WireCommit {
    fn from(c: Commit) -> Self {
        Self {
            timestamp: c.timestamp,
            parent: c.parent,
            graph: c.graph,
            merge_parents: c.merge_parents,
        }
    }
}

impl From<WireCommit> for Commit {
    fn from(c: WireCommit) -> Self {
        Self {
            timestamp: c.timestamp,
            parent: c.parent,
            graph: c.graph,
            merge_parents: c.merge_parents,
        }
    }
}

/// A request over the [`SYNC_ALPN`](crate::SYNC_ALPN) plane; one request per
/// QUIC bi-stream.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum SyncRequest {
    /// Protocol negotiation and access check.
    Hello { session: SessionId, proto: u32 },
    /// The full served store: a joiner's initial sync.
    Snapshot { session: SessionId },
    /// The scoped `name -> tip` map, for anti-entropy pulls.
    Heads { session: SessionId },
    /// Specific missing objects.
    Want { session: SessionId, want: Want },
}

/// The response to a [`SyncRequest`].
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum SyncResponse {
    Hello {
        proto: u32,
        accepted: bool,
    },
    Snapshot {
        heads: Vec<(String, CommitAddr)>,
        objects: Objects,
    },
    Heads {
        heads: Vec<(String, CommitAddr)>,
    },
    Objects(Objects),
    /// Unknown session, failed access check or protocol mismatch.
    Denied {
        reason: String,
    },
}

impl Want {
    /// Whether nothing is wanted.
    pub fn is_empty(&self) -> bool {
        self.commits.is_empty() && self.graphs.is_empty()
    }
}

/// Encode a wire value with postcard.
pub fn encode<T: Serialize>(value: &T) -> Vec<u8> {
    // Postcard serialization of our plain enums/structs cannot fail short of
    // allocation failure.
    postcard::to_allocvec(value).unwrap_or_default()
}

/// Decode a wire value with postcard.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, postcard::Error> {
    postcard::from_bytes(bytes)
}

/// The digest of a scoped `name -> tip` map, for [`GossipMsg::Digest`]
/// anti-entropy: blake3 over the sorted `(name, tip)` pairs.
pub fn heads_digest(heads: &BTreeMap<String, CommitAddr>) -> [u8; 32] {
    let mut hasher = gantz_ca::Hasher::new();
    for (name, tip) in heads {
        hasher.update(&(name.len() as u64).to_be_bytes());
        hasher.update(name.as_bytes());
        hasher.update(&gantz_ca::ContentAddr::from(*tip).0);
    }
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_types_round_trip() {
        let ca = CommitAddr::from(gantz_ca::ContentAddr::from([3; 32]));
        let ga = GraphAddr::from(gantz_ca::ContentAddr::from([4; 32]));
        let msg = GossipMsg::Tips {
            origin: PeerId([1; 32]),
            seq: 7,
            changed: vec![("main".to_string(), ca, ga)],
        };
        let decoded: GossipMsg = decode(&encode(&msg)).unwrap();
        let GossipMsg::Tips {
            origin,
            seq,
            changed,
        } = decoded
        else {
            panic!("wrong variant");
        };
        assert_eq!(origin, PeerId([1; 32]));
        assert_eq!(seq, 7);
        assert_eq!(changed, vec![("main".to_string(), ca, ga)]);

        let req = SyncRequest::Want {
            session: SessionId([2; 32]),
            want: Want {
                commits: vec![ca],
                graphs: vec![ga],
            },
        };
        let decoded: SyncRequest = decode(&encode(&req)).unwrap();
        assert!(matches!(decoded, SyncRequest::Want { .. }));
    }

    #[test]
    fn objects_round_trip_ordinary_commits() {
        // Regression: `Commit`'s skip-when-empty `merge_parents` cannot ride
        // postcard directly - the wire mirror must round-trip an ordinary
        // (merge-parent-free) commit faithfully.
        let ga = GraphAddr::from(gantz_ca::ContentAddr::from([4; 32]));
        let commit = Commit::new(std::time::Duration::from_secs(5), None, ga);
        let ca = gantz_ca::commit_addr(&commit);
        let objects = Objects {
            commits: vec![(ca, commit.clone().into())],
            graphs: vec![(ga, b"blob".to_vec())],
        };
        let decoded: Objects = decode(&encode(&objects)).unwrap();
        assert_eq!(decoded.commits, vec![(ca, commit.into())]);
        assert_eq!(decoded.graphs, objects.graphs);
    }

    #[test]
    fn heads_digest_is_order_independent_and_content_sensitive() {
        let ca = |n| CommitAddr::from(gantz_ca::ContentAddr::from([n; 32]));
        let a: BTreeMap<String, CommitAddr> = [("a".to_string(), ca(1)), ("b".to_string(), ca(2))]
            .into_iter()
            .collect();
        let b: BTreeMap<String, CommitAddr> = [("b".to_string(), ca(2)), ("a".to_string(), ca(1))]
            .into_iter()
            .collect();
        assert_eq!(heads_digest(&a), heads_digest(&b));
        let c: BTreeMap<String, CommitAddr> = [("a".to_string(), ca(9))].into_iter().collect();
        assert_ne!(heads_digest(&a), heads_digest(&c));
    }
}
