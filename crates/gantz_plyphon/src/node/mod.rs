//! The DSP node types.
//!
//! Each node implements [`gantz_core::Node`] (a placeholder, since DSP nodes are
//! inert in the Steel world), [`NodeDsp`](crate::NodeDsp) (the audio behaviour),
//! [`ToNodeDsp`](crate::ToNodeDsp) (discovery), and [`gantz_egui::NodeUi`] (the
//! GUI). Their `~` keyword-name prefix marks them as dsp nodes.

pub use lag::Lag;
pub use out::Out;
pub use scope_out::ScopeOut;
pub use sin_osc::SinOsc;

pub mod lag;
pub mod out;
pub mod scope_out;
pub mod sin_osc;
