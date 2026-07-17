//! Utilities for working with [`gantz_ca::Registry`]: node-aware
//! reachability over the content DAG.
//!
//! These wrap [`gantz_ca::closure`] with the node layer's edge reporting
//! ([`graph::out_refs`]): only this crate's callers can see inside node
//! payloads to find nested graph and blob references.

use crate::{Edge, Node, graph, node};
use gantz_ca::Name;
use petgraph::visit::{Data, IntoEdgesDirected, IntoNodeReferences, NodeIndexable, Visitable};
use std::collections::{BTreeSet, HashSet};

/// The live set reachable from ALL of the registry's heads plus the given
/// extra heads.
///
/// Suitable for pruning: everything outside the returned set is unused.
pub fn live<'a, G>(
    get_node: node::GetNode<'a>,
    reg: &gantz_ca::Registry<G>,
    extra_heads: impl IntoIterator<Item = impl std::borrow::Borrow<gantz_ca::Head>>,
) -> gantz_ca::LiveSet
where
    for<'g> &'g G: Data<EdgeWeight = Edge>
        + IntoEdgesDirected
        + IntoNodeReferences
        + NodeIndexable
        + Visitable,
    for<'g> <&'g G as Data>::NodeWeight: Node,
{
    let extra = head_seeds(reg, extra_heads);
    gantz_ca::closure(reg, extra, |g| graph::out_refs(get_node, g))
}

/// The live set reachable from ONLY the given heads.
///
/// Unlike [`live`], this does not seed from the registry's own heads - use
/// it for the minimal closure of a specific head set (e.g. single-head
/// export).
pub fn live_for_heads<'a, G>(
    get_node: node::GetNode<'a>,
    reg: &gantz_ca::Registry<G>,
    heads: impl IntoIterator<Item = impl std::borrow::Borrow<gantz_ca::Head>>,
) -> gantz_ca::LiveSet
where
    for<'g> &'g G: Data<EdgeWeight = Edge>
        + IntoEdgesDirected
        + IntoNodeReferences
        + NodeIndexable
        + Visitable,
    for<'g> <&'g G as Data>::NodeWeight: Node,
{
    let seeds = head_seeds(reg, heads);
    gantz_ca::closure_from(reg, seeds, |g| graph::out_refs(get_node, g))
}

/// Export a registry subset containing the transitive dependencies of all
/// heads plus the given extra heads.
pub fn export<'a, G>(
    get_node: node::GetNode<'a>,
    reg: &gantz_ca::Registry<G>,
    extra_heads: impl IntoIterator<Item = impl std::borrow::Borrow<gantz_ca::Head>>,
) -> gantz_ca::Registry<G>
where
    G: Clone,
    for<'g> &'g G: Data<EdgeWeight = Edge>
        + IntoEdgesDirected
        + IntoNodeReferences
        + NodeIndexable
        + Visitable,
    for<'g> <&'g G as Data>::NodeWeight: Node,
{
    gantz_ca::export(reg, &live(get_node, reg, extra_heads))
}

/// Export a registry subset containing only the transitive dependencies of
/// the given heads.
///
/// Unlike [`export`], which seeds from all heads (suitable for pruning),
/// this produces the minimal registry for a specific set of heads - only
/// the heads whose commits are transitively reachable are included.
pub fn export_heads<'a, G>(
    get_node: node::GetNode<'a>,
    reg: &gantz_ca::Registry<G>,
    heads: impl IntoIterator<Item = impl std::borrow::Borrow<gantz_ca::Head>>,
) -> gantz_ca::Registry<G>
where
    G: Clone,
    for<'g> &'g G: Data<EdgeWeight = Edge>
        + IntoEdgesDirected
        + IntoNodeReferences
        + NodeIndexable
        + Visitable,
    for<'g> <&'g G as Data>::NodeWeight: Node,
{
    gantz_ca::export(reg, &live_for_heads(get_node, reg, heads))
}

/// Find named graphs not referenced by any other graph in the registry.
///
/// A name is "root" if no graph in the registry contains a node whose
/// `required_addrs` points at the name's head graph. Returns names in
/// alphabetical order.
///
/// Note that reference identity is content identity: names whose heads
/// share identical graph content are either all root or all referenced.
pub fn root_names<'a, G>(get_node: node::GetNode<'a>, reg: &gantz_ca::Registry<G>) -> Vec<Name>
where
    for<'g> &'g G: Data<EdgeWeight = Edge>
        + IntoEdgesDirected
        + IntoNodeReferences
        + NodeIndexable
        + Visitable,
    for<'g> <&'g G as Data>::NodeWeight: Node,
{
    // Collect all graph addrs referenced by any graph in the registry.
    let mut referenced: HashSet<gantz_ca::GraphAddr> = HashSet::new();
    for graph in reg.graphs().values() {
        referenced.extend(
            graph::required_addrs(get_node, graph)
                .into_iter()
                .map(gantz_ca::GraphAddr::from),
        );
    }

    // Filter heads to those whose graph is NOT referenced.
    reg.heads()
        .filter(|(_, ca)| {
            reg.commits()
                .get(ca)
                .is_none_or(|commit| !referenced.contains(&commit.graph))
        })
        .map(|(name, _)| name.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Resolve the given heads to their commit addresses.
fn head_seeds<G>(
    reg: &gantz_ca::Registry<G>,
    heads: impl IntoIterator<Item = impl std::borrow::Borrow<gantz_ca::Head>>,
) -> Vec<gantz_ca::CommitAddr> {
    heads
        .into_iter()
        .filter_map(|head| reg.head_commit_ca(head.borrow()))
        .collect()
}
