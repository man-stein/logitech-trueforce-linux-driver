// SPDX-License-Identifier: GPL-2.0-only
//! BeamNG.drive OutGauge UDP telemetry.
//!
//! BeamNG emits the Live for Speed "OutGauge" protocol (Options > Other >
//! Protocols > OutGauge, 127.0.0.1:4444). It is a single fixed, packed,
//! little-endian struct, identical to the LFS / ETS2 OutGauge packet, so
//! the same layout serves any OutGauge source. Fields, from the packet
//! start:
//!
//! | offset | field       | type | unit    |
//! |--------|-------------|------|---------|
//! | 0      | time        | u32  | ms      |
//! | 4      | car[4]      | char | name    |
//! | 8      | flags       | u16  | OG_*    |
//! | 10     | gear        | i8   | R0 N1.. |
//! | 11     | plid        | u8   | id      |
//! | 12     | speed       | f32  | m/s     |
//! | 16     | rpm         | f32  | rpm     |
//! | 20     | turbo       | f32  | bar     |
//! | 24     | engTemp     | f32  | C       |
//! | 28     | fuel        | f32  | 0..1    |
//! | ...    | ...         | ...  | ...     |
//! | 44     | showLights  | u32  | DL_*    |
//! | 48     | throttle    | f32  | 0..1    |
//! | 52     | brake       | f32  | 0..1    |
//! | 56     | clutch      | f32  | 0..1    |
//!
//! The struct is 92 bytes, or 96 with the optional trailing OutGauge ID
//! (`int id`) when the game is configured with an ID string; both lengths
//! are accepted. OutGauge carries no redline, so `max_rpm` is the running
//! maximum RPM seen this session (the [`Decoder`] holds that state, reset
//! when the daemon tears the stream down). Sources: LFS OutGauge spec and
//! the BeamNG protocols documentation.

use crate::telemetry::Telemetry;

/// Game id for BeamNG.drive.
pub const ID: &str = "beamng";
/// Default OutGauge listen port (the BeamNG / LFS default).
pub const DEFAULT_PORT: u16 = 4444;

/// OutGauge without / with the optional trailing ID field.
const LEN_NO_ID: usize = 92;
const LEN_WITH_ID: usize = 96;

const OFF_GEAR: usize = 10;
const OFF_SPEED: usize = 12;
const OFF_RPM: usize = 16;
const OFF_THROTTLE: usize = 48;
const OFF_BRAKE: usize = 52;
const OFF_CLUTCH: usize = 56;
/// OutGauge `showLights`: the dashboard lamps the game says are lit, as a
/// DL_* bitfield.
const OFF_SHOW_LIGHTS: usize = 44;
/// `DL_PITSPEED` in the OutGauge DL_* set.
///
/// The bit position is from the LFS OutGauge specification that BeamNG
/// implements. Everything downstream of this constant is hardware-verified:
/// on 2026-07-29 a synthetic OutGauge packet with this bit set drove a real
/// RS50's rev strip through the full flash (see [`crate::leds`]).
///
/// What remains UNVERIFIED is BeamNG itself: nobody here has the game, so
/// whether it ever lights this particular lamp is unknown, and its cars
/// mostly have no pit limiter. A source that never sets the bit is
/// indistinguishable from a car that has none, so if BeamNG never sets it
/// this reports no limiter rather than a wrong one. The failure is silence.
const DL_PITSPEED: u32 = 0x0000_0008;

/// Traction-control and ABS lamps in the same DL_* set.
///
/// These carry the caveat the whole bitfield carries (see [`DL_PITSPEED`]):
/// the bit positions come from the LFS specification, and what a given
/// source does with them is its own business. There is a second caveat
/// specific to these two. A dashboard lamp for ABS or TC can mean either
/// "the system is intervening right now" or "the system has faulted", and
/// the wire does not say which. LFS and the sims that copy it flash the
/// lamp on intervention, which is the reading taken here.
///
/// Being wrong about that is survivable: the effects these feed are short
/// haptic ticks, so a fault lamp would produce a buzz rather than anything
/// dangerous. It is recorded here so that the assumption is visible if
/// someone later reports exactly that buzz.
const DL_TC: u32 = 0x0000_0010;
const DL_ABS: u32 = 0x0000_0400;

/// Reject engine rates above this as not a real OutGauge sample.
const RPM_CEILING: f32 = 30_000.0;

fn f32_at(pkt: &[u8], off: usize) -> Option<f32> {
    Some(f32::from_le_bytes(pkt.get(off..off + 4)?.try_into().ok()?))
}

fn u32_at(pkt: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(pkt.get(off..off + 4)?.try_into().ok()?))
}

/// A stateful OutGauge decoder. Stateful only to track the running redline
/// (`max_rpm`) the protocol omits; the per-packet decode is otherwise pure.
#[derive(Debug, Default)]
pub struct Decoder {
    running_max_rpm: f32,
}

