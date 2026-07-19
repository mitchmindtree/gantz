//! Plugin assembly must be order-insensitive: `PlyphonPlugin` reads the
//! shared `EvalEpoch` in `Plugin::finish`, so adding it BEFORE `GantzPlugin`
//! must work identically.

use bevy::MinimalPlugins;
use bevy::app::App;
use bevy_gantz::GantzPlugin;
use bevy_gantz_plyphon::{DspConfig, DspStatus, PlyphonPlugin};
use gantz_core::node::{AsRefNode, Ref};
use gantz_plyphon::{NodeDsp, ToNodeDsp};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct TestNode;

impl gantz_core::Node for TestNode {
    fn expr(&self, _ctx: gantz_core::node::ExprCtx<'_, '_>) -> gantz_core::node::ExprResult {
        gantz_core::node::parse_expr("'()")
    }
}

impl ToNodeDsp for TestNode {
    fn to_node_dsp(&self) -> Option<&dyn NodeDsp> {
        None
    }
}

impl AsRefNode for TestNode {
    fn as_ref_node(&self) -> Option<&Ref> {
        None
    }
}

/// A headless app with the gantz plugins added in REVERSED order builds,
/// finishes and ticks without panicking, with the DSP resources present.
#[test]
fn plugin_order_is_insensitive() {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    // Deliberately reversed: the DSP runtime before the core plugin.
    app.add_plugins(PlyphonPlugin::<TestNode>::new());
    app.add_plugins(GantzPlugin::<TestNode>::default());
    // The builtin set is the app's responsibility (see `GantzPlugin` docs).
    app.insert_resource(bevy_gantz::BuiltinNodes::<TestNode>::default());
    app.finish();
    app.cleanup();
    for _ in 0..3 {
        app.update();
    }
    assert!(app.world().contains_resource::<DspConfig>());
    assert!(
        app.world().contains_resource::<DspStatus>(),
        "DspStatus is inserted at finish regardless of device presence",
    );
}
