//! The [`Backend`] seam between derived synthdefs and a running synth engine.
//!
//! [`Embedded`] drives an in-process [`plyphon::Controller`] directly (no OSC,
//! no sockets). A future `Remote` backend could serialise the same operations to
//! OSC for a networked scsynth/plyphon - the compiler and nodes are unaffected.

use plyphon::synthdef::SynthDef;
use plyphon::{CommandTime, Controller};

pub use plyphon::{AddAction, ROOT_GROUP_ID};

/// A sink for installing synthdefs and controlling synths, abstracting over an
/// in-process engine ([`Embedded`]) or, in future, a networked one.
pub trait Backend {
    /// Install (or replace) a synth definition by name.
    fn install_synthdef(&mut self, def: SynthDef) -> Result<(), BackendError>;
    /// Free a previously installed synth definition by name.
    fn free_synthdef(&mut self, name: &str) -> Result<(), BackendError>;
    /// Spawn a synth from the named def at `action` relative to the node (or
    /// group) `target`, returning its node id. Placement matters across
    /// synthdef boundaries: a bus reader hears only writers computed *earlier*
    /// in the node tree this block, so writers must precede their readers.
    fn spawn(
        &mut self,
        def_name: &str,
        target: i32,
        action: AddAction,
    ) -> Result<i32, BackendError>;
    /// Free a running synth (or group) by node id.
    fn free_node(&mut self, node: i32) -> Result<(), BackendError>;
    /// Set control parameter `param` (by index) of `node` to `value`, immediately.
    fn set_control(&mut self, node: i32, param: usize, value: f32) -> Result<(), BackendError>;

    /// Set control parameter `param` of `node` to `value`, scheduled to take effect
    /// at the absolute OSC/NTP time `time_osc` on the engine's clock timeline.
    ///
    /// This is how timestamped control automation (e.g. a `tick!`-driven chain)
    /// lands sample-accurately. The default applies it immediately. A backend with
    /// a scheduling clock (like [`Embedded`]) overrides it.
    fn set_control_at(
        &mut self,
        node: i32,
        param: usize,
        value: f32,
        time_osc: u64,
    ) -> Result<(), BackendError> {
        let _ = time_osc;
        self.set_control(node, param, value)
    }
}

/// An error issuing a command to a [`Backend`].
#[derive(Debug)]
pub enum BackendError {
    /// The backend's command queue is full.
    QueueFull,
    /// A synth could not be spawned (e.g. unknown or invalid def).
    Spawn(String),
}

/// A [`Backend`] that drives an in-process [`plyphon::Controller`] directly.
pub struct Embedded<'a> {
    /// The plyphon control handle.
    pub controller: &'a mut Controller,
}

impl<'a> Embedded<'a> {
    /// Wrap a mutable controller reference as an embedded backend.
    pub fn new(controller: &'a mut Controller) -> Self {
        Embedded { controller }
    }
}

impl Backend for Embedded<'_> {
    fn install_synthdef(&mut self, def: SynthDef) -> Result<(), BackendError> {
        // `add_synthdef` defers compilation to the first `spawn`, so it cannot
        // fail here. A `BuildError` surfaces from `spawn` instead.
        self.controller.add_synthdef(def);
        Ok(())
    }

    fn free_synthdef(&mut self, name: &str) -> Result<(), BackendError> {
        self.controller
            .free_def(name)
            .map(|_| ())
            .map_err(|_| BackendError::QueueFull)
    }

    fn spawn(
        &mut self,
        def_name: &str,
        target: i32,
        action: AddAction,
    ) -> Result<i32, BackendError> {
        self.controller
            .synth_new(def_name, target, action)
            .map_err(|e| match e {
                // Transient: the ring drains within a block, so the caller can
                // retry next frame rather than treating the spawn as broken.
                plyphon::SynthNewError::QueueFull => BackendError::QueueFull,
                e => BackendError::Spawn(format!("{e:?}")),
            })
    }

    fn free_node(&mut self, node: i32) -> Result<(), BackendError> {
        self.controller
            .free(node)
            .map_err(|_| BackendError::QueueFull)
    }

    fn set_control(&mut self, node: i32, param: usize, value: f32) -> Result<(), BackendError> {
        self.controller
            .set_control(node, param, value)
            .map_err(|_| BackendError::QueueFull)
    }

    fn set_control_at(
        &mut self,
        node: i32,
        param: usize,
        value: f32,
        time_osc: u64,
    ) -> Result<(), BackendError> {
        // Open a scheduling window for this one command, then restore immediate
        // mode. `set_control` pushes to the RT ring tagged with the window's time.
        // The World holds it until `time_osc` arrives, resolving it to a sample.
        self.controller.begin_scheduled(CommandTime::At(time_osc));
        let res = self
            .controller
            .set_control(node, param, value)
            .map_err(|_| BackendError::QueueFull);
        self.controller.end_scheduled();
        res
    }
}
