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

use crate::dsp::{
    DspBuilder, GainRef, ParamBinding, ScopeOutBinding, Signal, ToNodeDsp, sum_signals,
};

/// An error deriving a synthdef from a graph.
#[derive(Debug)]
pub enum DeriveError {
    /// The graph has no dsp *sink* (no `~out` output and no `~scopeout` monitor), so
    /// there is nothing to root a synthdef at.
    NoSink,
    /// The `~bus` boundaries form a cycle between regions, so there is no
    /// writer-before-reader order to derive (or run) them in. Deliberate
    /// cross-region feedback (an `InFeedback`-based bus) is a planned follow-up.
    BusCycle,
    /// An instanced reference's target graph could not be resolved.
    Unresolved(gantz_ca::ContentAddr),
    /// Instanced references form a cycle (a graph transitively instancing
    /// itself), so there is no finite template to derive.
    RefCycle(gantz_ca::ContentAddr),
}

/// One side of a `~bus` boundary within a region's def: the bus unit whose
/// input 0 (the bus channel index) is a no-lag control param the driver sets to
/// a driver-allocated private bus via `set_control` after spawning (no def
/// mutation, so [`structural_sig`] stays stable across allocations).
#[derive(Clone, Debug)]
pub struct BusBinding {
    /// The `~bus` node's path - the driver's bus-allocation key. Consecutive
    /// buses alias (a `~bus` fed directly by another `~bus` shares the upstream
    /// bus), so reads name the *effective* upstream node's path.
    ///
    /// For an implicit endpoint bus (see [`output`](Self::output)) this is the
    /// endpoint *source* node's path instead.
    pub node_path: Vec<usize>,
    /// The bus's channel count - the boundary signal's width.
    pub channels: usize,
    /// The index within the def's `units` of the bus `Out` (write side) or `In`
    /// (read side) whose input 0 is the bus-index param.
    pub unit: usize,
    /// The no-lag control param the driver sets to the allocated bus channel
    /// via `set_control` after spawning.
    pub param: usize,
    /// `None` for a classic single-writer `~bus` (keyed by the effective bus
    /// node). `Some(port)` for an *implicit endpoint bus*: when a boundary's
    /// source chain fans out (several summands feed it), the `~bus` keeps only
    /// its cut role and each transitive endpoint gets its own single-writer
    /// bus, keyed by the endpoint source's path + output port. Readers emit
    /// one `In` per endpoint and sum them.
    pub output: Option<usize>,
}

/// A cross-region bus identity within [`derive_synthdefs`]: a classic
/// single-writer `~bus` chain (keyed by the effective bus node), or an
/// implicit per-endpoint bus where the chain fans out (keyed by the endpoint
/// source node + output port). See [`BusBinding::output`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum RegionBus {
    Bus(NodeIx),
    Src(NodeIx, usize),
}

