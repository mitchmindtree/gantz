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
use std::time::{Duration, Instant};

use bevy_app::{App, Plugin, Update};
use bevy_ecs::prelude::*;
use bevy_ecs::system::NonSendMut;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};

use gantz_ca as ca;
use gantz_core::node::graph::Graph;
use gantz_plyphon::{Backend, Embedded, GainRef, ToNodeDsp, derive_synthdef, structural_sig};
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

/// The minimum time a crossfade-retired synth keeps running before its deferred
/// free (its fade gains ramp to zero over their own lags; a `LagControl` decays
/// to 0.1% within its lag, so this floor comfortably covers
/// [`gantz_plyphon::FADE_LAG`]). A longer gain lag extends the deadline to match.
const FADE_GRACE: Duration = Duration::from_millis(100);

/// The most crossfade-retired synths kept per head: a structural-weight drag
/// respawns every frame, so without a cap fades would pile up faster than they
/// expire. Freeing the oldest early is near-click-free - its gain has already
/// been decaying for at least a frame.
const MAX_FADING_PER_HEAD: usize = 2;

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
    /// Crossfade-retired synths still ramping their gains to zero, freed once
    /// their deadline passes (oldest first - entries are pushed in replacement
    /// order). Their scope streams ride along: a stream index must not be
    /// re-cued while the old `ScopeOut` (bufnum baked into its def) can still
    /// write it.
    fading: Vec<FadingSynth>,
}

/// A synth retired by a crossfaded replacement: its gains have been set to zero
/// (ramping via their own lags) and it is freed once `deadline` passes.
struct FadingSynth {
    /// The head the synth belonged to (for the per-head backlog cap).
    entity: Entity,
    /// The running synth to free at `deadline`.
    node_id: i32,
    /// When the fade has died away and the synth (+ scopes) can be freed.
    deadline: Instant,
    /// The synth's cued scope streams, closed + freed only at `deadline`.
    scopes: Vec<ScopeSlot>,
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
    /// The def's driver-owned fade gains (one per sink), used to fade the synth
    /// in on spawn and out across a crossfaded replacement.
    gains: Vec<GainRef>,
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
                state,
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

    // Fade out synths whose heads are no longer open (their defs are freed now -
    // safe, a running synth holds its own compiled def until freed).
    let stale: Vec<Entity> = state
        .synths
        .keys()
        .copied()
        .filter(|e| !live.contains(e))
        .collect();
    for e in stale {
        if let Some(synth) = state.synths.remove(&e) {
            let def_name = synth.def_name.clone();
            fade_out(&mut audio.controller, state, e, synth);
            let _ = Embedded::new(&mut audio.controller).free_synthdef(&def_name);
        }
    }

    // Sweep crossfade-retired synths, freeing those whose fade has died away.
    for f in expire_fades(&mut state.fading, Instant::now()) {
        let _ = Embedded::new(&mut audio.controller).free_node(f.node_id);
        free_scopes(&mut audio.controller, &mut state.scope_alloc, f.scopes);
    }
}

/// Split the fade backlog: drains and returns the entries due for freeing - those
/// past their deadline, plus the *oldest* entries of any head whose backlog
/// exceeds [`MAX_FADING_PER_HEAD`] (entries are pushed in replacement order, so a
/// structural-weight drag that respawns every frame retires its pile-up early;
/// near-click-free, as their gains have already been decaying).
fn expire_fades(fading: &mut Vec<FadingSynth>, now: Instant) -> Vec<FadingSynth> {
    let mut backlog: HashMap<Entity, usize> = HashMap::new();
    for f in fading.iter() {
        *backlog.entry(f.entity).or_default() += 1;
    }
    let mut expired = Vec::new();
    for f in std::mem::take(fading) {
        let count = backlog.get_mut(&f.entity).expect("counted above");
        if f.deadline <= now || *count > MAX_FADING_PER_HEAD {
            *count -= 1;
            expired.push(f);
        } else {
            fading.push(f);
        }
    }
    expired
}

