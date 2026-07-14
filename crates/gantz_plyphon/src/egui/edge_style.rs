//! The DSP domain's graph-scene edge styling: signal edges rendered
//! distinctly from control edges (see [`DspEdgeStyle`]).

use std::collections::HashMap;
use std::sync::Arc;

use gantz_egui::widget::{EdgeStyle, EdgeStyleCtx, EdgeStyling};
use plyphon::Rate;

use crate::describe::rate_token;
use crate::dsp::PortShape;
use crate::port_info::RootPortInfo;

/// The DSP domain's [`EdgeStyle`]: an edge from a signal output into a
/// signal input styles by the source port's derive-time shape - one strand
/// per channel, rate-coded colour and dash, and a width/rate hover tooltip.
/// Everything else (control edges, non-DSP heads) keeps the default styling.
///
/// The per-head port classification requires the concrete node type, so a
/// provider computes it where that type is known (see
/// [`root_port_info`][crate::root_port_info]) and hands it over here.
#[derive(Debug, Default)]
pub struct DspEdgeStyle {
    /// Each open head's root port classification.
    pub heads: HashMap<gantz_ca::Head, Arc<RootPortInfo>>,
}

/// The notch spacing (graph units) of a signal edge's notched-cord texture -
/// the theme-neutral "this is a signal cord" cue: even dashes overpainted in
/// the extreme background colour. Rate stays encoded as the base dash.
const SIGNAL_NOTCH: f32 = 6.0;

/// A signal edge's stroke is slightly heavier than a control edge's, to
/// reinforce the notched cord as a signal.
const SIGNAL_WIDTH_SCALE: f32 = 1.8;

impl EdgeStyle for DspEdgeStyle {
    fn edge_styling(&self, ctx: &EdgeStyleCtx) -> Option<EdgeStyling> {
        let info = self.heads.get(ctx.head)?;
        if !info.signal_inputs.contains(&ctx.dst) {
            return None;
        }
        let shape = info.signal_outputs.get(&ctx.src)?;
        Some(dsp_edge_styling(*shape))
    }
}

/// The styling of a signal edge whose source port recorded `shape` at derive
/// time (`None`: the port is signal-classified but derivation materialized
/// nothing for it - it feeds no sink, or the head's shapes are unavailable,
/// e.g. an inlet/outlet boundary edge in a nested view).
///
/// Signal edges are distinguished from control edges by a notched-cord
/// texture, not colour. Rate is the base dash pattern (audio solid, control
/// dashed, scalar/demand dotted) and channel width is the parallel strand
/// count.
fn dsp_edge_styling(shape: Option<PortShape>) -> EdgeStyling {
    let mut styling = EdgeStyling::default();
    styling.width_scale = SIGNAL_WIDTH_SCALE;
    let Some(shape) = shape else {
        // Signal-classified but nothing materialized (a boundary edge, or a
        // port feeding no sink): the same notched cadence, but with the
        // off-segments left as gaps rather than filled - an "open" cord that
        // reads as an as-yet-unmaterialised signal.
        styling.dash = Some((SIGNAL_NOTCH, SIGNAL_NOTCH));
        styling.hover_text = Some("signal".to_string());
        return styling;
    };
    styling.notch = Some(SIGNAL_NOTCH);
    styling.strands = shape.width;
    styling.hover_text = Some(format!("{}ch {}", shape.width, rate_token(shape.rate)));
    match shape.rate {
        // Audio: solid base (notched).
        Rate::Audio => {}
        // Control: dashed base.
        Rate::Control => styling.dash = Some((6.0, 4.0)),
        // Scalar and demand: dotted base (a short dash reads as dots).
        Rate::Scalar | Rate::Demand => styling.dash = Some((1.5, 3.0)),
    }
    styling
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Signal edges style only when the source is a signal output AND the
    /// destination is a signal input of the viewed head. They distinguish
    /// themselves by a notched-cord texture, not colour, and pass the true
    /// channel count (the painter, not the styler, renders wide bundles).
    #[test]
    fn styles_signal_edges_only() {
        let mut info = RootPortInfo::default();
        info.signal_outputs.insert(
            (0, 0),
            Some(PortShape {
                width: 2,
                rate: Rate::Audio,
            }),
        );
        // A wide bus output, to check the strand count is not clamped here.
        info.signal_outputs.insert(
            (3, 0),
            Some(PortShape {
                width: 8,
                rate: Rate::Audio,
            }),
        );
        // A boundary/unmaterialised signal output (no recorded shape).
        info.signal_outputs.insert((4, 0), None);
        info.signal_inputs.insert((1, 0));
        let head = gantz_ca::Head::Branch("test".to_string());
        let style = DspEdgeStyle {
            heads: [(head.clone(), Arc::new(info))].into_iter().collect(),
        };
        let ctx = |src: (usize, usize), dst: (usize, usize)| EdgeStyleCtx::new(&head, src, dst);

        // The signal edge styles: heavier notched cord, no colour,
        // per-channel strands and the width/rate tooltip.
        let styling = style.edge_styling(&ctx((0, 0), (1, 0))).unwrap();
        assert!(styling.notch.is_some());
        assert!(styling.width_scale > 1.0);
        assert!(styling.color.is_none());
        assert_eq!(styling.strands, 2);
        assert_eq!(styling.hover_text.as_deref(), Some("2ch ar"));
        // The wide bus passes its true channel count (abridging is the
        // painter's job) and reports it in the tooltip.
        let wide = style.edge_styling(&ctx((3, 0), (1, 0))).unwrap();
        assert_eq!(wide.strands, 8);
        assert_eq!(wide.hover_text.as_deref(), Some("8ch ar"));
        // An unmaterialised signal edge (boundary/no-shape) is an "open"
        // cord: gapped dashes, no notch fill, "signal" tooltip.
        let open = style.edge_styling(&ctx((4, 0), (1, 0))).unwrap();
        assert!(open.notch.is_none());
        assert!(open.dash.is_some());
        assert_eq!(open.hover_text.as_deref(), Some("signal"));
        // A control destination (`~out`'s gain input 1) keeps the default.
        assert!(style.edge_styling(&ctx((0, 0), (1, 1))).is_none());
        // A control source into a signal input (a hybrid input's control
        // feed) keeps the default.
        assert!(style.edge_styling(&ctx((2, 0), (1, 0))).is_none());
        // An unknown head keeps the default.
        let other = gantz_ca::Head::Branch("other".to_string());
        let ctx = EdgeStyleCtx::new(&other, (0, 0), (1, 0));
        assert!(style.edge_styling(&ctx).is_none());
    }
}
