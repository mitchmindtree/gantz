//! The [`Backend`] seam between derived synthdefs and a running synth engine.
//!
//! [`Embedded`] drives an in-process [`plyphon::Controller`] directly (no OSC,
//! no sockets). A future `Remote` backend could serialise the same operations to
//! OSC for a networked scsynth/plyphon - the compiler and nodes are unaffected.

use plyphon::synthdef::SynthDef;
use plyphon::{AddAction, Controller, ROOT_GROUP_ID};

/// A sink for installing synthdefs and controlling synths, abstracting over an
/// in-process engine ([`Embedded`]) or, in future, a networked one.
pub trait Backend {
    /// Install (or replace) a synth definition by name.
    fn install_synthdef(&mut self, def: SynthDef) -> Result<(), BackendError>;
    /// Free a previously installed synth definition by name.
    fn free_synthdef(&mut self, name: &str) -> Result<(), BackendError>;
    /// Spawn a synth from the named def, returning its node id.
    fn spawn(&mut self, def_name: &str) -> Result<i32, BackendError>;
    /// Free a running synth (or group) by node id.
    fn free_node(&mut self, node: i32) -> Result<(), BackendError>;
    /// Set control parameter `param` (by index) of `node` to `value`.
    fn set_control(&mut self, node: i32, param: usize, value: f32) -> Result<(), BackendError>;
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
        // fail here; a `BuildError` surfaces from `spawn` instead.
        self.controller.add_synthdef(def);
        Ok(())
    }

    fn free_synthdef(&mut self, name: &str) -> Result<(), BackendError> {
        self.controller
            .free_def(name)
            .map(|_| ())
            .map_err(|_| BackendError::QueueFull)
    }

    fn spawn(&mut self, def_name: &str) -> Result<i32, BackendError> {
        self.controller
            .synth_new(def_name, ROOT_GROUP_ID, AddAction::Tail)
            .map_err(|e| BackendError::Spawn(format!("{e:?}")))
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
}
