//! The `~sine` sine-oscillator node.

use std::hash::{Hash, Hasher};

use gantz_ca::CaHash;
use gantz_core::node::{ExprCtx, ExprResult, MetaCtx, RegCtx};
use gantz_core::steel::SteelVal;
use gantz_egui::{
    InspectorRowsResponse, NodeCtx, NodeUi, NodeUiResponse, Registry, SocketDoc, SocketKind,
};
use gantz_nodetag::NodeTag;
use plyphon::Rate;
use plyphon::synthdef::{InputRef, UnitSpec};
use serde::{Deserialize, Serialize};

use crate::dsp::{DspBuilder, NodeDsp, ToNodeDsp};
use crate::param::{cahash_lag, control_input_expr, param_name, param_row, plyphon_param};

/// A sine oscillator. Emits a single `SinOsc.ar(freq)` UGen.
///
/// The `freq` (Hz) *value* lives in the node's VM state (path-keyed, like
/// `number`), so editing it does not churn the graph's content address; the audio
/// driver applies value changes via `set_control`. Only the smoothing `freq_lag`
/// (structural) lives in the node weight.
#[derive(Clone, Debug, Default, Serialize, Deserialize, NodeTag)]
pub struct Sine {
    #[serde(default)]
    freq_lag: f32,
}

impl Sine {
    /// The default frequency (Hz) a fresh `~sine` starts at.
    pub const DEFAULT_FREQ: f32 = 220.0;

    /// The frequency smoothing lag in seconds (`0.0` = instant).
    pub fn freq_lag(&self) -> f32 {
        self.freq_lag
    }

    /// Set the frequency smoothing lag in seconds (content-address affecting).
    pub fn set_freq_lag(&mut self, lag: f32) {
        self.freq_lag = lag;
    }
}

impl PartialEq for Sine {
    fn eq(&self, other: &Self) -> bool {
        self.freq_lag.to_bits() == other.freq_lag.to_bits()
    }
}

impl Eq for Sine {}

impl Hash for Sine {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Hash::hash(&self.freq_lag.to_bits(), state);
    }
}

impl CaHash for Sine {
    fn hash(&self, hasher: &mut gantz_ca::Hasher) {
        hasher.update(b"gantz.plyphon.sine");
        cahash_lag(hasher, self.freq_lag);
    }
}

impl gantz_core::Node for Sine {
    fn n_inputs(&self, _ctx: MetaCtx) -> usize {
        // A single control input: the frequency. (No dsp signal inputs.)
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
            SteelVal::NumV(Self::DEFAULT_FREQ as f64)
        })
        .unwrap()
    }

    fn expr(&self, ctx: ExprCtx<'_, '_>) -> ExprResult {
        // The audio engine reads the freq from state; this node is otherwise
        // Steel-inert. When its control input is connected, write the incoming
        // value into state (the audio driver applies it via `set_control`). The
        // placeholder output (the freq) feeds the inert dsp output edge.
        control_input_expr(&ctx, self.n_dsp_inputs(), "state")
    }
}

impl NodeDsp for Sine {
    fn n_dsp_outputs(&self) -> usize {
        1
    }

    fn ugens(&self, path: &[usize], _inputs: &[InputRef], b: &mut DspBuilder) -> Vec<InputRef> {
        // `freq` is a settable control param (a nominal default here; the driver
        // applies the live state value via `set_control`).
        let freq = b.push_param(
            path,
            plyphon_param(param_name(path, "freq"), Self::DEFAULT_FREQ, self.freq_lag),
        );
        let unit = b.push_unit(UnitSpec::new(
            "SinOsc",
            Rate::Audio,
            vec![InputRef::Param(freq), InputRef::Constant(0.0)],
            1,
        ));
        vec![InputRef::Unit { unit, output: 0 }]
    }
}

impl ToNodeDsp for Sine {
    fn to_node_dsp(&self) -> Option<&dyn NodeDsp> {
        Some(self)
    }
}

impl NodeUi for Sine {
    fn name(&self, _: &dyn Registry) -> &str {
        "~sine"
    }

    fn description(&self) -> Option<&'static str> {
        Some("Sine oscillator (audio rate)")
    }

    fn ui(&mut self, _ctx: NodeCtx, uictx: egui_graph::NodeCtx) -> NodeUiResponse {
        // The body shows just the node name; params are edited in the inspector.
        let framed =
            uictx.framed(|ui, _sockets| ui.add(egui::Label::new("~sine").selectable(false)));
        NodeUiResponse::new(framed)
    }

    fn inspector_rows(
        &mut self,
        ctx: &mut NodeCtx,
        body: &mut egui_extras::TableBody,
    ) -> InspectorRowsResponse {
        let mut resp = InspectorRowsResponse::default();
        // The value lives in VM state (a value edit must NOT change the content
        // address); the lag lives in the weight (a lag edit is structural).
        let mut value = ctx
            .extract::<f64>()
            .ok()
            .flatten()
            .unwrap_or(Self::DEFAULT_FREQ as f64) as f32;
        let mut lag = self.freq_lag;
        let dv = egui::DragValue::new(&mut value)
            .range(0.0..=20_000.0)
            .speed(1.0)
            .suffix(" Hz");
        let (value_changed, lag_changed) = param_row(body, "freq", dv, &mut lag);
        if value_changed {
            let _ = ctx.update::<f64>(value as f64);
        }
        if lag_changed {
            self.freq_lag = lag;
            resp.mark_changed();
        }
        resp
    }

    fn socket_doc(&self, _: &dyn Registry, kind: SocketKind, ix: usize) -> Option<SocketDoc> {
        match (kind, ix) {
            (SocketKind::Input, 0) => Some(SocketDoc::ty("number").with_description(
                "frequency (Hz) control; overrides the inspector value while connected",
            )),
            (SocketKind::Output, _) => Some(
                SocketDoc::ty("audio").with_description("sine signal at the configured frequency"),
            ),
            _ => None,
        }
    }
}
