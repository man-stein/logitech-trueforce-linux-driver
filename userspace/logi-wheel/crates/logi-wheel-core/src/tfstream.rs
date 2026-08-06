// SPDX-License-Identifier: GPL-2.0-only
//! Wire format for TrueForce samples captured from a game's own SDK calls.
//!
//! # What this carries, and why it is not the relay format
//!
//! [`crate::relay`] carries *telemetry* (rpm, throttle, gear) which the
//! daemon turns into an engine note. This carries *finished haptic samples*
//! that a game already produced. They are not interchangeable: one is an
//! input to synthesis, the other is the output of somebody else's.
//!
//! Assetto Corsa Competizione and Assetto Corsa EVO have real TrueForce and
//! generate it continuously. Under Proton they hand it to Logitech's SDK,
//! which needs a G HUB agent that does not exist on Linux, so on the
//! direct-drive wheels the SDK's own DLL drives the wheel directly and on a
//! G923 the data is simply dropped. That is why a G923 owner gets no
//! TrueForce in those two titles despite the wheel being perfectly capable
//! of it.
//!
//! The proxy DLL this project already installs sits in that call path. It
//! forwards every call to Logitech's library unchanged, and additionally
//! copies the TrueForce samples here, so the game's own haptics reach a
//! wheel the SDK will not drive.
//!
//! # Packet layout (little-endian, 8-byte header + samples)
//!
//! | offset | field   | type     | notes                              |
//! |--------|---------|----------|------------------------------------|
//! | 0      | magic   | [u8;4]   | `b"LTFT"`                           |
//! | 4      | version | u8       | 1                                   |
//! | 5      | flags   | u8       | reserved, must be sent as 0         |
//! | 6      | count   | u16      | samples following, 1..=[`MAX_SAMPLES`] |
//! | 8      | samples | f32 * count | normalized -1.0..=1.0            |
//!
//! # This layout is written twice
//!
//! The encoder is C, inside a Windows DLL running under Wine
//! (`tools/tf-range-proxy.c`); the decoder is this module. They cannot share
//! code across that boundary, so the layout above is the single source of
//! truth for both, [`GOLDEN`] pins it from the Rust side, and the C file
//! carries static assertions against the same numbers. A format that exists
//! in two languages with nothing checking them against each other is exactly
//! the shape of bug this project keeps finding.

/// Fixed packet magic. Distinct from the relay format's, because the two
/// mean entirely different things and may share a host.
pub const MAGIC: [u8; 4] = *b"LTFT";

/// The only wire version understood.
pub const VERSION: u8 = 1;

/// Bytes before the sample array.
pub const HEADER_LEN: usize = 8;

/// Most samples one datagram may carry. Chosen to stay inside a normal
/// 1500-byte MTU with room to spare: 256 * 4 + 8 = 1032 bytes.
pub const MAX_SAMPLES: usize = 256;

/// Default UDP port the daemon listens on for captured TrueForce.
pub const DEFAULT_PORT: u16 = 20781;

/// Largest encoded packet.
pub const MAX_PACKET_LEN: usize = HEADER_LEN + MAX_SAMPLES * 4;

/// Encode `samples` into a datagram, or `None` if there are none or too
/// many for one packet. Callers split longer runs themselves.
pub fn encode(samples: &[f32]) -> Option<Vec<u8>> {
    if samples.is_empty() || samples.len() > MAX_SAMPLES {
        return None;
    }
    let mut buf = Vec::with_capacity(HEADER_LEN + samples.len() * 4);
    buf.extend_from_slice(&MAGIC);
    buf.push(VERSION);
    buf.push(0);
    buf.extend_from_slice(&(samples.len() as u16).to_le_bytes());
    for s in samples {
        buf.extend_from_slice(&s.to_le_bytes());
    }
    Some(buf)
}

