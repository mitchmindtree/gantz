//! Bevy + plyphon audio runtime for gantz.
//!
//! [`PlyphonPlugin`] owns the audio engine: at startup it opens a cpal output
//! stream (its own audio thread, running [`plyphon::World::fill_at`] so the engine
//! clock is anchored to the host) and keeps the [`plyphon::Controller`] +
//! [`plyphon::Nrt`] handles. Each update, after [`bevy_gantz::VmSet`], it derives a
//! synthdef from each open head's DSP subgraph (rooted at its `~out` output and any
//! `~scopeout` monitors) and installs/respawns the synth via the [`gantz_plyphon::Backend`]
//! seam whenever the head's committed graph changes.
//!
//! The bridge runs both ways: control values drive dsp params via `set_control`,
//! and each `~scopeout` monitor's `SendTrig` `/tr`s are drained here into the node's
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
use gantz_plyphon::{Backend, Embedded, ToNodeDsp, derive_synthdef, structural_sig};
use plyphon::{Controller, InputRef, Nrt, Options, StreamConsumer, World, engine};

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

/// Frames per chunk in a `~scopeout`'s scope stream (one plyphon block).
const CHUNK_FRAMES: usize = 64;
/// Chunks pre-allocated per `~scopeout` scope stream (~43 ms of slack at 48 kHz before a
/// bounded overrun drops surplus - harmless, as the ring only keeps the last `size`).
const NUM_CHUNKS: usize = 32;

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
        // the param drain below in the same frame. `HeadSynths` is a NonSend resource:
        // it holds each `~scopeout`'s scope `StreamConsumer` (a `!Sync` SPSC handle).
        app.insert_non_send(HeadSynths::default())
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

/// Per-head synth bookkeeping (which committed graph produced the currently
/// installed synthdef and running synth, so we only re-derive on change), plus the
/// allocator of global scope-stream indices for `~scopeout`s. A NonSend resource - a
/// `~scopeout`'s scope `StreamConsumer` is `Send` but not `Sync`.
#[derive(Default)]
struct HeadSynths {
    synths: HashMap<Entity, HeadSynth>,
    scope_alloc: ScopeAlloc,
}

/// Allocates globally-unique scope-stream indices (plyphon recording-slot ids) for
/// live `~scopeout`s, reusing freed ones so a long editing session doesn't exhaust them.
#[derive(Default)]
struct ScopeAlloc {
    free: Vec<usize>,
    next: usize,
}

impl ScopeAlloc {
    /// A free index (reused if available, else a fresh one).
    fn alloc(&mut self) -> usize {
        self.free.pop().unwrap_or_else(|| {
            let index = self.next;
            self.next += 1;
            index
        })
    }

    /// Return an index for reuse.
    fn free(&mut self, index: usize) {
        self.free.push(index);
    }
}

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
    /// One slot per `~scopeout`: the cued scope stream whose samples the driver drains into
    /// the node's ring state each frame.
    scopes: Vec<ScopeSlot>,
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

