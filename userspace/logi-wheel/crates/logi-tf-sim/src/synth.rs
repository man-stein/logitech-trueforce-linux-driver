// SPDX-License-Identifier: GPL-2.0-only
//! Engine-note synthesis.
//!
//! Generates the 1 kHz sample stream the wheel's TrueForce DSP consumes:
//! a fundamental at the engine's FIRING rate plus 2x and 3x harmonics at falling
//! gain, amplitude `idle_floor + throttle * gain`, everything scaled by
//! the effective intensity (master x per-game, 0.0..1.0). The harmonic
//! gains (1, 1/2, 1/4) factor so the summed waveform crosses zero exactly
//! twice per fundamental cycle, which keeps the felt pitch equal to the
//! engine rate and makes the spectral test below exact.
//!
//! The generator is pure and stateful only in its oscillator phase, so
//! frequency changes are click-free. The libtrueforce stream thread does
//! the packetizing (4 samples per 250 Hz packet); this module only has to
//! produce samples at [`SAMPLE_RATE_HZ`].

/// The wheel's TrueForce sample rate.
pub const SAMPLE_RATE_HZ: f32 = 1000.0;

/// Samples per wire packet (the wheel consumes 4-sample packets at
/// 250 Hz); pushes are conveniently sized in multiples of this.
pub const SAMPLES_PER_PACKET: usize = 4;

/// Relative gains for the fundamental and the 2x / 3x harmonics.
const HARMONIC_GAINS: [f32; 3] = [1.0, 0.5, 0.25];
/// Sum of [`HARMONIC_GAINS`]; normalizes the mix so |sample| <= amplitude.
const GAIN_NORM: f32 = 1.75;

/// Cylinder count assumed when a game or car tells us nothing. A modern
/// four is the commonest thing anyone drives, and it is the value the old
/// hardcoded behaviour was closest to.
pub const DEFAULT_CYLINDERS: u8 = 4;

/// The firing frequency of a four-stroke engine, in Hz.
///
/// One full cycle is 720 degrees of crank, so every cylinder fires once
/// per two revolutions: `rpm / 60 * cylinders / 2`.
///
/// This used to be plain `rpm / 60`, the crank rotation rate, which is the
/// firing rate of a single-cylinder two-stroke and of nothing else anyone
/// drives. Every other engine was an octave or more flat: a four was out by
/// 2x, a V8 by 4x. The `pitch` setting existed to let people correct that by
/// ear without knowing what they were correcting, and it could not stretch
/// far enough to do it: clamped at 2.0, it could not reach a V8's firing
/// rate even at maximum. Named by TF4ALL's FiringPatterns notes, which state
/// the relationship plainly (GPL-2.0, same licence as this crate).
///
/// `pitch_scale` stays, but as what it always claimed to be: a preference
/// either side of the correct value, not a correction for a missing term.
pub fn firing_frequency(rpm: f32, cylinders: u8, pitch_scale: f32) -> f32 {
    let cyl = cylinders.max(1) as f32;
    (rpm.max(0.0) / 60.0 * (cyl / 2.0) * pitch_scale.clamp(0.1, 2.0)).min(SAMPLE_RATE_HZ * 0.45)
}

/// Amplitude at closed throttle (the engine is still running).
pub const IDLE_FLOOR: f32 = 0.15;
/// Additional amplitude at full throttle; floor + gain = 1.0 full scale.
pub const THROTTLE_GAIN: f32 = 0.85;

/// Everything about the engine that shapes one block of note.
///
/// Grouped rather than passed loose: these four always travel together, and
/// the argument list had grown past the point where a reader could tell
/// which float was which.
#[derive(Debug, Clone, Copy)]
pub struct EngineNote {
    /// Engine speed, revolutions per minute.
    pub rpm: f32,
    /// Throttle position 0..1; sets amplitude above [`IDLE_FLOOR`].
    pub throttle: f32,
    /// Cylinders, with `rpm` the other half of the firing rate.
    pub cylinders: u8,
    /// Taste, either side of the true firing rate. 1.0 is correct.
    pub pitch_scale: f32,
}

impl Default for EngineNote {
    fn default() -> Self {
        EngineNote { rpm: 0.0, throttle: 0.0, cylinders: DEFAULT_CYLINDERS, pitch_scale: 1.0 }
    }
}

/// Phase-continuous engine-note generator.
#[derive(Debug, Default)]
pub struct EngineSynth {
    /// Fundamental phase in cycles, kept in [0, 1).
    phase: f32,
}

