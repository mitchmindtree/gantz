//! Deriving a [`plyphon::SynthDef`] from a connected subgraph of [`NodeDsp`](crate::NodeDsp)
//! nodes.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};

use petgraph::Direction;
use petgraph::visit::EdgeRef;
use plyphon::Rate;
use plyphon::synthdef::{InputRef, Param, SynthDef, UnitSpec};

use gantz_core::compile::pull_eval_order;
use gantz_core::node::Conns;
use gantz_core::node::graph::{Graph, NodeIx};

use crate::dsp::{DspBuilder, ParamBinding, ScopeOutBinding, Signal, ToNodeDsp};

/// An error deriving a synthdef from a graph.
#[derive(Debug)]
pub enum DeriveError {
    /// The graph has no dsp *sink* (no `~out` output and no `~scopeout` monitor), so
    /// there is nothing to root a synthdef at.
    NoSink,
}

/// The output of [`derive_synthdef`]: the synthdef plus the bindings the audio
/// driver uses to bridge dsp node state and the running synth - [`ParamBinding`]s
/// (push each dsp node's live value to a synth param via `set_control`) and
/// [`ScopeOutBinding`]s (route each monitor's `/tr`s back into node state).
pub struct Derived {
    /// The compiled synth definition.
    pub def: SynthDef,
    /// One binding per control param, in param-index order.
    pub params: Vec<ParamBinding>,
    /// One binding per monitor (`~scopeout`), in `SendTrig`-id order.
    pub monitors: Vec<ScopeOutBinding>,
}

/// Derive a [`SynthDef`] named `name` from a graph's DSP subgraph, fanning the
/// output across `out_channels` channels.
///
/// A dsp port carries a whole channel group ([`Signal`]): an edge delivers its
/// source port's full group to the destination input, so channel width flows
/// *forward* through the derivation - nodes see their input widths and size
/// their output groups accordingly (no `gantz_core` graph or edge involvement).
///
/// A graph's dsp *sinks* are its `~out` outputs ([`is_output`](crate::NodeDsp::is_output))
/// and its `~scopeout` monitors ([`is_monitor`](crate::NodeDsp::is_monitor)); a graph
/// may have several of each (e.g. an output plus a couple of taps of interior
/// signals). Every sink seeds a pull over its *dsp* inputs in gantz_core's
/// pull-eval order ([`pull_eval_order`]) - the same order Steel uses - and the
/// per-sink orders are merged, first-occurrence wins. That merge preserves a
/// valid topological order of the whole DSP subgraph: within each order a node
/// precedes its dependents, so the earliest occurrence of any source still
/// precedes the earliest occurrence of its consumer. Each node then emits its
/// UGens via [`NodeDsp::ugens`](crate::NodeDsp::ugens) once, threading its outputs
/// into its consumers' inputs, so a signal feeding both `~out` and a `~scopeout`
/// compiles into one shared unit chain.
///
/// Seeding each sink's pull with its `n_dsp_inputs` (not `n_inputs`) means a
/// control edge at a higher input index (e.g. `~out`'s gain, `~scopeout`'s trigger)
/// falls outside the traversal - it is a Steel/state concern, not part of the dsp
/// signal graph. The same rule applies at *interior* nodes: only nodes that feed a
/// sink transitively through dsp inputs contribute units, so a dsp chain wired into
/// a control input (e.g. a `~lag` into `~sinosc`'s freq) emits nothing rather than
/// dead units whose params the driver would drive and whose presence would force
/// spurious respawns (they'd land in [`structural_sig`]).
///
/// Phase-1 limitations: a single edge per DSP input (no summing), acyclic graphs
/// only (no feedback), and flat concrete nodes (no nested graphs / refs).
pub fn derive_synthdef<N>(
    graph: &Graph<N>,
    out_channels: usize,
    name: impl Into<String>,
) -> Result<Derived, DeriveError>
where
    N: ToNodeDsp,
{
    // Every dsp sink: an audio output (`~out`) or a monitor (`~scopeout`).
    let sinks: Vec<NodeIx> = graph
        .node_indices()
        .filter(|&n| {
            graph[n]
                .to_node_dsp()
                .is_some_and(|d| d.is_output() || d.is_monitor())
        })
        .collect();
    if sinks.is_empty() {
        return Err(DeriveError::NoSink);
    }

    // The dsp-reachable set: dsp nodes that feed a sink transitively through *dsp*
    // inputs only. `pull_eval_order` masks only the seed's inputs - interior nodes
    // are traversed over ALL incoming edges - so the merged order below must be
    // intersected with this set to keep control-input feeds out of the def.
    let mut dsp_reachable: HashSet<NodeIx> = sinks.iter().copied().collect();
    let mut stack: Vec<NodeIx> = sinks.clone();
    while let Some(n) = stack.pop() {
        let n_dsp_in = graph[n].to_node_dsp().map_or(0, |d| d.n_dsp_inputs());
        for e in graph.edges_directed(n, Direction::Incoming) {
            if (e.weight().input.0 as usize) < n_dsp_in
                && graph[e.source()].to_node_dsp().is_some()
                && dsp_reachable.insert(e.source())
            {
                stack.push(e.source());
            }
        }
    }

    // Merge each sink's dsp-only pull-eval order into one topological order,
    // keeping only dsp-reachable nodes and the first occurrence of each (see the
    // fn docs).
    let mut order: Vec<NodeIx> = Vec::new();
    let mut seen: HashSet<NodeIx> = HashSet::new();
    for &sink in &sinks {
        let n_dsp_in = graph[sink].to_node_dsp().map_or(0, |d| d.n_dsp_inputs());
        let conns = Conns::connected(n_dsp_in).expect("n_dsp_inputs within Conns::MAX");
        for n in pull_eval_order(graph, sink, conns) {
            if dsp_reachable.contains(&n) && seen.insert(n) {
                order.push(n);
            }
        }
    }

    let mut builder = DspBuilder::new(out_channels);
    // Each processed node's per-port output signals, for its consumers to
    // reference. A whole channel group flows across an edge.
    let mut outputs: HashMap<NodeIx, Vec<Signal>> = HashMap::new();

    for n in order {
        let Some(dsp) = graph[n].to_node_dsp() else {
            continue;
        };
        let n_in = dsp.n_dsp_inputs();
        let mut inputs: Vec<Option<Signal>> = vec![None; n_in];
        for e in graph.edges_directed(n, Direction::Incoming) {
            let input_ix = e.weight().input.0 as usize;
            let output_ix = e.weight().output.0 as usize;
            if input_ix >= n_in {
                continue;
            }
            // Phase 1: only DSP sources contribute, and the first edge to an
            // input wins (no summing of multiple sources yet). Only a `None`
            // slot is filled - `edges_directed` iterates newest-edge-first, so
            // overwriting would quietly hand the port to the *latest* edge.
            if inputs[input_ix].is_none() {
                if let Some(src) = outputs.get(&e.source()).and_then(|o| o.get(output_ix)) {
                    inputs[input_ix] = Some(src.clone());
                }
            }
        }
        // Unconnected inputs default to mono silence.
        let inputs: Vec<Signal> = inputs
            .into_iter()
            .map(|i| i.unwrap_or_else(|| Signal::silent(1)))
            .collect();
        let outs = dsp.ugens(&[n.index()], &inputs, &mut builder);
        debug_assert_eq!(
            outs.len(),
            dsp.n_dsp_outputs(),
            "a node must return one Signal per dsp output port",
        );
        outputs.insert(n, outs);
    }

    let (def, params, monitors) = builder.finish(name);
    Ok(Derived {
        def,
        params,
        monitors,
    })
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
