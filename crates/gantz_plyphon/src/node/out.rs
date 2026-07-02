//! The `~out` audio-output sink node.

use std::hash::{Hash, Hasher};

use gantz_ca::CaHash;
use gantz_core::node::{ExprCtx, ExprResult, MetaCtx};
use gantz_egui::{NodeCtx, NodeUi, NodeUiResponse, Registry, SocketDoc, SocketKind};
use plyphon::Rate;
use plyphon::synthdef::{InputRef, UnitSpec};
use gantz_nodetag::NodeTag;
use serde::{Deserialize, Serialize};

use crate::dsp::{DspBuilder, NodeDsp, ToNodeDsp};

/// The audio output sink. Applies a master `gain` to its input and writes it to
/// output bus 0, fanned across every output channel. The compiler roots a
/// synthdef at this node.
#[derive(Clone, Debug, Serialize, Deserialize, NodeTag)]
pub struct Out {
    #[serde(default = "default_gain")]
    gain: f32,
}

impl Out {
    /// The master output gain (linear amplitude).
    pub fn gain(&self) -> f32 {
        self.gain
    }

    /// Set the master output gain (content-address affecting).
    pub fn set_gain(&mut self, gain: f32) {
        self.gain = gain;
    }
}

impl Default for Out {
    fn default() -> Self {
        Out {
            gain: default_gain(),
        }
    }
}

impl PartialEq for Out {
    fn eq(&self, other: &Self) -> bool {
        self.gain.to_bits() == other.gain.to_bits()
    }
}

impl Eq for Out {}

impl Hash for Out {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Hash::hash(&self.gain.to_bits(), state);
    }
}

impl CaHash for Out {
    fn hash(&self, hasher: &mut gantz_ca::Hasher) {
        hasher.update(b"gantz.plyphon.out");
        hasher.update(&self.gain.to_le_bytes());
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

    fn ugens(&self, inputs: &[InputRef], b: &mut DspBuilder) -> Vec<InputRef> {
        let sig = inputs.first().copied().unwrap_or(InputRef::Constant(0.0));
        // Apply master gain: sig * gain (BinaryOpUGen multiply, special_index 2).
        let gained = b.push_unit(UnitSpec {
            name: "BinaryOpUGen".to_string(),
            rate: Rate::Audio,
            inputs: vec![sig, InputRef::Constant(self.gain)],
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
        // address: mark `changed`.
        let mut gain = self.gain;
        let mut edited = false;
        let framed = uictx.framed(|ui, _sockets| {
            let res = ui.add(
                egui::DragValue::new(&mut gain)
                    .prefix("gain ")
                    .range(0.0..=1.0)
                    .speed(0.005),
            );
            edited = res.changed();
            res
        });
        let mut resp = NodeUiResponse::new(framed);
        if edited {
            self.gain = gain;
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

fn default_gain() -> f32 {
    0.2
}
