//! Bevy + plyphon audio runtime for gantz.
//!
//! [`PlyphonPlugin`] owns the audio engine: at startup it opens a cpal output
//! stream (its own audio thread, running [`plyphon::World::fill_at`] so the engine
//! clock is anchored to the host) and keeps the [`plyphon::Controller`] +
//! [`plyphon::Nrt`] handles. Each update, after [`bevy_gantz::VmSet`], it derives a
//! synthdef from each open head's DSP subgraph (rooted at its `~out` output and any
//! `~tap` monitors) and installs/respawns the synth via the [`gantz_plyphon::Backend`]
//! seam whenever the head's committed graph changes.
//!
//! The bridge runs both ways: control values drive dsp params via `set_control`,
//! and each `~tap` monitor's `SendTrig` `/tr`s are drained here into the node's
//! ring-buffer state (so the control world can scope an audio signal).
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
use gantz_core::node::graph::Graph;
use gantz_plyphon::{
    Backend, Embedded, MonitorBinding, ToNodeDsp, derive_synthdef, structural_sig,
};
use plyphon::{Controller, Nrt, Options, World, engine};

/// Re-export of [`plyphon`] so downstream crates can implement custom units
/// (`Unit`, `UnitDef`, `unit_spec`, …) against the exact version this runtime
/// uses, without pinning it separately. See [`PlyphonPlugin::with_units`].
pub use plyphon;

/// A callback that registers custom plyphon units into the embedded engine's
/// [`plyphon::UnitRegistry`] at startup (see [`PlyphonPlugin::with_units`]).
type UnitRegistrar = Box<dyn Fn(&mut plyphon::UnitRegistry) + Send + Sync>;

use bevy_gantz::head::{HeadRef, HeadVms, OpenHead, WorkingGraph};
use bevy_gantz::{AudioConfig, AudioStatus, EntrypointSet, EvalEpoch, Registry, VmSet};

/// OSC/NTP fixed-point units per second (OSC time is 32.32 fixed point: 2^32).
const OSC_UNITS_PER_SEC: f64 = 4_294_967_296.0;

/// Convert monotonic [`EvalEpoch`] seconds to an absolute OSC/NTP engine-clock time.
fn osc(secs: f64) -> u64 {
    (secs * OSC_UNITS_PER_SEC) as u64
}

/// Plugin wiring plyphon audio into a gantz bevy app.
///
/// Generic over `N`, the graph node type (must expose its DSP nodes via
/// [`ToNodeDsp`]). Add it *after* [`bevy_gantz::GantzPlugin`].
///
/// To use custom UGens, register them with [`with_units`](Self::with_units); a
/// node's [`NodeDsp::ugens`](gantz_plyphon::NodeDsp::ugens) can then name them in a
/// [`UnitSpec`](plyphon::UnitSpec).
pub struct PlyphonPlugin<N> {
    unit_registrars: Vec<UnitRegistrar>,
    _marker: std::marker::PhantomData<N>,
}

