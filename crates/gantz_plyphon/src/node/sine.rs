//! The `~sine` sine-oscillator node.

use gantz_ca::CaHash;
use gantz_core::node::{ExprCtx, ExprResult, MetaCtx};
use gantz_egui::{
    InspectorRowsResponse, NodeCtx, NodeUi, NodeUiResponse, Registry, SocketDoc, SocketKind,
};
use gantz_nodetag::NodeTag;
use plyphon::Rate;
use plyphon::synthdef::{InputRef, UnitSpec};
use serde::{Deserialize, Serialize};

use crate::dsp::{DspBuilder, NodeDsp, ToNodeDsp};
use crate::param::{DspParam, lag_row, param_name};

/// A sine oscillator. Emits a single `SinOsc.ar(freq)` UGen.
///
/// `freq` (Hz) lives in the node weight as a settable [`DspParam`]: editing the
/// value changes the content address, and the audio driver applies value changes
/// via `set_control` (no respawn). `freq` defaults to no smoothing (instant pitch).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, NodeTag)]
pub struct Sine {
    #[serde(default = "default_freq")]
    freq: DspParam,
}

impl Sine {
    /// The oscillator frequency in Hz.
    pub fn freq(&self) -> f32 {
        self.freq.value
    }

    /// Set the oscillator frequency in Hz (content-address affecting).
    pub fn set_freq(&mut self, freq: f32) {
        self.freq.value = freq;
    }
}

impl Default for Sine {
    fn default() -> Self {
        Sine {
            freq: default_freq(),
        }
    }
}

impl CaHash for Sine {
    fn hash(&self, hasher: &mut gantz_ca::Hasher) {
        hasher.update(b"gantz.plyphon.sine");
        self.freq.cahash(hasher);
    }
}

impl gantz_core::Node for Sine {
    fn n_outputs(&self, _ctx: MetaCtx) -> usize {
        1
    }

    fn expr(&self, _ctx: ExprCtx<'_, '_>) -> ExprResult {
        // Inert in the Steel (control-rate) world; the audio engine compiles
        // this node instead. A single placeholder output keeps any incidental
        // Steel reachability valid.
        gantz_core::node::parse_expr("0")
    }
}

impl NodeDsp for Sine {
    fn n_dsp_outputs(&self) -> usize {
        1
    }

    fn ugens(&self, path: &[usize], _inputs: &[InputRef], b: &mut DspBuilder) -> Vec<InputRef> {
        // `freq` is a settable control param, so dialer/control edits become
        // `set_control` on the running synth rather than a respawn.
        let freq = b.push_param(self.freq.to_plyphon(param_name(path, "freq")));
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
        // The frequency lives in the node weight, so an edit changes the content
        // address: mark `changed` (the driver then set_controls it, no respawn).
        let mut value = self.freq.value;
        let mut edited = false;
        let framed = uictx.framed(|ui, _sockets| {
            let res = ui.add(
                egui::DragValue::new(&mut value)
                    .suffix(" Hz")
                    .range(0.0..=20_000.0)
                    .speed(1.0),
            );
            edited = res.changed();
            res
        });
        let mut resp = NodeUiResponse::new(framed);
        if edited {
            self.freq.value = value;
            resp.mark_changed();
        }
        resp
    }

    fn inspector_rows(
        &mut self,
        _ctx: &mut NodeCtx,
        body: &mut egui_extras::TableBody,
    ) -> InspectorRowsResponse {
        let mut resp = InspectorRowsResponse::default();
        if lag_row(body, "freq lag", &mut self.freq) {
            resp.mark_changed();
        }
        resp
    }

    fn socket_doc(&self, _: &dyn Registry, kind: SocketKind, _ix: usize) -> Option<SocketDoc> {
        match kind {
            SocketKind::Output => Some(
                SocketDoc::ty("audio").with_description("sine signal at the configured frequency"),
            ),
            SocketKind::Input => None,
        }
    }
}

fn default_freq() -> DspParam {
    DspParam::new(220.0)
}
