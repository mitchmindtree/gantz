//! Tests for `root_port_info` - the root-level DSP port classification
//! behind DSP edge styling (#300).

use gantz_ca::{CaHash, ContentAddr};
use gantz_core::Edge;
use gantz_core::node::graph::Graph;
use gantz_core::node::{AsRefNode, ExprCtx, ExprResult, MetaCtx, Ref, parse_expr};
use gantz_plyphon::{Lag, NodeDsp, Out, PortShape, PortShapes, SinOsc, ToNodeDsp, root_port_info};
use plyphon::Rate;

/// A minimal node standing in for the app's node set.
#[derive(Clone)]
enum N {
    SinOsc(SinOsc),
    Lag(Lag),
    Out(Out),
    Ref(Ref),
    Inlet,
    Outlet,
    /// A non-DSP control node (e.g. a slider) with one output.
    Other,
}

impl ToNodeDsp for N {
    fn to_node_dsp(&self) -> Option<&dyn NodeDsp> {
        match self {
            N::SinOsc(s) => Some(s),
            N::Lag(l) => Some(l),
            N::Out(o) => Some(o),
            N::Ref(_) | N::Inlet | N::Outlet | N::Other => None,
        }
    }
}

impl AsRefNode for N {
    fn as_ref_node(&self) -> Option<&Ref> {
        match self {
            N::Ref(r) => Some(r),
            _ => None,
        }
    }
}

impl gantz_core::Node for N {
    fn expr(&self, _ctx: ExprCtx<'_, '_>) -> ExprResult {
        parse_expr("'()")
    }

    fn n_inputs(&self, ctx: MetaCtx) -> usize {
        match self {
            N::SinOsc(_) | N::Lag(_) | N::Outlet => 1,
            N::Out(o) => gantz_core::Node::n_inputs(o, ctx),
            N::Ref(_) | N::Inlet | N::Other => 0,
        }
    }

    fn n_outputs(&self, _ctx: MetaCtx) -> usize {
        match self {
            N::SinOsc(_) | N::Lag(_) | N::Inlet | N::Other => 1,
            N::Out(_) | N::Ref(_) | N::Outlet => 0,
        }
    }

    fn inlet(&self, _ctx: MetaCtx) -> bool {
        matches!(self, N::Inlet)
    }

    fn outlet(&self, _ctx: MetaCtx) -> bool {
        matches!(self, N::Outlet)
    }
}

impl CaHash for N {
    fn hash(&self, hasher: &mut gantz_ca::Hasher) {
        match self {
            N::SinOsc(s) => CaHash::hash(s, hasher),
            N::Lag(l) => CaHash::hash(l, hasher),
            N::Out(o) => CaHash::hash(o, hasher),
            N::Ref(r) => CaHash::hash(r, hasher),
            N::Inlet => {
                hasher.update(b"test.inlet");
            }
            N::Outlet => {
                hasher.update(b"test.outlet");
            }
            N::Other => {
                hasher.update(b"test.other");
            }
        }
    }
}

/// Commit `graph`, returning its commit address as a `ContentAddr` (the form
/// `Ref::content_addr` reports).
fn commit(registry: &mut gantz_ca::Registry<Graph<N>>, graph: Graph<N>) -> ContentAddr {
    let now = std::time::Duration::from_secs(1);
    let addr = gantz_ca::graph_addr(&graph);
    registry.commit_graph(now, None, addr, || graph).into()
}

fn shape(width: usize, rate: Rate) -> PortShape {
    PortShape { width, rate }
}

fn shapes<const M: usize>(entries: [(&[usize], usize, PortShape); M]) -> PortShapes {
    entries
        .into_iter()
        .map(|(path, port, shape)| ((path.to_vec(), port), shape))
        .collect()
}

