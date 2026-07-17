//! Root-level DSP port classification for a viewed graph (see
//! [`root_port_info`]), the data behind DSP edge styling.
//!
//! The GUI shows one graph per head, so an edge endpoint is fully identified
//! by a `(root node index, port)` pair. This module classifies those pairs
//! as *signal* ports (they carry DSP signals at derive time) or control
//! ports, and attaches the [`PortShape`] derivation recorded for each signal
//! output where one is recoverable. Typed probes like [`ToNodeDsp`] are
//! unreachable through the GUI's erased registry, so callers (e.g. a bevy
//! provider system) compute this where the concrete node type is known and
//! hand the result to the UI - the same shape as
//! [`dsp_graphs`][crate::ref_ext::dsp_graphs].
//!
//! Classification is structural, mirroring how flattening lowers the graph
//! (see [`flatten`][crate::flatten()]): a concrete DSP node's ports come
//! straight off [`NodeDsp`][crate::NodeDsp], a reference's ports resolve
//! recursively through the referenced graph's inlets/outlets, and root-level
//! boundary nodes forward their neighbours' classification. One deliberate
//! approximation: a chain that resolves through a referenced graph's *inlet*
//! (parent-side wiring, e.g. a pure `inlet -> outlet` wire child) is not
//! followed, so such a reference's ports classify as control.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use petgraph::Direction;
use petgraph::visit::EdgeRef;

use gantz_ca::ContentAddr;
use gantz_core::node::graph::{Graph, NodeIx};
use gantz_core::node::{AsRefNode, MetaCtx};
use plyphon::Rate;

use crate::dsp::{PortShape, PortShapes, ToNodeDsp};

/// Root-level DSP port classification for one head's viewed graph, keyed by
/// `(root node index, port)`. A pair absent from both maps is a control
/// port.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RootPortInfo {
    /// The signal *input* ports (the ports the synthdef compiler would wire).
    pub signal_inputs: BTreeSet<(usize, usize)>,
    /// The signal *output* ports, with the shape derivation recorded for
    /// them. `None` when no shape is recoverable: the port fed no sink at
    /// derive time, or the derivation's shapes are unavailable (e.g. a
    /// nested view whose own head derived silent).
    pub signal_outputs: BTreeMap<(usize, usize), Option<PortShape>>,
}

/// Memoized reference probes, shared across one [`root_port_info`] pass so
/// repeated references stay linear overall. Stacks guard reference cycles:
/// a chain re-entering a graph it is already resolving through contributes
/// nothing (mirroring [`dsp_graphs`][crate::ref_ext::dsp_graphs]).
#[derive(Default)]
struct Memos {
    inlets: HashMap<ContentAddr, Vec<bool>>,
    inlet_stack: Vec<ContentAddr>,
    outlets: HashMap<(ContentAddr, usize), Vec<(Vec<usize>, usize)>>,
    outlet_stack: Vec<(ContentAddr, usize)>,
}

/// Classify `graph`'s root-level ports, resolving references through
/// `registry` and attaching the [`PortShape`]s recorded in `shapes` (the
/// union of the head's derived parts' shapes, keyed by node paths absolute
/// to `graph` - see [`PortShapes`]).
pub fn root_port_info<N>(
    graph: &Graph<N>,
    registry: &gantz_ca::Registry<Graph<N>>,
    shapes: &PortShapes,
) -> RootPortInfo
where
    N: gantz_core::Node + AsRefNode + ToNodeDsp,
{
    let get_node = |ca: &ContentAddr| {
        registry
            .graph(&(*ca).into())
            .map(|g| g as &dyn gantz_core::Node)
    };
    let ctx = MetaCtx::new(&get_node);
    let mut memos = Memos::default();
    let mut info = RootPortInfo::default();

    // Concrete DSP nodes and references.
    for ix in graph.node_indices() {
        let i = ix.index();
        if let Some(dsp) = graph[ix].to_node_dsp() {
            for p in 0..dsp.n_dsp_inputs() {
                info.signal_inputs.insert((i, p));
            }
            for p in 0..dsp.n_dsp_outputs() {
                let shape = shapes.get(&(vec![i], p)).copied();
                info.signal_outputs.insert((i, p), shape);
            }
        } else if let Some(r) = graph[ix].as_ref_node() {
            let ca = r.content_addr();
            for (p, signal) in signal_inlets(registry, ctx, ca, &mut memos)
                .iter()
                .enumerate()
            {
                if *signal {
                    info.signal_inputs.insert((i, p));
                }
            }
            for p in 0..n_outlets(registry, ctx, ca) {
                let sources = outlet_sources(registry, ctx, ca, p, &mut memos);
                if !sources.is_empty() {
                    info.signal_outputs
                        .insert((i, p), sum_shape(shapes, i, &sources));
                }
            }
        }
    }

    // Root-level boundary nodes forward their neighbours' classification, so
    // nested views style their interface edges too. Inlets before outlets:
    // an outlet fed by a signal-classified inlet then classifies signal.
    for ix in graph.node_indices() {
        if graph[ix].inlet(ctx) {
            let signal = graph.edges_directed(ix, Direction::Outgoing).any(|e| {
                let dst = (e.target().index(), e.weight().input.0 as usize);
                info.signal_inputs.contains(&dst)
            });
            if signal {
                info.signal_outputs.insert((ix.index(), 0), None);
            }
        }
    }
    for ix in graph.node_indices() {
        if graph[ix].outlet(ctx) {
            let signal = graph.edges_directed(ix, Direction::Incoming).any(|e| {
                let src = (e.source().index(), e.weight().output.0 as usize);
                info.signal_outputs.contains_key(&src)
            });
            if signal {
                info.signal_inputs.insert((ix.index(), 0));
            }
        }
    }

    info
}