/// Re-derive `entity`'s synthdef and crossfade-replace the synth if its *structure*
/// changed (else keep the running synth, just recording the new graph address and
/// refreshing its `~scopeout` ring sizes). Fades the synth out if the head has no dsp
/// sink (no `~out` and no `~scopeout`).
///
/// The structural signature is computed on the *unpatched* def - every `~scopeout`'s
/// `ScopeOut` `bufnum` is still the `0.0` placeholder and every gain param still
/// carries its nominal default - so it is stable regardless of the patches below (a
/// re-derive of the same graph does not spuriously respawn). On a replacement, each
/// `~scopeout` gets a freshly-cued scope stream (its `ScopeOut` `bufnum` patched to the
/// stream's index) and each gain param's default is patched to `0.0` so the synth
/// *spawns silent*: param defaults seed both the control wire and the lag state, and
/// the same-frame param sync then ramps the live gain in through the param's own lag.
/// The old synth keeps playing until the replacement is up (on install/spawn failure
/// it is left untouched), then fades out via [`fade_out`] - together, a crossfade.
fn structural_sync<N>(
    controller: &mut Controller,
    state: &mut HeadSynths,
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
        // No dsp sink to root a synthdef at: fade out any running synth. Freeing
        // the def now is safe - the fading synth holds its own compiled copy.
        Err(gantz_plyphon::DeriveError::NoSink) => {
            if let Some(synth) = state.synths.remove(&entity) {
                fade_out(controller, state, entity, synth);
                let _ = Embedded::new(controller).free_synthdef(&def_name);
            }
            return;
        }
    };
    let sig = structural_sig(&derived.def);

    // Structure unchanged (e.g. a non-dsp edit, or a ring-`size` edit that is not in
    // the def): keep the running synth + its param slots + its cued scope streams;
    // record the new graph address and refresh each tap's ring `size` (matched by
    // node path) in case a size edit changed it.
    if state.synths.get(&entity).map(|s| s.sig) == Some(sig) {
        if let Some(prev) = state.synths.get_mut(&entity) {
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

    // Structural change (or this head's first synth): build the replacement first -
    // cue a fresh scope stream per `~scopeout` (patching its `ScopeOut` bufnum to the
    // cued index), zero the gain defaults, install + spawn - and only then retire
    // the old synth. Re-adding the def under the same name is safe while the old
    // synth fades: plyphon retires the previous compiled def, and a running synth
    // keeps its own reference.
    let mut scopes = Vec::new();
    for m in &derived.monitors {
        let index = state.scope_alloc.alloc();
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
                state.scope_alloc.free(index);
            }
        }
    }
    // Spawn silent: the same-frame param sync ramps each gain in from 0.
    for g in &derived.gains {
        derived.def.params[g.index].default = 0.0;
    }

    let mut backend = Embedded::new(controller);
    if let Err(e) = backend.install_synthdef(derived.def) {
        log::error!("bevy_gantz_plyphon: synthdef install failed: {e:?}");
        drop(backend);
        free_scopes(controller, &mut state.scope_alloc, scopes);
        return;
    }
    match backend.spawn(&def_name) {
        Ok(node_id) => {
            // A gain with no param slot (no node state feeds it, e.g. a bus
            // writer's fade gain) would stay stuck at the patched 0.0 default;
            // ramp it straight to unity instead.
            for g in &derived.gains {
                if !derived.params.iter().any(|b| b.index == g.index) {
                    if let Err(e) = backend.set_control(node_id, g.index, 1.0) {
                        log::error!("bevy_gantz_plyphon: fade-gain restore failed: {e:?}");
                    }
                }
            }
            drop(backend);
            // The replacement is live: fade the old synth out (the gain overlap
            // is the crossfade). The def name was just re-installed, so the old
            // def is already retired - nothing to free here.
            if let Some(old) = state.synths.remove(&entity) {
                fade_out(controller, state, entity, old);
            }
            let params = derived
                .params
                .iter()
                .map(|b| ParamSlot {
                    node_path: b.node_path.clone(),
                    index: b.index,
                    last: None,
                })
                .collect();
            state.synths.insert(
                entity,
                HeadSynth {
                    graph: graph_ca,
                    def_name,
                    node_id,
                    sig,
                    params,
                    scopes,
                    gains: derived.gains,
                },
            );
        }
        Err(e) => {
            // Keep the old synth playing untouched - strictly better than the
            // silence a teardown would leave.
            log::error!("bevy_gantz_plyphon: synth spawn failed: {e:?}");
            let _ = backend.free_synthdef(&def_name);
            drop(backend);
            free_scopes(controller, &mut state.scope_alloc, scopes);
        }
    }
}

