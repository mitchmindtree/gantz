//! The DSP domain's `Ref` extension: the per-reference `inline` flag (#295).
//!
//! [`DspRefExt`] is stored in the referenced node's ext slot (see
//! [`Ref::set_ext`](gantz_core::node::Ref::set_ext)) under
//! [`DSP_REF_EXT_KEY`], only when non-default so a default-configured
//! reference keeps its address. `crate::egui`'s `DspRefExtUi` (`egui` feature)
//! renders the inspector toggle for references whose graph contains DSP
//! nodes.

use crate::ToNodeDsp;
use gantz_ca::{ContentAddr, GraphAddr};
use gantz_core::data::ReifiedGraphs;
use gantz_core::node::AsRefNode;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// The ext key under which [`DspRefExt`] is stored.
pub const DSP_REF_EXT_KEY: &str = "plyphon.dsp-ref";

/// The DSP domain's per-reference options.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct DspRefExt {
    /// Inline the referenced graph's DSP nodes into the parent synthdef,
    /// rather than instancing a shared definition.
    ///
    /// Instancing is the default lowering: the child derives once into shared
    /// content-named synthdefs, installed once and spawned per instance with
    /// bus wiring set per synth. Inlining splices the child's units into the
    /// parent's def instead - each copy compiles its own units, buying full
    /// cross-boundary fusion at N-times the unit count.
    pub inline: bool,
}

/// The graph addresses (as [`ContentAddr`]s, the form
/// [`Ref::content_addr`](gantz_core::node::Ref::content_addr) reports) of
/// every registry graph containing DSP nodes, directly or transitively
/// through references.
///
/// Requires the concrete node type: typed probes like [`ToNodeDsp`] are
/// unreachable through the GUI's erased registry, so callers (e.g. a bevy
/// provider system) compute this where `N` is known - resolving typed graphs
/// through the reified cache - and hand the set to `DspRefExtUi`. Graphs
/// missing from the cache classify as non-DSP.
pub fn dsp_graphs<N>(
    registry: &gantz_ca::Registry,
    reified: &ReifiedGraphs<N>,
) -> HashSet<ContentAddr>
where
    N: ToNodeDsp + AsRefNode,
{
    let mut memo: HashMap<GraphAddr, bool> = HashMap::new();
    registry
        .graphs()
        .keys()
        .copied()
        .filter(|&ga| is_dsp_graph(reified, ga, &mut memo))
        .map(ContentAddr::from)
        .collect()
}

/// Whether the graph at `ga` contains a DSP node, directly or transitively
/// through references. Memoized in `memo` so repeated probes over one
/// registry (e.g. per ref during a flatten) stay linear overall.
pub(crate) fn is_dsp_graph<N>(
    reified: &ReifiedGraphs<N>,
    ga: GraphAddr,
    memo: &mut HashMap<GraphAddr, bool>,
) -> bool
where
    N: ToNodeDsp + AsRefNode,
{
    let mut stack: Vec<GraphAddr> = Vec::new();
    is_dsp(reified, ga, memo, &mut stack)
}

/// Whether the graph at `ga` contains a DSP node, directly or transitively
/// through references. Memoized per graph; reference cycles are treated as
/// non-DSP at the point of re-entry (a cycle cannot introduce a DSP node that
/// its members do not already contain).
fn is_dsp<N>(
    reified: &ReifiedGraphs<N>,
    ga: GraphAddr,
    memo: &mut HashMap<GraphAddr, bool>,
    stack: &mut Vec<GraphAddr>,
) -> bool
where
    N: ToNodeDsp + AsRefNode,
{
    if let Some(&known) = memo.get(&ga) {
        return known;
    }
    if stack.contains(&ga) {
        return false;
    }
    let Some(graph) = reified.get(&ga) else {
        memo.insert(ga, false);
        return false;
    };
    stack.push(ga);
    let dsp = graph
        .node_indices()
        .any(|ix| graph[ix].to_node_dsp().is_some())
        || graph.node_indices().any(|ix| {
            graph[ix]
                .as_ref_node()
                .is_some_and(|r| is_dsp(reified, r.content_addr().into(), memo, stack))
        });
    stack.pop();
    memo.insert(ga, dsp);
    dsp
}
