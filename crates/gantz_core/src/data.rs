//! The codec between typed nodes and the registry's erased
//! [`NodeData`]/[`DataGraph`] representation, plus the reified-graph cache.
//!
//! The registry stores graphs as plain data. Typed nodes cross that boundary
//! here: [`erase`] erases a working graph for storage and [`reify`]
//! reifies one for editing and compilation. Erasure rides the node set's
//! tag-dispatched serde (`gantz_format::impl_node_set_serde!`) through
//! [`Datum`], so the node-set manifest is the codec: a node type is storable
//! exactly when it is listed there.

use crate::node::graph::Graph;
use crate::node::{self, Node};
use crate::visit;
use gantz_ca::{
    ContentAddr, DataGraph, Datum, DatumError, GraphAddr, NodeData, Registry, SectionId, datum,
};
use petgraph::visit::EdgeRef;
use serde::{Serialize, de::DeserializeOwned};
use std::collections::{HashMap, HashSet, VecDeque};

/// An append-only cache of reified registry graphs, keyed by graph address.
///
/// Content addressing makes entries immutable: an address names exactly one
/// graph forever, so the cache never invalidates. [`ReifiedGraphs::retain_live`]
/// may drop entries to bound memory after a prune.
///
/// Intended use is two-phase: [`ReifiedGraphs::ensure`] everything a pass can
/// reach (requires `&mut self`), then serve the whole pass immutably through
/// [`ReifiedGraphs::get`] borrows (e.g. behind a `GetNode` closure).
#[derive(Debug)]
pub struct ReifiedGraphs<N> {
    graphs: HashMap<GraphAddr, Graph<N>>,
}

/// Failure to erase a node: its serde did not produce a `type`-tagged map
/// (i.e. it is not the node set's tag-dispatched serde), or errored outright.
#[derive(Clone, Debug, thiserror::Error)]
pub enum EraseNodeError {
    /// The node's own serde failed.
    #[error("node serde error: {0}")]
    Datum(#[from] DatumError),
    /// The node's serde produced a value without a `type`-tagged map.
    #[error("node serde produced a value without a `type`-tagged map")]
    Untagged,
}

/// Failure to erase one of a graph's nodes.
#[derive(Clone, Debug, thiserror::Error)]
#[error("node {node_ix}: {source}")]
pub struct EraseError {
    /// The graph index of the node that failed to erase.
    pub node_ix: usize,
    /// The node-level failure.
    #[source]
    pub source: EraseNodeError,
}

/// Failure to reify a typed node from its data form: the tag is unknown to
/// the node set, or the fields fail the node's own deserialization.
#[derive(Clone, Debug, thiserror::Error)]
#[error("node type `{tag}`: {source}")]
pub struct ReifyNodeError {
    /// The wire tag of the node that failed to decode.
    pub tag: String,
    /// The decode failure.
    #[source]
    pub source: DatumError,
}

/// Failure to reify one of a graph's nodes.
#[derive(Clone, Debug, thiserror::Error)]
#[error("node {node_ix}: {source}")]
pub struct ReifyError {
    /// The graph index of the node that failed to decode.
    pub node_ix: usize,
    /// The node-level failure.
    #[source]
    pub source: ReifyNodeError,
}

/// Failure to reify a registry graph while filling the cache.
#[derive(Clone, Debug, thiserror::Error)]
#[error("graph {graph}: {source}")]
pub struct EnsureError {
    /// The address of the registry graph that failed to reify.
    pub graph: GraphAddr,
    /// The graph-level failure.
    #[source]
    pub source: ReifyError,
}

impl<N> ReifiedGraphs<N> {
    /// An empty cache.
    pub fn new() -> Self {
        Self {
            graphs: HashMap::new(),
        }
    }

    /// The reified graph at the given address, if it has been ensured.
    pub fn get(&self, addr: &GraphAddr) -> Option<&Graph<N>> {
        self.graphs.get(addr)
    }

    /// Whether the given address has been reified.
    pub fn contains(&self, addr: &GraphAddr) -> bool {
        self.graphs.contains_key(addr)
    }

    /// Drop entries outside the given live set to bound memory after a prune.
    pub fn retain_live(&mut self, live: &gantz_ca::LiveSet) {
        self.graphs.retain(|addr, _| live.graphs.contains(addr));
    }
}

impl<N: DeserializeOwned> ReifiedGraphs<N> {
    /// Reify the given seed addresses and every graph they transitively
    /// reference.
    ///
    /// References are resolved through the stored graphs' [`NodeData::refs`]
    /// columns, a pure data walk: nothing is decoded to *find* the set.
    /// Addresses that don't resolve to registry graphs (e.g. builtin node
    /// addresses in a node's refs) are ignored, as are already-cached graphs.
    pub fn ensure(
        &mut self,
        reg: &Registry<DataGraph>,
        seeds: impl IntoIterator<Item = ContentAddr>,
    ) -> Result<(), EnsureError> {
        let mut queue: VecDeque<GraphAddr> = seeds.into_iter().map(GraphAddr::from).collect();
        while let Some(addr) = queue.pop_front() {
            if self.graphs.contains_key(&addr) {
                continue;
            }
            let Some(dg) = reg.graph(&addr) else { continue };
            queue.extend(
                dg.node_weights()
                    .flat_map(|n| n.refs.iter().copied().map(GraphAddr::from)),
            );
            let g = reify(dg).map_err(|source| EnsureError {
                graph: addr,
                source,
            })?;
            self.graphs.insert(addr, g);
        }
        Ok(())
    }

