//! Tests for `derive_synthdefs`: cutting a DSP graph into per-region synthdefs
//! at `~bus` boundaries, and running the split defs through the real engine.

use gantz_core::edge::Edge;
use gantz_core::node::graph::Graph;
use gantz_plyphon::{
    Bus, DeriveError, Lag, NodeDsp, Out, Pack, ScopeOut, SinOsc, ToNodeDsp, derive_synthdef,
    derive_synthdefs, structural_sig,
};
use plyphon::synthdef::InputRef;
use plyphon::{AddAction, Options, ROOT_GROUP_ID, Rate, World, engine};

const SR: f32 = 48_000.0;

/// A minimal erased node enum, standing in for the app's `Box<dyn Node>`.
enum N {
    SinOsc(SinOsc),
    Lag(Lag),
    Out(Out),
    ScopeOut(ScopeOut),
    Pack(Pack),
    Bus(Bus),
    Other,
}

impl ToNodeDsp for N {
    fn to_node_dsp(&self) -> Option<&dyn NodeDsp> {
        match self {
            N::SinOsc(s) => Some(s),
            N::Lag(l) => Some(l),
            N::Out(o) => Some(o),
            N::ScopeOut(t) => Some(t),
            N::Pack(p) => Some(p),
            N::Bus(b) => Some(b),
            N::Other => None,
        }
    }
}

#[test]
fn sine_bus_out_splits_two_regions() {
    // `~sinosc -> ~bus -> ~out`: two regions in writer-first order. The writer
    // ends in a fade-gained `Out` to a placeholder bus; the reader begins with
    // an `In` of the inferred width.
    let mut g = Graph::<N>::default();
    let s = g.add_node(N::SinOsc(SinOsc::default()));
    let b = g.add_node(N::Bus(Bus::default()));
    let o = g.add_node(N::Out(Out::default()));
    g.add_edge(s, b, Edge::new(0.into(), 0.into()));
    g.add_edge(b, o, Edge::new(0.into(), 0.into()));

    let regions = derive_synthdefs(&g, 1, "head").expect("derive");
    assert_eq!(regions.len(), 2);
    let (writer, reader) = (&regions[0], &regions[1]);
    assert!(writer.derived.def.name.starts_with("head-"));
    assert_ne!(writer.key, reader.key);

    // Writer: SinOsc(0) -> fade mul(1) -> Out(2, bus placeholder).
    let wdef = &writer.derived.def;
    assert_eq!(wdef.units.len(), 3, "SinOsc + fade mul + bus Out");
    assert_eq!(wdef.units[0].name, "SinOsc");
    assert_eq!(wdef.units[2].name, "Out");
    assert_eq!(writer.bus_writes.len(), 1);
    assert!(writer.bus_reads.is_empty());
    let w = &writer.bus_writes[0];
    assert_eq!(w.node_path, vec![b.index()]);
    assert_eq!(w.channels, 1);
    assert_eq!(wdef.units[w.unit].name, "Out");
    assert!(matches!(wdef.units[w.unit].inputs[0], InputRef::Param(p) if p == w.param as u32));
    assert_eq!(
        writer.derived.gains.len(),
        1,
        "the bus write carries a fade gain",
    );
    assert!(
        !writer
            .derived
            .params
            .iter()
            .any(|p| p.index == writer.derived.gains[0].index),
        "the fade has no node binding",
    );

    // Reader: In(0) -> level mul(1) -> channel mul(2) -> hardware Out(3).
    let rdef = &reader.derived.def;
    assert_eq!(rdef.units.len(), 4, "In + level/channel muls + Out");
    assert_eq!(reader.bus_reads.len(), 1);
    assert!(reader.bus_writes.is_empty());
    let r = &reader.bus_reads[0];
    assert_eq!(r.node_path, vec![b.index()]);
    assert_eq!(r.channels, 1);
    assert_eq!(rdef.units[r.unit].name, "In");
    assert_eq!(rdef.units[r.unit].num_outputs, 1);
    assert!(matches!(rdef.units[r.unit].inputs[0], InputRef::Param(p) if p == r.param as u32));
}

