//! Peer-to-peer collaborative session networking for gantz.
//!
//! A *session* shares one named graph (and its registry dependency closure)
//! between peers over [iroh]. This crate owns everything network-shaped and
//! nothing node-shaped: graphs cross the wire (and sit in the served
//! [`SessionStore`]) as opaque serialized blobs, so the crate stays agnostic
//! of the application's node type. Validating, merging and applying received
//! content is the application layer's job, built on `gantz_ca::sync`.
//!
//! Two planes:
//!
//! - **Gossip** ([`GossipMsg`], per-session topic): tip announcements,
//!   anti-entropy digests and presence. Small, broadcast, unordered.
//! - **Requests** ([`SYNC_ALPN`], one request per QUIC bi-stream): join
//!   snapshots, head listings and object fetches ([`SyncRequest`] /
//!   [`SyncResponse`]), served from the session's [`SessionStore`] and gated
//!   by its [`Access`] allowlist.
//!
//! The [`runtime`] drives an iroh endpoint on a dedicated thread (native) or
//! the browser's event loop (wasm), bridged to the application through plain
//! [`Command`]/[`Event`] channels. The channels are unbounded and the served
//! stores are runtime-owned, so the application side never blocks on a lock:
//! store content rides ordered [`Command::Register`]/[`Command::Update`]
//! sends.
//!
//! [iroh]: https://docs.rs/iroh

#[doc(inline)]
pub use identity::Identity;
#[doc(inline)]
pub use proto::{GossipMsg, Objects, SyncRequest, SyncResponse, Want, WireCommit, heads_digest};
#[doc(inline)]
pub use runtime::{Command, Event, Handle, Infra, PROTO_VERSION, RuntimeConfig, SYNC_ALPN, spawn};
#[doc(inline)]
pub use session::{Access, ConnState, PeerId, Role, Session, SessionId};
#[doc(inline)]
pub use store::{SessionEntry, SessionStore};
#[doc(inline)]
pub use ticket::SessionTicket;

pub mod identity;
pub mod proto;
pub mod runtime;
pub mod session;
pub mod store;
pub mod ticket;
