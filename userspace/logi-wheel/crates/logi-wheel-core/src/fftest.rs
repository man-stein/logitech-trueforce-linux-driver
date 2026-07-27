//! The Test view's two guarded force-feedback simulations: shared step
//! tables, kernel `ff_effect` construction, capability-based skipping and
//! the sequence runner both front-ends drive.
//!
//! Both front-ends used to upload and play exactly one canned effect
//! (a single `FF_CONSTANT` for "force", a single `FF_PERIODIC`/`FF_SINE`
//! for "texture") for a fixed 2 s. That let the wheel pin to one side and
//! stop, which is not a useful demonstration of anything the driver
//! actually implements. This module replaces each with a short sequence
//! of labelled steps exercising the palette the kernel driver advertises
//! (`FF_CONSTANT`, `FF_RAMP`, `FF_SPRING`, `FF_DAMPER`, `FF_PERIODIC`; see
//! `mainline/hid-logitech-hidpp.c`'s `hidpp_ff_effects`/`hidpp_ff_effects_v2`),
//! with a frequency progression for the texture side.
//!
//! What lives here vs. per front-end:
//! - the step tables ([`FORCE_SEQUENCE`], [`TEXTURE_SEQUENCE`]) and the
//!   `ff_effect` byte layout ([`build_ff_effect`]) are the one shared
//!   source of truth;
//! - the sequencing itself (capability filtering, the per-step countdown,
//!   upload/play/wait/stop/erase per step, cancellation, ENODEV handling)
//!   is also shared, via [`run_sequence`] against the [`FfDevice`] trait;
//! - only the actual file descriptor and the `ioctl`/`libc` calls that
//!   implement `FfDevice` stay in each front-end (this crate stays
//!   dependency-free and never opens a device node), mirroring how
//!   `evtest` keeps event decoding here while the open fd stays out;
//! - so does the rendered plan's state machine ([`StepState`],
//!   [`SequenceProgress`]): a front-end confirms a sequence, builds a
//!   [`SequenceProgress`] with every row `Pending` and shows the whole
//!   plan immediately, then folds each [`SequenceEvent`] the running
//!   sequence reports into it. Both front-ends render the exact same rows
//!   from the exact same source; only the widgets differ.
//!
//! Force/direction math is not invented here: `direction` values and the
//! condition-effect coefficient signs are exactly what
//! `mainline/hid-logitech-hidpp.c` expects (`hidpp_dd_project_constant`'s
//! `direction=0x4000` == east convention, and `hidpp_dd_condition_force`'s
//! restoring-force sign, which needs a POSITIVE coefficient on both
//! sides - a negative pair would build an anti-spring, not a centering
//! one) - for the DD engine. The G923's classic (lg4ff-style) engine does
//! not share that convention: see [`Side`] and [`resolve_direction`].
//! Every step table here stores a logical [`Side`] (`Right`/`Left`/`None`),
//! never a raw evdev direction value directly, and [`build_ff_effect`]
//! resolves it against the playing wheel's [`WheelModel`] at upload time.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::device::WheelModel;
use crate::evtest::EVENT_SIZE;

// ---------------------------------------------------------------------------
// evdev force-feedback constants (`linux/input-event-codes.h`).
// ---------------------------------------------------------------------------

/// `EV_FF`, the evdev event type for force-feedback play/stop/gain events.
pub const EV_FF: u16 = 0x15;

/// `ff_effect.type` values this crate builds. Verified against
/// `mainline/hid-logitech-hidpp.c` (which builds against the real kernel
/// header) and the ffb-proxy crate's `sink` module.
pub const FF_PERIODIC: u16 = 0x51;
pub const FF_CONSTANT: u16 = 0x52;
pub const FF_SPRING: u16 = 0x53;
pub const FF_DAMPER: u16 = 0x55;
pub const FF_RAMP: u16 = 0x57;
pub const FF_SINE: u16 = 0x5a;
/// Not an effect type: the `EV_FF` code for the device-gain write.
pub const FF_GAIN: u16 = 0x60;
/// Highest legal bit `EVIOCGBIT(EV_FF, ...)` can report (`FF_MAX` in
/// `linux/input-event-codes.h`; `FF_CNT` is one past it).
pub const FF_MAX: u16 = 0x7f;

/// Size of the union embedded at the end of `struct ff_effect`, sized to
/// fit the largest member (`ff_periodic_effect`, 32 bytes on a 64-bit
/// kernel).
const FF_UNION_SIZE: usize = 32;

/// Bytes an `EVIOCGBIT(EV_FF, ...)` capability query needs to cover every
/// bit up to [`FF_MAX`] (`(FF_MAX + 1) / 8`, rounded up, with headroom).
pub const FF_BITS_LEN: usize = 32;

/// evdev direction for "east" (`0x4000`, i.e. 90 degrees on evdev's
/// circular direction scale). On the DD engine this is the documented
/// convention for "a positive level/magnitude produces a positive
/// (rightward) force" (see `hidpp_dd_project_constant`'s doc comment -
/// direction 0 produces zero force for a signed level, which is why
/// every directional step here resolves to one of these two raw values,
/// never 0 - see [`Side`]/[`resolve_direction`]).
const DIRECTION_EAST: u16 = 0x4000;
/// evdev direction for "west" (`0xC000`, 270 degrees): flips the sign of
/// the same positive level/magnitude relative to [`DIRECTION_EAST`].
/// Condition effects (spring/damper) ignore `direction` entirely
/// (`hidpp_dd_condition_force` reads only the condition fields), so their
/// steps use [`Side::None`], which resolves to 0.
const DIRECTION_WEST: u16 = 0xC000;

/// Which side of center a directional step pulls the wheel toward,
/// independent of engine. Resolved to a raw evdev `direction` value only
/// at upload time, via [`resolve_direction`], because the DD and G923
/// (classic/lg4ff) engines disagree on which raw value means which side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Right,
    Left,
    /// Condition effects (spring/damper): direction is meaningless, always
    /// resolves to 0.
    None,
}

/// Resolve `side` to the raw `ff_effect.direction` value for a wheel of
/// `model`. Two force engines, two conventions:
/// - the DD engine (RS50, G PRO, and `Unknown` - assumed DD-shaped, same
///   as everywhere else `WheelModel` gates DD vs. classic behavior) uses
///   `hidpp_dd_project_constant`'s documented convention as-is:
///   [`DIRECTION_EAST`] (0x4000) is rightward, [`DIRECTION_WEST`] (0xC000)
///   is leftward.
/// - the G923's classic (lg4ff-style) engine does the opposite in
///   practice, hardware-verified 2026-07-27 on the owner's G923 across
///   three separate measurements (the original constant-force
///   validation, the TrueForce mirror sign test's baseline push, and this
///   sequence's own step 1): an effect with `direction=0x4000` and a
///   POSITIVE level rotates the wheel LEFT, not right. So a G923's
///   "right" step must carry [`DIRECTION_WEST`] and its "left" step
///   [`DIRECTION_EAST`] - the DD engine's values, swapped.
pub fn resolve_direction(side: Side, model: WheelModel) -> u16 {
    match (side, model) {
        (Side::None, _) => 0,
        (Side::Right, WheelModel::G923) => DIRECTION_WEST,
        (Side::Right, _) => DIRECTION_EAST,
        (Side::Left, WheelModel::G923) => DIRECTION_EAST,
        (Side::Left, _) => DIRECTION_WEST,
    }
}

/// ~30% of the `i16` force/magnitude range, the level every step in both
/// sequences uses (moderate, per the task's "keep amplitude moderate"
/// requirement).
pub const SIM_LEVEL_30: i16 = 9830;
/// ~30% of the `u16` saturation range, the torque cap the condition
/// (spring/damper) steps use.
pub const SIM_SATURATION_30: u16 = 19661;

/// How often a step's playback wait re-checks the cancel flag.
pub const SIM_CANCEL_POLL: Duration = Duration::from_millis(10);