impl<N> Default for PlyphonPlugin<N> {
    fn default() -> Self {
        Self {
            unit_registrars: Vec::new(),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<N> PlyphonPlugin<N> {
    /// A plugin with no custom units (equivalent to [`default`](Self::default)).
    pub fn new() -> Self {
        Self::default()
    }

    /// Register custom plyphon units into the embedded engine at startup, before
    /// any synth that names them is spawned. The callback gets the controller's
    /// [`plyphon::UnitRegistry`]; call [`register`](plyphon::UnitRegistry::register)
    /// (or `register_demand`) once per unit. Chainable.
    ///
    /// ```ignore
    /// PlyphonPlugin::<N>::new().with_units(|reg| {
    ///     reg.register("Saw", Box::new(SawCtor));
    /// })
    /// ```
    pub fn with_units(
        mut self,
        f: impl Fn(&mut plyphon::UnitRegistry) + Send + Sync + 'static,
    ) -> Self {
        self.unit_registrars.push(Box::new(f));
        self
    }
}

impl<N> Plugin for PlyphonPlugin<N>
where
    N: 'static + ToNodeDsp + Send + Sync,
{
    fn build(&self, app: &mut App) {
        // The shared monotonic epoch (set by `GantzPlugin`, added before this) is
        // the audio clock's time base; the cpal callback anchors the engine clock
        // to it via `fill_at`, matching the firing times queued into `%args`.
        let epoch = *app.world().resource::<EvalEpoch>();
        // Audio settings (the Settings → Audio tab) + the status it reports.
        app.init_resource::<AudioConfig>();
        let status = match build_audio_engine(epoch, &self.unit_registrars) {
            Some(engine) => {
                let status = AudioStatus {
                    present: true,
                    device: Some(engine.device.clone()),
                    sample_rate: engine.sample_rate,
                    channels: engine.out_channels,
                };
                app.insert_non_send(engine);
                status
            }
            None => {
                log::warn!(
                    "bevy_gantz_plyphon: no audio output device; `~out` nodes will be silent"
                );
                AudioStatus::default()
            }
        };
        app.insert_resource(status);
        // `.after(EntrypointSet)`: run once the `tick!`/`update!` drivers' triggered
        // evaluations have flushed, so the control values they queue are visible to
        // the param drain below in the same frame.
        app.init_resource::<HeadSynths>()
            .add_systems(Update, drive_synths::<N>.after(VmSet).after(EntrypointSet));
    }
}

/// The in-process plyphon engine: the control handle, the NRT cleanup handle, and
/// the held cpal output stream (kept alive for audio to continue). The plyphon
/// [`World`] itself lives inside the stream's audio callback.
struct AudioEngine {
    controller: Controller,
    nrt: Nrt,
    out_channels: usize,
    /// The active output device's name (for the Settings → Audio status readout).
    device: String,
    /// The output sample rate (Hz).
    sample_rate: f64,
    /// The cpal output stream; held to keep audio running, paused/played on mute.
    stream: cpal::Stream,
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
    /// One binding per `~tap` monitor: routes this synth's `/tr`s into the right
    /// node's ring state. Refreshed on every derive (a ring-size edit is not a
    /// structural change, so it must update without a respawn).
    monitors: Vec<MonitorBinding>,
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

/// Keep each open head's synth in sync, `.after(VmSet)` and `.after(EntrypointSet)`.
/// Two passes per head:
///
/// - *Structural sync* (only when the head's committed graph changed): re-derive
///   the synthdef and respawn the synth if its structure changed (else keep it).
/// - *Param sync* (every frame): drain each dsp param's queued `(time, value)`
///   control updates from VM state and *schedule* them on the running synth at
///   `time + SCHED_LEAD` via [`Backend::set_control_at`], so automation plays
///   sample-accurately; a direct inspector edit (no queue) is applied immediately.
///   Either way the synth is not respawned (preserving phase), and value/automation
///   edits no longer change the graph address.
///
/// Also tears down synths for closed heads.
fn drive_synths<N>(
    registry: Res<Registry<N>>,
    audio: Option<NonSendMut<AudioEngine>>,
    audio_config: Res<AudioConfig>,
    mut enabled_applied: Local<Option<bool>>,
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

    // Apply the enable/mute toggle by pausing/playing the output stream on change.
    if *enabled_applied != Some(audio_config.enabled) {
        let result = if audio_config.enabled {
            audio.stream.play()
        } else {
            audio.stream.pause()
        };
        if let Err(e) = result {
            log::error!("bevy_gantz_plyphon: audio stream play/pause failed: {e}");
        }
        *enabled_applied = Some(audio_config.enabled);
    }

    // Tick NRT cleanup off the audio thread (drops freed synths, surfaces events).
    audio.nrt.process();
    while audio.nrt.poll().is_some() {}

    // Drain this frame's monitor triggers (`~tap` `SendTrig` `/tr`s), batched per
    // (synth node id, `SendTrig` id) so each `~tap`'s ring takes a single state
    // write below. Best-effort: the engine drops `/tr`s past its ring capacity.
    let mut trig_batches: HashMap<(i32, i32), Vec<f32>> = HashMap::new();
    while let Some(t) = audio.nrt.poll_trigger() {
        trig_batches
            .entry((t.node, t.id))
            .or_default()
            .push(t.value);
    }

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

        // --- Param sync: drain each param's queued control updates and schedule
        // them ahead of the audio clock; direct (untimestamped) value edits apply
        // immediately. ---
        if let (Some(synth), Some(vm)) = (state.0.get_mut(&entity), vms.get_mut(&entity)) {
            let node_id = synth.node_id;
            let mut backend = Embedded::new(&mut audio.controller);
            for slot in &mut synth.params {
                let Some((value, pending)) = gantz_plyphon::param::drain_param(vm, &slot.node_path)
                else {
                    continue;
                };
                let value = value as f32;
                if pending.is_empty() {
                    // No automation this frame: apply a direct inspector edit
                    // immediately, only when the value actually changed.
                    if slot.last.map(f32::to_bits) != Some(value.to_bits()) {
                        if let Err(e) = backend.set_control(node_id, slot.index, value) {
                            log::error!("bevy_gantz_plyphon: set_control failed: {e:?}");
                        }
                        slot.last = Some(value);
                    }
                } else {
                    // Timestamped automation: schedule each update at its own time
                    // plus the lead, preserving the inter-tick spacing.
                    for (t, v) in pending {
                        let when = osc(t + audio_config.sched_lead.as_secs_f64());
                        if let Err(e) = backend.set_control_at(node_id, slot.index, v as f32, when)
                        {
                            log::error!("bevy_gantz_plyphon: set_control_at failed: {e:?}");
                        }
                    }
                    // The latest queued value is now current; record it so a later
                    // immediate pass doesn't resend it.
                    slot.last = Some(value);
                }
            }

            // Write each `~tap`'s freshly sampled values into its ring state (the
            // list its control `expr` surfaces on a trigger push).
            for mon in &synth.monitors {
                if let Some(values) = trig_batches.get(&(node_id, mon.id)) {
                    gantz_plyphon::monitor::push_ring(vm, &mon.node_path, values, mon.size);
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
/// (else keep the running synth, just recording the new graph address and
/// refreshing its monitor bindings). Tears the synth down if the head has no dsp
/// sink (no `~out` and no `~tap`).
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
    let def_name = format!("gantz-head-{}", entity.index());
    let derived = match derive_synthdef(graph, out_channels, def_name.clone()) {
        Ok(derived) => derived,
        // No dsp sink to root a synthdef at: tear down any running synth.
        Err(gantz_plyphon::DeriveError::NoSink) => {
            teardown(controller, synths.remove(&entity));
            return;
        }
    };
    let sig = structural_sig(&derived.def);

    // Structure unchanged (e.g. a non-dsp edit, or a ring-size edit that is not in
    // the def): keep the running synth + its param slots; record the new graph
    // address and refresh the monitor bindings (their sizes/paths may have changed
    // without changing the structural signature).
    if synths.get(&entity).map(|s| s.sig) == Some(sig) {
        if let Some(prev) = synths.get_mut(&entity) {
            prev.graph = graph_ca;
            prev.monitors = derived.monitors;
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
                    monitors: derived.monitors,
                },
            );
        }
        Err(e) => log::error!("bevy_gantz_plyphon: synth spawn failed: {e:?}"),
    }
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
/// Returns `None` (and the app runs silently) if no device is available. `epoch`
/// is the shared monotonic clock the callback anchors the engine clock to;
/// `unit_registrars` register any custom units into the controller's registry
/// before the stream starts.
fn build_audio_engine(epoch: EvalEpoch, unit_registrars: &[UnitRegistrar]) -> Option<AudioEngine> {
    let host = cpal::default_host();
    let device = host.default_output_device()?;
    // cpal's `Device` `Display` is its name (there is no `name()` in 0.18).
    let device_name = device.to_string();
    let supported = device.default_output_config().ok()?;
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let channels = config.channels as usize;
    let sample_rate = config.sample_rate as f64;

    let (mut controller, nrt, world) = engine(Options {
        sample_rate,
        output_channels: channels,
        ..Options::default()
    });

    // Register any custom units before the engine compiles/spawns a synth naming
    // them (the compiled GraphDef carries their fn-pointers to the audio thread).
    for register in unit_registrars {
        register(controller.registry_mut());
    }

    let stream = build_stream(&device, config, channels, sample_format, world, epoch)?;
    stream.play().ok()?;

    Some(AudioEngine {
        controller,
        nrt,
        out_channels: channels,
        device: device_name,
        sample_rate,
        stream,
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
    epoch: EvalEpoch,
) -> Option<cpal::Stream> {
    match format {
        cpal::SampleFormat::F32 => build_typed::<f32>(device, config, channels, world, epoch),
        cpal::SampleFormat::I16 => build_typed::<i16>(device, config, channels, world, epoch),
        cpal::SampleFormat::U16 => build_typed::<u16>(device, config, channels, world, epoch),
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
    epoch: EvalEpoch,
) -> Option<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    // Reused interleaved `f32` scratch; the engine writes it, then we convert.
    let mut scratch: Vec<f32> = Vec::new();
    device
        .build_output_stream(
            config,
            move |output: &mut [T], info: &cpal::OutputCallbackInfo| {
                scratch.clear();
                scratch.resize(output.len(), 0.0);
                // Anchor the engine clock to this buffer's heard-time on the shared
                // monotonic epoch (`now` + the callback-to-playback latency), so
                // scheduled control updates resolve to the right sample even as the
                // audio device clock drifts against the epoch.
                let ts = info.timestamp();
                let ahead = ts.playback.duration_since(ts.callback).as_secs_f64();
                let buffer_time = osc(epoch.now_secs() + ahead);
                world.fill_at(&mut scratch, channels, buffer_time);
                for (o, s) in output.iter_mut().zip(scratch.iter()) {
                    *o = T::from_sample(*s);
                }
            },
            |err| log::error!("bevy_gantz_plyphon: audio stream error: {err}"),
            None,
        )
        .ok()
}
