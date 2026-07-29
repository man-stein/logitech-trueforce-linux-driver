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

const OFF_SPEED: usize = 12;
const OFF_RPM: usize = 16;
const OFF_THROTTLE: usize = 48;
/// OutGauge `showLights`: the dashboard lamps the game says are lit, as a
/// DL_* bitfield. Only the pit-speed-limiter bit is read.
const OFF_SHOW_LIGHTS: usize = 44;
/// `DL_PITSPEED` in the OutGauge DL_* set. A source that never lights this
/// lamp simply reports no limiter, which is indistinguishable from a car
/// that has none.
const DL_PITSPEED: u32 = 0x0000_0008;

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
        let pit_limiter = u32_at(pkt, OFF_SHOW_LIGHTS).is_some_and(|l| l & DL_PITSPEED != 0);
        Some((
            ID,
            Telemetry { rpm, max_rpm, throttle: throttle.clamp(0.0, 1.0), speed, pit_limiter },
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
}