/// The lead-in countdown [`run_sequence`] runs before every step,
/// including the first: `ticks` one-second ticks counting down to 1, each
/// held out for `tick`. Replaces two things the sequence used to do
/// separately - a fixed 5s wait before the sequence started at all, and a
/// silent 400ms gap between steps with no visible countdown - with one
/// coherent behavior: every step, first or not, gets the same visible
/// "3, 2, 1" before it plays, so nothing stacks and the user always knows
/// exactly when the wheel is about to move. Cancellable at any tick, same
/// as a step's own playback wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Countdown {
    pub ticks: u64,
    pub tick: Duration,
}

impl Countdown {
    /// No countdown at all: zero ticks, so [`run_sequence`] moves straight
    /// from a step's `Step` event into uploading it. Only meant for tests
    /// that do not care about countdown behavior and want to stay fast;
    /// both front-ends always pass [`STEP_COUNTDOWN`].
    pub const NONE: Countdown = Countdown { ticks: 0, tick: Duration::ZERO };
}

/// The countdown both front-ends run in production: 3 real seconds (one
/// tick per second), before every step.
pub const STEP_COUNTDOWN: Countdown = Countdown { ticks: 3, tick: Duration::from_secs(1) };

// ---------------------------------------------------------------------------
// `struct ff_effect` (`linux/input.h`), mirrored field for field.
// ---------------------------------------------------------------------------

/// The trailing `union` of the kernel's `struct ff_effect`, modeled as a
/// plain byte array (never a live typed reference into misaligned
/// fields), the same convention the ffb-proxy crate's `sink` module uses.
#[repr(C, align(8))]
#[derive(Debug, Clone, Copy)]
pub struct FfUnion(pub [u8; FF_UNION_SIZE]);

/// Mirrors the kernel's `struct ff_effect`: `type`, `id`, `direction`,
/// trigger (button+interval), replay (length+delay), then the union.
/// `#[repr(C)]` with the union's own `align(8)` is what makes
/// `size_of::<FfEffect>()` match the kernel's 48 bytes (pinned by a test
/// below); a front-end's `EVIOCSFF`/`EVIOCRMFF` request numbers are
/// computed from this size, so a layout mismatch fails the ioctl outright
/// rather than silently.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FfEffect {
    pub type_: u16,
    pub id: i16,
    pub direction: u16,
    pub trigger_button: u16,
    pub trigger_interval: u16,
    pub replay_length: u16,
    pub replay_delay: u16,
    pub u: FfUnion,
}

/// Encode one `struct input_event` (64-bit ABI, zeroed timestamp; the
/// kernel fills timestamps in itself for written `EV_FF` events).
pub fn encode_ff_event(code: u16, value: i32) -> [u8; EVENT_SIZE] {
    let mut b = [0u8; EVENT_SIZE];
    b[16..18].copy_from_slice(&EV_FF.to_le_bytes());
    b[18..20].copy_from_slice(&code.to_le_bytes());
    b[20..24].copy_from_slice(&value.to_le_bytes());
    b
}

/// Whether `bits` (an `EVIOCGBIT(EV_FF, ...)` result, any length) marks
/// `ff_type` as supported. Pure bit test, no I/O: the ioctl call itself
/// stays in the front-end's [`FfDevice`] implementation.
pub fn ff_type_supported(bits: &[u8], ff_type: u16) -> bool {
    let idx = usize::from(ff_type);
    bits.get(idx / 8).is_some_and(|b| b & (1 << (idx % 8)) != 0)
}

// ---------------------------------------------------------------------------
// Step table.
// ---------------------------------------------------------------------------

/// One playable effect's kind and parameters, sized to build exactly one
/// [`FfEffect`] variant's union. Field layouts (and the sign convention
/// for the condition effects) are exactly what
/// `mainline/hid-logitech-hidpp.c` expects; see the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepEffect {
    /// `ff_constant_effect`: a steady pull.
    Constant { level: i16 },
    /// `ff_ramp_effect`: force rising (or falling) linearly over the
    /// step's duration.
    Ramp { start: i16, end: i16 },
    /// `ff_condition_effect`: a centering spring. `right_coeff`/
    /// `left_coeff` must both be positive (see the module doc) - a
    /// restoring spring, not an anti-spring on one side.
    Spring { right_coeff: i16, left_coeff: i16, right_sat: u16, left_sat: u16 },
    /// `ff_condition_effect`: resistance proportional to turning speed.
    /// Same positive-both-sides requirement as [`StepEffect::Spring`].
    Damper { right_coeff: i16, left_coeff: i16, right_sat: u16, left_sat: u16 },
    /// `ff_periodic_effect`: a waveform vibration (used for the sine step
    /// and the whole texture sequence's frequency progression).
    Periodic { waveform: u16, period_ms: u16, magnitude: i16 },
}

impl StepEffect {
    /// The `ff_effect.type` value this variant uploads as (what
    /// `EVIOCGBIT(EV_FF, ...)` capability bit gates it, and what
    /// [`build_ff_effect`] sets `type_` to).
    pub fn ff_type(&self) -> u16 {
        match self {
            StepEffect::Constant { .. } => FF_CONSTANT,
            StepEffect::Ramp { .. } => FF_RAMP,
            StepEffect::Spring { .. } => FF_SPRING,
            StepEffect::Damper { .. } => FF_DAMPER,
            StepEffect::Periodic { .. } => FF_PERIODIC,
        }
    }
}

/// One step of a test sequence: what to play, for how long, in which
/// logical direction, and the human label a front-end shows while it
/// plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimStep {
    /// Shown by both front-ends while this step plays (see
    /// [`step_status_text`]).
    pub label: &'static str,
    pub effect: StepEffect,
    /// `ff_effect.replay.length`, and how long [`run_sequence`] plays the
    /// step before moving on (or ending, if `cancel` flips first).
    pub duration_ms: u16,
    /// The logical side this step pulls toward; [`Side::None`] for the
    /// condition effects (spring/damper - see the module doc). Resolved
    /// to a raw `ff_effect.direction` value by [`build_ff_effect`], which
    /// needs the playing wheel's model to do it (see [`resolve_direction`]).
    pub direction: Side,
}

/// Build the kernel `ff_effect` for `step` on a wheel of `model` (which
/// resolves `step.direction`'s logical [`Side`] to the raw value this
/// model's engine expects - see [`resolve_direction`]), id `-1` (a fresh
/// upload; the kernel assigns one, which is why every step gets erased
/// before the next one uploads - see [`run_sequence`] - rather than ever
/// updating an existing id).
pub fn build_ff_effect(step: &SimStep, model: WheelModel) -> FfEffect {
    let mut u = [0u8; FF_UNION_SIZE];
    match step.effect {
        StepEffect::Constant { level } => {
            // ff_constant_effect: level:i16 @0, envelope (zeroed) @2.
            u[0..2].copy_from_slice(&level.to_le_bytes());
        }
        StepEffect::Ramp { start, end } => {
            // ff_ramp_effect: start_level:i16 @0, end_level:i16 @2.
            u[0..2].copy_from_slice(&start.to_le_bytes());
            u[2..4].copy_from_slice(&end.to_le_bytes());
        }
        StepEffect::Spring { right_coeff, left_coeff, right_sat, left_sat }
        | StepEffect::Damper { right_coeff, left_coeff, right_sat, left_sat } => {
            // ff_condition_effect: right_saturation:u16 @0,
            // left_saturation:u16 @2, right_coeff:i16 @4, left_coeff:i16
            // @6, deadband:u16 @8 (0) and center:i16 @10 (0) - a true
            // center, exactly what "spring pulls back to center" needs.
            u[0..2].copy_from_slice(&right_sat.to_le_bytes());
            u[2..4].copy_from_slice(&left_sat.to_le_bytes());
            u[4..6].copy_from_slice(&right_coeff.to_le_bytes());
            u[6..8].copy_from_slice(&left_coeff.to_le_bytes());
        }
        StepEffect::Periodic { waveform, period_ms, magnitude } => {
            // ff_periodic_effect: waveform:u16 @0, period:u16 @2,
            // magnitude:i16 @4; offset/phase/envelope (zeroed) follow.
            u[0..2].copy_from_slice(&waveform.to_le_bytes());
            u[2..4].copy_from_slice(&period_ms.to_le_bytes());
            u[4..6].copy_from_slice(&magnitude.to_le_bytes());
        }
    }
    FfEffect {
        type_: step.effect.ff_type(),
        id: -1,
        direction: resolve_direction(step.direction, model),
        trigger_button: 0,
        trigger_interval: 0,
        replay_length: step.duration_ms,
        replay_delay: 0,
        u: FfUnion(u),
    }
}

