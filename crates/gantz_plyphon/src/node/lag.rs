//! The `~lag` node: a one-pole smoother over a configurable duration.

use gantz_ca::CaHash;
use gantz_core::node::{ExprCtx, ExprResult, MetaCtx, RegCtx};
use gantz_egui::{
    InspectorRowsResponse, NodeCtx, NodeUi, NodeUiResponse, Registry, SocketDoc, SocketKind,
};
use gantz_nodetag::NodeTag;
use plyphon::synthdef::{InputRef, UnitSpec};
use serde::{Deserialize, Serialize};

use crate::dsp::{DspBuilder, NodeDsp, NodeRate, Signal, ToNodeDsp, cahash_rate};
use crate::param::{
    param_name, param_state, param_state_row, param_value, plyphon_param, rate_row, value_row,
    with_value,
};

/// A one-pole lag (smoother). Emits a `Lag` UGen per input channel at the
/// configured `rate`, smoothing the whole signal group over the shared `lag`
/// duration. (A `kr` lag fed by an audio-rate wire smooths the block's first
/// sample - the usual audio-to-control collapse.)
///
/// The `lag` duration (seconds) lives in the node's VM state (like `~sinosc`'s
/// freq), edited via the inspector and applied to the running synth via
/// `set_control` (the `Lag` UGen recomputes its coefficient on change, so it is
/// click-free). Only the (structural) `rate` lives in the node weight.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash, NodeTag)]
pub struct Lag {
    #[serde(default)]
    rate: NodeRate,
}

impl Lag {
    /// The default lag time (seconds) a fresh `~lag` starts at.
    pub const DEFAULT_DUR: f32 = 0.1;

    /// The ugen rate (`ar`/`kr`) the smoother runs at.
    pub fn rate(&self) -> NodeRate {
        self.rate
    }

    /// Set the ugen rate (content-address affecting; structural).
    pub fn set_rate(&mut self, rate: NodeRate) {
        self.rate = rate;
    }
}

impl CaHash for Lag {
    fn hash(&self, hasher: &mut gantz_ca::Hasher) {
        hasher.update(b"gantz.plyphon.lag");
        cahash_rate(hasher, self.rate);
    }
}

impl gantz_core::Node for Lag {
    fn n_inputs(&self, _ctx: MetaCtx) -> usize {
        // A single dsp signal input (the duration lives in state, not a socket).
        1
    }

    fn n_outputs(&self, _ctx: MetaCtx) -> usize {
        1
    }

    fn stateful(&self, _ctx: MetaCtx) -> bool {
        true
    }

    fn register(&self, mut ctx: RegCtx<'_, '_>) {
        let path = ctx.path();
        gantz_core::node::state::init_value_if_absent(ctx.vm(), path, || {
            param_state(Self::DEFAULT_DUR as f64)
        })
        .unwrap()
    }

    fn expr(&self, _ctx: ExprCtx<'_, '_>) -> ExprResult {
        // Steel-inert: the audio engine smooths the signal; the duration lives in
        // state and is applied via `set_control`. A placeholder output feeds the
        // inert dsp output edge.
        gantz_core::node::parse_expr("0")
    }
}

impl NodeDsp for Lag {
    fn n_dsp_inputs(&self) -> usize {
        1
    }

    fn n_dsp_outputs(&self) -> usize {
        1
    }

    fn ugens(&self, path: &[usize], inputs: &[Signal], b: &mut DspBuilder) -> Vec<Signal> {
        let signal = inputs.first().cloned().unwrap_or_else(|| Signal::silent(1));
        // The lag time is a settable control param (nominal default here; the
        // driver applies the live state value via `set_control`), shared by every
        // channel's `Lag` unit (params broadcast across the group).
        let dur = b.push_param(
            path,
            plyphon_param(param_name(path, "dur"), Self::DEFAULT_DUR, 0.0),
        );
        // One `Lag` per channel: width N in -> width N out.
        let smoothed = signal
            .channels()
            .map(|ch| {
                let unit = b.push_unit(UnitSpec::new(
                    "Lag",
                    self.rate.to_plyphon(),
                    vec![ch, InputRef::Param(dur)],
                    1,
                ));
                InputRef::Unit { unit, output: 0 }
            })
            .collect();
        vec![smoothed]
    }
}

impl ToNodeDsp for Lag {
    fn to_node_dsp(&self) -> Option<&dyn NodeDsp> {
        Some(self)
    }
}

impl NodeUi for Lag {
    fn name(&self, _: &dyn Registry) -> &str {
        "~lag"
    }

    fn description(&self) -> Option<&'static str> {
        Some("One-pole lag: smooth a signal over a duration")
    }

    fn ui(&mut self, _ctx: NodeCtx, uictx: egui_graph::NodeCtx) -> NodeUiResponse {
        let framed =
            uictx.framed(|ui, _sockets| ui.add(egui::Label::new("~lag").selectable(false)));
        NodeUiResponse::new(framed)
    }

    fn show_state(&self) -> bool {
        // A summarised "N queued" state row (in `inspector_rows`) replaces the raw
        // `{value, pending}` dump.
        false
    }

    fn inspector_rows(
        &mut self,
        ctx: &mut NodeCtx,
        body: &mut egui_extras::TableBody,
    ) -> InspectorRowsResponse {
        // The lag duration lives in VM state (a value edit must NOT change the
        // content address), shown as a single duration row.
        let state = ctx.extract_value().ok().flatten();
        param_state_row(body, state.as_ref());
        let mut value = state
            .as_ref()
            .and_then(param_value)
            .unwrap_or(Self::DEFAULT_DUR as f64) as f32;
        let dv = egui::DragValue::new(&mut value)
            .range(0.0..=10.0)
            .speed(0.001)
            .fixed_decimals(3)
            .suffix(" s");
        if value_row(body, "lag", dv) {
            let prev = state.unwrap_or_else(|| param_state(Self::DEFAULT_DUR as f64));
            let _ = ctx.update_value(with_value(prev, value as f64));
        }
        let mut resp = InspectorRowsResponse::default();
        if rate_row(body, &mut self.rate) {
            resp.mark_changed();
        }
        resp
    }

    fn socket_doc(&self, _: &dyn Registry, kind: SocketKind, _ix: usize) -> Option<SocketDoc> {
        match kind {
            SocketKind::Input => Some(SocketDoc::ty("signal").with_description("signal to smooth")),
            SocketKind::Output => Some(SocketDoc::ty("signal").with_description("smoothed signal")),
        }
    }
}
