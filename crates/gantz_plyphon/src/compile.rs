//! Deriving a [`plyphon::SynthDef`] from a connected subgraph of [`NodeDsp`]
//! nodes.

use std::collections::HashMap;

use petgraph::Direction;
use petgraph::visit::EdgeRef;
use plyphon::synthdef::{InputRef, SynthDef};

use gantz_core::compile::pull_eval_order;
use gantz_core::node::Conns;
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
/// Orders the DSP nodes using gantz_core's pull-eval ordering
/// ([`pull_eval_order`]) - the same order Steel uses when pulling from a node -
/// then keeps only the DSP nodes. (Filtering a topological order preserves a
/// valid topological order of the induced DSP subgraph.) Each node emits its
/// UGens via [`NodeDsp::ugens`](crate::NodeDsp::ugens), threading its outputs
/// into its consumers' inputs.
///
/// Phase-1 limitations: a single edge per DSP input (no summing), acyclic graphs
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
    let Some(root_dsp) = graph[root].to_node_dsp() else {
        return Err(DeriveError::RootNotDsp);
    };

    // Pull from the root over all its inputs, in gantz_core's eval order, keeping
    // only DSP nodes (non-DSP control sources are filtered out).
    let conns = Conns::connected(root_dsp.n_dsp_inputs()).expect("n_dsp_inputs within Conns::MAX");
    let order: Vec<NodeIx> = pull_eval_order(graph, root, conns)
        .filter(|&n| graph[n].to_node_dsp().is_some())
        .collect();

    let mut builder = DspBuilder::new(out_channels);
    // Each processed node's output sources, for its consumers to reference.
    let mut outputs: HashMap<NodeIx, Vec<InputRef>> = HashMap::new();

    for n in order {
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
            // Phase 1: only DSP sources contribute, and the first edge to an
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
