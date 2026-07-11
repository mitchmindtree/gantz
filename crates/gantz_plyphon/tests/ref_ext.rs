//! Tests for `dsp_commits` - the DSP-graph discovery backing the `inline`
//! ref-extension UI - and for the `DspRefExt`-driven lowering decision in
//! `flatten_from_registry`.

use gantz_ca::{CaHash, ContentAddr};
use gantz_core::node::graph::Graph;
use gantz_core::node::{AsRefNode, ExprCtx, ExprResult, MetaCtx, Ref, parse_expr};
use gantz_plyphon::{NodeDsp, SinOsc, ToNodeDsp, dsp_commits};

/// A minimal node standing in for the app's node set: one DSP node, the
/// reference node, boundary nodes and a non-DSP stand-in.
#[derive(Clone)]
enum N {
    SinOsc(SinOsc),
    Ref(Ref),
    Inlet,
    Outlet,
    Other,
}

impl ToNodeDsp for N {
    fn to_node_dsp(&self) -> Option<&dyn NodeDsp> {
        match self {
            N::SinOsc(s) => Some(s),
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

    fn n_outputs(&self, _ctx: MetaCtx) -> usize {
        1
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

/// Commit `graph` under `name`, returning its commit address as a
/// `ContentAddr` (the form `Ref::content_addr` reports).
fn commit(registry: &mut gantz_ca::Registry<Graph<N>>, name: &str, graph: Graph<N>) -> ContentAddr {
    let now = std::time::Duration::from_secs(1);
    let addr = gantz_ca::graph_addr(&graph);
    let ca = registry.commit_graph(now, None, addr, || graph);
    registry.insert_name(name.to_string(), ca);
    ca.into()
}

fn ref_node(ca: ContentAddr) -> N {
    N::Ref(Ref::new(ca))
}

/// `dsp_commits` finds directly-DSP graphs and graphs that only reach DSP
/// transitively through references, and excludes non-DSP graphs and
/// references to missing addresses.
#[test]
fn dsp_commits_discovers_direct_and_transitive() {
    let mut registry = gantz_ca::Registry::<Graph<N>>::default();

    // A graph containing a DSP node directly.
    let mut dsp: Graph<N> = Graph::default();
    dsp.add_node(N::SinOsc(SinOsc::default()));
    let dsp_ca = commit(&mut registry, "dsp", dsp);

    // A wrapper that only references the DSP graph.
    let mut wrapper: Graph<N> = Graph::default();
    wrapper.add_node(ref_node(dsp_ca));
    let wrapper_ca = commit(&mut registry, "wrapper", wrapper);

    // A second wrapper, two hops from the DSP node.
    let mut wrapper2: Graph<N> = Graph::default();
    wrapper2.add_node(ref_node(wrapper_ca));
    let wrapper2_ca = commit(&mut registry, "wrapper2", wrapper2);

    // A plain control graph.
    let mut plain: Graph<N> = Graph::default();
    plain.add_node(N::Other);
    let plain_ca = commit(&mut registry, "plain", plain);

    // A graph referencing a missing address (defensive: must not panic).
    let mut dangling: Graph<N> = Graph::default();
    dangling.add_node(ref_node(ContentAddr::from([9u8; 32])));
    let dangling_ca = commit(&mut registry, "dangling", dangling);

    let set = dsp_commits(&registry);
    assert!(set.contains(&dsp_ca));
    assert!(set.contains(&wrapper_ca), "one hop through a ref");
    assert!(set.contains(&wrapper2_ca), "two hops through refs");
    assert!(!set.contains(&plain_ca));
    assert!(!set.contains(&dangling_ca));
}

/// The lowering decision in `flatten_from_registry`: a DSP-bearing child
/// instances by default, its `DspRefExt { inline: true }` ext opts back into
/// splicing, and non-DSP children (including pure wire children) always
/// splice.
#[test]
fn default_lowering_instances_dsp_refs_and_splices_the_rest() {
    use gantz_plyphon::{DSP_REF_EXT_KEY, DspRefExt, Flat};

    let mut registry = gantz_ca::Registry::<Graph<N>>::default();

    // A DSP-bearing child.
    let mut dsp: Graph<N> = Graph::default();
    dsp.add_node(N::SinOsc(SinOsc::default()));
    let dsp_ca = commit(&mut registry, "dsp", dsp);

    // A pure wire child: bridges signals but contains no DSP.
    let mut wire: Graph<N> = Graph::default();
    let i = wire.add_node(N::Inlet);
    let o = wire.add_node(N::Outlet);
    wire.add_edge(i, o, gantz_core::Edge::new(0.into(), 0.into()));
    let wire_ca = commit(&mut registry, "wire", wire);

    // The head: a default DSP ref, an inline-flagged DSP ref, a wire ref.
    let mut inline_ref = Ref::new(dsp_ca);
    inline_ref
        .set_ext(DSP_REF_EXT_KEY, &DspRefExt { inline: true })
        .expect("datum-representable");
    let mut head: Graph<N> = Graph::default();
    head.add_node(ref_node(dsp_ca));
    head.add_node(N::Ref(inline_ref));
    head.add_node(ref_node(wire_ca));

    let flat = gantz_plyphon::flatten_from_registry(&head, &registry).expect("flatten");
    let markers: Vec<_> = flat
        .node_indices()
        .filter(|&n| matches!(flat[n], Flat::Instance { .. }))
        .collect();
    assert_eq!(markers.len(), 1, "only the default DSP ref instances");
    assert!(
        matches!(flat[markers[0]], Flat::Instance { child_ca, .. } if child_ca == dsp_ca),
        "the marker carries the DSP child's address",
    );
    // The inline-flagged ref spliced its sine; the wire child dissolved.
    let sines = flat
        .node_indices()
        .filter(|&n| {
            matches!(
                &flat[n],
                Flat::Node {
                    node: N::SinOsc(_),
                    ..
                }
            )
        })
        .count();
    assert_eq!(sines, 1, "the inline ref splices the child's sine");
}