/// One 0.45 s constant-force pulse toward `side`, at the sequence's
/// standard moderate level. Building block for [`FORCE_SEQUENCE`]'s
/// alternating-pulse opening (see its doc comment for why this replaced
/// two long one-directional constant steps).
const fn pulse(label: &'static str, side: Side) -> SimStep {
    SimStep { label, effect: StepEffect::Constant { level: SIM_LEVEL_30 }, duration_ms: 450, direction: side }
}

/// The force-feedback sequence: ten steps opening with six short
/// alternating constant-force pulses (left/right, repeated three times),
/// then a rising ramp, centering spring, damper resistance and a sine
/// vibration.
///
/// The pulses replace what used to be two long (1.2 s) one-directional
/// constant steps: unopposed at ~30% level for that long, the wheel
/// simply walked to the end stop and parked there (a lock, not a feel
/// test) - reported live on a G923 as "rotated fully right, then fully
/// left". Short pulses that alternate direction every 0.45 s instead rock
/// the wheel around wherever it started, which is what a force-feedback
/// demonstration is supposed to show. Each pulse is still its own full
/// upload/play/stop/erase cycle (see [`run_sequence`]), same as every
/// other step.
///
/// Nominal effect playback alone is ~10.7 s; with [`STEP_COUNTDOWN`]'s 3 s
/// lead-in before each of the ten steps, the whole run takes about 40.7 s
/// end to end - longer than earlier drafts of this sequence, but every
/// second of it is either a labelled step actually playing or a counted-
/// down "it is about to move" warning, never a silent, unexplained wait.
pub const FORCE_SEQUENCE: &[SimStep] = &[
    pulse("Constant force, left pulse", Side::Left),
    pulse("Constant force, right pulse", Side::Right),
    pulse("Constant force, left pulse", Side::Left),
    pulse("Constant force, right pulse", Side::Right),
    pulse("Constant force, left pulse", Side::Left),
    pulse("Constant force, right pulse", Side::Right),
    SimStep {
        label: "Ramp, rising force",
        effect: StepEffect::Ramp { start: 0, end: SIM_LEVEL_30 },
        duration_ms: 1500,
        direction: Side::Right,
    },
    SimStep {
        label: "Spring / centering - turn the wheel and feel it pull back toward center",
        effect: StepEffect::Spring {
            right_coeff: SIM_LEVEL_30,
            left_coeff: SIM_LEVEL_30,
            right_sat: SIM_SATURATION_30,
            left_sat: SIM_SATURATION_30,
        },
        duration_ms: 2500,
        direction: Side::None,
    },
    SimStep {
        label: "Damper - turn the wheel and feel the resistance to motion",
        effect: StepEffect::Damper {
            right_coeff: SIM_LEVEL_30,
            left_coeff: SIM_LEVEL_30,
            right_sat: SIM_SATURATION_30,
            left_sat: SIM_SATURATION_30,
        },
        duration_ms: 2500,
        direction: Side::None,
    },
    SimStep {
        label: "Sine vibration",
        effect: StepEffect::Periodic { waveform: FF_SINE, period_ms: 25, magnitude: SIM_LEVEL_30 },
        duration_ms: 1500,
        direction: Side::Right,
    },
];

/// The TrueForce texture sequence: a frequency progression through four
/// `FF_PERIODIC`/`FF_SINE` steps (10 Hz through 100 Hz) at the same
/// moderate amplitude, so the user can feel that the wheel reproduces a
/// range rather than one fixed tone. Nominal playback alone is 8 s; with
/// [`STEP_COUNTDOWN`]'s 3 s lead-in before each of the four steps, the
/// whole run takes about 20 s end to end.
pub const TEXTURE_SEQUENCE: &[SimStep] = &[
    SimStep {
        label: "Low rumble (~10 Hz) - a slow, heavy pulse",
        effect: StepEffect::Periodic { waveform: FF_SINE, period_ms: 100, magnitude: SIM_LEVEL_30 },
        duration_ms: 2000,
        direction: Side::Right,
    },
    SimStep {
        label: "Buzz (~25 Hz) - a coarse, gritty texture",
        effect: StepEffect::Periodic { waveform: FF_SINE, period_ms: 40, magnitude: SIM_LEVEL_30 },
        duration_ms: 2000,
        direction: Side::Right,
    },
    SimStep {
        label: "Mid-high texture (~50 Hz) - a finer grain",
        effect: StepEffect::Periodic { waveform: FF_SINE, period_ms: 20, magnitude: SIM_LEVEL_30 },
        duration_ms: 2000,
        direction: Side::Right,
    },
    SimStep {
        label: "High-frequency texture (~100 Hz) - a fine, tight buzz",
        effect: StepEffect::Periodic { waveform: FF_SINE, period_ms: 10, magnitude: SIM_LEVEL_30 },
        duration_ms: 2000,
        direction: Side::Right,
    },
];

/// Which sequence a confirmed simulation plays; replaces the old
/// `SimKind` both front-ends kept locally (`ConstantForce`/`Texture`),
/// now backed by [`FORCE_SEQUENCE`]/[`TEXTURE_SEQUENCE`] instead of one
/// fixed effect each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimKind {
    Force,
    Texture,
}

impl SimKind {
    pub fn label(self) -> &'static str {
        match self {
            SimKind::Force => "force feedback",
            SimKind::Texture => "TrueForce texture",
        }
    }

    /// The step table this kind plays, in order.
    pub fn steps(self) -> &'static [SimStep] {
        match self {
            SimKind::Force => FORCE_SEQUENCE,
            SimKind::Texture => TEXTURE_SEQUENCE,
        }
    }
}

/// The status text a front-end shows while `step` (0-based `row` of
/// `total`) plays. Shared so both front-ends' status lines read
/// identically.
pub fn step_status_text(row: usize, total: usize, step: &SimStep) -> String {
    format!("step {}/{total}: {}", row + 1, step.label)
}

// ---------------------------------------------------------------------------
// Per-step progress: the state machine both front-ends render the same
// way (a persistent list of every step, pending through done/skipped),
// fed by [`SequenceEvent`]s from a running (or finished) [`run_sequence`].
// ---------------------------------------------------------------------------

/// One step's state in a rendered plan. Exactly the five states the task
/// calls for: waiting its turn, ticking down before it plays, playing,
/// finished (fully, or stopped early by a cancel that still cleaned up),
/// or skipped for lacking device support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Pending,
    /// Counting down before this step plays; ticks from
    /// [`Countdown::ticks`] down to 1.
    Countdown(u64),
    Playing,
    Done,
    Skipped,
}

impl StepState {
    /// A short, state-only word (never relying on color alone to carry
    /// meaning, since some renderers show these next to a color swatch):
    /// "pending", "3..." while counting down, "playing", "done",
    /// "skipped".
    pub fn status_text(&self) -> String {
        match self {
            StepState::Pending => "pending".to_string(),
            StepState::Countdown(secs) => format!("{secs}..."),
            StepState::Playing => "playing".to_string(),
            StepState::Done => "done".to_string(),
            StepState::Skipped => "skipped".to_string(),
        }
    }
}