    /// Reify every graph in the registry's column, best effort.
    ///
    /// Graphs that fail to reify (e.g. an unknown tag from a domain not
    /// compiled in) are skipped and reported, and remain cache misses that
    /// lookups degrade over the same way as any missing node.
    pub fn ensure_all(&mut self, reg: &Registry<DataGraph>) -> Vec<EnsureError> {
        let mut errs = vec![];
        for (addr, dg) in reg.graphs() {
            if self.graphs.contains_key(addr) {
                continue;
            }
            match reify(dg) {
                Ok(g) => {
                    self.graphs.insert(*addr, g);
                }
                Err(source) => errs.push(EnsureError {
                    graph: *addr,
                    source,
                }),
            }
        }
        errs
    }
}

impl<N> Default for ReifiedGraphs<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Erase a typed node to data.
///
/// Runs the node's own (tag-dispatched) serde to a [`Datum`], splits the
/// `"type"` tag out, and extracts the node's direct outgoing references from
/// its own reporting ([`Node::required_addrs`]/[`Node::required_blobs`],
/// including physically nested nodes). The result is canonical, so its
/// [`NodeData::content_addr`] is the node's one network-wide address.
pub fn erase_node<N>(node: &N) -> Result<NodeData, EraseNodeError>
where
    N: Serialize + Node,
{
    let datum = datum::to_datum(node)?;
    let Datum::Map(mut entries) = datum else {
        return Err(EraseNodeError::Untagged);
    };
    let Some(ix) = entries.iter().position(|(k, _)| k == "type") else {
        return Err(EraseNodeError::Untagged);
    };
    let (_, tag) = entries.remove(ix);
    let Datum::Str(tag) = tag else {
        return Err(EraseNodeError::Untagged);
    };
    let (refs, blobs) = node_out_refs(node);
    let mut node_data = NodeData {
        tag,
        data: Datum::Map(entries),
        refs,
        blobs,
    };
    node_data.canonicalize();
    Ok(node_data)
}

/// Reify one typed node: rebuild the tagged map and run node-set serde.
pub fn reify_node<N>(node_data: &NodeData) -> Result<N, ReifyNodeError>
where
    N: DeserializeOwned,
{
    let err = |source| ReifyNodeError {
        tag: node_data.tag.clone(),
        source,
    };
    let Datum::Map(fields) = node_data.data.clone() else {
        return Err(err(serde::de::Error::custom("node data is not a map")));
    };
    // The tag leads, which is the node-set deserializer's streaming fast path.
    let datum = Datum::tagged(&node_data.tag, fields);
    datum::from_datum(datum).map_err(err)
}

/// Erase a typed graph and compute its registry address in one pass.
///
/// Registry graph addresses are ALWAYS computed on the erased form: the
/// typed node `CaHash` impls are not load-bearing for graph identity. Any
/// site that compares or mints a registry address for a typed working graph
/// goes through here (or erases first).
pub fn erase_with_addr<N>(g: &Graph<N>) -> Result<(DataGraph, GraphAddr), EraseError>
where
    N: Serialize + Node,
{
    let dg = erase(g)?;
    let addr = gantz_ca::graph_addr(&dg);
    Ok((dg, addr))
}

/// Erase a typed graph for storage: node weights through [`erase_node`],
/// indices and edges preserved verbatim.
pub fn erase<N>(g: &Graph<N>) -> Result<DataGraph, EraseError>
where
    N: Serialize + Node,
{
    let mut out = DataGraph::with_capacity(g.node_count(), g.edge_count());
    for (node_ix, w) in g.node_weights().enumerate() {
        let node_data = erase_node(w).map_err(|source| EraseError { node_ix, source })?;
        out.add_node(node_data);
    }
    for e in g.edge_references() {
        out.add_edge(e.source(), e.target(), *e.weight());
    }
    Ok(out)
}

/// Reify a typed graph from its stored data form: node weights through
/// [`reify_node`], indices and edges preserved verbatim.
pub fn reify<N>(g: &DataGraph) -> Result<Graph<N>, ReifyError>
where
    N: DeserializeOwned,
{
    let mut out = Graph::with_capacity(g.node_count(), g.edge_count());
    for (node_ix, node_data) in g.node_weights().enumerate() {
        let node = reify_node(node_data).map_err(|source| ReifyError { node_ix, source })?;
        out.add_node(node);
    }
    for e in g.edge_references() {
        out.add_edge(e.source(), e.target(), *e.weight());
    }
    Ok(out)
}

/// A node's direct outgoing references: its own reporting plus that of its
/// physically nested nodes.
///
/// The absent node lookup stops reference nodes from following their target
/// into other graphs, keeping the result direct - the reachability walk owns
/// the transitive closure.
fn node_out_refs<N: Node>(node: &N) -> (Vec<ContentAddr>, Vec<(SectionId, ContentAddr)>) {
    fn no_node(_: &ContentAddr) -> Option<&'static dyn Node> {
        None
    }
    let mut addrs = HashSet::new();
    let mut blobs = HashSet::new();
    node::visit(
        visit::Ctx::new(&no_node, &[], &[]),
        node,
        &mut visit::RequiredAddrs { addrs: &mut addrs },
    );
    node::visit(
        visit::Ctx::new(&no_node, &[], &[]),
        node,
        &mut visit::RequiredBlobs { blobs: &mut blobs },
    );
    let mut refs: Vec<_> = addrs.into_iter().collect();
    refs.sort();
    let mut blobs: Vec<_> = blobs.into_iter().collect();
    blobs.sort();
    (refs, blobs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::ExprResult;

    /// A minimal tag-dispatched node set: an internally-tagged enum serializes
    /// to exactly the `"type"`-tagged map shape the node-set macro produces.
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    #[serde(tag = "type")]
    enum TestNode {
        Num { v: i64 },
        Link { addr: ContentAddr },
    }

    impl Node for TestNode {
        fn expr(&self, _: node::ExprCtx) -> ExprResult {
            unimplemented!("not compiled in these tests")
        }

        fn required_addrs(&self) -> Vec<ContentAddr> {
            match self {
                TestNode::Num { .. } => vec![],
                TestNode::Link { addr } => vec![*addr],
            }
        }
    }

    fn num(v: i64) -> TestNode {
        TestNode::Num { v }
    }

    fn graph(nodes: impl IntoIterator<Item = TestNode>) -> Graph<TestNode> {
        let mut g = Graph::default();
        let ixs: Vec<_> = nodes.into_iter().map(|n| g.add_node(n)).collect();
        for w in ixs.windows(2) {
            g.add_edge(w[0], w[1], gantz_ca::Edge::from((0, 0)));
        }
        g
    }

    #[test]
    fn erase_node_splits_tag_and_extracts_refs() {
        let nd = erase_node(&num(42)).unwrap();
        assert_eq!(nd.tag, "Num");
        assert_eq!(nd.data, Datum::Map(vec![("v".into(), Datum::I64(42))]));
        assert!(nd.refs.is_empty() && nd.blobs.is_empty());
        assert!(nd.is_canonical());

        let target = ContentAddr([7; 32]);
        let nd = erase_node(&TestNode::Link { addr: target }).unwrap();
        assert_eq!(nd.tag, "Link");
        assert_eq!(nd.refs, vec![target]);
    }

    #[test]
    fn graph_round_trips_preserving_structure() {
        let mut g = graph([num(1), num(2), num(3)]);
        // A parallel edge and a distinct socket pairing survive.
        g.add_edge(0.into(), 2.into(), gantz_ca::Edge::from((1, 1)));
        let dg = erase(&g).unwrap();
        let back: Graph<TestNode> = reify(&dg).unwrap();
        let weights: Vec<_> = back.node_weights().cloned().collect();
        assert_eq!(weights, vec![num(1), num(2), num(3)]);
        let edges: Vec<_> = back
            .edge_references()
            .map(|e| (e.source().index(), e.target().index(), *e.weight()))
            .collect();
        let expected: Vec<_> = g
            .edge_references()
            .map(|e| (e.source().index(), e.target().index(), *e.weight()))
            .collect();
        assert_eq!(edges, expected);
    }

    #[test]
    fn reify_unknown_tag_names_node_and_tag() {
        let mut dg = erase(&graph([num(1)])).unwrap();
        dg.node_weights_mut().for_each(|n| n.tag = "Mystery".into());
        let err = reify::<TestNode>(&dg).unwrap_err();
        assert_eq!(err.node_ix, 0);
        assert_eq!(err.source.tag, "Mystery");
        assert!(err.to_string().contains("Mystery"), "{err}");
    }

    #[test]
    fn ensure_reifies_transitive_refs_and_ignores_unresolved() {
        let mut reg = Registry::<DataGraph>::default();
        let leaf = reg.add_graph(erase(&graph([num(1)])).unwrap());
        let mid = {
            let g = graph([TestNode::Link { addr: leaf.into() }, num(2)]);
            reg.add_graph(erase(&g).unwrap())
        };
        let root = {
            // One resolvable ref and one dangling (builtin-style) addr.
            let mut g = graph([TestNode::Link { addr: mid.into() }]);
            g.add_node(TestNode::Link {
                addr: ContentAddr([9; 32]),
            });
            reg.add_graph(erase(&g).unwrap())
        };

        let mut cache = ReifiedGraphs::<TestNode>::new();
        cache.ensure(&reg, [root.into()]).unwrap();
        assert!(cache.contains(&root) && cache.contains(&mid) && cache.contains(&leaf));
        assert!(!cache.contains(&GraphAddr::from(ContentAddr([9; 32]))));
        assert_eq!(cache.get(&leaf).unwrap().node_count(), 1);

        // Ensuring again is a no-op walk over cached entries.
        cache.ensure(&reg, [root.into()]).unwrap();
    }
}
