//! The DSP domain's `Ref` extension: the per-reference `inline` flag (#295).
//!
//! [`DspRefExt`] is stored in the referenced node's ext slot (see
//! [`Ref::set_ext`](gantz_core::node::Ref::set_ext)) under
//! [`DSP_REF_EXT_KEY`], only when non-default so a default-configured
//! reference keeps its address. `crate::egui`'s `DspRefExtUi` (`egui` feature)
//! renders the inspector toggle for references whose graph contains DSP
//! nodes.

use crate::ToNodeDsp;
use gantz_ca::{CommitAddr, ContentAddr};
use gantz_core::node::AsRefNode;
use gantz_core::node::graph::Graph;
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
    /// Inlining is currently the only lowering, so the flag records intent
    /// until shared-synthdef instancing lands.
    pub inline: bool,
}

/// The commit addresses (as [`ContentAddr`]s, the form
/// [`Ref::content_addr`](gantz_core::node::Ref::content_addr) reports) of
/// every registry commit whose graph contains DSP nodes, directly or
/// transitively through references.
///
/// Requires the concrete node type: typed probes like [`ToNodeDsp`] are
/// unreachable through the GUI's erased registry, so callers (e.g. a bevy
/// provider system) compute this where `N` is known and hand the set to
/// `DspRefExtUi`.
pub fn dsp_commits<N>(registry: &gantz_ca::Registry<Graph<N>>) -> HashSet<ContentAddr>
where
    N: ToNodeDsp + AsRefNode,
{
    let mut memo: HashMap<CommitAddr, bool> = HashMap::new();
    let mut stack: Vec<CommitAddr> = Vec::new();
    registry
        .commits()
        .keys()
        .copied()
        .collect::<Vec<_>>()
        .into_iter()
        .filter(|&ca| is_dsp(registry, ca, &mut memo, &mut stack))
        .map(ContentAddr::from)
        .collect()
}

/// Whether the graph at `ca` contains a DSP node, directly or transitively
/// through references. Memoized per commit; reference cycles are treated as
/// non-DSP at the point of re-entry (a cycle cannot introduce a DSP node that
/// its members do not already contain).
fn is_dsp<N>(
    registry: &gantz_ca::Registry<Graph<N>>,
    ca: CommitAddr,
    memo: &mut HashMap<CommitAddr, bool>,
    stack: &mut Vec<CommitAddr>,
) -> bool
where
    N: ToNodeDsp + AsRefNode,
{
    if let Some(&known) = memo.get(&ca) {
        return known;
    }
    if stack.contains(&ca) {
        return false;
    }
    let Some(graph) = registry.commit_graph_ref(&ca) else {
        memo.insert(ca, false);
        return false;
    };
    stack.push(ca);
    let dsp = graph
        .node_indices()
        .any(|ix| graph[ix].to_node_dsp().is_some())
        || graph.node_indices().any(|ix| {
            graph[ix]
                .as_ref_node()
                .is_some_and(|r| is_dsp(registry, r.content_addr().into(), memo, stack))
        });
    stack.pop();
    memo.insert(ca, dsp);
    dsp
}