/// Decode a datagram into samples.
///
/// Rejects a wrong magic, an unknown version, a count that disagrees with
/// the packet length, and any non-finite sample. A non-finite value reaching
/// the wheel would be a torque command nobody can predict, so the whole
/// packet is dropped rather than partially trusted.
pub fn decode(pkt: &[u8]) -> Option<Vec<f32>> {
    if pkt.len() < HEADER_LEN || pkt[0..4] != MAGIC || pkt[4] != VERSION {
        return None;
    }
    let count = u16::from_le_bytes(pkt[6..8].try_into().ok()?) as usize;
    if count == 0 || count > MAX_SAMPLES || pkt.len() != HEADER_LEN + count * 4 {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let at = HEADER_LEN + i * 4;
        let v = f32::from_le_bytes(pkt[at..at + 4].try_into().ok()?);
        if !v.is_finite() {
            return None;
        }
        // The wheel takes a normalized torque. A game that hands the SDK
        // something outside the range is clamped rather than dropped: it is
        // a loud sample, not a corrupt packet.
        out.push(v.clamp(-1.0, 1.0));
    }
    Some(out)
}

/// The exact bytes for a known packet, so the C encoder has something to be
/// checked against rather than a prose description.
pub const GOLDEN: [u8; 16] = [
    0x4c, 0x54, 0x46, 0x54, // "LTFT"
    0x01, // version
    0x00, // flags
    0x02, 0x00, // count = 2
    0x00, 0x00, 0x00, 0x3f, // 0.5
    0x00, 0x00, 0x80, 0xbf, // -1.0
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let samples = [0.0, 0.25, -0.5, 1.0, -1.0];
        assert_eq!(decode(&encode(&samples).unwrap()).unwrap(), samples);
    }

    /// The C encoder is checked against these bytes, so they are the
    /// contract rather than the prose above.
    #[test]
    fn golden_bytes_match_the_documented_layout() {
        assert_eq!(encode(&[0.5, -1.0]).unwrap(), GOLDEN);
        assert_eq!(decode(&GOLDEN).unwrap(), vec![0.5, -1.0]);
    }

    #[test]
    fn a_full_packet_fits_inside_a_normal_mtu() {
        let samples = vec![0.1f32; MAX_SAMPLES];
        let pkt = encode(&samples).unwrap();
        assert_eq!(pkt.len(), MAX_PACKET_LEN);
        assert!(pkt.len() < 1400, "{} bytes would fragment", pkt.len());
        assert_eq!(decode(&pkt).unwrap().len(), MAX_SAMPLES);
    }

    #[test]
    fn empty_and_oversized_runs_are_refused() {
        assert!(encode(&[]).is_none());
        assert!(encode(&vec![0.0f32; MAX_SAMPLES + 1]).is_none());
    }

    /// A count that disagrees with the length is the shape a truncated or
    /// hand-built packet takes, and it must not be trusted.
    #[test]
    fn a_count_that_lies_about_the_length_is_rejected() {
        let mut pkt = encode(&[0.5, -1.0]).unwrap();
        pkt[6] = 9;
        assert!(decode(&pkt).is_none());

        let good = encode(&[0.5, -1.0]).unwrap();
        assert!(decode(&good[..good.len() - 1]).is_none(), "truncated");

        let mut long = good.clone();
        long.push(0);
        assert!(decode(&long).is_none(), "trailing bytes");
    }

    #[test]
    fn a_bad_magic_or_version_is_rejected() {
        let mut m = encode(&[0.5]).unwrap();
        m[0] = b'X';
        assert!(decode(&m).is_none());

        let mut v = encode(&[0.5]).unwrap();
        v[4] = VERSION + 1;
        assert!(decode(&v).is_none());
    }

    /// A non-finite sample is a torque command with no defined meaning, so
    /// the packet goes rather than being partly believed.
    #[test]
    fn a_non_finite_sample_drops_the_whole_packet() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let pkt = encode(&[0.5, bad, 0.25]).unwrap();
            assert!(decode(&pkt).is_none(), "{bad} should drop the packet");
        }
    }

    /// Out-of-range is clamped, not dropped: it is a loud sample rather than
    /// a corrupt one, and silence would be the wrong answer.
    #[test]
    fn out_of_range_samples_are_clamped() {
        let pkt = encode(&[5.0, -5.0]).unwrap();
        assert_eq!(decode(&pkt).unwrap(), vec![1.0, -1.0]);
    }
}
