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

use bevy_gantz::head::{HeadRef, HeadVms, OpenHead, WorkingGraph};
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
    /// unchanged across a structural-graph edit means the synth need not respawn.
    sig: u64,
    /// One slot per control param, binding a dsp node's state value to its synth
    /// param index, with the last value pushed via `set_control`.
    params: Vec<ParamSlot>,
}

/// Binds one synth control param to a dsp node's VM state value.
struct ParamSlot {
    /// The dsp node's path in the graph (where its value lives in VM state).
    node_path: Vec<usize>,
    /// The param's index within the synth.
    index: usize,
    /// The last value pushed via `set_control` (`None` until first applied).
    last: Option<f32>,
}

/// Keep each open head's synth in sync, `.after(VmSet)`. Two passes per head:
///
/// - *Structural sync* (only when the head's committed graph changed): re-derive
///   the synthdef and respawn the synth if its structure changed (else keep it).
/// - *Param sync* (every frame): push each dsp node's live param value - read from
///   VM node state - to the running synth via `set_control`, so value/automation
///   edits (which no longer change the graph address) reach the audio thread
///   without a respawn (preserving phase).
///
/// Also tears down synths for closed heads.
fn drive_synths<N>(
    registry: Res<Registry<N>>,
    audio: Option<NonSendMut<AudioEngine>>,
    mut state: ResMut<HeadSynths>,
    mut vms: NonSendMut<HeadVms>,
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

        // --- Structural sync: only when the committed graph changed. ---
        if state.0.get(&entity).map(|h| h.graph) != Some(graph_ca) {
            structural_sync(
                &mut audio.controller,
                &mut state.0,
                entity,
                graph_ca,
                &wg.0,
                out_channels,
            );
        }

        // --- Param sync: push live state values to the synth every frame. ---
        if let (Some(synth), Some(vm)) = (state.0.get_mut(&entity), vms.get_mut(&entity)) {
            let node_id = synth.node_id;
            let mut backend = Embedded::new(&mut audio.controller);
            for slot in &mut synth.params {
                let Some(value) = gantz_core::node::state::extract::<f64>(vm, &slot.node_path)
                    .ok()
                    .flatten()
                    .map(|v| v as f32)
                else {
                    continue;
                };
                if slot.last.map(f32::to_bits) != Some(value.to_bits()) {
                    if let Err(e) = backend.set_control(node_id, slot.index, value) {
                        log::error!("bevy_gantz_plyphon: set_control failed: {e:?}");
                    }
                    slot.last = Some(value);
                }
            }
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

/// Re-derive `entity`'s synthdef and respawn the synth if its *structure* changed
/// (else keep the running synth, just recording the new graph address). Tears the
/// synth down if the head no longer has a `~out`.
fn structural_sync<N>(
    controller: &mut Controller,
    synths: &mut HashMap<Entity, HeadSynth>,
    entity: Entity,
    graph_ca: ca::GraphAddr,
    graph: &Graph<N>,
    out_channels: usize,
) where
    N: ToNodeDsp,
{
    let Some(root) = find_output(graph) else {
        teardown(controller, synths.remove(&entity));
        return;
    };

    let def_name = format!("gantz-head-{}", entity.index());
    let derived = match derive_synthdef(graph, root, out_channels, def_name.clone()) {
        Ok(derived) => derived,
        Err(e) => {
            log::error!("bevy_gantz_plyphon: synthdef derivation failed: {e:?}");
            return;
        }
    };
    let sig = structural_sig(&derived.def);

    // Structure unchanged (e.g. a non-dsp edit changed the head address): keep the
    // running synth + its param slots; just record the new graph address.
    if synths.get(&entity).map(|s| s.sig) == Some(sig) {
        if let Some(prev) = synths.get_mut(&entity) {
            prev.graph = graph_ca;
        }
        return;
    }

    // Structural change (or this head's first synth): respawn.
    let mut backend = Embedded::new(controller);
    if let Some(prev) = synths.get(&entity) {
        let _ = backend.free_node(prev.node_id);
    }
    if let Err(e) = backend.install_synthdef(derived.def) {
        log::error!("bevy_gantz_plyphon: synthdef install failed: {e:?}");
        return;
    }
    match backend.spawn(&def_name) {
        Ok(node_id) => {
            let params = derived
                .params
                .iter()
                .map(|b| ParamSlot {
                    node_path: b.node_path.clone(),
                    index: b.index,
                    last: None,
                })
                .collect();
            synths.insert(
                entity,
                HeadSynth {
                    graph: graph_ca,
                    def_name,
                    node_id,
                    sig,
                    params,
                },
            );
        }
        Err(e) => log::error!("bevy_gantz_plyphon: synth spawn failed: {e:?}"),
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