/// Which of the graph-at-`ca`'s inlets (ascending node index, the "input i
/// to inlet i" contract) transitively feed a DSP input, i.e. which of a
/// reference's inputs carry signals. Empty when `ca` is unresolved or
/// re-entered (a reference cycle).
fn signal_inlets<N>(
    registry: &gantz_ca::Registry<Graph<N>>,
    ctx: MetaCtx,
    ca: ContentAddr,
    memos: &mut Memos,
) -> Vec<bool>
where
    N: gantz_core::Node + AsRefNode + ToNodeDsp,
{
    if let Some(known) = memos.inlets.get(&ca) {
        return known.clone();
    }
    if memos.inlet_stack.contains(&ca) {
        return Vec::new();
    }
    let Some(graph) = registry.graph(&ca.into()) else {
        memos.inlets.insert(ca, Vec::new());
        return Vec::new();
    };
    memos.inlet_stack.push(ca);
    let inlets: Vec<_> = graph
        .node_indices()
        .filter(|&n| graph[n].inlet(ctx))
        .collect();
    let mut result = Vec::with_capacity(inlets.len());
    for inlet in inlets {
        let consumers: Vec<(usize, usize)> = graph
            .edges_directed(inlet, Direction::Outgoing)
            .map(|e| (e.target().index(), e.weight().input.0 as usize))
            .collect();
        let signal = consumers.into_iter().any(|(t, tp)| {
            let t = NodeIx::new(t);
            if let Some(dsp) = graph[t].to_node_dsp() {
                tp < dsp.n_dsp_inputs()
            } else if let Some(r) = graph[t].as_ref_node() {
                signal_inlets(registry, ctx, r.content_addr(), memos)
                    .get(tp)
                    .copied()
                    .unwrap_or(false)
            } else {
                false
            }
        });
        result.push(signal);
    }
    memos.inlet_stack.pop();
    memos.inlets.insert(ca, result.clone());
    result
}

/// The number of outlets of the graph at `ca` (a reference's output count).
fn n_outlets<N>(registry: &gantz_ca::Registry<Graph<N>>, ctx: MetaCtx, ca: ContentAddr) -> usize
where
    N: gantz_core::Node,
{
    registry
        .graph(&ca.into())
        .map(|g| g.node_indices().filter(|&n| g[n].outlet(ctx)).count())
        .unwrap_or(0)
}

/// The concrete DSP `(path relative to the graph at ca, output port)`
/// sources that transitively feed the graph-at-`ca`'s `outlet`-th outlet,
/// i.e. the DSP sources behind a reference's output. Chains that dead-end
/// (a non-DSP source, an inlet - parent-side wiring - or a cycle) contribute
/// nothing; an empty result means the output carries no signal.
fn outlet_sources<N>(
    registry: &gantz_ca::Registry<Graph<N>>,
    ctx: MetaCtx,
    ca: ContentAddr,
    outlet: usize,
    memos: &mut Memos,
) -> Vec<(Vec<usize>, usize)>
where
    N: gantz_core::Node + AsRefNode + ToNodeDsp,
{
    let key = (ca, outlet);
    if let Some(known) = memos.outlets.get(&key) {
        return known.clone();
    }
    if memos.outlet_stack.contains(&key) {
        return Vec::new();
    }
    let Some(graph) = registry.graph(&ca.into()) else {
        memos.outlets.insert(key, Vec::new());
        return Vec::new();
    };
    memos.outlet_stack.push(key);
    let mut sources = Vec::new();
    let outlet_node = graph
        .node_indices()
        .filter(|&n| graph[n].outlet(ctx))
        .nth(outlet);
    if let Some(o) = outlet_node {
        let feeds: Vec<(usize, usize)> = graph
            .edges_directed(o, Direction::Incoming)
            .filter(|e| e.weight().input.0 == 0)
            .map(|e| (e.source().index(), e.weight().output.0 as usize))
            .collect();
        for (s, sp) in feeds {
            let six = NodeIx::new(s);
            if let Some(dsp) = graph[six].to_node_dsp() {
                if sp < dsp.n_dsp_outputs() {
                    sources.push((vec![s], sp));
                }
            } else if let Some(r) = graph[six].as_ref_node() {
                for (rel, port) in outlet_sources(registry, ctx, r.content_addr(), sp, memos) {
                    let mut path = vec![s];
                    path.extend(rel);
                    sources.push((path, port));
                }
            }
        }
    }
    memos.outlet_stack.pop();
    memos.outlets.insert(key, sources.clone());
    sources
}

/// The shape of a reference output fed by the given concrete DSP `sources`
/// (paths relative to the reference, which sits at root index `root_ix`).
/// Derivation sums a multi-fed input, so the width is the widest summand and
/// the rate is audio if any summand is audio (mirroring
/// [`sum_signals`][crate::dsp::sum_signals] + [`signal_rate`][crate::signal_rate]).
/// `None` when no source has a recorded shape.
fn sum_shape(
    shapes: &PortShapes,
    root_ix: usize,
    sources: &[(Vec<usize>, usize)],
) -> Option<PortShape> {
    let mut found = sources.iter().filter_map(|(rel, port)| {
        let mut path = Vec::with_capacity(rel.len() + 1);
        path.push(root_ix);
        path.extend(rel);
        shapes.get(&(path, *port)).copied()
    });
    let first = found.next()?;
    Some(found.fold(first, |acc, s| PortShape {
        width: acc.width.max(s.width),
        rate: match (acc.rate, s.rate) {
            (Rate::Audio, _) | (_, Rate::Audio) => Rate::Audio,
            (rate, _) => rate,
        },
    }))
}