/// Concrete DSP nodes classify their leading dsp ports; control inputs
/// beyond `n_dsp_inputs` and non-DSP nodes stay unclassified.
#[test]
fn flat_graph_classifies_dsp_ports() {
    let registry = gantz_ca::Registry::<Graph<N>>::default();
    let mut g: Graph<N> = Graph::default();
    let sin = g.add_node(N::SinOsc(SinOsc::default()));
    let out = g.add_node(N::Out(Out::default()));
    let slider = g.add_node(N::Other);
    let slider2 = g.add_node(N::Other);
    g.add_edge(sin, out, Edge::new(0.into(), 0.into()));
    // Control connections: `~out`'s gain (input 1) and `~sinosc`'s hybrid freq.
    g.add_edge(slider, out, Edge::new(0.into(), 1.into()));
    g.add_edge(slider2, sin, Edge::new(0.into(), 0.into()));

    let shapes = shapes([(&[0], 0, shape(1, Rate::Audio))]);
    let info = root_port_info(&g, &registry, &shapes);

    // `~sinosc`'s hybrid input and `~out`'s signal input are signal inputs.
    let sins: Vec<_> = info.signal_inputs.iter().copied().collect();
    assert_eq!(sins, vec![(sin.index(), 0), (out.index(), 0)]);
    // Only `~sinosc`'s output is a signal output (`~out` has no dsp outputs),
    // carrying its recorded shape. The sliders are unclassified.
    let souts: Vec<_> = info.signal_outputs.iter().collect();
    assert_eq!(
        souts,
        vec![(&(sin.index(), 0), &Some(shape(1, Rate::Audio)))],
    );
}

/// A reference's ports classify through the referenced graph's boundaries,
/// and its output shapes resolve through absolutized `PortShapes` keys.
#[test]
fn ref_ports_classify_through_child() {
    let mut registry = gantz_ca::Registry::<Graph<N>>::default();

    // Child: inlet -> ~lag -> outlet.
    let mut child: Graph<N> = Graph::default();
    let i = child.add_node(N::Inlet);
    let l = child.add_node(N::Lag(Lag::default()));
    let o = child.add_node(N::Outlet);
    child.add_edge(i, l, Edge::new(0.into(), 0.into()));
    child.add_edge(l, o, Edge::new(0.into(), 0.into()));
    let ca = commit(&mut registry, child);

    // Root: ~sinosc -> ref -> ~out.
    let mut g: Graph<N> = Graph::default();
    let sin = g.add_node(N::SinOsc(SinOsc::default()));
    let r = g.add_node(N::Ref(Ref::new(ca)));
    let out = g.add_node(N::Out(Out::default()));
    g.add_edge(sin, r, Edge::new(0.into(), 0.into()));
    g.add_edge(r, out, Edge::new(0.into(), 0.into()));

    // The lag inside the ref keys its shape by its absolute path `[1, 1]`.
    let shapes = shapes([
        (&[0], 0, shape(1, Rate::Audio)),
        (&[1, 1], 0, shape(2, Rate::Audio)),
    ]);
    let info = root_port_info(&g, &registry, &shapes);

    assert!(info.signal_inputs.contains(&(r.index(), 0)));
    assert!(info.signal_inputs.contains(&(out.index(), 0)));
    assert_eq!(
        info.signal_outputs.get(&(r.index(), 0)),
        Some(&Some(shape(2, Rate::Audio))),
    );
}

/// A signal-classified reference output with no recorded shape (e.g. the
/// head derived silent) still classifies, with an unknown shape.
#[test]
fn ref_output_without_recorded_shape_is_signal_with_none() {
    let mut registry = gantz_ca::Registry::<Graph<N>>::default();
    let mut child: Graph<N> = Graph::default();
    let s = child.add_node(N::SinOsc(SinOsc::default()));
    let o = child.add_node(N::Outlet);
    child.add_edge(s, o, Edge::new(0.into(), 0.into()));
    let ca = commit(&mut registry, child);

    let mut g: Graph<N> = Graph::default();
    let r = g.add_node(N::Ref(Ref::new(ca)));

    let info = root_port_info(&g, &registry, &PortShapes::default());
    assert_eq!(info.signal_outputs.get(&(r.index(), 0)), Some(&None));
}