/// The whole plan's live progress: one [`StepState`] per row of the step
/// table currently playing (or just finished), plus which row is
/// currently active. Lives here, not per front-end, so the GUI and TUI
/// render the exact same rows from the exact same source - only the
/// widgets differ. A front-end builds one with [`SequenceProgress::new`]
/// the moment a sequence is confirmed (so the full plan renders before
/// anything plays), then folds every [`SequenceEvent`] the running
/// sequence reports into it with [`SequenceProgress::apply`], and keeps
/// showing the result after the run ends - nothing here ever reverts a
/// row back to `Pending`, which is what keeps a finished step's row on
/// screen instead of it disappearing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceProgress {
    pub states: Vec<StepState>,
    /// The row a `Countdown` or `Step` event last pointed at; cleared back
    /// to `None` once that row's `Done` event lands. `None` before
    /// anything starts and once the whole run has ended.
    pub current: Option<usize>,
}

impl SequenceProgress {
    /// One row per entry in `steps`, all `Pending`, nothing active yet.
    pub fn new(steps: &[SimStep]) -> Self {
        SequenceProgress { states: vec![StepState::Pending; steps.len()], current: None }
    }

    /// Fold one [`SequenceEvent`] in: mark the row(s) it names with the
    /// matching [`StepState`]. Unknown rows (out of range for whatever
    /// `steps` this was built from) are ignored rather than panicking -
    /// defensive only, every real event's row always fits the table it
    /// came from.
    pub fn apply(&mut self, event: &SequenceEvent) {
        match *event {
            SequenceEvent::Skipped(rows) => {
                for &(row, _) in rows {
                    if let Some(s) = self.states.get_mut(row) {
                        *s = StepState::Skipped;
                    }
                }
            }
            SequenceEvent::Countdown { row, seconds_left, .. } => {
                if let Some(s) = self.states.get_mut(row) {
                    *s = StepState::Countdown(seconds_left);
                }
                self.current = Some(row);
            }
            SequenceEvent::Step { row, .. } => {
                if let Some(s) = self.states.get_mut(row) {
                    *s = StepState::Playing;
                }
                self.current = Some(row);
            }
            SequenceEvent::Done { row, .. } => {
                if let Some(s) = self.states.get_mut(row) {
                    *s = StepState::Done;
                }
                self.current = None;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The sequence runner.
// ---------------------------------------------------------------------------

/// An error from one [`FfDevice`] operation. `Gone` (`ENODEV`: the wheel
/// was unplugged mid-run) ends the sequence quietly; `Other` surfaces its
/// message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceError {
    Gone,
    Other(String),
}

impl DeviceError {
    fn into_end(self) -> SequenceEnd {
        match self {
            DeviceError::Gone => SequenceEnd::DeviceGone,
            DeviceError::Other(msg) => SequenceEnd::Failed(msg),
        }
    }
}

/// What [`run_sequence`] needs from an open device connection. Each
/// front-end implements this over its own `std::fs::File` + `libc`
/// `ioctl`s (`EVIOCSFF`/`EVIOCRMFF`/`EVIOCGBIT`); this crate never opens a
/// device node, so none of that plumbing lives here.
pub trait FfDevice {
    /// Set the overall device gain (0..=0xFFFF). Called once before the
    /// first step (a zero gain, left by another tool or a fresh power-up,
    /// would make every step silently do nothing).
    fn set_gain(&mut self, value: i32) -> Result<(), DeviceError>;
    /// Upload `effect` (`EVIOCSFF`), returning the kernel-assigned id.
    fn upload(&mut self, effect: &FfEffect) -> Result<i16, DeviceError>;
    /// Start (`value != 0`) or stop (`value == 0`) an uploaded effect.
    fn play(&mut self, id: i16, value: i32) -> Result<(), DeviceError>;
    /// Erase a previously uploaded effect (`EVIOCRMFF`). Best-effort by
    /// design: [`run_sequence`] calls this as unconditional cleanup after
    /// every step, including ones that already failed, so there is
    /// nothing further to recover from an erase error.
    fn erase(&mut self, id: i16);
    /// The `EV_FF` capability bitmap (`EVIOCGBIT(EV_FF, ...)`, at least
    /// [`FF_BITS_LEN`] bytes), used to skip steps the device does not
    /// advertise.
    fn ff_bits(&mut self) -> [u8; FF_BITS_LEN];
}

/// How a run of [`run_sequence`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SequenceEnd {
    /// Every runnable step played out its full duration.
    Completed,
    /// `cancel` flipped before or during a step; already-uploaded effects
    /// were stopped and erased first.
    Cancelled,
    /// The device disappeared mid-run (`ENODEV`); ended quietly, no error.
    DeviceGone,
    /// A device operation failed for some other reason.
    Failed(String),
}

/// The outcome of one whole sequence run: how it ended, how many steps
/// actually played (fully or partially, if cancelled mid-step), and which
/// steps were skipped for lacking device support.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceOutcome {
    pub end: SequenceEnd,
    pub ran: usize,
    pub skipped: Vec<&'static str>,
}

impl SequenceOutcome {
    /// A one-line human summary a front-end can show as the final status
    /// (it already showed [`step_status_text`] for each step along the
    /// way; this is just how the run ended).
    pub fn summary(&self) -> String {
        let base = match &self.end {
            SequenceEnd::Completed => "finished".to_string(),
            SequenceEnd::Cancelled => "stopped".to_string(),
            SequenceEnd::DeviceGone => "wheel disconnected, stopped".to_string(),
            SequenceEnd::Failed(msg) => format!("error: {msg}"),
        };
        if self.skipped.is_empty() {
            base
        } else {
            format!("{base} (not supported by this wheel, skipped: {})", self.skipped.join(", "))
        }
    }
}

/// What [`run_sequence`] reports back through `on_event`, in the order
/// they can happen: the skip list once up front (only if non-empty, and
/// always before the first `Countdown`, so a rendered plan can mark those
/// rows before anything else happens), then a `Countdown`/`Step`/`Done`
/// triple per runnable step. `row` is always 0-based into the `steps`
/// slice `run_sequence` was given, and `total` is always `steps.len()` -
/// both stay stable across skips, so a front-end's rendered plan (one row
/// per table entry, in table order) never needs to renumber anything.
#[derive(Debug, Clone, Copy)]
pub enum SequenceEvent<'a> {
    /// The full skip list, determined once up front from the device's
    /// `EVIOCGBIT` capability query: each unsupported step's row and
    /// label.
    Skipped(&'a [(usize, &'static str)]),
    /// Row `row` is counting down before it plays; `seconds_left` ticks
    /// from the countdown's tick count down to 1.
    Countdown { row: usize, total: usize, step: &'a SimStep, seconds_left: u64 },
    /// Row `row` is now playing.
    Step { row: usize, total: usize, step: &'a SimStep },
    /// Row `row` finished: it played out its full duration, or was
    /// stopped early by a cancel that arrived mid-play - either way it
    /// was cleanly stopped and erased first, so its row is done either
    /// way.
    Done { row: usize, total: usize },
}

/// Run `steps` against `device`, resolving each step's logical [`Side`]
/// against `model` (see [`resolve_direction`] - this is the one place the
/// DD/G923 direction-sign divergence actually matters to playback).
///
/// The device's [`FfDevice::ff_bits`] capability query determines the
/// full skip list up front (reported once via [`SequenceEvent::Skipped`]
/// before anything else); every other row then gets `countdown`'s
/// lead-in (see [`Countdown`]), plays, and is stopped and erased - always
/// all three, on every path, before the next row's countdown ever starts,
/// so no effect slot leaks.
///
/// Cancellable at any point: before a row's countdown starts, during a
/// countdown tick, or during a row's play wait - in every case the
/// sequence stops right there (with whatever was already uploaded
/// cleanly stopped and erased) rather than continuing to the next row.
pub fn run_sequence(
    device: &mut impl FfDevice,
    steps: &[SimStep],
    model: WheelModel,
    cancel: &AtomicBool,
    countdown: Countdown,
    mut on_event: impl FnMut(SequenceEvent),
) -> SequenceOutcome {
    let bits = device.ff_bits();
    let total = steps.len();
    let mut skip_mask = vec![false; total];
    let mut skipped_rows: Vec<(usize, &'static str)> = Vec::new();
    let mut skipped_labels: Vec<&'static str> = Vec::new();
    for (row, step) in steps.iter().enumerate() {
        if !ff_type_supported(&bits, step.effect.ff_type()) {
            skip_mask[row] = true;
            skipped_rows.push((row, step.label));
            skipped_labels.push(step.label);
        }
    }
    if !skipped_rows.is_empty() {
        on_event(SequenceEvent::Skipped(&skipped_rows));
    }

    if skipped_labels.len() == total {
        return SequenceOutcome { end: SequenceEnd::Completed, ran: 0, skipped: skipped_labels };
    }

    if let Err(e) = device.set_gain(0xFFFF) {
        return SequenceOutcome { end: e.into_end(), ran: 0, skipped: skipped_labels };
    }

    let mut ran = 0;
    for (row, step) in steps.iter().enumerate() {
        if skip_mask[row] {
            continue;
        }
        if cancel.load(Ordering::Relaxed) {
            return SequenceOutcome { end: SequenceEnd::Cancelled, ran, skipped: skipped_labels };
        }

        for seconds_left in (1..=countdown.ticks).rev() {
            on_event(SequenceEvent::Countdown { row, total, step, seconds_left });
            if wait_out(countdown.tick, cancel) == WaitOutcome::Cancelled {
                return SequenceOutcome { end: SequenceEnd::Cancelled, ran, skipped: skipped_labels };
            }
        }

        on_event(SequenceEvent::Step { row, total, step });

        let effect = build_ff_effect(step, model);
        let id = match device.upload(&effect) {
            Ok(id) => id,
            Err(e) => return SequenceOutcome { end: e.into_end(), ran, skipped: skipped_labels },
        };

        let played = device.play(id, 1);
        let wait_outcome = if played.is_ok() {
            wait_out(Duration::from_millis(u64::from(step.duration_ms)), cancel)
        } else {
            WaitOutcome::Completed
        };

        // Unconditional cleanup for this step: stop, then erase, whatever
        // happened above (full duration, cancel, or a failed play write).
        let _ = device.play(id, 0);
        device.erase(id);

        if let Err(e) = played {
            return SequenceOutcome { end: e.into_end(), ran, skipped: skipped_labels };
        }

        ran += 1;
        on_event(SequenceEvent::Done { row, total });

        if wait_outcome == WaitOutcome::Cancelled {
            return SequenceOutcome { end: SequenceEnd::Cancelled, ran, skipped: skipped_labels };
        }
    }

    SequenceOutcome { end: SequenceEnd::Completed, ran, skipped: skipped_labels }
}

/// How a playback (or gap) wait ended: it ran out `duration`, or `cancel`
/// flipped first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitOutcome {
    Completed,
    Cancelled,
}

/// Sleep out `duration` in [`SIM_CANCEL_POLL`] ticks, returning early as
/// soon as `cancel` flips (including if it is already set on entry, which
/// returns immediately with no sleep at all - what makes cancellation
/// tests fast).
fn wait_out(duration: Duration, cancel: &AtomicBool) -> WaitOutcome {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        if cancel.load(Ordering::Relaxed) {
            return WaitOutcome::Cancelled;
        }
        std::thread::sleep(SIM_CANCEL_POLL.min(duration));
    }
    WaitOutcome::Completed
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // ff_effect layout / capability bits.
    // -----------------------------------------------------------------

    #[test]
    fn ff_effect_layout_matches_kernel_abi() {
        assert_eq!(std::mem::size_of::<FfEffect>(), 48);
        assert_eq!(std::mem::align_of::<FfEffect>(), 8);
        let e = build_ff_effect(&FORCE_SEQUENCE[0], WheelModel::Rs50);
        let union_offset = (&e.u as *const _ as usize) - (&e as *const _ as usize);
        assert_eq!(union_offset, 16);
    }

    #[test]
    fn ff_type_supported_reads_the_bitmap() {
        let mut bits = [0u8; FF_BITS_LEN];
        bits[FF_CONSTANT as usize / 8] |= 1 << (FF_CONSTANT as usize % 8);
        assert!(ff_type_supported(&bits, FF_CONSTANT));
        assert!(!ff_type_supported(&bits, FF_RAMP));
        // A too-short slice never panics, just reports unsupported.
        assert!(!ff_type_supported(&[], FF_CONSTANT));
    }

    // -----------------------------------------------------------------
    // Step tables: exactly what we intend to upload.
    // -----------------------------------------------------------------

    #[test]
    fn force_sequence_has_the_ten_specified_steps_in_order() {
        assert_eq!(FORCE_SEQUENCE.len(), 10);
        let types: Vec<u16> = FORCE_SEQUENCE.iter().map(|s| s.effect.ff_type()).collect();
        assert_eq!(
            types,
            vec![
                FF_CONSTANT, FF_CONSTANT, FF_CONSTANT, FF_CONSTANT, FF_CONSTANT, FF_CONSTANT,
                FF_RAMP, FF_SPRING, FF_DAMPER, FF_PERIODIC,
            ]
        );
        // Nominal effect playback alone (excluding every step's
        // STEP_COUNTDOWN lead-in) stays in the task's original
        // ~10-11s "thorough" budget for the steps themselves.
        let total_ms: u32 = FORCE_SEQUENCE.iter().map(|s| u32::from(s.duration_ms)).sum();
        assert!((10_000..=11_000).contains(&total_ms), "total_ms={total_ms}");
    }

    #[test]
    fn force_sequence_opens_with_six_short_alternating_pulses_of_equal_moderate_level() {
        // Replaces the old two long (1.2s) one-directional constant
        // steps, which walked the wheel to the end stop and parked it
        // there instead of demonstrating anything - see FORCE_SEQUENCE's
        // doc comment. Each pulse is short (0.45s) and the side
        // alternates every pulse, so the wheel rocks around its starting
        // position instead of pinning to a lock.
        let pulses = &FORCE_SEQUENCE[0..6];
        for (i, step) in pulses.iter().enumerate() {
            let (expected_side, expected_word) =
                if i % 2 == 0 { (Side::Left, "left") } else { (Side::Right, "right") };
            assert_eq!(step.direction, expected_side, "pulse {i} side");
            assert!(step.label.contains(expected_word), "pulse {i} label: {}", step.label);
            assert_eq!(step.duration_ms, 450, "pulse {i} duration");
            let StepEffect::Constant { level } = step.effect else {
                panic!("expected a Constant pulse")
            };
            assert_eq!(level, SIM_LEVEL_30, "pulse {i} level");
        }
    }

    #[test]
    fn force_sequence_ramp_rises_from_zero() {
        let StepEffect::Ramp { start, end } = FORCE_SEQUENCE[6].effect else {
            panic!("expected a Ramp step");
        };
        assert_eq!(start, 0);
        assert!(end > 0);
    }

    #[test]
    fn force_sequence_spring_and_damper_use_a_true_restoring_pair() {
        for step in [&FORCE_SEQUENCE[7], &FORCE_SEQUENCE[8]] {
            assert_eq!(step.direction, Side::None, "condition effects ignore direction");
            let (right_coeff, left_coeff, right_sat, left_sat) = match step.effect {
                StepEffect::Spring { right_coeff, left_coeff, right_sat, left_sat }
                | StepEffect::Damper { right_coeff, left_coeff, right_sat, left_sat } => {
                    (right_coeff, left_coeff, right_sat, left_sat)
                }
                _ => panic!("expected a condition step"),
            };
            // Both coefficients must be positive and equal: a negative
            // pair would build an anti-spring/anti-damper on one side
            // instead of a symmetric centering force (see
            // hidpp_dd_condition_force's sign convention in the module
            // doc).
            assert!(right_coeff > 0);
            assert!(left_coeff > 0);
            assert_eq!(right_coeff, left_coeff);
            assert!(right_sat > 0);
            assert_eq!(right_sat, left_sat);
        }
    }

    #[test]
    fn force_sequence_sine_step_is_periodic_sine_with_moderate_magnitude() {
        let StepEffect::Periodic { waveform, magnitude, .. } = FORCE_SEQUENCE[9].effect else {
            panic!("expected a Periodic step");
        };
        assert_eq!(waveform, FF_SINE);
        assert!(magnitude > 0 && magnitude < i16::MAX / 2, "moderate, not full-scale");
    }

    #[test]
    fn resolve_direction_is_swapped_on_the_g923_but_not_on_dd_wheels() {
        // DD wheels (RS50, G PRO, and Unknown - treated as DD-shaped, same
        // as everywhere else WheelModel gates DD vs. classic behavior)
        // keep today's values: hidpp_dd_project_constant's documented
        // convention, direction=0x4000 is rightward.
        for model in [WheelModel::Rs50, WheelModel::GPro, WheelModel::Unknown] {
            assert_eq!(resolve_direction(Side::Right, model), 0x4000, "{model:?} right");
            assert_eq!(resolve_direction(Side::Left, model), 0xC000, "{model:?} left");
        }
        // The G923's classic (lg4ff-style) engine does the opposite in
        // practice (hardware-verified 2026-07-27): its "left" pulls with
        // the DD engine's "right" value, and vice versa.
        assert_eq!(resolve_direction(Side::Left, WheelModel::G923), 0x4000);
        assert_eq!(resolve_direction(Side::Right, WheelModel::G923), 0xC000);
        // Condition effects ignore direction on every model.
        for model in [WheelModel::Rs50, WheelModel::GPro, WheelModel::Unknown, WheelModel::G923] {
            assert_eq!(resolve_direction(Side::None, model), 0, "{model:?} condition effect");
        }
    }

    #[test]
    fn build_ff_effect_resolves_the_force_sequences_pulse_directions_per_model() {
        // The task's exact acceptance check: on a G923 the "left" step
        // must carry 0x4000 and "right" must carry 0xC000; on an RS50
        // (the primary DD device) it is the reverse - today's values,
        // unchanged.
        let left = &FORCE_SEQUENCE[0];
        let right = &FORCE_SEQUENCE[1];
        assert!(left.label.contains("left"));
        assert!(right.label.contains("right"));

        assert_eq!(build_ff_effect(left, WheelModel::Rs50).direction, 0xC000, "Rs50 left: today's value");
        assert_eq!(build_ff_effect(right, WheelModel::Rs50).direction, 0x4000, "Rs50 right: today's value");
        assert_eq!(build_ff_effect(left, WheelModel::G923).direction, 0x4000, "G923 left: swapped");
        assert_eq!(build_ff_effect(right, WheelModel::G923).direction, 0xC000, "G923 right: swapped");
    }

    #[test]
    fn texture_sequence_progresses_through_rising_frequencies_at_one_amplitude() {
        assert!((3..=4).contains(&TEXTURE_SEQUENCE.len()));
        let mut last_period = u16::MAX;
        for step in TEXTURE_SEQUENCE {
            let StepEffect::Periodic { waveform, period_ms, magnitude } = step.effect else {
                panic!("expected every texture step to be Periodic");
            };
            assert_eq!(waveform, FF_SINE);
            assert_eq!(magnitude, SIM_LEVEL_30, "one moderate amplitude throughout");
            assert!(period_ms < last_period, "each step's frequency must rise (period must fall)");
            last_period = period_ms;
        }
        let total_ms: u32 = TEXTURE_SEQUENCE.iter().map(|s| u32::from(s.duration_ms)).sum();
        assert!((6_000..=10_000).contains(&total_ms), "total_ms={total_ms}");
    }

    #[test]
    fn build_ff_effect_matches_the_step_it_was_built_from() {
        let e = build_ff_effect(&FORCE_SEQUENCE[1], WheelModel::Rs50);
        assert_eq!(e.type_, FF_CONSTANT);
        assert_eq!(e.id, -1, "fresh upload, kernel assigns the id");
        assert_eq!(e.direction, 0x4000, "Rs50 right pulse: today's convention");
        assert_eq!(e.replay_length, FORCE_SEQUENCE[1].duration_ms);
        assert_eq!(i16::from_le_bytes([e.u.0[0], e.u.0[1]]), SIM_LEVEL_30);

        let spring = build_ff_effect(&FORCE_SEQUENCE[7], WheelModel::Rs50);
        assert_eq!(spring.type_, FF_SPRING);
        assert_eq!(u16::from_le_bytes([spring.u.0[0], spring.u.0[1]]), SIM_SATURATION_30);
        assert_eq!(i16::from_le_bytes([spring.u.0[4], spring.u.0[5]]), SIM_LEVEL_30, "right_coeff");
        assert_eq!(i16::from_le_bytes([spring.u.0[6], spring.u.0[7]]), SIM_LEVEL_30, "left_coeff");
    }

    #[test]
    fn sim_kind_maps_to_the_matching_table() {
        assert_eq!(SimKind::Force.steps(), FORCE_SEQUENCE);
        assert_eq!(SimKind::Texture.steps(), TEXTURE_SEQUENCE);
    }

    // -----------------------------------------------------------------
    // The runner, against a mock FfDevice.
    // -----------------------------------------------------------------

    /// A device double that never touches a real fd: counts uploads and
    /// erases (to catch a leaked effect slot), records every play (id,
    /// value) call, and can be told which `ff_effect.type`s it supports
    /// and to "disappear" (ENODEV) after a given number of uploads.
    struct MockDevice {
        supported: Vec<u16>,
        uploads: usize,
        erases: usize,
        plays: Vec<(i16, i32)>,
        next_id: i16,
        gone_after_uploads: Option<usize>,
        /// If set, `erase` flips this once its call count reaches the
        /// given number - used to exercise the between-steps cancel check
        /// (the top-of-loop check ahead of the next row's countdown)
        /// without any real sleeping.
        cancel_after_erases: Option<(usize, std::sync::Arc<AtomicBool>)>,
    }

    impl MockDevice {
        fn supporting(types: &[u16]) -> Self {
            MockDevice {
                supported: types.to_vec(),
                uploads: 0,
                erases: 0,
                plays: Vec::new(),
                next_id: 0,
                gone_after_uploads: None,
                cancel_after_erases: None,
            }
        }
    }

    impl FfDevice for MockDevice {
        fn set_gain(&mut self, _value: i32) -> Result<(), DeviceError> {
            Ok(())
        }

        fn upload(&mut self, effect: &FfEffect) -> Result<i16, DeviceError> {
            self.uploads += 1;
            if self.gone_after_uploads == Some(self.uploads) {
                return Err(DeviceError::Gone);
            }
            let id = self.next_id;
            self.next_id += 1;
            let _ = effect;
            Ok(id)
        }

        fn play(&mut self, id: i16, value: i32) -> Result<(), DeviceError> {
            self.plays.push((id, value));
            Ok(())
        }

        fn erase(&mut self, _id: i16) {
            self.erases += 1;
            if let Some((n, cancel)) = &self.cancel_after_erases {
                if self.erases == *n {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        }

        fn ff_bits(&mut self) -> [u8; FF_BITS_LEN] {
            let mut bits = [0u8; FF_BITS_LEN];
            for t in &self.supported {
                bits[usize::from(*t) / 8] |= 1 << (usize::from(*t) % 8);
            }
            bits
        }
    }

    /// Tiny local steps (duration/gap 0) so runner tests exercise the
    /// exact same code paths as the real sequences without sleeping out
    /// their real durations.
    const QUICK_STEPS: &[SimStep] = &[
        SimStep { label: "one", effect: StepEffect::Constant { level: 100 }, duration_ms: 0, direction: Side::Right },
        SimStep { label: "two", effect: StepEffect::Ramp { start: 0, end: 100 }, duration_ms: 0, direction: Side::Right },
        SimStep { label: "three", effect: StepEffect::Periodic { waveform: FF_SINE, period_ms: 10, magnitude: 100 }, duration_ms: 0, direction: Side::Right },
    ];

    /// The model every runner test plays against; the runner's own model-
    /// resolution logic is covered separately (`resolve_direction_is_
    /// swapped_on_the_g923_but_not_on_dd_wheels`,
    /// `build_ff_effect_resolves_the_force_sequences_pulse_directions_per_
    /// model`), so these tests just need any fixed model to exercise the
    /// upload/play/stop/erase machinery.
    const RUNNER_TEST_MODEL: WheelModel = WheelModel::Rs50;

    #[test]
    fn run_sequence_completes_every_step_with_no_leaked_effect_slot() {
        let mut device = MockDevice::supporting(&[FF_CONSTANT, FF_RAMP, FF_PERIODIC]);
        let cancel = AtomicBool::new(false);
        let mut seen = Vec::new();
        let outcome = run_sequence(&mut device, QUICK_STEPS, RUNNER_TEST_MODEL, &cancel, Countdown::NONE, |ev| {
            if let SequenceEvent::Step { row, total, step } = ev {
                seen.push((row, total, step.label));
            }
        });
        assert_eq!(outcome.end, SequenceEnd::Completed);
        assert_eq!(outcome.ran, 3);
        assert!(outcome.skipped.is_empty());
        assert_eq!(seen, vec![(0, 3, "one"), (1, 3, "two"), (2, 3, "three")]);
        assert_eq!(device.uploads, 3);
        assert_eq!(device.erases, 3, "every upload must be erased - no leaked slot");
        // Each step plays (id, 1) then stops (id, 0), in order.
        assert_eq!(device.plays, vec![(0, 1), (0, 0), (1, 1), (1, 0), (2, 1), (2, 0)]);
    }

    #[test]
    fn run_sequence_skips_steps_the_device_does_not_advertise() {
        // Only FF_CONSTANT is supported: the Ramp and Periodic steps must
        // be skipped, not attempted (and never leaked, since they are
        // never uploaded at all). Rows stay 0-based into the full table,
        // so "two" is row 1 and "three" is row 2 even though row 0 ("one")
        // is the only one that actually runs.
        let mut device = MockDevice::supporting(&[FF_CONSTANT]);
        let cancel = AtomicBool::new(false);
        let mut skip_report = None;
        let outcome = run_sequence(&mut device, QUICK_STEPS, RUNNER_TEST_MODEL, &cancel, Countdown::NONE, |ev| {
            if let SequenceEvent::Skipped(rows) = ev {
                skip_report = Some(rows.to_vec());
            }
        });
        assert_eq!(outcome.end, SequenceEnd::Completed);
        assert_eq!(outcome.ran, 1);
        assert_eq!(outcome.skipped, vec!["two", "three"]);
        assert_eq!(skip_report, Some(vec![(1, "two"), (2, "three")]));
        assert_eq!(device.uploads, 1);
        assert_eq!(device.erases, 1);
        assert!(outcome.summary().contains("not supported"));
    }

    #[test]
    fn run_sequence_skipping_every_step_runs_nothing_and_still_completes() {
        let mut device = MockDevice::supporting(&[]);
        let cancel = AtomicBool::new(false);
        let outcome = run_sequence(&mut device, QUICK_STEPS, RUNNER_TEST_MODEL, &cancel, Countdown::NONE, |_| {});
        assert_eq!(outcome.end, SequenceEnd::Completed);
        assert_eq!(outcome.ran, 0);
        assert_eq!(outcome.skipped.len(), 3);
        assert_eq!(device.uploads, 0);
        assert_eq!(device.erases, 0);
    }

    #[test]
    fn run_sequence_cancelled_before_it_starts_runs_nothing() {
        let mut device = MockDevice::supporting(&[FF_CONSTANT, FF_RAMP, FF_PERIODIC]);
        let cancel = AtomicBool::new(true);
        let outcome = run_sequence(&mut device, QUICK_STEPS, RUNNER_TEST_MODEL, &cancel, Countdown::NONE, |_| {});
        assert_eq!(outcome.end, SequenceEnd::Cancelled);
        assert_eq!(outcome.ran, 0);
        assert_eq!(device.uploads, 0);
    }

    #[test]
    fn run_sequence_cancelled_mid_step_stops_and_erases_then_ends() {
        // Flip `cancel` as soon as the second step's upload is about to
        // start (its Step event); the runner must still finish that
        // step's own upload/play/stop/erase cleanly (it already
        // committed to it) but never reach the third step.
        let mut device = MockDevice::supporting(&[FF_CONSTANT, FF_RAMP, FF_PERIODIC]);
        let cancel = AtomicBool::new(false);
        let mut seen = Vec::new();
        let outcome = run_sequence(&mut device, QUICK_STEPS, RUNNER_TEST_MODEL, &cancel, Countdown::NONE, |ev| {
            if let SequenceEvent::Step { row, step, .. } = ev {
                seen.push(step.label);
                if row == 1 {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        });
        assert_eq!(outcome.end, SequenceEnd::Cancelled);
        assert_eq!(outcome.ran, 2, "the interrupted step counts as ran: it was stopped and erased cleanly");
        assert_eq!(seen, vec!["one", "two"], "the third step's Step event never fires");
        assert_eq!(device.uploads, 2);
        assert_eq!(device.erases, 2, "no leaked slot on a mid-step cancel");
    }

    #[test]
    fn run_sequence_cancelled_between_steps_ends_before_the_next_one_starts() {
        // No Step event ever sets `cancel`; instead the mock flips it the
        // moment the first step's erase happens, exercising the
        // between-steps cancel check (the top-of-loop check ahead of the
        // next row's countdown) rather than a step's own play-wait or
        // countdown-tick wait.
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let mut device = MockDevice::supporting(&[FF_CONSTANT, FF_RAMP, FF_PERIODIC]);
        device.cancel_after_erases = Some((1, cancel.clone()));
        let mut seen = Vec::new();
        let outcome = run_sequence(&mut device, QUICK_STEPS, RUNNER_TEST_MODEL, &cancel, Countdown::NONE, |ev| {
            if let SequenceEvent::Step { step, .. } = ev {
                seen.push(step.label);
            }
        });
        assert_eq!(outcome.end, SequenceEnd::Cancelled);
        assert_eq!(outcome.ran, 1);
        assert_eq!(seen, vec!["one"], "the second step never starts");
        assert_eq!(device.uploads, 1);
        assert_eq!(device.erases, 1);
    }

    #[test]
    fn run_sequence_ends_quietly_on_device_gone_mid_upload() {
        let mut device = MockDevice::supporting(&[FF_CONSTANT, FF_RAMP, FF_PERIODIC]);
        device.gone_after_uploads = Some(2); // the second step's upload
        let cancel = AtomicBool::new(false);
        let outcome = run_sequence(&mut device, QUICK_STEPS, RUNNER_TEST_MODEL, &cancel, Countdown::NONE, |_| {});
        assert_eq!(outcome.end, SequenceEnd::DeviceGone);
        assert_eq!(outcome.ran, 1, "only the first step ever completed");
        assert_eq!(device.uploads, 2, "the failed upload still counts as attempted");
        assert_eq!(device.erases, 1, "nothing to erase for the upload that never succeeded");
        assert!(!outcome.summary().contains("error"), "ENODEV must not read as a generic error");
    }

    #[test]
    fn run_sequence_surfaces_a_non_enodev_failure() {
        struct FailGain;
        impl FfDevice for FailGain {
            fn set_gain(&mut self, _v: i32) -> Result<(), DeviceError> {
                Err(DeviceError::Other("permission denied".to_string()))
            }
            fn upload(&mut self, _e: &FfEffect) -> Result<i16, DeviceError> {
                unreachable!()
            }
            fn play(&mut self, _id: i16, _v: i32) -> Result<(), DeviceError> {
                unreachable!()
            }
            fn erase(&mut self, _id: i16) {
                unreachable!()
            }
            fn ff_bits(&mut self) -> [u8; FF_BITS_LEN] {
                let mut b = [0u8; FF_BITS_LEN];
                b[FF_CONSTANT as usize / 8] |= 1 << (FF_CONSTANT as usize % 8);
                b
            }
        }
        let mut device = FailGain;
        let cancel = AtomicBool::new(false);
        let outcome =
            run_sequence(&mut device, &QUICK_STEPS[..1], RUNNER_TEST_MODEL, &cancel, Countdown::NONE, |_| {});
        assert_eq!(outcome.end, SequenceEnd::Failed("permission denied".to_string()));
        assert_eq!(outcome.ran, 0);
        assert!(outcome.summary().contains("permission denied"));
    }

    #[test]
    fn step_status_text_names_the_step_and_its_position() {
        let text = step_status_text(1, 10, &FORCE_SEQUENCE[1]);
        assert_eq!(text, "step 2/10: Constant force, right pulse");
    }

    // -----------------------------------------------------------------
    // The per-step countdown: ticks down before every step, including
    // the first, and is cancellable mid-tick just like a step's own
    // play-wait.
    // -----------------------------------------------------------------

    #[test]
    fn run_sequence_counts_down_before_every_step_including_the_first() {
        let mut device = MockDevice::supporting(&[FF_CONSTANT, FF_RAMP, FF_PERIODIC]);
        let cancel = AtomicBool::new(false);
        let countdown = Countdown { ticks: 3, tick: Duration::ZERO };
        let mut events: Vec<String> = Vec::new();
        let outcome = run_sequence(&mut device, &QUICK_STEPS[..1], RUNNER_TEST_MODEL, &cancel, countdown, |ev| {
            match ev {
                SequenceEvent::Countdown { row, seconds_left, .. } => {
                    events.push(format!("countdown row={row} secs={seconds_left}"))
                }
                SequenceEvent::Step { row, .. } => events.push(format!("step row={row}")),
                SequenceEvent::Done { row, .. } => events.push(format!("done row={row}")),
                SequenceEvent::Skipped(_) => {}
            }
        });
        assert_eq!(outcome.end, SequenceEnd::Completed);
        assert_eq!(
            events,
            vec![
                "countdown row=0 secs=3",
                "countdown row=0 secs=2",
                "countdown row=0 secs=1",
                "step row=0",
                "done row=0",
            ],
            "the very first step gets the same lead-in as every other one"
        );
    }

    #[test]
    fn run_sequence_cancelled_mid_countdown_never_plays_that_step() {
        // Cancel as soon as the countdown reaches 2; the tick's own
        // nonzero wait must catch it before the step ever uploads.
        let mut device = MockDevice::supporting(&[FF_CONSTANT, FF_RAMP, FF_PERIODIC]);
        let cancel = AtomicBool::new(false);
        let countdown = Countdown { ticks: 3, tick: Duration::from_millis(20) };
        let mut seen: Vec<u64> = Vec::new();
        let outcome = run_sequence(&mut device, QUICK_STEPS, RUNNER_TEST_MODEL, &cancel, countdown, |ev| {
            if let SequenceEvent::Countdown { seconds_left, .. } = ev {
                seen.push(seconds_left);
                if seconds_left == 2 {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        });
        assert_eq!(outcome.end, SequenceEnd::Cancelled);
        assert_eq!(outcome.ran, 0, "the step never played: it was still counting down");
        assert_eq!(seen, vec![3, 2], "the tick after the cancel never fires");
        assert_eq!(device.uploads, 0, "cancelling mid-countdown must never upload the effect");
    }

    // -----------------------------------------------------------------
    // SequenceProgress: the rendered-plan state machine both front-ends
    // fold every SequenceEvent into.
    // -----------------------------------------------------------------

    #[test]
    fn sequence_progress_starts_pending_and_matches_the_step_table() {
        let progress = SequenceProgress::new(FORCE_SEQUENCE);
        assert_eq!(progress.states.len(), FORCE_SEQUENCE.len(), "one row per step, no more, no less");
        assert!(progress.states.iter().all(|s| *s == StepState::Pending));
        assert_eq!(progress.current, None);
    }

    #[test]
    fn sequence_progress_follows_one_step_through_countdown_play_and_done() {
        let mut progress = SequenceProgress::new(QUICK_STEPS);
        progress.apply(&SequenceEvent::Countdown { row: 0, total: 3, step: &QUICK_STEPS[0], seconds_left: 3 });
        assert_eq!(progress.states[0], StepState::Countdown(3));
        assert_eq!(progress.current, Some(0));
        assert_eq!(progress.states[1], StepState::Pending, "other rows are untouched");

        progress.apply(&SequenceEvent::Countdown { row: 0, total: 3, step: &QUICK_STEPS[0], seconds_left: 1 });
        assert_eq!(progress.states[0], StepState::Countdown(1));

        progress.apply(&SequenceEvent::Step { row: 0, total: 3, step: &QUICK_STEPS[0] });
        assert_eq!(progress.states[0], StepState::Playing);
        assert_eq!(progress.current, Some(0));

        progress.apply(&SequenceEvent::Done { row: 0, total: 3 });
        assert_eq!(progress.states[0], StepState::Done);
        assert_eq!(progress.current, None, "nothing active once the step is done");

        // A finished row's state is never reverted: the whole point is
        // that a completed step stays visible as done, not pending, once
        // the run has moved on (or ended).
        assert_eq!(progress.states[0], StepState::Done);
    }

    #[test]
    fn sequence_progress_marks_skipped_rows_without_touching_others() {
        let mut progress = SequenceProgress::new(QUICK_STEPS);
        progress.apply(&SequenceEvent::Skipped(&[(1, "two"), (2, "three")]));
        assert_eq!(progress.states, vec![StepState::Pending, StepState::Skipped, StepState::Skipped]);
    }

    #[test]
    fn sequence_progress_reflects_a_full_run_including_a_skip() {
        // Fold every event a real run_sequence call against QUICK_STEPS
        // (with only FF_CONSTANT/FF_PERIODIC supported, skipping the
        // Ramp) would report, and check the final rendered plan matches
        // exactly what actually happened: row 0 done, row 1 skipped
        // (never touched otherwise), row 2 done, nothing left active.
        let mut device = MockDevice::supporting(&[FF_CONSTANT, FF_PERIODIC]);
        let cancel = AtomicBool::new(false);
        let mut progress = SequenceProgress::new(QUICK_STEPS);
        let outcome = run_sequence(&mut device, QUICK_STEPS, RUNNER_TEST_MODEL, &cancel, Countdown::NONE, |ev| {
            progress.apply(&ev);
        });
        assert_eq!(outcome.end, SequenceEnd::Completed);
        assert_eq!(progress.states, vec![StepState::Done, StepState::Skipped, StepState::Done]);
        assert_eq!(progress.current, None);
    }

    #[test]
    fn sequence_progress_keeps_a_cancelled_run_s_last_states_visible() {
        // Mirrors run_sequence_cancelled_mid_step_stops_and_erases_then_ends:
        // row 0 finishes, row 1 is interrupted mid-play by its own cancel
        // (still cleanly stopped and erased, so it reads as done), row 2
        // never starts and stays pending - none of that gets cleared away
        // once the run ends, which is the point of the whole feature.
        let mut device = MockDevice::supporting(&[FF_CONSTANT, FF_RAMP, FF_PERIODIC]);
        let cancel = AtomicBool::new(false);
        let mut progress = SequenceProgress::new(QUICK_STEPS);
        let outcome = run_sequence(&mut device, QUICK_STEPS, RUNNER_TEST_MODEL, &cancel, Countdown::NONE, |ev| {
            progress.apply(&ev);
            if let SequenceEvent::Step { row, .. } = ev {
                if row == 1 {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        });
        assert_eq!(outcome.end, SequenceEnd::Cancelled);
        assert_eq!(progress.states, vec![StepState::Done, StepState::Done, StepState::Pending]);
    }

    #[test]
    fn step_state_status_text_never_relies_on_color_alone() {
        // Every state renders as a distinct word (or a ticking number),
        // never just a color swatch - the user is colorblind, so text is
        // the primary signal, color only ever a secondary one.
        assert_eq!(StepState::Pending.status_text(), "pending");
        assert_eq!(StepState::Countdown(3).status_text(), "3...");
        assert_eq!(StepState::Playing.status_text(), "playing");
        assert_eq!(StepState::Done.status_text(), "done");
        assert_eq!(StepState::Skipped.status_text(), "skipped");
    }
}
