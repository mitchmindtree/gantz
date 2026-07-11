//! The [`NodeUi`] implementations for the DSP node set.
//!
//! Node *behaviour* (fields, `Node`, `NodeDsp`) lives in [`crate::node`];
//! only the egui surface lives here, reaching the nodes through their public
//! accessors.

use crate::node::{Bus, Lag, Out, Pack, ScopeOut, SinOsc, Unpack};
use crate::param::{param_state, param_value, with_value};
use crate::ui::param::{param_row, param_state_row, rate_row, value_row};
use gantz_core::steel::SteelVal;
use gantz_egui::{
    InspectorRowsResponse, NodeCtx, NodeUi, NodeUiResponse, Registry, SocketDoc, SocketKind,
};

impl NodeUi for Bus {
    fn name(&self, _: &dyn Registry) -> &str {
        "~bus"
    }

    fn description(&self) -> Option<&'static str> {
        Some("Synthdef boundary: edits beyond it leave this side's synth running")
    }

    fn ui(&mut self, _ctx: NodeCtx, uictx: egui_graph::NodeCtx) -> NodeUiResponse {
        let framed =
            uictx.framed(|ui, _sockets| ui.add(egui::Label::new("~bus").selectable(false)));
        NodeUiResponse::new(framed)
    }

    fn socket_doc(&self, _: &dyn Registry, kind: SocketKind, _ix: usize) -> Option<SocketDoc> {
        match kind {
            SocketKind::Input => Some(SocketDoc::ty("signal").with_description(
                "signal to carry across the synthdef boundary (any channel width)",
            )),
            SocketKind::Output => Some(
                SocketDoc::ty("signal")
                    .with_description("the same signal, entering the downstream synthdef"),
            ),
        }
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
        let mut rate = self.rate();
        if rate_row(body, &mut rate) {
            self.set_rate(rate);
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
        let mut lag = self.gain_lag();
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
            self.set_gain_lag(lag);
            resp.mark_changed();
        }
        resp
    }

    fn socket_doc(&self, _: &dyn Registry, kind: SocketKind, ix: usize) -> Option<SocketDoc> {
        match (kind, ix) {
            (SocketKind::Input, 0) => Some(SocketDoc::ty("signal").with_description(
                "signal to send to the audio output; mono fans across every \
                device channel, wider signals write channel i to bus i",
            )),
            (SocketKind::Input, 1) => Some(SocketDoc::ty("number").with_description(
                "master gain control; overrides the inspector value while connected",
            )),
            _ => None,
        }
    }
}

impl NodeUi for Pack {
    fn name(&self, _: &dyn Registry) -> &str {
        "~pack"
    }

    fn description(&self) -> Option<&'static str> {
        Some("Concatenate signals into one multichannel signal (packed, not mixed)")
    }

    fn ui(&mut self, _ctx: NodeCtx, uictx: egui_graph::NodeCtx) -> NodeUiResponse {
        let framed =
            uictx.framed(|ui, _sockets| ui.add(egui::Label::new("~pack").selectable(false)));
        NodeUiResponse::new(framed)
    }

    fn inspector_rows(
        &mut self,
        _ctx: &mut NodeCtx,
        body: &mut egui_extras::TableBody,
    ) -> InspectorRowsResponse {
        let mut resp = InspectorRowsResponse::default();
        // Input count (structural: it changes the node's sockets -> respawn).
        let mut count = self.count();
        let dv = egui::DragValue::new(&mut count).range(1..=64).speed(1.0);
        if value_row(body, "count", dv) {
            self.set_count(count);
            resp.mark_changed();
        }
        resp
    }

    fn socket_doc(&self, _: &dyn Registry, kind: SocketKind, ix: usize) -> Option<SocketDoc> {
        match kind {
            SocketKind::Input => Some(SocketDoc::ty("signal").with_description(format!(
                "signal {ix} to pack (any channel width; silence when unconnected)"
            ))),
            SocketKind::Output => Some(SocketDoc::ty("signal").with_description(
                "every input's channels concatenated (width = the sum of input widths)",
            )),
        }
    }
}

impl NodeUi for ScopeOut {
    fn name(&self, _: &dyn Registry) -> &str {
        "~scopeout"
    }

