//! The `~scopeout` node: monitor N dsp channels into a ring buffer, read out on a trigger.

use gantz_ca::CaHash;
use gantz_core::node::{Conns, EvalConf, ExprCtx, ExprResult, MetaCtx, RegCtx};
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

/// A signal *tap*: streams every sample of its `channels` dsp inputs (interleaved)
/// into a ring buffer held in VM state (the audio driver writes it, draining a
/// plyphon `ScopeOut` scope stream), and - only on a control-trigger push - outputs
/// the ring's current samples (output 0) and the channel count (output 1).
///
/// Wire the signal(s) into the dsp input(s) to monitor them, drive the trigger input
/// (after the channel inputs; e.g. with a `tick!`), and plug output 0 into a `plot`
/// (deinterleave it with output 1 for a per-channel view). `size` is the ring length
/// in *frames*; set it to 1 to monitor the latest frame. It is a dsp *sink* (no
/// passthrough); to keep hearing a signal, also wire it to `~out`. A `~peak`/`~rms`
/// node placed before a `~scopeout` gives level metering.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, NodeTag)]
pub struct ScopeOut {
    #[serde(default = "default_channels")]
    channels: usize,
    #[serde(default = "default_size")]
    size: usize,
}

impl ScopeOut {
    /// The default channel count a fresh `~scopeout` taps.
    pub const DEFAULT_CHANNELS: usize = 1;

    /// The default ring-buffer length (frames) a fresh `~scopeout` starts at.
    pub const DEFAULT_SIZE: usize = 256;

    /// The number of dsp channels tapped (interleaved into the ring).
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Set the channel count (content-address affecting; structural - changes the
    /// synthdef's `ScopeOut` input count).
    pub fn set_channels(&mut self, channels: usize) {
        self.channels = channels.max(1);
    }

    /// The ring-buffer length in frames.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Set the ring-buffer length in frames (content-address affecting).
    pub fn set_size(&mut self, size: usize) {
        self.size = size.max(1);
    }
}

impl Default for ScopeOut {
    fn default() -> Self {
        ScopeOut {
            channels: default_channels(),
            size: default_size(),
        }
    }
}

impl CaHash for ScopeOut {
    fn hash(&self, hasher: &mut gantz_ca::Hasher) {
        hasher.update(b"gantz.plyphon.scopeout");
        hasher.update(&self.channels.to_le_bytes());
        hasher.update(&self.size.to_le_bytes());
    }
}

impl gantz_core::Node for ScopeOut {
    fn n_inputs(&self, _ctx: MetaCtx) -> usize {
        // Inputs `0..channels` are the audio signals (dsp edges); input `channels` is
        // the control trigger that reads the buffer out the outlets.
        self.channels + 1
    }

    fn n_outputs(&self, _ctx: MetaCtx) -> usize {
        // Output 0 = the interleaved samples; output 1 = the channel count.
        2
    }

    fn stateful(&self, _ctx: MetaCtx) -> bool {
        true
    }

    fn branches(&self, _ctx: MetaCtx) -> Vec<EvalConf> {
        // Fire the outlets only when the control trigger is active: branch 0 activates
        // both outputs, branch 1 activates neither. Which branch the `expr` selects at
        // eval time gates whether downstream (e.g. a `plot`) evaluates - so a push
        // arriving through an inert dsp edge no longer surfaces the buffer.
        vec![
            EvalConf::Set(Conns::try_from([true, true]).unwrap()),
            EvalConf::Set(Conns::try_from([false, false]).unwrap()),
        ]
    }

    fn register(&self, mut ctx: RegCtx<'_, '_>) {
        let path = ctx.path();
        gantz_core::node::state::init_value_if_absent(ctx.vm(), path, || {
            SteelVal::ListV(Default::default())
        })
        .unwrap()
    }

    fn expr(&self, ctx: ExprCtx<'_, '_>) -> ExprResult {
        // The ring buffer (maintained by the audio driver) IS output 0; the channel
        // count is output 1. The trigger is input `channels` (after the dsp inputs);
        // emit only when it fired this eval - `branches` then gates the outlets. The
        // dsp inputs are inert here (the driver fills the ring), so they are ignored.
        let triggered = ctx.inputs().get(self.channels).is_some_and(Option::is_some);
        let src = if triggered {
            // Branch 0: both outputs active -> `(list samples channels)`.
            format!("(list 0 (list state {}))", self.channels)
        } else {
            // Branch 1: no outputs active.
            "(list 1 '())".to_string()
        };
        gantz_core::node::parse_expr(&src)
    }
}

impl NodeDsp for ScopeOut {
    fn n_dsp_inputs(&self) -> usize {
        self.channels
    }

    fn n_dsp_outputs(&self) -> usize {
        // A tap sink: it reads the signal, it does not pass it through.
        0
    }