/// One region of a boundary-cut graph: its derived synthdef + bindings, plus
/// the buses its def writes and reads. Produced by [`derive_synthdefs`] in
/// region-DAG topological order (bus writers before their readers - the order
/// their synths must also take in the node tree).
pub struct RegionDerived {
    /// A stable identity across re-derives: a hash of the region's sink and
    /// boundary node paths. The driver matches old and new regions by key for
    /// its per-region keep/replace decision.
    pub key: u64,
    /// The region's synthdef + param/monitor/gain bindings.
    pub derived: Derived,
    /// The buses this region's def writes (`Out` with a patchable bus input).
    pub bus_writes: Vec<BusBinding>,
    /// The buses this region's def reads (`In` with a patchable bus input).
    pub bus_reads: Vec<BusBinding>,
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
    /// The params that gate the def's whole output (e.g. `~out`'s gain), which
    /// the driver fades through on a crossfaded replacement.
    pub gains: Vec<GainRef>,
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
/// and its `~scopeout` monitors ([`is_monitor`](crate::NodeDsp::is_monitor)). A graph
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
/// a control input emits nothing rather than dead units whose params the driver
/// would drive and whose presence would force spurious respawns (they'd land in
/// [`structural_sig`]). A *hybrid* dsp input (e.g. `~sinosc`'s freq, see
/// [`NodeDsp::n_dsp_inputs`](crate::NodeDsp::n_dsp_inputs)) IS part of the
/// traversal: a dsp chain wired into it emits units and drives the input
/// directly (audio-rate FM), and the input's fallback param is only baked while
/// no dsp source is connected.
///
/// Nested graphs are supported via a pre-derivation pass: [`flatten`](crate::flatten())
/// resolves graph refs and splices their nodes into a single flat graph (each
/// carrying its original nested path via [`ToNodeDsp::node_path`]) before
/// derivation runs.
///
/// Multiple edges into one dsp input *sum*: the input's value is the
/// unity-gain mix of every incoming edge ([`sum_signals`]) - the result is as
/// wide as the widest summand, a mono summand broadcasts across every channel
/// and a narrower one contributes silence past its own width. Summands take a
/// canonical order (sorted by source node path + output port), so the derived
/// def is independent of edge insertion order. A single edge passes through
/// unit-free.
///
/// Phase-1 limitation: acyclic graphs only (no feedback).
pub fn derive_synthdef<N>(
    graph: &Graph<N>,
    out_channels: usize,
    name: impl Into<String>,
) -> Result<Derived, DeriveError>
where
    N: ToNodeDsp,
{
    let sinks = dsp_sinks(graph);
    if sinks.is_empty() {
        return Err(DeriveError::NoSink);
    }
    let reachable = dsp_reachable(graph, &sinks);
    let sources = resolved_sources(graph, &reachable);

    // Merge each sink's dsp-only pull-eval order into one topological order,
    // keeping only dsp-reachable nodes and the first occurrence of each (see the
    // fn docs).
    let order = merged_pull_order(graph, &sinks, |n| reachable.contains(&n));

    let mut builder = DspBuilder::new(out_channels);
    // Each processed node's per-port output signals, for its consumers to
    // reference. A whole channel group flows across an edge.
    let mut outputs: HashMap<NodeIx, Vec<Signal>> = HashMap::new();

    for n in order {
        let Some(dsp) = graph[n].to_node_dsp() else {
            continue;
        };
        let inputs: Vec<Option<Signal>> = sources[&n]
            .iter()
            .map(|summands| {
                let sigs: Vec<Signal> = summands
                    .iter()
                    .filter_map(|&(s, port)| outputs.get(&s).and_then(|o| o.get(port)).cloned())
                    .collect();
                // `None` iff no summand materialized a signal (unconnected, or
                // e.g. a dangling `~unpack` port), so hybrid inputs fall back
                // to their param exactly when the Steel side keeps it driven.
                (!sigs.is_empty()).then(|| sum_signals(&mut builder, &sigs))
            })
            .collect();
        let outs = dsp.ugens(&graph[n].node_path(n.index()), &inputs, &mut builder);
        debug_assert_eq!(
            outs.len(),
            dsp.n_dsp_outputs(),
            "a node must return one Signal per dsp output port",
        );
        outputs.insert(n, outs);
    }

    let (def, params, monitors, gains) = builder.finish(name);
    Ok(Derived {
        def,
        params,
        monitors,
        gains,
    })
}

/// Every dsp sink of `graph`: an audio output (`~out`) or a monitor (`~scopeout`).
pub(crate) fn dsp_sinks<N: ToNodeDsp>(graph: &Graph<N>) -> Vec<NodeIx> {
    graph
        .node_indices()
        .filter(|&n| {
            graph[n]
                .to_node_dsp()
                .is_some_and(|d| d.is_output() || d.is_monitor())
        })
        .collect()
}

/// The dsp-reachable set: dsp nodes that feed a sink transitively through *dsp*
/// inputs only. `pull_eval_order` masks only the seed's inputs - interior nodes
/// are traversed over ALL incoming edges - so derivation intersects its merged
/// orders with this set to keep control-input feeds out of the defs.
fn dsp_reachable<N: ToNodeDsp>(graph: &Graph<N>, sinks: &[NodeIx]) -> HashSet<NodeIx> {
    let mut reachable: HashSet<NodeIx> = sinks.iter().copied().collect();
    let mut stack: Vec<NodeIx> = sinks.to_vec();
    while let Some(n) = stack.pop() {
        let n_dsp_in = graph[n].to_node_dsp().map_or(0, |d| d.n_dsp_inputs());
        for e in graph.edges_directed(n, Direction::Incoming) {
            if (e.weight().input.0 as usize) < n_dsp_in
                && graph[e.source()].to_node_dsp().is_some()
                && reachable.insert(e.source())
            {
                stack.push(e.source());
            }
        }
    }
    reachable
}

/// The summand `(source node, output port)`s per dsp input of every reachable
/// node: only reachable dsp sources contribute, every edge into an input is a
/// summand (an empty list = unconnected), and summands take a canonical order
/// (sorted by source node path + output port, duplicates kept) so the derived
/// def is independent of edge insertion order.
#[allow(clippy::type_complexity)]
fn resolved_sources<N: ToNodeDsp>(
    graph: &Graph<N>,
    reachable: &HashSet<NodeIx>,
) -> HashMap<NodeIx, Vec<Vec<(NodeIx, usize)>>> {
    reachable
        .iter()
        .map(|&n| {
            let n_dsp_in = graph[n].to_node_dsp().map_or(0, |d| d.n_dsp_inputs());
            let mut inputs: Vec<Vec<(NodeIx, usize)>> = vec![Vec::new(); n_dsp_in];
            for e in graph.edges_directed(n, Direction::Incoming) {
                let input_ix = e.weight().input.0 as usize;
                let s = e.source();
                if input_ix < n_dsp_in && reachable.contains(&s) && graph[s].to_node_dsp().is_some()
                {
                    inputs[input_ix].push((s, e.weight().output.0 as usize));
                }
            }
            for summands in &mut inputs {
                summands.sort_by_cached_key(|&(s, port)| (graph[s].node_path(s.index()), port));
            }
            (n, inputs)
        })
        .collect()
}

/// Merge each sink's dsp-only pull-eval order into one topological order over
/// the nodes selected by `keep`, first occurrence wins (a filtered subsequence
/// of a topological order remains topological for the kept subgraph).
pub(crate) fn merged_pull_order<N: ToNodeDsp>(
    graph: &Graph<N>,
    seeds: &[NodeIx],
    keep: impl Fn(NodeIx) -> bool,
) -> Vec<NodeIx> {
    let mut order: Vec<NodeIx> = Vec::new();
    let mut seen: HashSet<NodeIx> = HashSet::new();
    for &seed in seeds {
        let n_dsp_in = graph[seed].to_node_dsp().map_or(0, |d| d.n_dsp_inputs());
        let conns = Conns::connected(n_dsp_in).expect("n_dsp_inputs within Conns::MAX");
        for n in pull_eval_order(graph, seed, conns) {
            if keep(n) && seen.insert(n) {
                order.push(n);
            }
        }
    }
    order
}

/// Derive one [`SynthDef`] per boundary-cut *region* of the graph's DSP
/// subgraph, in region-DAG topological order (bus writers before readers).
///
/// Where [`derive_synthdef`] fuses the whole DSP subgraph into a single def
/// (boundary nodes lower as plain wires), this splits it at every cutting
/// `~bus` ([`is_boundary`](crate::NodeDsp::is_boundary)): regions are the
/// connected components of the dsp-reachable subgraph over non-boundary edges,
/// and a boundary between two regions lowers to an `Out` to a private bus in
/// the writer's def and an `In` in each reader's - both with a no-lag
/// bus-index control param the driver sets via `set_control` after spawning
/// (see [`BusBinding`]). The point: each
/// region carries its own [`structural_sig`], so an edit respawns only its own
/// region's synth and every other region's unit state survives untouched.
///
/// A boundary whose two sides share a region lowers to a plain wire. A
/// boundary fed directly by another boundary *aliases* it (no relay def, no
/// extra latency). An unconnected boundary reads as mono silence. A boundary
/// fed by *several* summands keeps only its cut role: each transitive endpoint
/// writes its own implicit single-writer bus ([`BusBinding::output`]) and
/// every reader emits one `In` per endpoint, summing after the reads
/// ([`sum_signals`] - so mono-broadcast reconciles on materialized signals). A
/// region is derived only if it feeds a sink transitively. Bus writes are
/// lifted to audio rate ([`DspBuilder::ensure_audio`]) and fade-gained (the
/// crossfade lever, [`DspBuilder::push_fade_gain`]). Widths flow forward
/// across boundaries - hence the topological derivation order. Defs are named
/// `<name_prefix>-<region key>`.
pub fn derive_synthdefs<N>(
    graph: &Graph<N>,
    out_channels: usize,
    name_prefix: &str,
) -> Result<Vec<RegionDerived>, DeriveError>
where
    N: ToNodeDsp,
{
    let sinks = dsp_sinks(graph);
    if sinks.is_empty() {
        return Err(DeriveError::NoSink);
    }
    let reachable = dsp_reachable(graph, &sinks);
    let sources = resolved_sources(graph, &reachable);
    let is_boundary =
        |n: NodeIx| -> bool { graph[n].to_node_dsp().is_some_and(|d| d.is_boundary()) };

    // Regions: connected components of the reachable NON-boundary nodes over
    // their dsp edges (edges into or out of a boundary never join).
    let mut comp: HashMap<NodeIx, usize> = HashMap::new();
    let mut n_comps = 0;
    for start in graph.node_indices() {
        if !reachable.contains(&start) || is_boundary(start) || comp.contains_key(&start) {
            continue;
        }
        let id = n_comps;
        n_comps += 1;
        comp.insert(start, id);
        let mut stack = vec![start];
        while let Some(n) = stack.pop() {
            // Upstream: this node's summand sources.
            for &(s, _) in sources[&n].iter().flatten() {
                if !is_boundary(s) && comp.insert(s, id).is_none() {
                    stack.push(s);
                }
            }
            // Downstream: reachable non-boundary consumers with this node
            // among that input's summands.
            for e in graph.edges_directed(n, Direction::Outgoing) {
                let t = e.target();
                if !reachable.contains(&t) || is_boundary(t) || comp.contains_key(&t) {
                    continue;
                }
                let input_ix = e.weight().input.0 as usize;
                let among = sources[&t]
                    .get(input_ix)
                    .is_some_and(|ss| ss.iter().any(|&(s, _)| s == n));
                if among {
                    comp.insert(t, id);
                    stack.push(t);
                }
            }
        }
    }

    // Boundary lowering. A *pure* single-summand chain of boundaries keeps the
    // classic single-writer bus identity: consecutive buses alias, keyed by the
    // *effective* (top-most) bus node. A boundary whose chain fans out keeps
    // only its cut role: each transitive non-boundary endpoint gets its own
    // implicit single-writer bus and readers sum after their `In`s (width
    // reconciliation needs locally materialized signals - a writer cannot know
    // the sum's final width at its own derive time). A pure boundary cycle
    // degrades to an unsourced (silent) bus.
    let boundaries: Vec<NodeIx> = graph
        .node_indices()
        .filter(|&n| reachable.contains(&n) && is_boundary(n))
        .collect();
    let effective = |b: NodeIx| -> NodeIx {
        let mut cur = b;
        let mut visited = HashSet::new();
        while let Some(&[(s, _)]) = sources[&cur].first().map(|v| v.as_slice()) {
            if !is_boundary(s) || !visited.insert(cur) {
                break;
            }
            cur = s;
        }
        cur
    };
    // The classic case: the effective bus's lone summand is a non-boundary
    // source (every hop of the chain had exactly one). `None` = the chain fans
    // out somewhere, is unsourced, or is a pure bus cycle.
    let classic_source = |b: NodeIx| -> Option<(NodeIx, usize)> {
        match sources[&effective(b)].first().map(|v| v.as_slice()) {
            Some(&[(s, port)]) if !is_boundary(s) => Some((s, port)),
            _ => None,
        }
    };
    // Every transitive non-boundary endpoint feeding `b`, canonical order,
    // duplicates kept (each is a summand). Empty = unsourced (silence).
    let bus_endpoints = |b: NodeIx| -> Vec<(NodeIx, usize)> {
        let mut endpoints = Vec::new();
        let mut visited = HashSet::new();
        let mut stack = vec![b];
        while let Some(cur) = stack.pop() {
            if !visited.insert(cur) {
                continue;
            }
            for &(s, port) in sources[&cur].iter().flatten() {
                match is_boundary(s) {
                    true => stack.push(s),
                    false => endpoints.push((s, port)),
                }
            }
        }
        endpoints.sort_by_cached_key(|&(s, port)| (graph[s].node_path(s.index()), port));
        endpoints
    };
    // The buses a boundary lowers to, each with its writing source.
    let region_buses = |b: NodeIx| -> Vec<(RegionBus, (NodeIx, usize))> {
        match classic_source(b) {
            Some(src) => vec![(RegionBus::Bus(effective(b)), src)],
            None => bus_endpoints(b)
                .into_iter()
                .map(|(s, port)| (RegionBus::Src(s, port), (s, port)))
                .collect(),
        }
    };

    // Cross-region reads - (reader component, bus), from every boundary
    // summand whose bus originates in another component - and each bus's
    // writing source.
    let mut cross_reads: HashSet<(usize, RegionBus)> = HashSet::new();
    let mut bus_writer: HashMap<RegionBus, (NodeIx, usize)> = HashMap::new();
    for (&n, srcs) in &sources {
        if is_boundary(n) {
            continue;
        }
        for &(s, _) in srcs.iter().flatten() {
            if !is_boundary(s) {
                continue;
            }
            for (bus, (src, port)) in region_buses(s) {
                bus_writer.insert(bus, (src, port));
                if comp[&src] != comp[&n] {
                    cross_reads.insert((comp[&n], bus));
                }
            }
        }
    }

    // Needed components: those holding sinks, plus (transitively) the writers
    // of every bus a needed component reads.
    let mut needed: HashSet<usize> = sinks.iter().map(|s| comp[s]).collect();
    loop {
        let mut grew = false;
        for &(reader, bus) in &cross_reads {
            if needed.contains(&reader) {
                let (src, _) = bus_writer[&bus];
                grew |= needed.insert(comp[&src]);
            }
        }
        if !grew {
            break;
        }
    }

    // Region DAG (writer -> reader) over needed components. Kahn's algorithm
    // yields the derivation (and node-tree) order, or reports a bus cycle.
    let mut deps: HashMap<usize, HashSet<usize>> = HashMap::new(); // reader -> writers
    for &(reader, bus) in &cross_reads {
        if !needed.contains(&reader) {
            continue;
        }
        let (src, _) = bus_writer[&bus];
        let writer = comp[&src];
        if writer != reader {
            deps.entry(reader).or_default().insert(writer);
        }
    }
    let mut topo: Vec<usize> = Vec::with_capacity(needed.len());
    let mut placed: HashSet<usize> = HashSet::new();
    // Component ids were assigned in node-index order, so iterating them in
    // order keeps the result deterministic.
    while topo.len() < needed.len() {
        let next = (0..n_comps).find(|c| {
            needed.contains(c)
                && !placed.contains(c)
                && deps
                    .get(c)
                    .is_none_or(|ws| ws.iter().all(|w| placed.contains(w)))
        });
        match next {
            Some(c) => {
                placed.insert(c);
                topo.push(c);
            }
            None => return Err(DeriveError::BusCycle),
        }
    }

    // Derive each region in topo order; widths flow forward via `bus_width`.
    let mut regions = Vec::with_capacity(topo.len());
    let mut bus_width: HashMap<RegionBus, usize> = HashMap::new();
    for c in topo {
        // The region's roots: its sinks, plus the sources of the buses it
        // writes (a bus sourced here and read from another needed component).
        // Boundary node-index order keeps the write order deterministic.
        let mut writes: Vec<(RegionBus, (NodeIx, usize))> = Vec::new();
        for &b in &boundaries {
            for (bus, src) in region_buses(b) {
                if comp[&src.0] == c
                    && cross_reads
                        .iter()
                        .any(|&(r, rb)| rb == bus && needed.contains(&r))
                    && !writes.iter().any(|&(wb, _)| wb == bus)
                {
                    writes.push((bus, src));
                }
            }
        }
        let region_sinks: Vec<NodeIx> = sinks.iter().copied().filter(|s| comp[s] == c).collect();

        let seeds: Vec<NodeIx> = region_sinks
            .iter()
            .copied()
            .chain(writes.iter().map(|&(_, (s, _))| s))
            .collect();
        let order = merged_pull_order(graph, &seeds, |n| comp.get(&n) == Some(&c));

        let mut builder = DspBuilder::new(out_channels);
        let mut outputs: HashMap<NodeIx, Vec<Signal>> = HashMap::new();
        let mut bus_reads: Vec<BusBinding> = Vec::new();
        // One `In` per bus read, shared by every consumer in the region.
        let mut in_signals: HashMap<RegionBus, Signal> = HashMap::new();

        for n in order {
            let Some(dsp) = graph[n].to_node_dsp() else {
                continue;
            };
            // Each input sums its summands: a plain summand wires directly, a
            // boundary summand lowers to its buses - in-region wires or `In`s.
            let mut inputs: Vec<Option<Signal>> = Vec::with_capacity(sources[&n].len());
            for summands in &sources[&n] {
                let mut sigs: Vec<Signal> = Vec::new();
                for &(s, port) in summands {
                    let lowered: Vec<(Option<RegionBus>, (NodeIx, usize))> = match is_boundary(s) {
                        true => region_buses(s)
                            .into_iter()
                            .map(|(bus, src)| (Some(bus), src))
                            .collect(),
                        false => vec![(None, (s, port))],
                    };
                    for (bus, (src, sport)) in lowered {
                        match bus {
                            Some(bus) if comp[&src] != c => {
                                let sig = in_signals
                                    .entry(bus)
                                    .or_insert_with(|| {
                                        let channels = bus_width.get(&bus).copied().unwrap_or(1);
                                        let (path, label, output) = bus_param_at(graph, bus);
                                        let bus_param = builder.push_control_param(&path, &label);
                                        let unit = builder.push_unit(UnitSpec::new(
                                            "In",
                                            Rate::Audio,
                                            vec![InputRef::Param(bus_param)],
                                            channels,
                                        ));
                                        bus_reads.push(BusBinding {
                                            node_path: path,
                                            channels,
                                            unit: unit as usize,
                                            param: bus_param as usize,
                                            output,
                                        });
                                        (0..channels as u32)
                                            .map(|output| InputRef::Unit { unit, output })
                                            .collect()
                                    })
                                    .clone();
                                sigs.push(sig);
                            }
                            // A dangling port materializes nothing.
                            _ => sigs.extend(outputs.get(&src).and_then(|o| o.get(sport)).cloned()),
                        }
                    }
                }
                // `None` iff no summand materialized a signal (unconnected, an
                // unsourced boundary, or a dangling port), so hybrid inputs
                // fall back to their param exactly when the Steel side keeps
                // it driven.
                inputs.push((!sigs.is_empty()).then(|| sum_signals(&mut builder, &sigs)));
            }
            let outs = dsp.ugens(&graph[n].node_path(n.index()), &inputs, &mut builder);
            debug_assert_eq!(
                outs.len(),
                dsp.n_dsp_outputs(),
                "a node must return one Signal per dsp output port",
            );
            outputs.insert(n, outs);
        }

        // Emit the region's bus writes: lift each channel to audio, apply the
        // driver's fade gain (one per param path), and write to a bus-index
        // param.
        let mut fades: HashMap<Vec<usize>, u32> = HashMap::new();
        let mut bus_writes = Vec::with_capacity(writes.len());
        for (bus, (src, port)) in writes {
            let sig = outputs
                .get(&src)
                .and_then(|o| o.get(port))
                .cloned()
                .unwrap_or_else(|| Signal::silent(1));
            let (path, label, output) = bus_param_at(graph, bus);
            let fade = *fades
                .entry(path.clone())
                .or_insert_with(|| builder.push_fade_gain(&path));
            let bus_param = builder.push_control_param(&path, &label);
            let mut out_inputs = vec![InputRef::Param(bus_param)];
            for ch in sig.channels() {
                let ch = builder.ensure_audio(ch);
                let mul = builder.push_unit(UnitSpec {
                    name: "BinaryOpUGen".to_string(),
                    rate: Rate::Audio,
                    inputs: vec![ch, InputRef::Param(fade)],
                    num_outputs: 1,
                    special_index: 2,
                });
                out_inputs.push(InputRef::Unit {
                    unit: mul,
                    output: 0,
                });
            }
            let unit = builder.push_unit(UnitSpec::new("Out", Rate::Audio, out_inputs, 0));
            bus_writes.push(BusBinding {
                node_path: path,
                channels: sig.width(),
                unit: unit as usize,
                param: bus_param as usize,
                output,
            });
            bus_width.insert(bus, sig.width());
        }

        // A stable region identity: its sink and boundary roles + node paths
        // (an endpoint bus also hashes its output port; a classic bus hashes
        // nothing extra, so pre-summing region keys survive).
        let mut h = DefaultHasher::new();
        for s in &region_sinks {
            (0u8, graph[*s].node_path(s.index())).hash(&mut h);
        }
        for w in &bus_writes {
            (1u8, &w.node_path).hash(&mut h);
            if let Some(o) = w.output {
                o.hash(&mut h);
            }
        }
        for r in &bus_reads {
            (2u8, &r.node_path).hash(&mut h);
            if let Some(o) = r.output {
                o.hash(&mut h);
            }
        }
        let key = h.finish();

        let name = format!("{name_prefix}-{key:016x}");
        let (def, params, monitors, gains) = builder.finish(name);
        regions.push(RegionDerived {
            key,
            derived: Derived {
                def,
                params,
                monitors,
                gains,
            },
            bus_writes,
            bus_reads,
        });
    }
    Ok(regions)
}

/// The param path, label and endpoint port of a region bus: a classic bus is
/// keyed by the effective `~bus` node with the plain `"bus"` label, an
/// endpoint bus by its source node with a port-suffixed `"bus{port}"` label
/// (matching the instancing pipeline's `Src` convention).
fn bus_param_at<N: ToNodeDsp>(
    graph: &Graph<N>,
    bus: RegionBus,
) -> (Vec<usize>, String, Option<usize>) {
    match bus {
        RegionBus::Bus(b) => (graph[b].node_path(b.index()), "bus".to_string(), None),
        RegionBus::Src(s, port) => (
            graph[s].node_path(s.index()),
            format!("bus{port}"),
            Some(port),
        ),
    }
}

/// The content-addressed name for a def with the given [`structural_sig`].
///
/// Purely a function of the def's structure - no head or region prefix - so
/// structurally identical defs derived from different heads (or from many
/// instances of one referenced child graph) collide by design, and the audio
/// driver's per-name install refcounting shares one installed def between
/// them. Names change exactly when the structure does.
pub fn content_def_name(sig: u64) -> String {
    format!("gantz-def-{sig:016x}")
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
