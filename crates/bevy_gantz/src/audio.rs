//! Audio runtime configuration + status, shared between the audio runtime
//! (`bevy_gantz_plyphon`, which owns the engine) and the GUI (`bevy_gantz_egui`,
//! which renders the Settings → Audio tab). They live here because those two
//! crates do not depend on each other; `bevy_gantz` is their common dependency.
//!
//! Both resources are inserted only when an audio runtime is present, so the GUI
//! reads them as `Option<Res<…>>` and simply omits the Audio tab otherwise.

use bevy_ecs::prelude::Resource;
use std::time::Duration;

/// Editable audio settings (the Settings → Audio tab). Runtime-only - not persisted,
/// so it resets to the defaults each session (like [`CompileConfig`](crate::CompileConfig)).
#[derive(Clone, Debug, Resource)]
pub struct AudioConfig {
    /// How far ahead of the audio clock a timestamped control update is scheduled -
    /// the latency↔sample-accuracy trade-off (must exceed output latency + frame
    /// jitter). The audio driver reads this in place of a fixed constant.
    pub sched_lead: Duration,
    /// Whether audio output is enabled. When `false`, the output stream is paused
    /// (muted) and resumed when re-enabled.
    pub enabled: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sched_lead: Duration::from_millis(50),
            enabled: true,
        }
    }
}

/// Read-only audio status, written by the audio runtime for the GUI to display.
#[derive(Clone, Debug, Default, Resource)]
pub struct AudioStatus {
    /// Whether an audio output device is present (else the app runs silent).
    pub present: bool,
    /// The active output device's name, if present.
    pub device: Option<String>,
    /// The output sample rate (Hz).
    pub sample_rate: f64,
    /// The number of output channels.
    pub channels: usize,
}
