//! The `~pack` node: concatenate signals into one channel group.

use gantz_ca::CaHash;
use gantz_core::node::{ExprCtx, ExprResult, MetaCtx};
#[cfg(feature = "egui")]
use gantz_egui::{
    InspectorRowsResponse, NodeCtx, NodeUi, NodeUiResponse, Registry, SocketDoc, SocketKind,
};
use gantz_nodetag::NodeTag;
use serde::{Deserialize, Serialize};

use crate::dsp::{DspBuilder, NodeDsp, Signal, ToNodeDsp};
#[cfg(feature = "egui")]
use crate::param::value_row;

/// Concatenate `count` input signals into one channel group (like Max's
/// `mc.pack~` or a VCV merge): the output's width is the sum of the input
/// widths, an unconnected input contributing one channel of silence. Channels
/// are *packed*, never summed - mixing is a different (future `~mix`) node.
///
/// A routing node: it emits no UGens, it only re-groups wires at
/// synthdef-derivation time (and is Steel-inert like the other dsp nodes).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, NodeTag)]
pub struct Pack {
    #[serde(default = "default_count")]
    count: usize,
}

impl Pack {
    /// The number of inputs a fresh `~pack` starts with.
    pub const DEFAULT_COUNT: usize = 2;

    /// The number of dsp inputs to concatenate.
    pub fn count(&self) -> usize {
        self.count
    }

    /// Set the input count (content-address affecting; structural - it changes
    /// the node's input sockets).
    pub fn set_count(&mut self, count: usize) {
        self.count = count.max(1);
    }
}

impl Default for Pack {
    fn default() -> Self {
        Pack {
            count: default_count(),
        }
    }
}

impl CaHash for Pack {
    fn hash(&self, hasher: &mut gantz_ca::Hasher) {
        hasher.update(b"gantz.plyphon.pack");
        hasher.update(&self.count.to_le_bytes());
    }
}

impl gantz_core::Node for Pack {
    fn n_inputs(&self, _ctx: MetaCtx) -> usize {
        // Every input is a dsp signal (any channel width each).
        self.count
    }

    fn n_outputs(&self, _ctx: MetaCtx) -> usize {
        1
    }

    fn expr(&self, _ctx: ExprCtx<'_, '_>) -> ExprResult {
        // Steel-inert: the packing happens at synthdef derivation. A placeholder
        // output feeds the inert dsp output edge.
        gantz_core::node::parse_expr("0")
    }
}

impl NodeDsp for Pack {
    fn n_dsp_inputs(&self) -> usize {
        self.count
    }

    fn n_dsp_outputs(&self) -> usize {
        1
    }

    fn ugens(&self, _path: &[usize], inputs: &[Signal], _b: &mut DspBuilder) -> Vec<Signal> {
        // Pure re-grouping: no units, just the concatenation of every input's
        // channels (unconnected inputs are already mono silence).
        vec![Signal::concat(inputs.iter().cloned())]
    }
}

impl ToNodeDsp for Pack {
    fn to_node_dsp(&self) -> Option<&dyn NodeDsp> {
        Some(self)
    }
}

#[cfg(feature = "egui")]
impl NodeUi for Pack {
    fn name(&self, _: &dyn Registry) -> &str {
        "~pack"
    }

    fn description(&self) -> Option<&'static str> {
        Some("Concatenate signals into one multichannel signal (packed, not mixed)")
    }

    fn ui(&mut self, _ctx: NodeCtx, uictx: egui_graph::NodeCtx) -> NodeUiResponse {
        let framed =
            uictx.framed(|ui, _sockets| ui.add(egui::Label::new("~pack").selectable(false)));
        NodeUiResponse::new(framed)
    }

    fn inspector_rows(
        &mut self,
        _ctx: &mut NodeCtx,
        body: &mut egui_extras::TableBody,
    ) -> InspectorRowsResponse {
        let mut resp = InspectorRowsResponse::default();
        // Input count (structural: it changes the node's sockets -> respawn).
        let mut count = self.count;
        let dv = egui::DragValue::new(&mut count).range(1..=64).speed(1.0);
        if value_row(body, "count", dv) {
            self.count = count.max(1);
            resp.mark_changed();
        }
        resp
    }

    fn socket_doc(&self, _: &dyn Registry, kind: SocketKind, ix: usize) -> Option<SocketDoc> {
        match kind {
            SocketKind::Input => Some(SocketDoc::ty("signal").with_description(format!(
                "signal {ix} to pack (any channel width; silence when unconnected)"
            ))),
            SocketKind::Output => Some(SocketDoc::ty("signal").with_description(
                "every input's channels concatenated (width = the sum of input widths)",
            )),
        }
    }
}

fn default_count() -> usize {
    Pack::DEFAULT_COUNT
}
