//! The `~out` audio-output sink node.

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

/// The audio output sink. Applies a master `gain` to its input and writes it to
/// output bus 0, fanned across every output channel. The compiler roots a
/// synthdef at this node.
///
/// `gain` is a settable [`DspParam`] with a small default smoothing lag, so gain
/// changes de-click without a respawn.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, NodeTag)]
pub struct Out {
    #[serde(default = "default_gain")]
    gain: DspParam,
}

impl Out {
    /// The master output gain (linear amplitude).
    pub fn gain(&self) -> f32 {
        self.gain.value
    }

    /// Set the master output gain (content-address affecting).
    pub fn set_gain(&mut self, gain: f32) {
        self.gain.value = gain;
    }
}

impl Default for Out {
    fn default() -> Self {
        Out {
            gain: default_gain(),
        }
    }
}

impl CaHash for Out {
    fn hash(&self, hasher: &mut gantz_ca::Hasher) {
        hasher.update(b"gantz.plyphon.out");
        self.gain.cahash(hasher);
    }
}

impl gantz_core::Node for Out {
    fn n_inputs(&self, _ctx: MetaCtx) -> usize {
        1
    }

    fn expr(&self, _ctx: ExprCtx<'_, '_>) -> ExprResult {
        // A 0-output sink, inert in the Steel world (the audio engine drives it):
        // emit the empty-output value, matching the 0-output convention.
        gantz_core::node::parse_expr("'()")
    }
}

impl NodeDsp for Out {
    fn n_dsp_inputs(&self) -> usize {
        1
    }

    fn n_dsp_outputs(&self) -> usize {
        0
    }

    fn is_output(&self) -> bool {
        true
    }

    fn ugens(&self, path: &[usize], inputs: &[InputRef], b: &mut DspBuilder) -> Vec<InputRef> {
        let sig = inputs.first().copied().unwrap_or(InputRef::Constant(0.0));
        // Apply master gain: sig * gain (BinaryOpUGen multiply, special_index 2).
        // `gain` is a settable (smoothed) param so changes de-click in place.
        let gain = b.push_param(self.gain.to_plyphon(param_name(path, "gain")));
        let gained = b.push_unit(UnitSpec {
            name: "BinaryOpUGen".to_string(),
            rate: Rate::Audio,
            inputs: vec![sig, InputRef::Param(gain)],
            num_outputs: 1,
            special_index: 2,
        });
        let gained = InputRef::Unit {
            unit: gained,
            output: 0,
        };
        // `Out.ar(0, [sig; channels])`: bus index followed by one signal input
        // per output channel, fanning the mono signal across them.
        let mut out_inputs = vec![InputRef::Constant(0.0)];
        out_inputs.extend(std::iter::repeat_n(gained, b.out_channels()));
        b.push_unit(UnitSpec::new("Out", Rate::Audio, out_inputs, 0));
        vec![]
    }
}

impl ToNodeDsp for Out {
    fn to_node_dsp(&self) -> Option<&dyn NodeDsp> {
        Some(self)
    }
}

impl NodeUi for Out {
    fn name(&self, _: &dyn Registry) -> &str {
        "~out"
    }

    fn description(&self) -> Option<&'static str> {
        Some("Audio output: master gain to the speakers")
    }

    fn ui(&mut self, _ctx: NodeCtx, uictx: egui_graph::NodeCtx) -> NodeUiResponse {
        // The gain lives in the node weight, so an edit changes the content
        // address: mark `changed` (the driver then set_controls it, no respawn).
        let mut value = self.gain.value;
        let mut edited = false;
        let framed = uictx.framed(|ui, _sockets| {
            let res = ui.add(
                egui::DragValue::new(&mut value)
                    .prefix("gain ")
                    .range(0.0..=1.0)
                    .speed(0.005),
            );
            edited = res.changed();
            res
        });
        let mut resp = NodeUiResponse::new(framed);
        if edited {
            self.gain.value = value;
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
        if lag_row(body, "gain lag", &mut self.gain) {
            resp.mark_changed();
        }
        resp
    }

    fn socket_doc(&self, _: &dyn Registry, kind: SocketKind, _ix: usize) -> Option<SocketDoc> {
        match kind {
            SocketKind::Input => {
                Some(SocketDoc::ty("audio").with_description("signal to send to the audio output"))
            }
            SocketKind::Output => None,
        }
    }
}

fn default_gain() -> DspParam {
    // A short de-click lag on the master gain (per "lag the gain, not the freq").
    DspParam::lagged(0.2, 0.01)
}
