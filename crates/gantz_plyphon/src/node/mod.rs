//! The DSP node types.
//!
//! Each node implements [`gantz_core::Node`] (a placeholder, since DSP nodes are
//! inert in the Steel world), [`NodeDsp`](crate::NodeDsp) (the audio behaviour),
//! [`ToNodeDsp`](crate::ToNodeDsp) (discovery), and `gantz_egui::NodeUi` (the
//! GUI, `egui` feature). Their `~` keyword-name prefix marks them as dsp nodes.

pub use bus::Bus;
pub use lag::Lag;
pub use out::Out;
pub use pack::Pack;
pub use scope_out::ScopeOut;
pub use sin_osc::SinOsc;
pub use unpack::Unpack;

pub mod bus;
pub mod lag;
pub mod out;
pub mod pack;
pub mod scope_out;
pub mod sin_osc;
pub mod unpack;
