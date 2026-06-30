//! Deriving a [`plyphon::SynthDef`] from a connected subgraph of [`NodeDsp`]
//! nodes.

use std::collections::{HashMap, HashSet};

use petgraph::Direction;
use petgraph::visit::EdgeRef;
use plyphon::synthdef::{InputRef, SynthDef};

use gantz_core::node::graph::{Graph, NodeIx};

use crate::dsp::{DspBuilder, ToNodeDsp};

/// An error deriving a synthdef from a graph.
#[derive(Debug)]
pub enum DeriveError {
    /// The given root node is not a [`NodeDsp`](crate::NodeDsp).
    RootNotDsp,
}

/// Derive a [`SynthDef`] named `name` from the DSP subgraph feeding `root` (a
/// sink node such as `~out`), fanning the output across `out_channels` channels.
///
/// Walks the DSP nodes reachable *backwards* from `root` (over signal edges) in
/// topological order, letting each node emit its UGens via
/// [`NodeDsp::ugens`](crate::NodeDsp::ugens) and threading each node's outputs
/// into its consumers' inputs.
///
/// Phase-0 limitations: a single edge per DSP input (no summing), acyclic graphs
/// only (no feedback), and flat concrete nodes (no nested graphs / refs).
pub fn derive_synthdef<N>(
    graph: &Graph<N>,
    root: NodeIx,
    out_channels: usize,
    name: impl Into<String>,
) -> Result<SynthDef, DeriveError>
where
    N: ToNodeDsp,
{
    if graph[root].to_node_dsp().is_none() {
        return Err(DeriveError::RootNotDsp);
    }

    let mut builder = DspBuilder::new(out_channels);
    // Each processed node's output sources, for its consumers to reference.
    let mut outputs: HashMap<NodeIx, Vec<InputRef>> = HashMap::new();

    for n in topo_order(graph, root) {
        let Some(dsp) = graph[n].to_node_dsp() else {
            continue;
        };
        let n_in = dsp.n_dsp_inputs();
        // Unconnected inputs default to silence.
        let mut inputs = vec![InputRef::Constant(0.0); n_in];
        for e in graph.edges_directed(n, Direction::Incoming) {
            let input_ix = e.weight().input.0 as usize;
            let output_ix = e.weight().output.0 as usize;
            if input_ix >= n_in {
                continue;
            }
            // Phase 0: only DSP sources contribute, and the first edge to an
            // input wins (no summing of multiple sources yet).
            if let Some(src) = outputs.get(&e.source()).and_then(|o| o.get(output_ix)) {
                inputs[input_ix] = *src;
            }
        }
        let outs = dsp.ugens(&inputs, &mut builder);
        outputs.insert(n, outs);
    }

    Ok(builder.finish(name))
}

/// Topological order (sources first) of the DSP nodes feeding `root`, via a
/// post-order DFS over incoming edges. Non-DSP sources are not traversed, so the
/// walk stops at the DSP/control boundary.
fn topo_order<N>(graph: &Graph<N>, root: NodeIx) -> Vec<NodeIx>
where
    N: ToNodeDsp,
{
    let mut order = Vec::new();
    let mut visited = HashSet::new();
    // Each stack entry is `(node, post)`: `post == true` marks the second visit,
    // at which point the node is emitted (after all its producers).
    let mut stack = vec![(root, false)];
    while let Some((n, post)) = stack.pop() {
        if post {
            order.push(n);
            continue;
        }
        if !visited.insert(n) {
            continue;
        }
        stack.push((n, true));
        for e in graph.edges_directed(n, Direction::Incoming) {
            let src = e.source();
            if !visited.contains(&src) && graph[src].to_node_dsp().is_some() {
                stack.push((src, false));
            }
        }
    }
    order
}
