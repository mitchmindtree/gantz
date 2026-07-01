//! The `~tap` node: monitor a dsp signal into a ring buffer, read out on a trigger.

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
use crate::param::value_row;

/// A signal *tap*: streams every sample of its dsp input into a ring buffer held in
/// VM state (the audio driver writes it, draining a plyphon `ScopeOut` scope stream),
/// and on a control trigger outputs the ring's current samples as a list.
///
/// Wire a signal into the dsp input to monitor it, drive the trigger input (e.g.
/// with a `tick!`), and plug the list output into a `plot` (or any widget) to see
/// it. Set `size` to 1 to monitor a single latest value; larger for a waveform
/// window. It is a dsp *sink* (no passthrough); to keep hearing a signal, also wire
/// it to `~out`. A `~peak`/`~rms` node placed before a `~tap` gives level metering.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, NodeTag)]
pub struct Tap {
    #[serde(default = "default_size")]
    size: usize,
}

impl Tap {
    /// The default ring-buffer length (samples) a fresh `~tap` starts at.
    pub const DEFAULT_SIZE: usize = 256;

    /// The ring-buffer length (samples).
    pub fn size(&self) -> usize {
        self.size
    }

    /// Set the ring-buffer length (samples; content-address affecting).
    pub fn set_size(&mut self, size: usize) {
        self.size = size.max(1);
    }
}

impl Default for Tap {
    fn default() -> Self {
        Tap {
            size: default_size(),
        }
    }
}

impl CaHash for Tap {
    fn hash(&self, hasher: &mut gantz_ca::Hasher) {
        hasher.update(b"gantz.plyphon.tap");
        hasher.update(&self.size.to_le_bytes());
    }
}

impl gantz_core::Node for Tap {
    fn n_inputs(&self, _ctx: MetaCtx) -> usize {
        // Input 0 is the audio signal (a dsp edge); input 1 is the control trigger
        // that reads the buffer out the outlet.
        2
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
            SteelVal::ListV(Default::default())
        })
        .unwrap()
    }

    fn expr(&self, _ctx: ExprCtx<'_, '_>) -> ExprResult {
        // Stateful: the ring buffer (maintained by the audio driver) IS the output.
        // On a control trigger push, surface the current samples and leave the ring
        // unchanged. The dsp input (index 0) is a dsp edge, ignored here.
        gantz_core::node::parse_expr("state")
    }
}

impl NodeDsp for Tap {
    fn n_dsp_inputs(&self) -> usize {
        1
    }

    fn n_dsp_outputs(&self) -> usize {
        // A tap sink: it reads the signal, it does not pass it through.
        0
    }

    fn is_monitor(&self) -> bool {
        true
    }

    fn ugens(&self, path: &[usize], inputs: &[InputRef], b: &mut DspBuilder) -> Vec<InputRef> {
        let sig = inputs.first().copied().unwrap_or(InputRef::Constant(0.0));
        // `ScopeOut.ar(bufnum, sig)`: stream *every* sample of `sig` off the audio
        // thread into a cued scope stream the driver drains into this node's ring.
        // `bufnum` is a placeholder here; the driver allocates a globally-unique cued
        // index and patches it into this unit before installing the def.
        let scope_unit = b.push_unit(UnitSpec::new(
            "ScopeOut",
            Rate::Audio,
            vec![InputRef::Constant(0.0), sig],
            0,
        ));
        b.push_monitor(path, self.size, scope_unit as usize);
        vec![]
    }
}

impl ToNodeDsp for Tap {
    fn to_node_dsp(&self) -> Option<&dyn NodeDsp> {
        Some(self)
    }
}

impl NodeUi for Tap {
    fn name(&self, _: &dyn Registry) -> &str {
        "~tap"
    }

    fn description(&self) -> Option<&'static str> {
        Some("Tap a dsp signal into a ring buffer; read it out on a trigger")
    }

    fn ui(&mut self, _ctx: NodeCtx, uictx: egui_graph::NodeCtx) -> NodeUiResponse {
        // The body shows just the node name; the buffered samples surface via the
        // outlet (into a `plot`), and the ring length is edited in the inspector.
        let framed =
            uictx.framed(|ui, _sockets| ui.add(egui::Label::new("~tap").selectable(false)));
        NodeUiResponse::new(framed)
    }

    fn inspector_rows(
        &mut self,
        _ctx: &mut NodeCtx,
        body: &mut egui_extras::TableBody,
    ) -> InspectorRowsResponse {
        // The ring length lives in the node weight (structural: it re-derives the
        // monitor binding, but not the synthdef signature, so no respawn).
        let mut resp = InspectorRowsResponse::default();
        let mut size = self.size;
        let dv = egui::DragValue::new(&mut size)
            .range(1..=16_384)
            .speed(1.0)
            .suffix(" samples");
        if value_row(body, "size", dv) {
            self.size = size.max(1);
            resp.mark_changed();
        }
        resp
    }

    fn socket_doc(&self, _: &dyn Registry, kind: SocketKind, ix: usize) -> Option<SocketDoc> {
        match (kind, ix) {
            (SocketKind::Input, 0) => Some(
                SocketDoc::ty("audio").with_description("signal to sample into the ring buffer"),
            ),
            (SocketKind::Input, 1) => Some(
                SocketDoc::ty("bang")
                    .with_description("trigger: output the buffered samples as a list"),
            ),
            (SocketKind::Output, _) => Some(
                SocketDoc::ty("list")
                    .with_description("the buffered samples (oldest first), on a trigger"),
            ),
            _ => None,
        }
    }
}

fn default_size() -> usize {
    Tap::DEFAULT_SIZE
}