/// Begin fading `synth` out: ramp every gain to zero (each through its own lag)
/// and queue the synth - with its cued scope streams - for a deferred free once
/// the slowest ramp has died away. A synth with no gains (e.g. monitor-only) has
/// no audible output to de-click and is freed immediately.
fn fade_out(controller: &mut Controller, state: &mut HeadSynths, entity: Entity, synth: HeadSynth) {
    if synth.gains.is_empty() {
        let _ = Embedded::new(controller).free_node(synth.node_id);
        free_scopes(controller, &mut state.scope_alloc, synth.scopes);
        return;
    }
    let mut backend = Embedded::new(controller);
    let mut max_lag = 0.0f32;
    for g in &synth.gains {
        max_lag = max_lag.max(g.lag);
        if let Err(e) = backend.set_control(synth.node_id, g.index, 0.0) {
            log::error!("bevy_gantz_plyphon: fade-out set_control failed: {e:?}");
        }
    }
    state.fading.push(FadingSynth {
        entity,
        node_id: synth.node_id,
        deadline: Instant::now() + FADE_GRACE.max(Duration::from_secs_f32(max_lag)),
        scopes: synth.scopes,
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A fading entry with no scope streams, due at `deadline`.
    fn fade(entity: Entity, deadline: Instant) -> FadingSynth {
        FadingSynth {
            entity,
            node_id: 0,
            deadline,
            scopes: Vec::new(),
        }
    }

    /// `secs` seconds after `base`.
    fn later(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    fn entities(n: usize) -> Vec<Entity> {
        let mut world = bevy_ecs::world::World::new();
        (0..n).map(|_| world.spawn_empty().id()).collect()
    }

    /// Entries past their deadline drain; the rest stay, in order.
    #[test]
    fn expire_fades_drains_past_deadline() {
        let e = entities(1);
        let base = Instant::now();
        let mut fading = vec![fade(e[0], base), fade(e[0], later(base, 1))];
        let expired = expire_fades(&mut fading, later(base, 0));
        assert_eq!(expired.len(), 1);
        assert_eq!(fading.len(), 1);
        assert!(fading[0].deadline > base);
    }

    /// A head's backlog over the cap retires its OLDEST entries early, leaving
    /// at most `MAX_FADING_PER_HEAD`; other heads' entries are untouched.
    #[test]
    fn expire_fades_caps_per_head_backlog() {
        let e = entities(2);
        let base = Instant::now();
        let mut fading: Vec<FadingSynth> = (0..4).map(|i| fade(e[0], later(base, 1 + i))).collect();
        fading.push(fade(e[1], later(base, 1)));
        let expired = expire_fades(&mut fading, base);
        // The two oldest of e0's four go; e1's single entry stays.
        assert_eq!(expired.len(), 4 - MAX_FADING_PER_HEAD);
        assert!(expired.iter().all(|f| f.entity == e[0]));
        assert_eq!(
            fading.iter().filter(|f| f.entity == e[0]).count(),
            MAX_FADING_PER_HEAD,
        );
        assert_eq!(fading.iter().filter(|f| f.entity == e[1]).count(), 1);
        // The kept entries are the newest (latest deadlines).
        assert!(
            fading
                .iter()
                .filter(|f| f.entity == e[0])
                .all(|f| f.deadline >= later(base, 3)),
        );
    }

    /// Under the cap and before any deadline, nothing drains.
    #[test]
    fn expire_fades_keeps_recent_under_cap() {
        let e = entities(1);
        let base = Instant::now();
        let mut fading = vec![fade(e[0], later(base, 1)), fade(e[0], later(base, 2))];
        assert!(expire_fades(&mut fading, base).is_empty());
        assert_eq!(fading.len(), 2);
    }
}
