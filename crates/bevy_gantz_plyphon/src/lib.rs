//! Bevy + plyphon audio runtime for gantz.
//!
//! [`PlyphonPlugin`] owns the audio engine: at startup it opens a cpal output
//! stream (its own audio thread, running [`plyphon::World::fill`]) and keeps the
//! [`plyphon::Controller`] + [`plyphon::Nrt`] handles. Each update, after
//! [`bevy_gantz::VmSet`], it derives a synthdef from each open head's DSP
//! subgraph (rooted at its `~out` node) and installs/respawns the synth via the
//! [`gantz_plyphon::Backend`] seam whenever the head's committed graph changes.
//!
//! Mixing across heads is free: every head's `~out` synth writes to output bus 0,
//! and plyphon sums all synths on that bus.

use std::collections::{HashMap, HashSet};

use bevy_app::{App, Plugin, Update};
use bevy_ecs::prelude::*;
use bevy_ecs::system::NonSendMut;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};

use gantz_ca as ca;
use gantz_core::node::graph::{Graph, NodeIx};
use gantz_plyphon::{Backend, Embedded, ToNodeDsp, derive_synthdef, structural_sig};
use plyphon::{Controller, Nrt, Options, World, engine};

use bevy_gantz::head::{HeadRef, OpenHead, WorkingGraph};
use bevy_gantz::{Registry, VmSet};

/// Plugin wiring plyphon audio into a gantz bevy app.
///
/// Generic over `N`, the graph node type (must expose its DSP nodes via
/// [`ToNodeDsp`]). Add it *after* [`bevy_gantz::GantzPlugin`].
pub struct PlyphonPlugin<N>(std::marker::PhantomData<N>);

impl<N> Default for PlyphonPlugin<N> {
    fn default() -> Self {
        Self(std::marker::PhantomData)
    }
}

impl<N> Plugin for PlyphonPlugin<N>
where
    N: 'static + ToNodeDsp + Send + Sync,
{
    fn build(&self, app: &mut App) {
        match build_audio_engine() {
            Some(engine) => {
                app.insert_non_send(engine);
            }
            None => log::warn!(
                "bevy_gantz_plyphon: no audio output device; `~out` nodes will be silent"
            ),
        }
        app.init_resource::<HeadSynths>()
            .add_systems(Update, drive_synths::<N>.after(VmSet));
    }
}

/// The in-process plyphon engine: the control handle, the NRT cleanup handle, and
/// the held cpal output stream (kept alive for audio to continue). The plyphon
/// [`World`] itself lives inside the stream's audio callback.
struct AudioEngine {
    controller: Controller,
    nrt: Nrt,
    out_channels: usize,
    _stream: cpal::Stream,
}

/// Per-head synth bookkeeping: which committed graph produced the currently
/// installed synthdef and running synth, so we only re-derive on change.
#[derive(Default, Resource)]
struct HeadSynths(HashMap<Entity, HeadSynth>);

/// The installed synthdef + running synth for one open head.
struct HeadSynth {
    graph: ca::GraphAddr,
    def_name: String,
    node_id: i32,
    /// Structural signature of the running synth's def (excludes param values) -
    /// an unchanged signature means a graph edit was param-only, so the params
    /// can be `set_control`'d instead of respawning.
    sig: u64,
    /// The running synth's current param values, indexed by param.
    param_defaults: Vec<f32>,
}

