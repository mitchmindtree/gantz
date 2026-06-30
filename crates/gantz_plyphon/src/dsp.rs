//! The [`NodeDsp`] trait, the [`DspBuilder`] that accumulates a synthdef, and
//! the [`ToNodeDsp`] downcast hook used to discover DSP nodes in an erased graph.

use plyphon::synthdef::{InputRef, Param, SynthDef, UnitSpec};

/// A gantz node that contributes one or more plyphon UGens to a synthdef.
///
/// This is the audio/DSP analogue of [`gantz_core::Node`]: where `Node::expr`
/// emits control-rate Steel, [`NodeDsp::ugens`] emits plyphon [`UnitSpec`]s into
/// the synthdef under construction. A node is "DSP" simply by implementing this
/// trait (and being discoverable via [`ToNodeDsp`]); the same gantz graph is
/// compiled by both backends independently.
pub trait NodeDsp {
    /// The number of DSP (signal) inputs - the leading inputs that carry audio,
    /// wired into the synthdef. A node's [`gantz_core::Node::n_inputs`] may exceed
    /// this: any inputs at indices `>= n_dsp_inputs` are *control* inputs, a purely
    /// Steel/state concern (a connected control value is written into the node's
    /// param state by its `expr`), and are ignored by the synthdef compiler.
    fn n_dsp_inputs(&self) -> usize {
        0
    }

    /// The number of DSP (signal) outputs. Matches
    /// [`gantz_core::Node::n_outputs`].
    fn n_dsp_outputs(&self) -> usize {
        1
    }

    /// Whether this node is a synthdef *sink* (e.g. `~out`) that the compiler
    /// uses as a root when deriving a synthdef.
    fn is_output(&self) -> bool {
        false
    }

    /// Emit this node's UGens into `b`, given the resolved source for each DSP
    /// input, returning one [`InputRef`] per DSP output (so downstream nodes can
    /// reference them).
    ///
    /// `path` is the node's path within the graph (e.g. `[2]` for the node at
    /// index 2 of a flat graph); use it to name any control [`Param`](plyphon::Param)s
    /// uniquely within the synthdef (see [`param_name`](crate::param::param_name)).
    /// `inputs` has length [`n_dsp_inputs`](Self::n_dsp_inputs); an unconnected
    /// input is [`InputRef::Constant`]`(0.0)` (silence).
    fn ugens(&self, path: &[usize], inputs: &[InputRef], b: &mut DspBuilder) -> Vec<InputRef>;
}

/// A downcast hook so the synthdef compiler and the audio driver can find
/// [`NodeDsp`] nodes inside an erased node type (e.g. `Box<dyn Node>`).
///
/// Implemented per concrete DSP node type (returning `Some(self)`); the
/// application implements it for its boxed node enum by trying each known DSP
/// node type - mirroring `ToTickBang` in `bevy_gantz_egui`. (A blanket
/// `impl<T: NodeDsp>` is deliberately avoided so the application's
/// `impl ToNodeDsp for Box<dyn Node>` does not collide with it.)
pub trait ToNodeDsp {
    /// This value as a [`NodeDsp`], if it is one.
    fn to_node_dsp(&self) -> Option<&dyn NodeDsp>;
}

/// Records which dsp node a synthdef [`Param`] came from, so the audio driver can
/// map a node's live state value to the right synth param index.
#[derive(Clone, Debug)]
pub struct ParamBinding {
    /// The dsp node's path within the graph (e.g. `[2]` for a flat graph).
    pub node_path: Vec<usize>,
    /// The param's index within the synthdef's `params`.
    pub index: usize,
}

/// Accumulates the [`UnitSpec`]s and [`Param`]s of a synthdef as nodes emit them.
///
/// Also carries the engine's output-channel count so a sink node (`~out`) can
/// fan a mono signal across every output channel, and records a [`ParamBinding`]
/// per pushed param.
pub struct DspBuilder {
    units: Vec<UnitSpec>,
    params: Vec<Param>,
    bindings: Vec<ParamBinding>,
    out_channels: usize,
}

impl DspBuilder {
    /// A new, empty builder targeting `out_channels` output-bus channels.
    pub fn new(out_channels: usize) -> Self {
        DspBuilder {
            units: Vec::new(),
            params: Vec::new(),
            bindings: Vec::new(),
            out_channels: out_channels.max(1),
        }
    }

    /// Push a unit, returning its index for use in [`InputRef::Unit`].
    pub fn push_unit(&mut self, spec: UnitSpec) -> u32 {
        let ix = self.units.len() as u32;
        self.units.push(spec);
        ix
    }

    /// Declare a control parameter belonging to the dsp node at `path`, returning
    /// its index for [`InputRef::Param`] and recording its [`ParamBinding`].
    pub fn push_param(&mut self, path: &[usize], param: Param) -> u32 {
        let index = self.params.len();
        self.params.push(param);
        self.bindings.push(ParamBinding {
            node_path: path.to_vec(),
            index,
        });
        index as u32
    }

    /// The number of output-bus channels a sink should fan its signal across.
    pub fn out_channels(&self) -> usize {
        self.out_channels
    }

    /// Consume the builder into a finished [`SynthDef`] and its param bindings.
    pub fn finish(self, name: impl Into<String>) -> (SynthDef, Vec<ParamBinding>) {
        let def = SynthDef {
            name: name.into(),
            params: self.params,
            units: self.units,
        };
        (def, self.bindings)
    }
}