#[test]
fn width_flows_across_the_boundary() {
    // 2 sines -> ~pack(2) -> ~bus -> ~out: the writer's bus `Out` carries both
    // channels (each fade-gained) and the reader's `In` is 2 wide.
    let mut g = Graph::<N>::default();
    let s0 = g.add_node(N::SinOsc(SinOsc::default()));
    let s1 = g.add_node(N::SinOsc(SinOsc::default()));
    let pk = g.add_node(N::Pack(Pack::default()));
    let b = g.add_node(N::Bus(Bus::default()));
    let o = g.add_node(N::Out(Out::default()));
    g.add_edge(s0, pk, Edge::new(0.into(), 0.into()));
    g.add_edge(s1, pk, Edge::new(0.into(), 1.into()));
    g.add_edge(pk, b, Edge::new(0.into(), 0.into()));
    g.add_edge(b, o, Edge::new(0.into(), 0.into()));

    let regions = derive_synthdefs(&g, 2, "head").expect("derive");
    assert_eq!(regions.len(), 2);
    let (writer, reader) = (&regions[0], &regions[1]);
    let w = &writer.bus_writes[0];
    assert_eq!(w.channels, 2);
    assert_eq!(
        writer.derived.def.units[w.unit].inputs.len(),
        1 + 2,
        "bus + one signal per channel",
    );
    let r = &reader.bus_reads[0];
    assert_eq!(r.channels, 2);
    assert_eq!(reader.derived.def.units[r.unit].num_outputs, 2);
}

#[test]
fn same_region_bus_is_a_wire() {
    // A `~bus` whose two sides share a region (an uncut path also connects
    // them): one region, no bus units - the def equals the bus-less graph's.
    let mut g = Graph::<N>::default();
    let s = g.add_node(N::SinOsc(SinOsc::default()));
    let b = g.add_node(N::Bus(Bus::default()));
    let pk = g.add_node(N::Pack(Pack::default()));
    let o = g.add_node(N::Out(Out::default()));
    g.add_edge(s, b, Edge::new(0.into(), 0.into())); // sine -> bus -> pack ch 0
    g.add_edge(b, pk, Edge::new(0.into(), 0.into()));
    g.add_edge(s, pk, Edge::new(0.into(), 1.into())); // sine -> pack ch 1 (uncut)
    g.add_edge(pk, o, Edge::new(0.into(), 0.into()));

    let regions = derive_synthdefs(&g, 1, "head").expect("derive");
    assert_eq!(regions.len(), 1);
    let region = &regions[0];
    assert!(region.bus_writes.is_empty() && region.bus_reads.is_empty());

    // The same graph without the bus derives the identical unit list.
    let mut g2 = Graph::<N>::default();
    let s2 = g2.add_node(N::SinOsc(SinOsc::default()));
    let pk2 = g2.add_node(N::Pack(Pack::default()));
    let o2 = g2.add_node(N::Out(Out::default()));
    g2.add_edge(s2, pk2, Edge::new(0.into(), 0.into()));
    g2.add_edge(s2, pk2, Edge::new(0.into(), 1.into()));
    g2.add_edge(pk2, o2, Edge::new(0.into(), 0.into()));
    let plain = derive_synthdef(&g2, 1, "t").expect("derive");
    let names = |def: &plyphon::synthdef::SynthDef| {
        def.units.iter().map(|u| u.name.clone()).collect::<Vec<_>>()
    };
    assert_eq!(names(&region.derived.def), names(&plain.def));
}

#[test]
fn bus_chain_aliases_upstream() {
    // `~bus -> ~bus` aliases rather than relaying: still two regions, and the
    // reader reads the *upstream* bus's path.
    let mut g = Graph::<N>::default();
    let s = g.add_node(N::SinOsc(SinOsc::default()));
    let b1 = g.add_node(N::Bus(Bus::default()));
    let b2 = g.add_node(N::Bus(Bus::default()));
    let o = g.add_node(N::Out(Out::default()));
    g.add_edge(s, b1, Edge::new(0.into(), 0.into()));
    g.add_edge(b1, b2, Edge::new(0.into(), 0.into()));
    g.add_edge(b2, o, Edge::new(0.into(), 0.into()));

    let regions = derive_synthdefs(&g, 1, "head").expect("derive");
    assert_eq!(regions.len(), 2);
    assert_eq!(regions[0].bus_writes[0].node_path, vec![b1.index()]);
    assert_eq!(regions[1].bus_reads[0].node_path, vec![b1.index()]);
}

