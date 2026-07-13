//! The DSP domain's egui implementations, everything behind the `egui`
//! feature in one place: the node set's `NodeUi` impls, their shared
//! inspector-row helpers, the settings subtab and the reference inspector's
//! `inline` toggle.
//!
//! Keeping every egui-flavoured item here means the feature holds with a
//! single cfg gate (this module's declaration in the crate root) - the rest
//! of the crate is headless by construction, so new UI code cannot
//! accidentally leak egui into a `--no-default-features` build.

pub use edge_style::DspEdgeStyle;
pub use pane::{DSP_PANE_KEY, DspPane, DspPaneHead};
pub use ref_ext::DspRefExtUi;
pub use settings::DspSettingsTab;

pub mod edge_style;
pub mod node;
pub mod pane;
pub mod param;
pub mod ref_ext;
pub mod settings;