/// Reference output shapes resolve through nested references, composing the
/// relative paths into the absolutized key.
#[test]
fn nested_ref_paths_compose() {
    let mut registry = gantz_ca::Registry::<Graph<N>>::default();

    // Inner: ~sinosc -> outlet.
    let mut inner: Graph<N> = Graph::default();
    let s = inner.add_node(N::SinOsc(SinOsc::default()));
    let o = inner.add_node(N::Outlet);
    inner.add_edge(s, o, Edge::new(0.into(), 0.into()));
    let inner_ca = commit(&mut registry, inner);

    // Outer: ref(inner) -> outlet.
    let mut outer: Graph<N> = Graph::default();
    let ri = outer.add_node(N::Ref(Ref::new(inner_ca)));
    let oo = outer.add_node(N::Outlet);
    outer.add_edge(ri, oo, Edge::new(0.into(), 0.into()));
    let outer_ca = commit(&mut registry, outer);

    // Root: ref(outer) at index 0. The sine's absolute path is [0, 0, 0].
    let mut g: Graph<N> = Graph::default();
    let r = g.add_node(N::Ref(Ref::new(outer_ca)));

    let shapes = shapes([(&[0, 0, 0], 0, shape(2, Rate::Audio))]);
    let info = root_port_info(&g, &registry, &shapes);
    assert_eq!(
        info.signal_outputs.get(&(r.index(), 0)),
        Some(&Some(shape(2, Rate::Audio))),
    );
}

/// Root-level boundary nodes forward classification: an inlet feeding a
/// signal input classifies as a signal source, and an outlet fed by a
/// signal output classifies as a signal consumer (nested views style their
/// interface edges).
#[test]
fn root_boundaries_forward_classification() {
    let registry = gantz_ca::Registry::<Graph<N>>::default();
    let mut g: Graph<N> = Graph::default();
    let i = g.add_node(N::Inlet);
    let l = g.add_node(N::Lag(Lag::default()));
    let o = g.add_node(N::Outlet);
    g.add_edge(i, l, Edge::new(0.into(), 0.into()));
    g.add_edge(l, o, Edge::new(0.into(), 0.into()));

    let info = root_port_info(&g, &registry, &PortShapes::default());
    assert_eq!(info.signal_outputs.get(&(i.index(), 0)), Some(&None));
    assert!(info.signal_inputs.contains(&(l.index(), 0)));
    assert!(info.signal_inputs.contains(&(o.index(), 0)));

    // A pure inlet -> outlet wire carries no DSP: neither side classifies.
    let mut wire: Graph<N> = Graph::default();
    let wi = wire.add_node(N::Inlet);
    let wo = wire.add_node(N::Outlet);
    wire.add_edge(wi, wo, Edge::new(0.into(), 0.into()));
    let info = root_port_info(&wire, &registry, &PortShapes::default());
    assert!(info.signal_inputs.is_empty());
    assert!(info.signal_outputs.is_empty());
}

/// A reference to a missing commit classifies as control without panicking.
#[test]
fn dangling_ref_is_control() {
    let registry = gantz_ca::Registry::<Graph<N>>::default();
    let mut g: Graph<N> = Graph::default();
    let r = g.add_node(N::Ref(Ref::new(ContentAddr::from([9u8; 32]))));
    let sin = g.add_node(N::SinOsc(SinOsc::default()));
    g.add_edge(sin, r, Edge::new(0.into(), 0.into()));

    let info = root_port_info(&g, &registry, &PortShapes::default());
    assert!(!info.signal_inputs.contains(&(r.index(), 0)));
    assert!(!info.signal_outputs.contains_key(&(r.index(), 0)));
}

/// A multi-fed reference outlet sums its sources' shapes: the width is the
/// widest summand and audio rate dominates (mirroring derivation).
#[test]
fn multi_fed_ref_outlet_sums_shapes() {
    let mut registry = gantz_ca::Registry::<Graph<N>>::default();

    // Child: two sources feeding the one outlet.
    let mut child: Graph<N> = Graph::default();
    let a = child.add_node(N::SinOsc(SinOsc::default()));
    let b = child.add_node(N::Lag(Lag::default()));
    let o = child.add_node(N::Outlet);
    child.add_edge(a, o, Edge::new(0.into(), 0.into()));
    child.add_edge(b, o, Edge::new(0.into(), 0.into()));
    let ca = commit(&mut registry, child);

    let mut g: Graph<N> = Graph::default();
    let r = g.add_node(N::Ref(Ref::new(ca)));

    let shapes = shapes([
        (&[0, 0], 0, shape(2, Rate::Control)),
        (&[0, 1], 0, shape(1, Rate::Audio)),
    ]);
    let info = root_port_info(&g, &registry, &shapes);
    assert_eq!(
        info.signal_outputs.get(&(r.index(), 0)),
        Some(&Some(shape(2, Rate::Audio))),
    );
}