    fn description(&self) -> Option<&'static str> {
        Some("Scope a signal into per-channel ring buffers; read them out on a trigger")
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
        // raw dump of the per-channel sample rings.
        false
    }

    fn inspector_rows(
        &mut self,
        ctx: &mut NodeCtx,
        body: &mut egui_extras::TableBody,
    ) -> InspectorRowsResponse {
        let mut resp = InspectorRowsResponse::default();

        // State summary: how many rings the driver has written (the tapped
        // signal's width), and how many frames each holds.
        let (frames, channels) = ctx
            .extract_value()
            .ok()
            .flatten()
            .map_or((0, 0), |v| match v {
                SteelVal::ListV(rings) => {
                    let frames = rings.iter().next().map_or(0, |r| match r {
                        SteelVal::ListV(ring) => ring.len(),
                        _ => 0,
                    });
                    (frames, rings.len())
                }
                _ => (0, 0),
            });
        let row_h = gantz_egui::widget::node_inspector::table_row_h(body.ui_mut());
        body.row(row_h, |mut row| {
            row.col(|ui| {
                ui.label("state");
            });
            row.col(|ui| {
                ui.label(format!("{frames} frames × {channels} channels"))
                    .on_hover_text("buffered dsp samples (one ring per channel)");
            });
        });

        // Ring length in frames (non-structural: not in the def; the driver caps
        // each per-channel ring at `size` frames).
        let mut size = self.size();
        let size_dv = egui::DragValue::new(&mut size)
            .range(1..=16_384)
            .speed(1.0)
            .suffix(" frames");
        if value_row(body, "size", size_dv) {
            self.set_size(size);
            resp.mark_changed();
        }

        resp
    }

    fn socket_doc(&self, _: &dyn Registry, kind: SocketKind, ix: usize) -> Option<SocketDoc> {
        match (kind, ix) {
            (SocketKind::Input, 0) => Some(SocketDoc::ty("signal").with_description(
                "signal to sample into per-channel ring buffers (any channel width)",
            )),
            (SocketKind::Input, 1) => {
                Some(SocketDoc::ty("bang").with_description("trigger: output the buffered samples"))
            }
            (SocketKind::Output, 0) => Some(
                SocketDoc::ty("list")
                    .with_description("one list of buffered samples per channel (oldest first)"),
            ),
            (SocketKind::Output, 1) => Some(
                SocketDoc::ty("number")
                    .with_description("the channel count (0 until the audio driver writes)"),
            ),
            _ => None,
        }
    }
}

impl NodeUi for SinOsc {
    fn name(&self, _: &dyn Registry) -> &str {
        "~sinosc"
    }

    fn description(&self) -> Option<&'static str> {
        Some("Sine oscillator (audio or control rate)")
    }

    fn ui(&mut self, _ctx: NodeCtx, uictx: egui_graph::NodeCtx) -> NodeUiResponse {
        // The body shows just the node name; params are edited in the inspector.
        let framed =
            uictx.framed(|ui, _sockets| ui.add(egui::Label::new("~sinosc").selectable(false)));
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
        // The value lives in VM state (a value edit must NOT change the content
        // address); the lag lives in the weight (a lag edit is structural).
        let state = ctx.extract_value().ok().flatten();
        param_state_row(body, state.as_ref());
        let mut value = state
            .as_ref()
            .and_then(param_value)
            .unwrap_or(Self::DEFAULT_FREQ as f64) as f32;
        let mut lag = self.freq_lag();
        let dv = egui::DragValue::new(&mut value)
            .range(0.0..=20_000.0)
            .speed(1.0)
            .suffix(" Hz");
        let (value_changed, lag_changed) = param_row(body, "freq", dv, &mut lag);
        if value_changed {
            // Preserve any queued `pending` updates; only the value changes.
            let prev = state.unwrap_or_else(|| param_state(Self::DEFAULT_FREQ as f64));
            let _ = ctx.update_value(with_value(prev, value as f64));
        }
        if lag_changed {
            self.set_freq_lag(lag);
            resp.mark_changed();
        }
        let mut rate = self.rate();
        if rate_row(body, &mut rate) {
            self.set_rate(rate);
            resp.mark_changed();
        }
        resp
    }

    fn socket_doc(&self, _: &dyn Registry, kind: SocketKind, ix: usize) -> Option<SocketDoc> {
        match (kind, ix) {
            (SocketKind::Input, 0) => Some(SocketDoc::ty("number").with_description(
                "frequency (Hz) control; overrides the inspector value while connected",
            )),
            (SocketKind::Output, _) => Some(SocketDoc::ty("signal").with_description(
                "sine signal at the configured frequency (and the configured ar/kr rate)",
            )),
            _ => None,
        }
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
        let mut count = self.count();
        let dv = egui::DragValue::new(&mut count).range(1..=64).speed(1.0);
        if value_row(body, "count", dv) {
            self.set_count(count);
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