/// Derive, install and (re)spawn a synth per open head whose committed graph
/// changed, and tear down synths for closed heads. Runs `.after(VmSet)`.
fn drive_synths<N>(
    registry: Res<Registry<N>>,
    audio: Option<NonSendMut<AudioEngine>>,
    mut state: ResMut<HeadSynths>,
    heads: Query<(Entity, &HeadRef, &WorkingGraph<N>), With<OpenHead>>,
) where
    N: 'static + ToNodeDsp + Send + Sync,
{
    let Some(audio) = audio else {
        return;
    };
    let audio = audio.into_inner();

    // Tick NRT cleanup off the audio thread (drops freed synths, surfaces events).
    audio.nrt.process();
    while audio.nrt.poll().is_some() {}

    let out_channels = audio.out_channels;
    let mut live: HashSet<Entity> = HashSet::new();

    for (entity, head_ref, wg) in heads.iter() {
        live.insert(entity);
        let Some(graph_ca) = registry.head_commit(&head_ref.0).map(|c| c.graph) else {
            continue;
        };

        // Unchanged committed graph: nothing to do.
        if state.0.get(&entity).map(|h| h.graph) == Some(graph_ca) {
            continue;
        }

        let graph: &Graph<N> = &wg.0;
        let Some(root) = find_output(graph) else {
            // No `~out` in this head: tear down any synth it had.
            teardown(&mut audio.controller, state.0.remove(&entity));
            continue;
        };

        let def_name = format!("gantz-head-{}", entity.index());
        let def = match derive_synthdef(graph, root, out_channels, def_name.clone()) {
            Ok(def) => def,
            Err(e) => {
                log::error!("bevy_gantz_plyphon: synthdef derivation failed: {e:?}");
                continue;
            }
        };
        let sig = structural_sig(&def);
        let param_defaults: Vec<f32> = def.params.iter().map(|p| p.default).collect();

        let mut backend = Embedded::new(&mut audio.controller);

        // Param-only change (same structure): `set_control` the changed values on
        // the running synth - no respawn, so oscillator phase is preserved.
        if let Some(prev) = state.0.get_mut(&entity) {
            if prev.sig == sig {
                for (i, (&old, &new)) in prev.param_defaults.iter().zip(&param_defaults).enumerate()
                {
                    if old.to_bits() != new.to_bits() {
                        if let Err(e) = backend.set_control(prev.node_id, i, new) {
                            log::error!("bevy_gantz_plyphon: set_control failed: {e:?}");
                        }
                    }
                }
                prev.graph = graph_ca;
                prev.param_defaults = param_defaults;
                continue;
            }
        }

        // Structural change (or this head's first synth): respawn.
        if let Some(prev) = state.0.get(&entity) {
            let _ = backend.free_node(prev.node_id);
        }
        if let Err(e) = backend.install_synthdef(def) {
            log::error!("bevy_gantz_plyphon: synthdef install failed: {e:?}");
            continue;
        }
        match backend.spawn(&def_name) {
            Ok(node_id) => {
                state.0.insert(
                    entity,
                    HeadSynth {
                        graph: graph_ca,
                        def_name,
                        node_id,
                        sig,
                        param_defaults,
                    },
                );
            }
            Err(e) => log::error!("bevy_gantz_plyphon: synth spawn failed: {e:?}"),
        }
    }

    // Tear down synths whose heads are no longer open.
    let stale: Vec<Entity> = state
        .0
        .keys()
        .copied()
        .filter(|e| !live.contains(e))
        .collect();
    for e in stale {
        teardown(&mut audio.controller, state.0.remove(&e));
    }
}

/// The first `~out` (output sink) node in `graph`, if any.
fn find_output<N>(graph: &Graph<N>) -> Option<NodeIx>
where
    N: ToNodeDsp,
{
    graph
        .node_indices()
        .find(|&ix| graph[ix].to_node_dsp().is_some_and(|d| d.is_output()))
}

/// Free a head's running synth and its synthdef, if it had one.
fn teardown(controller: &mut Controller, synth: Option<HeadSynth>) {
    if let Some(s) = synth {
        let mut backend = Embedded::new(controller);
        let _ = backend.free_node(s.node_id);
        let _ = backend.free_synthdef(&s.def_name);
    }
}

/// Build the plyphon engine + cpal output stream from the default output device.
/// Returns `None` (and the app runs silently) if no device is available.
fn build_audio_engine() -> Option<AudioEngine> {
    let host = cpal::default_host();
    let device = host.default_output_device()?;
    let supported = device.default_output_config().ok()?;
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let channels = config.channels as usize;
    let sample_rate = config.sample_rate as f64;

    let (controller, nrt, world) = engine(Options {
        sample_rate,
        output_channels: channels,
        ..Options::default()
    });

    let stream = build_stream(&device, config, channels, sample_format, world)?;
    stream.play().ok()?;

    Some(AudioEngine {
        controller,
        nrt,
        out_channels: channels,
        _stream: stream,
    })
}

/// Build the output stream, dispatching on the device's native sample format.
/// `world` is moved onto the audio thread and pulled once per callback.
fn build_stream(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    format: cpal::SampleFormat,
    world: World,
) -> Option<cpal::Stream> {
    match format {
        cpal::SampleFormat::F32 => build_typed::<f32>(device, config, channels, world),
        cpal::SampleFormat::I16 => build_typed::<i16>(device, config, channels, world),
        cpal::SampleFormat::U16 => build_typed::<u16>(device, config, channels, world),
        other => {
            log::error!("bevy_gantz_plyphon: unsupported sample format {other:?}");
            None
        }
    }
}

/// Construct a typed output stream, reblocking the engine's `f32` fill into the
/// device's sample format `T`.
fn build_typed<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    mut world: World,
) -> Option<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    // Reused interleaved `f32` scratch; the engine writes it, then we convert.
    let mut scratch: Vec<f32> = Vec::new();
    device
        .build_output_stream(
            config,
            move |output: &mut [T], _: &cpal::OutputCallbackInfo| {
                scratch.clear();
                scratch.resize(output.len(), 0.0);
                world.fill(&mut scratch, channels);
                for (o, s) in output.iter_mut().zip(scratch.iter()) {
                    *o = T::from_sample(*s);
                }
            },
            |err| log::error!("bevy_gantz_plyphon: audio stream error: {err}"),
            None,
        )
        .ok()
}
