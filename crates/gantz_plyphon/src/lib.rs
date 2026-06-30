//! DSP nodes for gantz plus a compiler that derives [`plyphon`] synthdefs from
//! connected subgraphs of [`NodeDsp`] nodes.
//!
//! The same gantz graph is compiled by two independent backends: the existing
//! control-rate Steel VM (`gantz_core`), and - for nodes implementing
//! [`NodeDsp`] - the plyphon audio engine via [`derive_synthdef`]. DSP nodes are
//! inert in the Steel world (their [`Node::expr`](gantz_core::Node::expr) is a
//! placeholder); an audio driver (see `bevy_gantz_plyphon`) installs and runs
//! the derived synthdefs through a [`Backend`].

pub use backend::{Backend, BackendError, Embedded};
pub use compile::{DeriveError, Derived, derive_synthdef, structural_sig};
pub use dsp::{DspBuilder, NodeDsp, ParamBinding, ToNodeDsp};
pub use node::{Out, Sine};

pub mod backend;
pub mod compile;
pub mod dsp;
pub mod node;
pub mod param;