impl EngineSynth {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `count` samples for the given engine state to `out`.
    ///
    /// `rpm` sets the fundamental (`rpm / 60` Hz, capped below Nyquist),
    /// `throttle` (0..1) sets the amplitude above [`IDLE_FLOOR`], and
    /// `intensity` (0..1) scales the result. Intensity 0 emits exact
    /// silence. Out-of-range inputs are clamped.
    /// `cylinders` sets the firing rate together with `rpm`; see
    /// [`firing_frequency`]. `pitch_scale` (0.1..2.0) then shifts that by
    /// taste, 1.0 being the engine's true firing rate. Tunable via the
    /// config's `cylinders` and `pitch` keys.
    pub fn generate(&mut self, note: &EngineNote, intensity: f32, count: usize, out: &mut Vec<f32>) {
        let intensity = intensity.clamp(0.0, 1.0);
        let throttle = note.throttle.clamp(0.0, 1.0);
        let freq = firing_frequency(note.rpm, note.cylinders, note.pitch_scale);
        let amplitude = (IDLE_FLOOR + THROTTLE_GAIN * throttle) * intensity;
        let step = freq / SAMPLE_RATE_HZ;

        out.reserve(count);
        for _ in 0..count {
            let sample = if amplitude > 0.0 && freq > 0.0 {
                let mut acc = 0.0f32;
                for (k, gain) in HARMONIC_GAINS.iter().enumerate() {
                    let harmonic = (k + 1) as f32;
                    acc += gain * (std::f32::consts::TAU * harmonic * self.phase).sin();
                }
                acc / GAIN_NORM * amplitude
            } else {
                0.0
            };
            out.push(sample);
            self.phase += step;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(rpm: f32, throttle: f32, intensity: f32, count: usize) -> Vec<f32> {
        let mut synth = EngineSynth::new();
        let mut out = Vec::new();
        synth.generate(&EngineNote { rpm, throttle, cylinders: DEFAULT_CYLINDERS, pitch_scale: 1.0 }, intensity, count, &mut out);
        out
    }

    /// Sign changes between consecutive samples; two per fundamental cycle
    /// thanks to the 1 / 0.5 / 0.25 harmonic-gain factorization.
    fn zero_crossings(buf: &[f32]) -> usize {
        buf.windows(2).filter(|w| (w[0] > 0.0) != (w[1] > 0.0) && w[1] != 0.0).count()
    }

    /// A second of engine note at `rpm` for a given cylinder count.
    fn buffer_cyl(rpm: f32, cylinders: u8, count: usize) -> Vec<f32> {
        let mut synth = EngineSynth::new();
        let mut out = Vec::new();
        synth.generate(&EngineNote { rpm, cylinders, throttle: 1.0, pitch_scale: 1.0 }, 1.0, count, &mut out);
        out
    }

    fn peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    #[test]
    fn fundamental_tracks_the_firing_rate() {
        // Counts doubled when the cylinder term was added, which is the
        // whole point: at 3000 rpm a four fires at 100 Hz, not the 50 Hz
        // crank rate this test used to assert.
        // 3000 rpm, 4 cyl -> 100 Hz -> ~200 crossings over 1000 samples.
        let crossings = zero_crossings(&buffer(3000.0, 1.0, 1.0, 1000));
        assert!((190..=210).contains(&crossings), "3000 rpm: {crossings} crossings");
        // 6000 rpm -> 200 Hz -> ~400 crossings.
        let crossings = zero_crossings(&buffer(6000.0, 1.0, 1.0, 1000));
        assert!((390..=410).contains(&crossings), "6000 rpm: {crossings} crossings");
    }

    #[test]
    fn a_v8_sounds_an_octave_above_a_four_at_the_same_rpm() {
        let four = zero_crossings(&buffer_cyl(3000.0, 4, 1000));
        let v8 = zero_crossings(&buffer_cyl(3000.0, 8, 1000));
        // Twice the cylinders, twice the firing rate, twice the crossings.
        let ratio = v8 as f32 / four as f32;
        assert!((ratio - 2.0).abs() < 0.1, "four {four}, V8 {v8}, ratio {ratio}");
    }

    #[test]
    fn amplitude_scales_linearly_with_intensity() {
        let full = peak(&buffer(3000.0, 1.0, 1.0, 1000));
        let half = peak(&buffer(3000.0, 1.0, 0.5, 1000));
        assert!(full > 0.5, "full-intensity peak {full}");
        assert!((full / half - 2.0).abs() < 1e-3, "ratio {}", full / half);
    }

    #[test]
    fn amplitude_rises_with_throttle_above_the_idle_floor() {
        let idle = peak(&buffer(3000.0, 0.0, 1.0, 1000));
        let wot = peak(&buffer(3000.0, 1.0, 1.0, 1000));
        assert!(idle > 0.0, "idle floor keeps the engine audible");
        assert!(wot / idle > 4.0, "throttle swing: idle {idle}, wot {wot}");
    }

    #[test]
    fn silence_at_intensity_zero() {
        assert!(buffer(6000.0, 1.0, 0.0, 500).iter().all(|&s| s == 0.0));
    }

    #[test]
    fn silence_at_zero_rpm() {
        assert!(buffer(0.0, 1.0, 1.0, 500).iter().all(|&s| s == 0.0));
    }

    #[test]
    fn firing_frequency_follows_cylinder_count() {
        // A four-stroke fires every cylinder once per two revolutions, so
        // at 6000 rpm the crank turns at 100 Hz and a four fires at 200.
        assert_eq!(firing_frequency(6000.0, 4, 1.0), 200.0);
        assert_eq!(firing_frequency(6000.0, 8, 1.0), 400.0, "a V8 fires twice as often as a four");
        assert_eq!(firing_frequency(6000.0, 6, 1.0), 300.0);
        // A single-cylinder four-stroke fires once per two revolutions,
        // i.e. at half the crank rate. This is the only engine for which
        // the old rpm/60 model was ever close, and it was out by 2x even
        // there.
        assert_eq!(firing_frequency(6000.0, 1, 1.0), 50.0);
    }

    #[test]
    fn the_default_config_emits_exactly_what_it_did_before_the_fix() {
        // The old model was rpm/60 * pitch with pitch defaulting to 0.5.
        // The new one is rpm/60 * cyl/2 * pitch with cyl 4 and pitch 0.25.
        // These must agree: the maths was corrected without changing what
        // anyone feels, because the old default was chosen by feel on real
        // hardware and correcting the model does not invalidate that.
        for rpm in [800.0f32, 3000.0, 7500.0] {
            let old = rpm / 60.0 * 0.5;
            let new = firing_frequency(rpm, DEFAULT_CYLINDERS, 0.25);
            assert!((old - new).abs() < 1e-3, "rpm {rpm}: old {old} vs new {new}");
        }
    }

    #[test]
    fn pitch_is_now_a_preference_rather_than_a_missing_term() {
        // The old clamp could not reach a V8's firing rate even at maximum:
        // rpm/60 * 2.0 is still half of rpm/60 * 8/2.
        let v8_true = firing_frequency(6000.0, 8, 1.0);
        let old_model_at_max_pitch = 6000.0 / 60.0 * 2.0;
        assert!(v8_true > old_model_at_max_pitch, "a V8 was out of reach of the old pitch range");
        // And pitch still scales either side of correct.
        assert_eq!(firing_frequency(6000.0, 4, 0.5), 100.0);
        assert_eq!(firing_frequency(6000.0, 4, 2.0), 400.0);
    }

    #[test]
    fn firing_frequency_is_bounded_and_guards_nonsense_input() {
        assert_eq!(firing_frequency(-1.0, 4, 1.0), 0.0, "negative rpm reads as stopped");
        assert_eq!(firing_frequency(6000.0, 0, 1.0), firing_frequency(6000.0, 1, 1.0),
                   "zero cylinders is treated as one rather than silencing the engine");
        // Never above Nyquist for the 1 kHz stream.
        assert!(firing_frequency(20000.0, 16, 2.0) <= SAMPLE_RATE_HZ * 0.45);
    }

    #[test]
    fn samples_stay_in_range_and_inputs_are_clamped() {
        let buf = buffer(50_000.0, 7.0, 3.0, 2000);
        assert!(buf.iter().all(|s| s.abs() <= 1.0));
    }

    #[test]
    fn phase_is_continuous_across_calls() {
        let mut synth = EngineSynth::new();
        let mut joined = Vec::new();
        for _ in 0..10 {
            synth.generate(&EngineNote { rpm: 3000.0, throttle: 1.0, cylinders: DEFAULT_CYLINDERS, pitch_scale: 1.0 }, 1.0, 100, &mut joined);
        }
        // Same crossing count as one contiguous second: no phase resets.
        let crossings = zero_crossings(&joined);
        assert!((190..=210).contains(&crossings), "{crossings} crossings");
    }
}
