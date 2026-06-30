//! Deriving a [`plyphon::SynthDef`] from a connected subgraph of [`NodeDsp`]
//! nodes.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use petgraph::Direction;
use petgraph::visit::EdgeRef;
use plyphon::Rate;
use plyphon::synthdef::{InputRef, Param, SynthDef, UnitSpec};

use gantz_core::compile::pull_eval_order;
use gantz_core::node::Conns;
use gantz_core::node::graph::{Graph, NodeIx};

use crate::dsp::{DspBuilder, ParamBinding, ToNodeDsp};

/// An error deriving a synthdef from a graph.
#[derive(Debug)]
pub enum DeriveError {
    /// The given root node is not a [`NodeDsp`](crate::NodeDsp).
    RootNotDsp,
}

/// The output of [`derive_synthdef`]: the synthdef plus the [`ParamBinding`]s the
/// audio driver uses to push each dsp node's live state value to the right synth
/// param via `set_control`.
pub struct Derived {
    /// The compiled synth definition.
    pub def: SynthDef,
    /// One binding per control param, in param-index order.
    pub params: Vec<ParamBinding>,
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
) -> Result<Derived, DeriveError>
where
    N: ToNodeDsp,
{
    let Some(root_dsp) = graph[root].to_node_dsp() else {
        return Err(DeriveError::RootNotDsp);
    };

    // Pull from the root over only its *dsp* inputs, in gantz_core's eval order,
    // keeping only DSP nodes. Seeding with `n_dsp_inputs` (not `n_inputs`) means a
    // control edge at a higher input index (e.g. `~out`'s gain) falls outside
    // `conns` and is ignored by the pull traversal - it is a Steel/state concern,
    // applied via `set_control`, not part of the dsp signal graph.
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
        let outs = dsp.ugens(&[n.index()], &inputs, &mut builder);
        outputs.insert(n, outs);
    }

    let (def, params) = builder.finish(name);
    Ok(Derived { def, params })
}

/// A hash of a synthdef's *structure* - everything except parameter values.
///
/// Two synthdefs that differ only in their [`Param`] defaults (the settable
/// values) share a signature, so the audio driver can `set_control` those values
/// on the running synth rather than respawning it (preserving phase). A change to
/// the unit graph, the wiring, a baked constant, or a param's name/rate/lag *does*
/// change the signature, forcing a respawn.
pub fn structural_sig(def: &SynthDef) -> u64 {
    let mut h = DefaultHasher::new();
    def.units.len().hash(&mut h);
    for u in &def.units {
        hash_unit(&mut h, u);
    }
    def.params.len().hash(&mut h);
    for p in &def.params {
        hash_param_struct(&mut h, p);
    }
    h.finish()
}

fn hash_unit(h: &mut DefaultHasher, u: &UnitSpec) {
    u.name.hash(h);
    rate_tag(u.rate).hash(h);
    u.num_outputs.hash(h);
    u.special_index.hash(h);
    u.inputs.len().hash(h);
    for i in &u.inputs {
        hash_input(h, i);
    }
}

fn hash_input(h: &mut DefaultHasher, i: &InputRef) {
    match i {
        InputRef::Constant(c) => {
            0u8.hash(h);
            c.to_bits().hash(h);
        }
        InputRef::Param(p) => {
            1u8.hash(h);
            p.hash(h);
        }
        InputRef::Unit { unit, output } => {
            2u8.hash(h);
            unit.hash(h);
            output.hash(h);
        }
    }
}

/// Hash a param's structure - name, rate, trigger flag and lag - but NOT its
/// default value (which the driver sets live via `set_control`).
fn hash_param_struct(h: &mut DefaultHasher, p: &Param) {
    p.name.hash(h);
    rate_tag(p.rate).hash(h);
    p.is_trig.hash(h);
    p.lag.map(f32::to_bits).hash(h);
}

fn rate_tag(rate: Rate) -> u8 {
    match rate {
        Rate::Scalar => 0,
        Rate::Control => 1,
        Rate::Audio => 2,
        Rate::Demand => 3,
    }
}
