//! Tests for `dsp_commits`: the DSP-graph discovery backing the `inline`
//! ref-extension UI.

use gantz_ca::{CaHash, ContentAddr};
use gantz_core::node::graph::Graph;
use gantz_core::node::{ExprCtx, ExprResult, MetaCtx, parse_expr};
use gantz_egui::node::NamedRef;
use gantz_egui::sync::AsNamedRef;
use gantz_plyphon::{NodeDsp, SinOsc, ToNodeDsp, dsp_commits};

/// A minimal node standing in for the app's node set: one DSP node, the
/// reference node, and a non-DSP stand-in.
#[derive(Clone)]
enum N {
    SinOsc(SinOsc),
    Named(NamedRef),
    Other,
}

impl ToNodeDsp for N {
    fn to_node_dsp(&self) -> Option<&dyn NodeDsp> {
        match self {
            N::SinOsc(s) => Some(s),
            N::Named(_) | N::Other => None,
        }
    }
}

impl AsNamedRef for N {
    fn as_named_ref(&self) -> Option<&NamedRef> {
        match self {
            N::Named(nr) => Some(nr),
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
}

impl CaHash for N {
    fn hash(&self, hasher: &mut gantz_ca::Hasher) {
        match self {
            N::SinOsc(s) => CaHash::hash(s, hasher),
            N::Named(nr) => CaHash::hash(nr, hasher),
            N::Other => {
                hasher.update(b"test.other");
            }
        }
    }
}

/// Commit `graph` under `name`, returning its commit address as a
/// `ContentAddr` (the form `NamedRef::content_addr` reports).
fn commit(registry: &mut gantz_ca::Registry<Graph<N>>, name: &str, graph: Graph<N>) -> ContentAddr {
    let now = std::time::Duration::from_secs(1);
    let addr = gantz_ca::graph_addr(&graph);
    let ca = registry.commit_graph(now, None, addr, || graph);
    registry.insert_name(name.to_string(), ca);
    ca.into()
}

fn named(name: &str, ca: ContentAddr) -> N {
    N::Named(NamedRef::new(
        name.to_string(),
        gantz_core::node::Ref::new(ca),
    ))
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
    wrapper.add_node(named("dsp", dsp_ca));
    let wrapper_ca = commit(&mut registry, "wrapper", wrapper);

    // A second wrapper, two hops from the DSP node.
    let mut wrapper2: Graph<N> = Graph::default();
    wrapper2.add_node(named("wrapper", wrapper_ca));
    let wrapper2_ca = commit(&mut registry, "wrapper2", wrapper2);

    // A plain control graph.
    let mut plain: Graph<N> = Graph::default();
    plain.add_node(N::Other);
    let plain_ca = commit(&mut registry, "plain", plain);

    // A graph referencing a missing address (defensive: must not panic).
    let mut dangling: Graph<N> = Graph::default();
    dangling.add_node(named("gone", ContentAddr::from([9u8; 32])));
    let dangling_ca = commit(&mut registry, "dangling", dangling);

    let set = dsp_commits(&registry);
    assert!(set.contains(&dsp_ca));
    assert!(set.contains(&wrapper_ca), "one hop through a ref");
    assert!(set.contains(&wrapper2_ca), "two hops through refs");
    assert!(!set.contains(&plain_ca));
    assert!(!set.contains(&dangling_ca));
}
