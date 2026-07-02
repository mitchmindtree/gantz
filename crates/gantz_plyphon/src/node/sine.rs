//! The `~sine` sine-oscillator node.

use std::hash::{Hash, Hasher};

use gantz_ca::CaHash;
use gantz_core::node::{ExprCtx, ExprResult, MetaCtx};
use gantz_egui::{NodeCtx, NodeUi, NodeUiResponse, Registry, SocketDoc, SocketKind};
use plyphon::Rate;
use plyphon::synthdef::{InputRef, UnitSpec};
use gantz_nodetag::NodeTag;
use serde::{Deserialize, Serialize};

use crate::dsp::{DspBuilder, NodeDsp, ToNodeDsp};

/// A sine oscillator. Emits a single `SinOsc.ar(freq)` UGen.
///
/// `freq` (Hz) lives in the node weight, so editing it changes the node's
/// content address (and thus the derived synthdef).
#[derive(Clone, Debug, Serialize, Deserialize, NodeTag)]
pub struct Sine {
    #[serde(default = "default_freq")]
    freq: f32,
}

impl Sine {
    /// The oscillator frequency in Hz.
    pub fn freq(&self) -> f32 {
        self.freq
    }

    /// Set the oscillator frequency in Hz (content-address affecting).
    pub fn set_freq(&mut self, freq: f32) {
        self.freq = freq;
    }
}

impl Default for Sine {
    fn default() -> Self {
        Sine {
            freq: default_freq(),
        }
    }
}

impl PartialEq for Sine {
    fn eq(&self, other: &Self) -> bool {
        self.freq.to_bits() == other.freq.to_bits()
    }
}

impl Eq for Sine {}

impl Hash for Sine {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Hash::hash(&self.freq.to_bits(), state);
    }
}

impl CaHash for Sine {
    fn hash(&self, hasher: &mut gantz_ca::Hasher) {
        hasher.update(b"gantz.plyphon.sine");
        hasher.update(&self.freq.to_le_bytes());
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

    fn ugens(&self, _inputs: &[InputRef], b: &mut DspBuilder) -> Vec<InputRef> {
        let unit = b.push_unit(UnitSpec::new(
            "SinOsc",
            Rate::Audio,
            vec![InputRef::Constant(self.freq), InputRef::Constant(0.0)],
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
        // address: mark `changed` (unlike `number`, which edits VM state).
        let mut freq = self.freq;
        let mut edited = false;
        let framed = uictx.framed(|ui, _sockets| {
            let res = ui.add(
                egui::DragValue::new(&mut freq)
                    .suffix(" Hz")
                    .range(0.0..=20_000.0)
                    .speed(1.0),
            );
            edited = res.changed();
            res
        });
        let mut resp = NodeUiResponse::new(framed);
        if edited {
            self.freq = freq;
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

fn default_freq() -> f32 {
    220.0
}
