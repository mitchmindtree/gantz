//! The `~sinosc` sine-oscillator node.

use std::hash::{Hash, Hasher};

use gantz_ca::CaHash;
use gantz_core::node::{ExprCtx, ExprResult, MetaCtx, RegCtx};
use gantz_nodetag::NodeTag;
use plyphon::synthdef::{InputRef, UnitSpec};
use serde::{Deserialize, Serialize};

use crate::dsp::{DspBuilder, NodeDsp, NodeRate, Signal, ToNodeDsp, cahash_rate};
use crate::param::{cahash_lag, control_input_expr, param_name, param_state, plyphon_param};

/// A sine oscillator. Emits a single `SinOsc` UGen at the configured `rate`
/// (audio by default; control rate for modulator duty).
///
/// The `freq` (Hz) *value* lives in the node's VM state (path-keyed, like
/// `number`), so editing it does not churn the graph's content address; the audio
/// driver applies value changes via `set_control`. Only the smoothing `freq_lag`
/// and the `rate` (both structural) live in the node weight.
#[derive(Clone, Debug, Default, Serialize, Deserialize, NodeTag)]
pub struct SinOsc {
    #[serde(default)]
    freq_lag: f32,
    #[serde(default)]
    rate: NodeRate,
}

impl SinOsc {
    /// The default frequency (Hz) a fresh `~sinosc` starts at.
    pub const DEFAULT_FREQ: f32 = 220.0;

    /// The default frequency smoothing lag in seconds (`0.0` = instant/unsmoothed).
    pub const DEFAULT_FREQ_LAG: f32 = 0.0;

    /// The frequency smoothing lag in seconds (`0.0` = instant).
    pub fn freq_lag(&self) -> f32 {
        self.freq_lag
    }

    /// Set the frequency smoothing lag in seconds (content-address affecting).
    pub fn set_freq_lag(&mut self, lag: f32) {
        self.freq_lag = lag;
    }

    /// The ugen rate (`ar`/`kr`) the oscillator runs at.
    pub fn rate(&self) -> NodeRate {
        self.rate
    }

    /// Set the ugen rate (content-address affecting; structural).
    pub fn set_rate(&mut self, rate: NodeRate) {
        self.rate = rate;
    }
}

impl PartialEq for SinOsc {
    fn eq(&self, other: &Self) -> bool {
        self.freq_lag.to_bits() == other.freq_lag.to_bits() && self.rate == other.rate
    }
}

impl Eq for SinOsc {}

impl Hash for SinOsc {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Hash::hash(&self.freq_lag.to_bits(), state);
        Hash::hash(&self.rate, state);
    }
}

impl CaHash for SinOsc {
    fn hash(&self, hasher: &mut gantz_ca::Hasher) {
        hasher.update(b"gantz.plyphon.sinosc");
        cahash_lag(hasher, self.freq_lag);
        cahash_rate(hasher, self.rate);
    }
}

impl gantz_core::Node for SinOsc {
    fn n_inputs(&self, _ctx: MetaCtx) -> usize {
        // A single control input: the frequency. (No dsp signal inputs.)
        1
    }

    fn n_outputs(&self, _ctx: MetaCtx) -> usize {
        1
    }

    fn stateful(&self, _ctx: MetaCtx) -> bool {
        true
    }

    fn register(&self, mut ctx: RegCtx<'_, '_>) {
        let path = ctx.path();
        gantz_core::node::state::init_value_if_absent(ctx.vm(), path, || {
            param_state(Self::DEFAULT_FREQ as f64)
        })
        .unwrap()
    }

    fn expr(&self, ctx: ExprCtx<'_, '_>) -> ExprResult {
        // The audio engine reads the freq from state; this node is otherwise
        // Steel-inert. When its control input is connected, write the incoming
        // value into state (the audio driver applies it via `set_control`). The
        // placeholder output (the freq) feeds the inert dsp output edge.
        control_input_expr(&ctx, self.n_dsp_inputs(), "state")
    }
}

impl NodeDsp for SinOsc {
    fn n_dsp_outputs(&self) -> usize {
        1
    }

    fn ugens(&self, path: &[usize], _inputs: &[Option<Signal>], b: &mut DspBuilder) -> Vec<Signal> {
        // `freq` is a settable control param (a nominal default here; the driver
        // applies the live state value via `set_control`).
        let freq = b.push_param(
            path,
            plyphon_param(param_name(path, "freq"), Self::DEFAULT_FREQ, self.freq_lag),
        );
        let unit = b.push_unit(UnitSpec::new(
            "SinOsc",
            self.rate.to_plyphon(),
            vec![InputRef::Param(freq), InputRef::Constant(0.0)],
            1,
        ));
        vec![Signal::mono(InputRef::Unit { unit, output: 0 })]
    }
}

impl ToNodeDsp for SinOsc {
    fn to_node_dsp(&self) -> Option<&dyn NodeDsp> {
        Some(self)
    }
}
