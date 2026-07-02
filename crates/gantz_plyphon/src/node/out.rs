//! The `~out` audio-output sink node.

use std::hash::{Hash, Hasher};

use gantz_ca::CaHash;
use gantz_core::node::{ExprCtx, ExprResult, MetaCtx, RegCtx};
use gantz_egui::{
    InspectorRowsResponse, NodeCtx, NodeUi, NodeUiResponse, Registry, SocketDoc, SocketKind,
};
use gantz_nodetag::NodeTag;
use plyphon::Rate;
use plyphon::synthdef::{InputRef, UnitSpec};
use serde::{Deserialize, Serialize};

use crate::dsp::{DspBuilder, NodeDsp, ToNodeDsp};
use crate::param::{
    cahash_lag, control_input_expr, param_name, param_row, param_state, param_state_row,
    param_value, plyphon_param, with_value,
};

/// The audio output sink. Applies a master `gain` to its input and writes it to
/// output bus 0, fanned across every output channel. The compiler roots a
/// synthdef at this node.
///
/// The `gain` *value* lives in the node's VM state (like `number`); only the
/// smoothing `gain_lag` (structural; a small de-click by default) is in the weight.
#[derive(Clone, Debug, Serialize, Deserialize, NodeTag)]
pub struct Out {
    #[serde(default = "default_gain_lag")]
    gain_lag: f32,
}

impl Out {
    /// The default master gain (linear amplitude) a fresh `~out` starts at.
    pub const DEFAULT_GAIN: f32 = 0.2;

    /// The default gain smoothing lag in seconds (a short de-click).
    pub const DEFAULT_GAIN_LAG: f32 = 0.01;

    /// The gain smoothing lag in seconds (`0.0` = instant).
    pub fn gain_lag(&self) -> f32 {
        self.gain_lag
    }

    /// Set the gain smoothing lag in seconds (content-address affecting).
    pub fn set_gain_lag(&mut self, lag: f32) {
        self.gain_lag = lag;
    }
}

impl Default for Out {
    fn default() -> Self {
        Out {
            gain_lag: default_gain_lag(),
        }
    }
}

impl PartialEq for Out {
    fn eq(&self, other: &Self) -> bool {
        self.gain_lag.to_bits() == other.gain_lag.to_bits()
    }
}

impl Eq for Out {}

impl Hash for Out {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Hash::hash(&self.gain_lag.to_bits(), state);
    }
}

impl CaHash for Out {
    fn hash(&self, hasher: &mut gantz_ca::Hasher) {
        hasher.update(b"gantz.plyphon.out");
        cahash_lag(hasher, self.gain_lag);
    }
}

impl gantz_core::Node for Out {
    fn n_inputs(&self, _ctx: MetaCtx) -> usize {
        // Input 0 is the audio signal (a dsp edge); input 1 is the gain control.
        2
    }

    fn stateful(&self, _ctx: MetaCtx) -> bool {
        true
    }

    fn register(&self, mut ctx: RegCtx<'_, '_>) {
        let path = ctx.path();
        gantz_core::node::state::init_value_if_absent(ctx.vm(), path, || {
            param_state(Self::DEFAULT_GAIN as f64)
        })
        .unwrap()
    }

    fn expr(&self, ctx: ExprCtx<'_, '_>) -> ExprResult {
        // A 0-output sink. The audio input (index 0) is a dsp edge handled by the
        // synthdef and ignored here; when the gain control (index 1) is connected,
        // write it into state (the audio driver applies it via `set_control`).
        control_input_expr(&ctx, self.n_dsp_inputs(), "'()")
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
        // `gain` is a settable (smoothed) control param; the driver applies its
        // live state value via `set_control`.
        let gain = b.push_param(
            path,
            plyphon_param(param_name(path, "gain"), Self::DEFAULT_GAIN, self.gain_lag),
        );
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
        // The body shows just the node name; params are edited in the inspector.
        let framed =
            uictx.framed(|ui, _sockets| ui.add(egui::Label::new("~out").selectable(false)));
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
        let mut resp = InspectorRowsResponse::default();
        let state = ctx.extract_value().ok().flatten();
        param_state_row(body, state.as_ref());
        let mut value = state
            .as_ref()
            .and_then(param_value)
            .unwrap_or(Self::DEFAULT_GAIN as f64) as f32;
        let mut lag = self.gain_lag;
        let dv = egui::DragValue::new(&mut value)
            .range(0.0..=1.0)
            .speed(0.005);
        let (value_changed, lag_changed) = param_row(body, "gain", dv, &mut lag);
        if value_changed {
            // Preserve any queued `pending` updates; only the value changes.
            let prev = state.unwrap_or_else(|| param_state(Self::DEFAULT_GAIN as f64));
            let _ = ctx.update_value(with_value(prev, value as f64));
        }
        if lag_changed {
            self.gain_lag = lag;
            resp.mark_changed();
        }
        resp
    }

    fn socket_doc(&self, _: &dyn Registry, kind: SocketKind, ix: usize) -> Option<SocketDoc> {
        match (kind, ix) {
            (SocketKind::Input, 0) => {
                Some(SocketDoc::ty("signal").with_description("signal to send to the audio output"))
            }
            (SocketKind::Input, 1) => Some(SocketDoc::ty("number").with_description(
                "master gain control; overrides the inspector value while connected",
            )),
            _ => None,
        }
    }
}

fn default_gain_lag() -> f32 {
    // A short de-click lag on the master gain (per "lag the gain, not the freq").
    Out::DEFAULT_GAIN_LAG
}