#[test]
fn unconnected_bus_reads_silence() {
    // A `~bus` with nothing upstream: one region, no bus units, the consumer's
    // channel resolves to silence.
    let mut g = Graph::<N>::default();
    let b = g.add_node(N::Bus(Bus::default()));
    let o = g.add_node(N::Out(Out::default()));
    g.add_edge(b, o, Edge::new(0.into(), 0.into()));

    let regions = derive_synthdefs(&g, 1, "head").expect("derive");
    assert_eq!(regions.len(), 1);
    let region = &regions[0];
    assert!(region.bus_writes.is_empty() && region.bus_reads.is_empty());
    assert!(!region.derived.def.units.iter().any(|u| u.name == "In"));
    let mul = region
        .derived
        .def
        .units
        .iter()
        .find(|u| u.name == "BinaryOpUGen" && matches!(u.rate, Rate::Audio))
        .expect("channel mul");
    assert!(matches!(mul.inputs[0], InputRef::Constant(c) if c == 0.0));
}

#[test]
fn bus_into_scopeout_only() {
    // `~sinosc -> ~bus -> ~scopeout`: the monitor roots the reader region; its
    // binding lands in the reader's `Derived`.
    let mut g = Graph::<N>::default();
    let s = g.add_node(N::SinOsc(SinOsc::default()));
    let b = g.add_node(N::Bus(Bus::default()));
    let t = g.add_node(N::ScopeOut(ScopeOut::default()));
    g.add_edge(s, b, Edge::new(0.into(), 0.into()));
    g.add_edge(b, t, Edge::new(0.into(), 0.into()));

    let regions = derive_synthdefs(&g, 1, "head").expect("derive");
    assert_eq!(regions.len(), 2);
    let reader = &regions[1];
    assert_eq!(reader.derived.monitors.len(), 1);
    assert_eq!(reader.derived.monitors[0].node_path, vec![t.index()]);
    let names: Vec<&str> = reader
        .derived
        .def
        .units
        .iter()
        .map(|u| u.name.as_str())
        .collect();
    assert_eq!(names, vec!["In", "ScopeOut"]);
}

#[test]
fn two_buses_between_the_same_regions() {
    // Two `~bus`es from one writer region into one reader region: still two
    // regions, with two write/read pairs.
    let mut g = Graph::<N>::default();
    let s = g.add_node(N::SinOsc(SinOsc::default()));
    let b0 = g.add_node(N::Bus(Bus::default()));
    let b1 = g.add_node(N::Bus(Bus::default()));
    let pk = g.add_node(N::Pack(Pack::default()));
    let o = g.add_node(N::Out(Out::default()));
    g.add_edge(s, b0, Edge::new(0.into(), 0.into()));
    g.add_edge(s, b1, Edge::new(0.into(), 0.into()));
    g.add_edge(b0, pk, Edge::new(0.into(), 0.into()));
    g.add_edge(b1, pk, Edge::new(0.into(), 1.into()));
    g.add_edge(pk, o, Edge::new(0.into(), 0.into()));

    let regions = derive_synthdefs(&g, 1, "head").expect("derive");
    assert_eq!(regions.len(), 2);
    assert_eq!(regions[0].bus_writes.len(), 2);
    assert_eq!(regions[1].bus_reads.len(), 2);
}

