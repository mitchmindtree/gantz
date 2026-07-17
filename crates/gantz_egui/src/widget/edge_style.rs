//! The domain seam for styling graph-scene edges (see [`EdgeStyle`]).

use gantz_core::node;

/// A domain-supplied edge styler - the edge analogue of
/// [`RefExtUi`][crate::node::RefExtUi].
///
/// Domains style the edges of the graph scene by supplying implementations to
/// the [`Gantz`][super::Gantz] widget via
/// [`Gantz::edge_styles`][super::Gantz::edge_styles]. Implementations
/// self-gate: return `None` for edges (or heads) the domain has no interest
/// in, leaving the default egui-visuals styling. When several stylers are
/// supplied, the first `Some` wins (supply order).
///
/// Styling affects painting only - edge interaction (hover, selection,
/// deletion, context menu) is identical for styled and unstyled edges.
pub trait EdgeStyle {
    /// The styling for the given edge, or `None` for the default.
    fn edge_styling(&self, ctx: &EdgeStyleCtx) -> Option<EdgeStyling>;
}

/// What the widget knows about an edge when asking for its styling.
///
/// `#[non_exhaustive]` so future context (e.g. node state, evaluation
/// timing) reaches stylers without breaking implementors: constructed via
/// [`EdgeStyleCtx::new`] (by the widget, and by downstream styler tests),
/// with any future fields defaulting there.
#[non_exhaustive]
pub struct EdgeStyleCtx<'a> {
    /// The head whose root graph is being viewed. Nested graphs are separate
    /// heads, so the root-level node ids below fully identify the endpoints.
    pub head: &'a gantz_ca::Head,
    /// The edge's source: root-level node id and output port.
    pub src: (node::Id, usize),
    /// The edge's destination: root-level node id and input port.
    pub dst: (node::Id, usize),
}

impl<'a> EdgeStyleCtx<'a> {
    /// The context for the edge from `src` to `dst` in `head`'s root graph.
    pub fn new(head: &'a gantz_ca::Head, src: (node::Id, usize), dst: (node::Id, usize)) -> Self {
        Self { head, src, dst }
    }
}

/// A declarative edge style, interpreted by the graph scene's painter.
///
/// `#[non_exhaustive]` so styling capabilities can grow without breaking
/// constructors: start from [`EdgeStyling::default`] and set fields.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct EdgeStyling {
    /// Colour for the default (unselected, unhovered) state. Hovered and
    /// selected edges keep the theme strokes so those affordances stay
    /// consistent across all edges.
    pub color: Option<egui::Color32>,
    /// A multiplier on the theme stroke width, applied in every state so the
    /// edge keeps its weight when hovered or selected. `1.0` leaves the width
    /// unchanged. The strand spacing and notch scale with it.
    pub width_scale: f32,
    /// Dash and gap lengths in graph units. Solid when `None`. A dash length
    /// close to the stroke width reads as dotted.
    pub dash: Option<(f32, f32)>,
    /// The number of parallel strands to paint, e.g. a channel count. Counts
    /// above a painter cap still draw the cap-width band, overlaid with
    /// diagonal wrap stripes that read as a thick bound bundle rather than
    /// adding ever-thinner strands (see [`graph_scene`][super::graph_scene]).
    pub strands: usize,
    /// A notched-cord texture: even dashes (equal dash and gap of this
    /// length, graph units) painted over the line in the theme's extreme
    /// background colour, or `None` for none. The notches stay within the
    /// line's own width - a theme-neutral cue independent of the line colour.
    pub notch: Option<f32>,
    /// Hover tooltip text, e.g. `"2ch ar"`.
    pub hover_text: Option<String>,
}

impl Default for EdgeStyling {
    fn default() -> Self {
        Self {
            color: None,
            width_scale: 1.0,
            dash: None,
            strands: 1,
            notch: None,
            hover_text: None,
        }
    }
}

/// The styling for the given edge: the first `Some` among `styles`, in
/// supply order, or `None` when no styler claims the edge.
pub fn edge_styling(styles: &[&dyn EdgeStyle], ctx: &EdgeStyleCtx) -> Option<EdgeStyling> {
    styles.iter().find_map(|s| s.edge_styling(ctx))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Claims edges out of the given source node with the given colour.
    struct StubStyle {
        src_node: node::Id,
        color: egui::Color32,
    }

    impl EdgeStyle for StubStyle {
        fn edge_styling(&self, ctx: &EdgeStyleCtx) -> Option<EdgeStyling> {
            (ctx.src.0 == self.src_node).then(|| EdgeStyling {
                color: Some(self.color),
                ..Default::default()
            })
        }
    }

    /// Stylers self-gate, the first `Some` wins, and unclaimed edges yield
    /// `None` (the default styling).
    #[test]
    fn first_claiming_styler_wins() {
        let red = egui::Color32::RED;
        let blue = egui::Color32::BLUE;
        let a = StubStyle {
            src_node: 0,
            color: red,
        };
        let b = StubStyle {
            src_node: 0,
            color: blue,
        };
        let c = StubStyle {
            src_node: 1,
            color: blue,
        };
        let styles: [&dyn EdgeStyle; 3] = [&a, &b, &c];
        let head = gantz_ca::Head::Branch("test".parse().unwrap());
        let ctx = |src_node| EdgeStyleCtx {
            head: &head,
            src: (src_node, 0),
            dst: (2, 0),
        };
        // Both `a` and `b` claim node 0's edges: supply order breaks the tie.
        assert_eq!(edge_styling(&styles, &ctx(0)).unwrap().color, Some(red));
        // Only `c` claims node 1's edges.
        assert_eq!(edge_styling(&styles, &ctx(1)).unwrap().color, Some(blue));
        // No styler claims node 2's edges.
        assert!(edge_styling(&styles, &ctx(2)).is_none());
    }
}
