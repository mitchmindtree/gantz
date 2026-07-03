//! The `~unpack` node: split a channel group into mono outputs.

use gantz_ca::CaHash;
use gantz_core::node::{ExprCtx, ExprResult, MetaCtx};
use gantz_egui::{
    InspectorRowsResponse, NodeCtx, NodeUi, NodeUiResponse, Registry, SocketDoc, SocketKind,
};
use gantz_nodetag::NodeTag;
use serde::{Deserialize, Serialize};

use crate::dsp::{DspBuilder, NodeDsp, Signal, ToNodeDsp};
use crate::param::value_row;

/// Split a channel group into `count` mono outputs (like Max's `mc.unpack~` or
/// a VCV split): output `i` carries the input's channel `i`, or silence past
/// the input's width.
///
/// A routing node: it emits no UGens, it only re-groups wires at
/// synthdef-derivation time (and is Steel-inert like the other dsp nodes).
///
/// Shrinking `count` while edges hang off the removed outputs leaves those
/// edges dangling: the Steel compile surfaces an error diagnostic until they
/// are deleted (the same contract as `Expr`'s `#:out`), while synthdef
/// derivation silently ignores them.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, NodeTag)]
pub struct Unpack {
    #[serde(default = "default_count")]
    count: usize,
}

impl Unpack {
    /// The number of outputs a fresh `~unpack` starts with.
    pub const DEFAULT_COUNT: usize = 2;

    /// The number of mono outputs the input signal splits into.
    pub fn count(&self) -> usize {
        self.count
    }

    /// Set the output count (content-address affecting; structural - it changes
    /// the node's output sockets).
    pub fn set_count(&mut self, count: usize) {
        self.count = count.max(1);
    }
}

impl Default for Unpack {
    fn default() -> Self {
        Unpack {
            count: default_count(),
        }
    }
}

impl CaHash for Unpack {
    fn hash(&self, hasher: &mut gantz_ca::Hasher) {
        hasher.update(b"gantz.plyphon.unpack");
        hasher.update(&self.count.to_le_bytes());
    }
}

impl gantz_core::Node for Unpack {
    fn n_inputs(&self, _ctx: MetaCtx) -> usize {
        // A single dsp signal input carrying the whole channel group.
        1
    }

    fn n_outputs(&self, _ctx: MetaCtx) -> usize {
        self.count
    }

    fn expr(&self, _ctx: ExprCtx<'_, '_>) -> ExprResult {
        // Steel-inert: the splitting happens at synthdef derivation. Placeholder
        // outputs feed the inert dsp output edges - a single value for one
        // output, a list of values otherwise (the multi-output expr contract).
        let src = match self.count {
            1 => "0".to_string(),
            n => format!("(list {})", vec!["0"; n].join(" ")),
        };
        gantz_core::node::parse_expr(&src)
    }
}

impl NodeDsp for Unpack {
    fn n_dsp_inputs(&self) -> usize {
        1
    }

    fn n_dsp_outputs(&self) -> usize {
        self.count
    }

    fn ugens(&self, _path: &[usize], inputs: &[Signal], _b: &mut DspBuilder) -> Vec<Signal> {
        // Pure re-grouping: no units, output `i` = the input's channel `i` (or
        // mono silence past the input's width).
        let signal = inputs.first().cloned().unwrap_or_else(|| Signal::silent(1));
        (0..self.count)
            .map(|i| {
                signal
                    .channel(i)
                    .map(Signal::mono)
                    .unwrap_or_else(|| Signal::silent(1))
            })
            .collect()
    }
}

impl ToNodeDsp for Unpack {
    fn to_node_dsp(&self) -> Option<&dyn NodeDsp> {
        Some(self)
    }
}

impl NodeUi for Unpack {
    fn name(&self, _: &dyn Registry) -> &str {
        "~unpack"
    }

    fn description(&self) -> Option<&'static str> {
        Some("Split a multichannel signal into mono outputs")
    }

    fn ui(&mut self, _ctx: NodeCtx, uictx: egui_graph::NodeCtx) -> NodeUiResponse {
        let framed =
            uictx.framed(|ui, _sockets| ui.add(egui::Label::new("~unpack").selectable(false)));
        NodeUiResponse::new(framed)
    }

    fn inspector_rows(
        &mut self,
        _ctx: &mut NodeCtx,
        body: &mut egui_extras::TableBody,
    ) -> InspectorRowsResponse {
        let mut resp = InspectorRowsResponse::default();
        // Output count (structural: it changes the node's sockets -> respawn).
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
            SocketKind::Input => Some(
                SocketDoc::ty("signal")
                    .with_description("signal to split into mono channels (any channel width)"),
            ),
            SocketKind::Output => Some(SocketDoc::ty("signal").with_description(format!(
                "channel {ix} of the input (silence past its width)"
            ))),
        }
    }
}

fn default_count() -> usize {
    Unpack::DEFAULT_COUNT
}