#[test]
fn bus_cycle_is_rejected() {
    // Two regions reading each other's buses: no writer-before-reader order
    // exists, so derivation reports the cycle (deliberate feedback is a planned
    // InFeedback follow-up).
    let mut g = Graph::<N>::default();
    let l0 = g.add_node(N::Lag(Lag::default()));
    let l1 = g.add_node(N::Lag(Lag::default()));
    let b0 = g.add_node(N::Bus(Bus::default()));
    let b1 = g.add_node(N::Bus(Bus::default()));
    let o = g.add_node(N::Out(Out::default()));
    g.add_edge(l0, b0, Edge::new(0.into(), 0.into()));
    g.add_edge(b0, l1, Edge::new(0.into(), 0.into()));
    g.add_edge(l1, b1, Edge::new(0.into(), 0.into()));
    g.add_edge(b1, l0, Edge::new(0.into(), 0.into()));
    g.add_edge(l1, o, Edge::new(0.into(), 0.into()));

    assert!(matches!(
        derive_synthdefs(&g, 1, "head"),
        Err(DeriveError::BusCycle),
    ));
}

#[test]
fn no_boundary_graph_matches_single_def() {
    // Without a `~bus`, `derive_synthdefs` yields one region whose def matches
    // `derive_synthdef`'s exactly (modulo the key-suffixed name), and the key is
    // stable across unrelated (non-dsp) additions.
    let mut g = Graph::<N>::default();
    let s = g.add_node(N::SinOsc(SinOsc::default()));
    let l = g.add_node(N::Lag(Lag::default()));
    let o = g.add_node(N::Out(Out::default()));
    g.add_edge(s, l, Edge::new(0.into(), 0.into()));
    g.add_edge(l, o, Edge::new(0.into(), 0.into()));

    let regions = derive_synthdefs(&g, 1, "head").expect("derive");
    assert_eq!(regions.len(), 1);
    let single = derive_synthdef(&g, 1, "t").expect("derive");
    assert_eq!(
        format!("{:?}", regions[0].derived.def.units),
        format!("{:?}", single.def.units),
    );
    assert_eq!(
        format!("{:?}", regions[0].derived.def.params),
        format!("{:?}", single.def.params),
    );

    let key = regions[0].key;
    g.add_node(N::Other);
    let regions = derive_synthdefs(&g, 1, "head").expect("derive");
    assert_eq!(regions[0].key, key, "unrelated additions keep the key");
}

#[test]
fn bus_index_param_is_unlagged_and_sig_stable() {
    // The bus channel index is a no-lag control param (a lagged bus index would
    // glide through wrong buses), baked at `0.0` and set per spawn via
    // `set_control`. The driver never mutates the def, so `structural_sig` is
    // computed on the final def and is stable across re-derives.
    let mut g = Graph::<N>::default();
    let s = g.add_node(N::SinOsc(SinOsc::default()));
    let b = g.add_node(N::Bus(Bus::default()));
    let o = g.add_node(N::Out(Out::default()));
    g.add_edge(s, b, Edge::new(0.into(), 0.into()));
    g.add_edge(b, o, Edge::new(0.into(), 0.into()));

    let regions = derive_synthdefs(&g, 1, "head").expect("derive");
    let writer = &regions[0];
    let w = &writer.bus_writes[0];
    let param = &writer.derived.def.params[w.param];
    assert_eq!(param.default, 0.0, "bus-index defaults to 0.0");
    assert_eq!(param.lag, None, "bus-index must be unlagged");
    assert!(
        !writer.derived.params.iter().any(|b| b.index == w.param),
        "the bus-index param has no node binding (the driver alone sets it)",
    );

    let sig = structural_sig(&writer.derived.def);
    let regions2 = derive_synthdefs(&g, 1, "head").expect("re-derive");
    assert_eq!(
        sig,
        structural_sig(&regions2[0].derived.def),
        "re-deriving the same graph yields the same sig (no per-spawn patching)",
    );
}