/// A `~scopeout`'s live scope stream: every sample of its dsp input arrives here (via
/// plyphon `ScopeOut` → `cue_scope`) for the driver to append into the node's rings.
struct ScopeSlot {
    /// The `~scopeout` node's path in the graph (where its ring state lives).
    node_path: Vec<usize>,
    /// Each per-channel ring's length in *frames*.
    size: usize,
    /// The number of interleaved channels the stream carries (`cue_scope`'s width) -
    /// the tapped signal's width, inferred at derive time.
    channels: usize,
    /// The cued scope-stream index (a global recording-slot id, baked into the def's
    /// `ScopeOut` and freed on teardown).
    index: usize,
    /// The consumer the driver drains (`pop_filled`/`recycle`) each frame.
    consumer: StreamConsumer,
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
/// - *Scope sync* (every frame): drain each `~scopeout`'s scope stream and append its
///   samples into the node's ring state (capped at the tap's `size`).
///
/// Also tears down synths for closed heads.
fn drive_synths<N>(
    registry: Res<Registry<N>>,
    audio: Option<NonSendMut<AudioEngine>>,
    audio_config: Res<AudioConfig>,
    mut enabled_applied: Local<Option<bool>>,
    state: NonSendMut<HeadSynths>,
    mut vms: NonSendMut<HeadVms>,
    heads: Query<(Entity, &HeadRef, &WorkingGraph<N>), With<OpenHead>>,
) where
    N: 'static + ToNodeDsp + Send + Sync,
{
    let Some(audio) = audio else {
        return;
    };
    let audio = audio.into_inner();
    let state = state.into_inner();

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

    let out_channels = audio.out_channels;
    let sample_rate = audio.sample_rate;
    let mut live: HashSet<Entity> = HashSet::new();

    for (entity, head_ref, wg) in heads.iter() {
        live.insert(entity);
        let Some(graph_ca) = registry.head_commit(&head_ref.0).map(|c| c.graph) else {
            continue;
        };

        // --- Structural sync: only when the committed graph changed. ---
        if state.synths.get(&entity).map(|h| h.graph) != Some(graph_ca) {
            structural_sync(
                &mut audio.controller,
                &mut state.synths,
                &mut state.scope_alloc,
                entity,
                graph_ca,
                &wg.0,
                out_channels,
                sample_rate,
            );
        }

        // --- Param sync: drain each param's queued control updates and schedule
        // them ahead of the audio clock; direct (untimestamped) value edits apply
        // immediately. ---
        if let (Some(synth), Some(vm)) = (state.synths.get_mut(&entity), vms.get_mut(&entity)) {
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

            // Scope sync: drain each `~scopeout`'s scope stream and append every streamed
            // sample into its per-channel ring state (the list-of-rings its control
            // `expr` surfaces on a trigger push; `push_ring` deinterleaves and keeps
            // the last `size` frames per channel).
            for scope in &mut synth.scopes {
                let mut samples = Vec::new();
                while let Some(chunk) = scope.consumer.pop_filled() {
                    samples.extend_from_slice(chunk.filled_samples());
                    scope.consumer.recycle(chunk);
                }
                if !samples.is_empty() {
                    gantz_plyphon::monitor::push_ring(
                        vm,
                        &scope.node_path,
                        &samples,
                        scope.size,
                        scope.channels,
                    );
                }
            }
        }
    }

    // Tear down synths whose heads are no longer open.
    let stale: Vec<Entity> = state
        .synths
        .keys()
        .copied()
        .filter(|e| !live.contains(e))
        .collect();
    for e in stale {
        let synth = state.synths.remove(&e);
        teardown(&mut audio.controller, &mut state.scope_alloc, synth);
    }
}

/// Re-derive `entity`'s synthdef and respawn the synth if its *structure* changed
/// (else keep the running synth, just recording the new graph address and refreshing
/// its `~scopeout` ring sizes). Tears the synth down if the head has no dsp sink (no `~out`
/// and no `~scopeout`).
///
/// The structural signature is computed on the *unpatched* def - every `~scopeout`'s
/// `ScopeOut` `bufnum` is still the `0.0` placeholder - so it is stable regardless of
/// the global scope indices we allocate below (a re-derive of the same graph does not
/// spuriously respawn). On a respawn, each `~scopeout` gets a freshly-cued scope stream and
/// its `ScopeOut` `bufnum` patched to that stream's index before the def is installed.
fn structural_sync<N>(
    controller: &mut Controller,
    synths: &mut HashMap<Entity, HeadSynth>,
    scope_alloc: &mut ScopeAlloc,
    entity: Entity,
    graph_ca: ca::GraphAddr,
    graph: &Graph<N>,
    out_channels: usize,
    sample_rate: f64,
) where
    N: ToNodeDsp,
{
    let def_name = format!("gantz-head-{}", entity.index());
    let mut derived = match derive_synthdef(graph, out_channels, def_name.clone()) {
        Ok(derived) => derived,
        // No dsp sink to root a synthdef at: tear down any running synth.
        Err(gantz_plyphon::DeriveError::NoSink) => {
            teardown(controller, scope_alloc, synths.remove(&entity));
            return;
        }
    };
    let sig = structural_sig(&derived.def);

    // Structure unchanged (e.g. a non-dsp edit, or a ring-`size` edit that is not in
    // the def): keep the running synth + its param slots + its cued scope streams;
    // record the new graph address and refresh each tap's ring `size` (matched by
    // node path) in case a size edit changed it.
    if synths.get(&entity).map(|s| s.sig) == Some(sig) {
        if let Some(prev) = synths.get_mut(&entity) {
            prev.graph = graph_ca;
            for m in &derived.monitors {
                if let Some(slot) = prev.scopes.iter_mut().find(|s| s.node_path == m.node_path) {
                    slot.size = m.size;
                    // A width change always changes the `ScopeOut` unit's input
                    // count and thus the sig, so it can't reach this branch.
                    debug_assert_eq!(
                        slot.channels, m.channels,
                        "sig-unchanged sync must not change a scope's width",
                    );
                }
            }
        }
        return;
    }

    // Structural change (or this head's first synth): free the old synth + its scope
    // streams, cue a fresh scope stream per `~scopeout` (patching its `ScopeOut` bufnum to
    // the cued index), then install + respawn.
    teardown(controller, scope_alloc, synths.remove(&entity));

    let mut scopes = Vec::new();
    for m in &derived.monitors {
        let index = scope_alloc.alloc();
        derived.def.units[m.scope_unit].inputs[0] = InputRef::Constant(index as f32);
        let channels = m.channels.max(1);
        match controller.cue_scope(index, channels, sample_rate, CHUNK_FRAMES, NUM_CHUNKS) {
            Ok(consumer) => scopes.push(ScopeSlot {
                node_path: m.node_path.clone(),
                size: m.size,
                channels,
                index,
                consumer,
            }),
            Err(e) => {
                log::error!("bevy_gantz_plyphon: cue_scope failed: {e:?}");
                scope_alloc.free(index);
            }
        }
    }

    let mut backend = Embedded::new(controller);
    if let Err(e) = backend.install_synthdef(derived.def) {
        log::error!("bevy_gantz_plyphon: synthdef install failed: {e:?}");
        drop(backend);
        free_scopes(controller, scope_alloc, scopes);
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
                    scopes,
                },
            );
        }
        Err(e) => {
            log::error!("bevy_gantz_plyphon: synth spawn failed: {e:?}");
            let _ = backend.free_synthdef(&def_name);
            drop(backend);
            free_scopes(controller, scope_alloc, scopes);
        }
    }
}

/// Free a head's running synth, its synthdef, and its `~scopeout` scope streams, if any.
fn teardown(controller: &mut Controller, scope_alloc: &mut ScopeAlloc, synth: Option<HeadSynth>) {
    if let Some(s) = synth {
        let mut backend = Embedded::new(controller);
        let _ = backend.free_node(s.node_id);
        let _ = backend.free_synthdef(&s.def_name);
        drop(backend);
        free_scopes(controller, scope_alloc, s.scopes);
    }
}

/// Close each scope stream's cued recording slot and return its index to the
/// allocator for reuse (the `StreamConsumer`s drop with the vec).
fn free_scopes(controller: &mut Controller, scope_alloc: &mut ScopeAlloc, scopes: Vec<ScopeSlot>) {
    for scope in scopes {
        let _ = controller.close_recording(scope.index);
        scope_alloc.free(scope.index);
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