impl Decoder {
    pub fn new() -> Self {
        Decoder::default()
    }

    /// Forget the learned redline (called when the stream is torn down).
    pub fn reset(&mut self) {
        self.running_max_rpm = 0.0;
    }

    /// Parse one OutGauge datagram. Returns the [`ID`] and a sample for a
    /// running engine, or `None` for a wrong length or an engine-off sample.
    pub fn parse(&mut self, pkt: &[u8]) -> Option<(&'static str, Telemetry)> {
        if pkt.len() != LEN_NO_ID && pkt.len() != LEN_WITH_ID {
            return None;
        }
        let speed = f32_at(pkt, OFF_SPEED)?;
        let rpm = f32_at(pkt, OFF_RPM)?;
        let throttle = f32_at(pkt, OFF_THROTTLE)?;

        if !speed.is_finite() || !throttle.is_finite() || rpm <= 0.0 || rpm > RPM_CEILING {
            return None;
        }
        self.running_max_rpm = self.running_max_rpm.max(rpm);
        let max_rpm = self.running_max_rpm.max(1.0);
        let lamps = u32_at(pkt, OFF_SHOW_LIGHTS).unwrap_or(0);

        // OutGauge numbers gears from reverse: 0 R, 1 N, 2 first. The
        // normalized form counts from neutral, so shift the origin.
        let gear = pkt
            .get(OFF_GEAR)
            .map_or(0, |g| (*g as i8).saturating_sub(1));
        let brake = f32_at(pkt, OFF_BRAKE).unwrap_or(0.0).clamp(0.0, 1.0);
        let clutch = f32_at(pkt, OFF_CLUTCH).unwrap_or(0.0).clamp(0.0, 1.0);
        Some((
            ID,
            Telemetry {
                rpm,
                max_rpm,
                throttle: throttle.clamp(0.0, 1.0),
                speed,
                pit_limiter: lamps & DL_PITSPEED != 0,
                gear,
                brake,
                clutch,
                abs_active: lamps & DL_ABS != 0,
                traction_control: lamps & DL_TC != 0,
                // OutGauge carries no slip, suspension, or damage channel,
                // so the effects that need those stay inert rather than
                // being fed a guess. Deciding what a lit TC lamp implies
                // about slip is the effect's job, not the decoder's: this
                // function reports what the packet said and nothing more.
                ..Default::default()
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an OutGauge fixture (92 bytes, no trailing ID) with known
    /// engine values.
    fn packet(rpm: f32, throttle: f32, speed: f32) -> Vec<u8> {
        let mut pkt = vec![0u8; LEN_NO_ID];
        pkt[OFF_SPEED..OFF_SPEED + 4].copy_from_slice(&speed.to_le_bytes());
        pkt[OFF_RPM..OFF_RPM + 4].copy_from_slice(&rpm.to_le_bytes());
        pkt[OFF_THROTTLE..OFF_THROTTLE + 4].copy_from_slice(&throttle.to_le_bytes());
        pkt
    }

    #[test]
    fn pit_limiter_follows_the_dl_pitspeed_lamp() {
        let mut dec = Decoder::new();
        let plain = packet(4000.0, 0.5, 30.0);
        let (_, tel) = dec.parse(&plain).unwrap();
        assert!(!tel.pit_limiter, "no lamps lit means no limiter");

        let mut lit = packet(4000.0, 0.5, 30.0);
        lit[OFF_SHOW_LIGHTS..OFF_SHOW_LIGHTS + 4].copy_from_slice(&DL_PITSPEED.to_le_bytes());
        let (_, tel) = dec.parse(&lit).unwrap();
        assert!(tel.pit_limiter);

        // Another lamp on its own must not read as the limiter: DL_HANDBRAKE
        // sits next to DL_PITSPEED in the same bitfield.
        let mut other = packet(4000.0, 0.5, 30.0);
        other[OFF_SHOW_LIGHTS..OFF_SHOW_LIGHTS + 4].copy_from_slice(&4u32.to_le_bytes());
        let (_, tel) = dec.parse(&other).unwrap();
        assert!(!tel.pit_limiter);

        // And it must still be found when other lamps are lit alongside it.
        let mut both = packet(4000.0, 0.5, 30.0);
        both[OFF_SHOW_LIGHTS..OFF_SHOW_LIGHTS + 4]
            .copy_from_slice(&(DL_PITSPEED | 4 | 256).to_le_bytes());
        let (_, tel) = dec.parse(&both).unwrap();
        assert!(tel.pit_limiter);
    }

    #[test]
    fn outgauge_packet_parses() {
        let mut d = Decoder::new();
        let (id, t) = d.parse(&packet(3500.0, 0.75, 27.5)).unwrap();
        assert_eq!(id, ID);
        assert_eq!(t.rpm, 3500.0);
        assert_eq!(t.max_rpm, 3500.0, "first sample: running max == rpm");
        assert!((t.throttle - 0.75).abs() < 1e-6);
        assert!((t.speed - 27.5).abs() < 1e-6);
    }

    #[test]
    fn the_96_byte_variant_with_the_id_field_parses() {
        let mut d = Decoder::new();
        let mut pkt = packet(4000.0, 1.0, 10.0);
        pkt.extend_from_slice(&42i32.to_le_bytes());
        assert_eq!(pkt.len(), LEN_WITH_ID);
        let (_, t) = d.parse(&pkt).unwrap();
        assert_eq!(t.rpm, 4000.0);
    }

    #[test]
    fn running_max_tracks_the_session_high() {
        let mut d = Decoder::new();
        d.parse(&packet(2000.0, 0.3, 5.0)).unwrap();
        let (_, t) = d.parse(&packet(7200.0, 1.0, 50.0)).unwrap();
        assert_eq!(t.max_rpm, 7200.0);
        let (_, t2) = d.parse(&packet(3000.0, 0.4, 20.0)).unwrap();
        assert_eq!(t2.max_rpm, 7200.0, "keeps the learned high");
        d.reset();
        let (_, t3) = d.parse(&packet(3000.0, 0.4, 20.0)).unwrap();
        assert_eq!(t3.max_rpm, 3000.0, "reset re-learns");
    }

    #[test]
    fn engine_off_and_wrong_lengths_are_rejected() {
        let mut d = Decoder::new();
        assert!(d.parse(&packet(0.0, 0.0, 0.0)).is_none(), "engine off");
        let mut short = packet(3000.0, 0.5, 10.0);
        short.pop();
        assert!(d.parse(&short).is_none(), "91 bytes");
        assert!(d.parse(&[]).is_none(), "empty");
        // A classic-Codemasters-sized datagram must not match.
        assert!(d.parse(&vec![0u8; 264]).is_none());
    }

    #[test]
    fn gear_is_rebased_from_outgauges_reverse_origin() {
        // OutGauge counts 0 R, 1 N, 2 first; the normalized form counts
        // from neutral so that 0 is neutral and the sign gives direction.
        for (wire, want) in [(0u8, -1i8), (1, 0), (2, 1), (7, 6)] {
            let mut pkt = packet(3000.0, 0.5, 20.0);
            pkt[OFF_GEAR] = wire;
            let (_, t) = Decoder::default().parse(&pkt).expect("valid packet");
            assert_eq!(t.gear, want, "wire gear {wire}");
        }
    }

    #[test]
    fn the_pedals_are_read_and_clamped() {
        let mut pkt = packet(3000.0, 0.5, 20.0);
        pkt[OFF_BRAKE..OFF_BRAKE + 4].copy_from_slice(&0.75f32.to_le_bytes());
        pkt[OFF_CLUTCH..OFF_CLUTCH + 4].copy_from_slice(&1.5f32.to_le_bytes());
        let (_, t) = Decoder::default().parse(&pkt).expect("valid packet");
        assert!((t.brake - 0.75).abs() < 1e-6);
        assert_eq!(t.clutch, 1.0, "an out-of-range pedal clamps, it does not wrap");
    }

    #[test]
    fn the_abs_and_tc_lamps_are_read_independently() {
        let cases = [
            (0u32, false, false),
            (DL_ABS, true, false),
            (DL_TC, false, true),
            (DL_ABS | DL_TC, true, true),
            // A neighbouring lamp must not read as either of ours.
            (DL_PITSPEED | 0x0000_0100, false, false),
        ];
        for (lamps, want_abs, want_tc) in cases {
            let mut pkt = packet(3000.0, 0.5, 20.0);
            pkt[OFF_SHOW_LIGHTS..OFF_SHOW_LIGHTS + 4].copy_from_slice(&lamps.to_le_bytes());
            let (_, t) = Decoder::default().parse(&pkt).expect("valid packet");
            assert_eq!(t.abs_active, want_abs, "lamps {lamps:#x}");
            assert_eq!(t.traction_control, want_tc, "lamps {lamps:#x}");
        }
    }

    #[test]
    fn what_outgauge_does_not_carry_stays_inert() {
        // The guarantee that lets an effect ship before every format can
        // feed it: an absent channel reads as "not happening", never as a
        // guess. If a future change starts inferring these from the lamps,
        // this test is the thing that should stop it.
        let mut pkt = packet(7000.0, 1.0, 60.0);
        pkt[OFF_SHOW_LIGHTS..OFF_SHOW_LIGHTS + 4]
            .copy_from_slice(&(DL_ABS | DL_TC | DL_PITSPEED).to_le_bytes());
        let (_, t) = Decoder::default().parse(&pkt).expect("valid packet");
        assert_eq!(t.wheel_slip, 0.0);
        assert_eq!(t.surface_roughness, 0.0);
        assert_eq!(t.impact_g, 0.0);
        assert!(!t.airborne);
        assert!(!t.drs_active);
    }
}