    fn is_monitor(&self) -> bool {
        true
    }

    fn ugens(&self, path: &[usize], inputs: &[InputRef], b: &mut DspBuilder) -> Vec<InputRef> {
        // `ScopeOut.ar(bufnum, ch0, ch1, …)`: stream *every* sample of each channel
        // (interleaved) off the audio thread into a cued scope stream the driver
        // drains into this node's ring. `bufnum` is a placeholder; the driver
        // allocates a globally-unique cued index and patches it before installing.
        let mut scope_inputs = Vec::with_capacity(self.channels + 1);
        scope_inputs.push(InputRef::Constant(0.0));
        for i in 0..self.channels {
            scope_inputs.push(inputs.get(i).copied().unwrap_or(InputRef::Constant(0.0)));
        }
        let scope_unit = b.push_unit(UnitSpec::new("ScopeOut", Rate::Audio, scope_inputs, 0));
        b.push_monitor(path, self.size, self.channels, scope_unit as usize);
        vec![]
    }
}

impl ToNodeDsp for ScopeOut {
    fn to_node_dsp(&self) -> Option<&dyn NodeDsp> {
        Some(self)
    }
}

impl NodeUi for ScopeOut {
    fn name(&self, _: &dyn Registry) -> &str {
        "~scopeout"
    }

    fn description(&self) -> Option<&'static str> {
        Some("Scope dsp channels into a ring buffer; read them out on a trigger")
    }

    fn ui(&mut self, _ctx: NodeCtx, uictx: egui_graph::NodeCtx) -> NodeUiResponse {
        // The body shows just the node name; the buffered samples surface via the
        // outlet (into a `plot`), and the config is edited in the inspector.
        let framed =
            uictx.framed(|ui, _sockets| ui.add(egui::Label::new("~scopeout").selectable(false)));
        NodeUiResponse::new(framed)
    }

    fn show_state(&self) -> bool {
        // A summarised "frames × channels" state row (in `inspector_rows`) replaces the
        // raw dump of the interleaved sample list.
        false
    }

    fn inspector_rows(
        &mut self,
        ctx: &mut NodeCtx,
        body: &mut egui_extras::TableBody,
    ) -> InspectorRowsResponse {
        let mut resp = InspectorRowsResponse::default();

        // State summary: how many interleaved frames the ring holds, and its width.
        let n_samples = ctx.extract_value().ok().flatten().map_or(0, |v| match v {
            SteelVal::ListV(list) => list.len(),
            _ => 0,
        });
        let frames = n_samples / self.channels.max(1);
        let row_h = gantz_egui::widget::node_inspector::table_row_h(body.ui_mut());
        let channels = self.channels;
        body.row(row_h, |mut row| {
            row.col(|ui| {
                ui.label("state");
            });
            row.col(|ui| {
                ui.label(format!("{frames} frames × {channels} channels"))
                    .on_hover_text("buffered dsp samples (interleaved)");
            });
        });

        // Channel count (structural: it changes the ScopeOut input count → respawn).
        let mut channels = self.channels;
        let ch_dv = egui::DragValue::new(&mut channels).range(1..=64).speed(1.0);
        if value_row(body, "channels", ch_dv) {
            self.channels = channels.max(1);
            resp.mark_changed();
        }

        // Ring length in frames (non-structural: not in the def; the driver caps the
        // ring at `size * channels` samples).
        let mut size = self.size;
        let size_dv = egui::DragValue::new(&mut size)
            .range(1..=16_384)
            .speed(1.0)
            .suffix(" frames");
        if value_row(body, "size", size_dv) {
            self.size = size.max(1);
            resp.mark_changed();
        }

        resp
    }

    fn socket_doc(&self, _: &dyn Registry, kind: SocketKind, ix: usize) -> Option<SocketDoc> {
        match (kind, ix) {
            (SocketKind::Input, i) if i < self.channels => {
                let desc = if self.channels == 1 {
                    "signal to sample into the ring buffer".to_string()
                } else {
                    format!("channel {i} signal to sample into the ring buffer")
                };
                Some(SocketDoc::ty("audio").with_description(desc))
            }
            (SocketKind::Input, i) if i == self.channels => {
                Some(SocketDoc::ty("bang").with_description("trigger: output the buffered samples"))
            }
            (SocketKind::Output, 0) => Some(
                SocketDoc::ty("list")
                    .with_description("the buffered samples, interleaved (oldest first)"),
            ),
            (SocketKind::Output, 1) => {
                Some(SocketDoc::ty("number").with_description("the channel count"))
            }
            _ => None,
        }
    }
}

fn default_channels() -> usize {
    ScopeOut::DEFAULT_CHANNELS
}

fn default_size() -> usize {
    ScopeOut::DEFAULT_SIZE
}