#[test]
fn split_regions_play_through_the_engine() {
    // `~sinosc -> ~bus -> ~out` end to end offline: set each region's bus-index
    // param to a real private channel via `set_control`, spawn writer-then-reader,
    // and the tone crosses the boundary. Reversed spawn order renders silence -
    // `In` reads only channels written EARLIER in the node tree this block (the ordering
    // requirement the driver's topo placement exists for).
    let mut g = Graph::<N>::default();
    let s = g.add_node(N::SinOsc(SinOsc::default()));
    let b = g.add_node(N::Bus(Bus::default()));
    let o = g.add_node(N::Out(Out::default()));
    g.add_edge(s, b, Edge::new(0.into(), 0.into()));
    g.add_edge(b, o, Edge::new(0.into(), 0.into()));

    let opts = || Options {
        sample_rate: SR as f64,
        output_channels: 1,
        ..Options::default()
    };
    // The first private channel sits after the hardware output + input banks.
    let bus = (opts().output_channels + opts().input_channels) as f32;

    let regions = derive_synthdefs(&g, 1, "head").expect("derive");
    let (writer, reader) = (&regions[0], &regions[1]);

    let rms = |s: &[f32]| (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt();
    let render = |world: &mut World, frames: usize| {
        let mut out = vec![0.0f32; frames];
        for block in out.chunks_mut(64) {
            world.fill(block, 1);
        }
        out
    };
    // Wire a region's bus-index param and ramp its fade gains to unity after
    // spawning (the synth spawns silent behind its baked fade 0.0 default).
    let wire = |controller: &mut plyphon::Controller,
                node: i32,
                bus: &gantz_plyphon::BusBinding,
                gains: &[gantz_plyphon::GainRef],
                channel: f32| {
        controller
            .set_control(node, bus.param, channel)
            .expect("set bus");
        for g in gains {
            controller.set_control(node, g.index, 1.0).expect("fade in");
        }
    };

    // Writer before reader: the tone crosses the bus.
    let (mut controller, _nrt, mut world) = engine(opts());
    controller.add_synthdef(writer.derived.def.clone());
    controller.add_synthdef(reader.derived.def.clone());
    let w = controller
        .synth_new(&writer.derived.def.name, ROOT_GROUP_ID, AddAction::Tail)
        .expect("writer");
    let r = controller
        .synth_new(&reader.derived.def.name, ROOT_GROUP_ID, AddAction::Tail)
        .expect("reader");
    wire(
        &mut controller,
        w,
        &writer.bus_writes[0],
        &writer.derived.gains,
        bus,
    );
    wire(
        &mut controller,
        r,
        &reader.bus_reads[0],
        &reader.derived.gains,
        bus,
    );
    let out = render(&mut world, SR as usize / 4);
    assert!(
        rms(&out) > 0.05,
        "tone must cross the bus: rms={}",
        rms(&out)
    );
    let (m220, m440) = (goertzel(&out, 220.0), goertzel(&out, 440.0));
    assert!(m220 > 5.0 * m440, "220 Hz dominant: {m220} vs {m440}");

    // Reader before writer: silence (documents the ordering requirement).
    let (mut controller, _nrt, mut world) = engine(opts());
    controller.add_synthdef(writer.derived.def.clone());
    controller.add_synthdef(reader.derived.def.clone());
    let r = controller
        .synth_new(&reader.derived.def.name, ROOT_GROUP_ID, AddAction::Tail)
        .expect("reader");
    let w = controller
        .synth_new(&writer.derived.def.name, ROOT_GROUP_ID, AddAction::Tail)
        .expect("writer");
    wire(
        &mut controller,
        w,
        &writer.bus_writes[0],
        &writer.derived.gains,
        bus,
    );
    wire(
        &mut controller,
        r,
        &reader.bus_reads[0],
        &reader.derived.gains,
        bus,
    );
    let out = render(&mut world, SR as usize / 4);
    assert!(
        rms(&out) < 1e-4,
        "a reader ordered before its writer hears silence: rms={}",
        rms(&out),
    );
}

/// Goertzel magnitude estimate at `freq` (Hz) over mono `samples` sampled at [`SR`].
fn goertzel(samples: &[f32], freq: f32) -> f32 {
    let n = samples.len();
    let k = (0.5 + n as f32 * freq / SR).floor();
    let w = 2.0 * std::f32::consts::PI * k / n as f32;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for &x in samples {
        let s = x + coeff * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
    power.max(0.0).sqrt() / n as f32
}
