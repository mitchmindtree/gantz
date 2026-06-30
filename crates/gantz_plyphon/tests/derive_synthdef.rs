//! Tests that `derive_synthdef` builds the right plyphon `SynthDef` from a DSP
//! graph, and that the derived def actually produces the expected audio when run
//! through the real engine offline.

use gantz_core::edge::Edge;
use gantz_core::node::graph::{Graph, NodeIx};
use gantz_plyphon::{DeriveError, NodeDsp, Out, Sine, ToNodeDsp, derive_synthdef, structural_sig};
use plyphon::synthdef::InputRef;
use plyphon::{AddAction, Options, ROOT_GROUP_ID, World, engine};

const SR: f32 = 48_000.0;

/// A minimal erased node enum, standing in for the app's `Box<dyn Node>`.
/// `Other` is a non-DSP node (a stand-in for any control-rate node).
enum N {
    Sine(Sine),
    Out(Out),
    Other,
}

impl ToNodeDsp for N {
    fn to_node_dsp(&self) -> Option<&dyn NodeDsp> {
        match self {
            N::Sine(s) => Some(s),
            N::Out(o) => Some(o),
            N::Other => None,
        }
    }
}

/// Build a `~sine(freq) -> ~out(gain)` graph, returning it with the `~out` index.
fn sine_to_out(freq: f32, gain: f32) -> (Graph<N>, NodeIx) {
    let mut g = Graph::<N>::default();
    let mut sine = Sine::default();
    sine.set_freq(freq);
    let mut out = Out::default();
    out.set_gain(gain);
    let s = g.add_node(N::Sine(sine));
    let o = g.add_node(N::Out(out));
    g.add_edge(s, o, Edge::new(0.into(), 0.into()));
    (g, o)
}

#[test]
fn derives_expected_units() {
    let (g, out_ix) = sine_to_out(220.0, 0.2);
    let def = derive_synthdef(&g, out_ix, 1, "test").expect("derive");

    assert_eq!(def.units.len(), 3, "SinOsc + gain-mul + Out");

    // Two settable control params: the sine's freq (param 0) and the out's gain
    // (param 1) - so value edits become `set_control`, not respawns.
    assert_eq!(def.params.len(), 2);
    assert!(def.params[0].name.ends_with("/freq"));
    assert_eq!(def.params[0].default, 220.0);
    assert_eq!(def.params[0].lag, None, "freq is unsmoothed by default");
    assert!(def.params[1].name.ends_with("/gain"));
    assert_eq!(def.params[1].default, 0.2);
    assert_eq!(
        def.params[1].lag,
        Some(0.01),
        "gain has a default de-click lag"
    );

    // unit 0: SinOsc.ar(freq-param, 0)
    assert_eq!(def.units[0].name, "SinOsc");
    assert!(matches!(def.units[0].inputs[0], InputRef::Param(0)));

    // unit 1: BinaryOpUGen multiply (SinOsc * gain-param)
    assert_eq!(def.units[1].name, "BinaryOpUGen");
    assert_eq!(def.units[1].special_index, 2, "multiply selector");
    assert!(matches!(
        def.units[1].inputs[0],
        InputRef::Unit { unit: 0, output: 0 }
    ));
    assert!(matches!(def.units[1].inputs[1], InputRef::Param(1)));

    // unit 2: Out.ar(0, gained)
    assert_eq!(def.units[2].name, "Out");
    assert_eq!(def.units[2].num_outputs, 0);
    assert!(matches!(def.units[2].inputs[0], InputRef::Constant(b) if b == 0.0));
    assert!(matches!(
        def.units[2].inputs[1],
        InputRef::Unit { unit: 1, output: 0 }
    ));
}

#[test]
fn structural_sig_stable_across_value_changes() {
    // A value change keeps the structural signature (so the driver `set_control`s
    // rather than respawning).
    let (g220, out220) = sine_to_out(220.0, 0.2);
    let (g440, out440) = sine_to_out(440.0, 0.2);
    let d220 = derive_synthdef(&g220, out220, 1, "t").expect("derive");
    let d440 = derive_synthdef(&g440, out440, 1, "t").expect("derive");
    assert_eq!(
        structural_sig(&d220),
        structural_sig(&d440),
        "a param value change must not change the structural signature",
    );

    // A lag change is structural (it is baked into the synthdef), so it does.
    let mut lagged = d220.clone();
    lagged.params[0].lag = Some(0.5);
    assert_ne!(
        structural_sig(&d220),
        structural_sig(&lagged),
        "a param lag change must change the structural signature",
    );
}

#[test]
fn fans_output_across_channels() {
    let (g, out_ix) = sine_to_out(220.0, 0.2);
    let def = derive_synthdef(&g, out_ix, 2, "test").expect("derive");
    // `Out` gets the bus index followed by one signal input per channel.
    assert_eq!(def.units[2].name, "Out");
    assert_eq!(def.units[2].inputs.len(), 1 + 2);
}

#[test]
fn non_dsp_root_is_rejected() {
    let mut g = Graph::<N>::default();
    let root = g.add_node(N::Other);
    assert!(matches!(
        derive_synthdef(&g, root, 1, "nope"),
        Err(DeriveError::RootNotDsp)
    ));
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

/// Render `frames` of mono audio, cycling buffer sizes to exercise the reblocker.
fn render(world: &mut World, frames: usize) -> Vec<f32> {
    let sizes = [64usize, 100, 128, 480, 512, 333];
    let mut out = Vec::with_capacity(frames + 512);
    let mut buf = Vec::new();
    let mut i = 0;
    while out.len() < frames {
        let size = sizes[i % sizes.len()];
        i += 1;
        buf.clear();
        buf.resize(size, 0.0);
        world.fill(&mut buf, 1);
        out.extend_from_slice(&buf);
    }
    out.truncate(frames);
    out
}

#[test]
fn derived_synth_plays_expected_tone() {
    let (g, out_ix) = sine_to_out(220.0, 0.2);
    let def = derive_synthdef(&g, out_ix, 1, "test").expect("derive");

    let (mut controller, _nrt, mut world) = engine(Options {
        sample_rate: SR as f64,
        output_channels: 1,
        ..Options::default()
    });
    controller.add_synthdef(def);
    controller
        .synth_new("test", ROOT_GROUP_ID, AddAction::Tail)
        .expect("synth_new");

    // Render ~0.5 s and confirm a 220 Hz tone at the configured 0.2 gain.
    let a = render(&mut world, SR as usize / 2);
    assert!(
        a.iter().any(|s| s.abs() > 0.1),
        "derived synth produced silence"
    );
    assert!(
        a.iter().all(|s| s.abs() <= 1.001),
        "output exceeded full scale"
    );
    let m220 = goertzel(&a, 220.0);
    let m440 = goertzel(&a, 440.0);
    assert!(
        m220 > 5.0 * m440,
        "expected 220 Hz dominant: m220={m220}, m440={m440}"
    );
}
