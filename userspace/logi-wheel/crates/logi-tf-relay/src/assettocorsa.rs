// SPDX-License-Identifier: GPL-2.0-only
//! Decoder for Assetto Corsa's `acpmf_physics` / `acpmf_static` sections.
//!
//! # Why this one can be written without a captured fixture
//!
//! Two reasons, and they cover the two blocks separately, because the risk
//! is not the same in both.
//!
//! **The physics block is safe because everything read here is in its first
//! 32 bytes.** Kunos have appended fields to that struct steadily since
//! 2014, but appending does not move what came before, and `packetId`,
//! `gas`, `gear`, `rpms` and `speedKmh` have been the first six members
//! since AC 1.0. The part of this format that drifts is the tail, which is
//! the part this decoder never touches.
//!
//! **The static block is the risky one**, because `maxRpm` sits at offset
//! 412, behind five `wchar_t[33]` name fields. That offset is only correct
//! if Windows' 16-bit `wchar_t` is what the game wrote. So it is checked
//! rather than assumed: see [`static_layout_looks_right`], which reads the
//! `smVersion` string that opens the block and confirms it really is UTF-16.
//! If the assumption were wrong that check fails and the sample is dropped,
//! rather than a number from the wrong place becoming a redline.
//!
//! The offsets were not counted by hand. They were produced by declaring the
//! documented structs and printing `offsetof`, cross-checked against two
//! independent MIT-licensed implementations of the same interface, and are
//! pinned by [`tests::offsets_match_the_documented_layout`].
//!
//! # Why this is not the UDP route
//!
//! Assetto Corsa also has a documented UDP telemetry protocol on port 9996.
//! It is not used here because it is a *conversational* protocol: the client
//! sends a handshake, the game replies, the client subscribes, and only then
//! does data flow. The daemon's other sources are all passive listeners, and
//! shared memory keeps this game the same shape as iRacing and RaceRoom
//! rather than adding a second pattern for one title.
//!
//! ## Layout (`Local\acpmf_physics`, first 32 bytes)
//!
//! | offset | field      | type | notes                                  |
//! |--------|------------|------|----------------------------------------|
//! | 0      | `packetId` | i32  | increments per physics tick            |
//! | 4      | `gas`      | f32  | throttle, 0.0..=1.0                    |
//! | 8      | `brake`    | f32  | unused here                            |
//! | 12     | `fuel`     | f32  | unused here                            |
//! | 16     | `gear`     | i32  | **0 reverse, 1 neutral, 2 first**      |
//! | 20     | `rpms`     | i32  | engine speed, already rpm              |
//! | 28     | `speedKmh` | f32  | unused here                            |
//!
//! ## Layout (`Local\acpmf_static`)
//!
//! | offset | field       | type       | notes                           |
//! |--------|-------------|------------|---------------------------------|
//! | 0      | `smVersion` | wchar_t[15]| UTF-16; the layout guard        |
//! | 412    | `maxRpm`    | i32        | redline                         |
//!
//! The gear convention is this format's trap: Assetto Corsa numbers reverse
//! as 0 and neutral as 1, one higher than every other source the daemon
//! reads, so an untranslated gear silently reports first gear as second.

#![cfg_attr(not(windows), allow(dead_code))]

use logi_wheel_core::relay::RelayTelemetry;

/// The relay wire id for this game.
pub const ID: &str = "assetto";

/// Windows named section carrying the per-tick physics block.
pub const SECTION_PHYSICS: &str = "Local\\acpmf_physics";

/// Windows named section carrying the per-session static block.
pub const SECTION_STATIC: &str = "Local\\acpmf_static";

// Physics offsets (see module docs).
const OFF_GAS: usize = 4;
const OFF_GEAR: usize = 16;
const OFF_RPMS: usize = 20;

/// Through the last physics field read, `rpms`.
const MIN_PHYSICS_LEN: usize = OFF_RPMS + 4;

// Static offsets (see module docs).
const OFF_MAX_RPM: usize = 412;

/// Through `maxRpm`.
const MIN_STATIC_LEN: usize = OFF_MAX_RPM + 4;

/// Above this, the buffer is not engine data however plausible it looked.
const MAX_PLAUSIBLE_RPM: f32 = 30_000.0;

fn i32_at(buf: &[u8], off: usize) -> Option<i32> {
    Some(i32::from_le_bytes(buf.get(off..off + 4)?.try_into().ok()?))
}

fn f32_at(buf: &[u8], off: usize) -> Option<f32> {
    Some(f32::from_le_bytes(buf.get(off..off + 4)?.try_into().ok()?))
}

/// Confirm the static block really begins with a UTF-16 `smVersion` string,
/// which is what makes [`OFF_MAX_RPM`] correct.
///
/// `smVersion` holds something like `"1.7"`. Encoded as Windows UTF-16 that
/// is `31 00 2E 00 ...`: two consecutive 16-bit units, both printable ASCII.
/// Read with any other character width the second unit would be zero, so
/// requiring two printable units in a row is what distinguishes the layout
/// this decoder assumes from the one it would misread.
fn static_layout_looks_right(buf: &[u8]) -> bool {
    let Some(head) = buf.get(0..4) else { return false };
    let first = u16::from_le_bytes([head[0], head[1]]);
    let second = u16::from_le_bytes([head[2], head[3]]);
    let printable = |u: u16| (0x20..0x7f).contains(&u);
    printable(first) && printable(second)
}

/// Decode one read of the physics and static sections.
///
/// Both are required: the physics block carries engine speed but no
/// redline, and without a redline there is nothing to scale an engine note
/// against. Returns `None` for a short buffer, a static block whose layout
/// fails its guard, or a session that is not running (Assetto Corsa leaves
/// the engine fields zeroed in menus).
pub fn decode(physics: &[u8], statics: &[u8]) -> Option<RelayTelemetry> {
    if physics.len() < MIN_PHYSICS_LEN || statics.len() < MIN_STATIC_LEN {
        return None;
    }
    if !static_layout_looks_right(statics) {
        return None;
    }

    let max_rpm = i32_at(statics, OFF_MAX_RPM)? as f32;
    let rpm = i32_at(physics, OFF_RPMS)? as f32;
    if max_rpm <= 0.0 || rpm < 0.0 || rpm > MAX_PLAUSIBLE_RPM || max_rpm > MAX_PLAUSIBLE_RPM {
        return None;
    }

    let throttle = f32_at(physics, OFF_GAS)?;
    let throttle = if throttle.is_finite() { throttle.clamp(0.0, 1.0) } else { 0.0 };

    // Assetto Corsa numbers reverse 0, neutral 1, first 2. The relay wants
    // reverse -1, neutral 0, first 1, so every gear shifts down by one.
    let gear = match i32_at(physics, OFF_GEAR)? {
        g @ 0..=16 => (g - 1) as i16,
        _ => 0,
    };

    Some(RelayTelemetry { game_id: ID, rpm, max_rpm, throttle, gear })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn physics(gas: f32, gear: i32, rpms: i32) -> Vec<u8> {
        let mut b = vec![0u8; MIN_PHYSICS_LEN];
        b[OFF_GAS..][..4].copy_from_slice(&gas.to_le_bytes());
        b[OFF_GEAR..][..4].copy_from_slice(&gear.to_le_bytes());
        b[OFF_RPMS..][..4].copy_from_slice(&rpms.to_le_bytes());
        b
    }

    /// A static block with a believable UTF-16 `smVersion` of "1.7".
    fn statics(max_rpm: i32) -> Vec<u8> {
        let mut b = vec![0u8; MIN_STATIC_LEN];
        for (i, c) in "1.7".encode_utf16().enumerate() {
            b[i * 2..][..2].copy_from_slice(&c.to_le_bytes());
        }
        b[OFF_MAX_RPM..][..4].copy_from_slice(&max_rpm.to_le_bytes());
        b
    }

    /// These numbers are the whole decoder. `maxRpm` in particular sits
    /// behind five wchar_t[33] fields, so it is the one most easily moved by
    /// a careless edit.
    #[test]
    fn offsets_match_the_documented_layout() {
        assert_eq!(OFF_GAS, 4);
        assert_eq!(OFF_GEAR, 16);
        assert_eq!(OFF_RPMS, 20);
        assert_eq!(OFF_MAX_RPM, 412);
        assert_eq!(SECTION_PHYSICS, "Local\\acpmf_physics");
        assert_eq!(SECTION_STATIC, "Local\\acpmf_static");
    }

    #[test]
    fn decodes_a_running_session() {
        let s = decode(&physics(0.75, 3, 6200), &statics(7500)).expect("valid buffers");
        assert_eq!(s.game_id, ID);
        assert_eq!(s.rpm, 6200.0);
        assert_eq!(s.max_rpm, 7500.0);
        assert!((s.throttle - 0.75).abs() < 1e-6);
    }

    /// The trap in this format. Assetto Corsa's 2 is first gear, not second,
    /// and an untranslated read is wrong by exactly one the whole way up.
    #[test]
    fn gears_shift_down_by_one_from_assetto_corsas_numbering() {
        let cases = [(0, -1), (1, 0), (2, 1), (3, 2), (8, 7)];
        for (ac, expected) in cases {
            let s = decode(&physics(0.5, ac, 6000), &statics(7500)).unwrap();
            assert_eq!(s.gear, expected, "AC gear {ac} should relay as {expected}");
        }
    }

    /// The static block's layout guard. Anything that is not a UTF-16
    /// version string at offset 0 means `maxRpm` is not at 412 either.
    #[test]
    fn a_static_block_that_is_not_utf16_is_refused() {
        let mut wrong = statics(7500);
        // UTF-32-shaped: '1', 0, 0, 0. The second 16-bit unit reads zero.
        wrong[2] = 0;
        wrong[3] = 0;
        assert!(decode(&physics(0.5, 3, 6000), &wrong).is_none());

        let mut zeroed = statics(7500);
        zeroed[0..4].fill(0);
        assert!(decode(&physics(0.5, 3, 6000), &zeroed).is_none(), "unwritten block");
    }

    #[test]
    fn short_buffers_are_refused_rather_than_read_past() {
        let p = physics(0.5, 3, 6000);
        let s = statics(7500);
        assert!(decode(&p[..MIN_PHYSICS_LEN - 1], &s).is_none());
        assert!(decode(&p, &s[..MIN_STATIC_LEN - 1]).is_none());
        assert!(decode(&[], &[]).is_none());
    }

    /// Menus leave the engine fields zeroed, and a car with no redline
    /// gives an engine note nothing to scale against.
    #[test]
    fn a_session_that_is_not_running_yields_nothing() {
        assert!(decode(&physics(0.0, 0, 0), &statics(0)).is_none(), "no redline");
        assert!(decode(&physics(0.0, 0, 0), &statics(7500)).is_some(), "idle is still a session");
    }

    #[test]
    fn implausible_engine_values_are_refused() {
        assert!(decode(&physics(0.5, 3, -100), &statics(7500)).is_none());
        assert!(decode(&physics(0.5, 3, 90_000), &statics(7500)).is_none());
        assert!(decode(&physics(0.5, 3, 6000), &statics(90_000)).is_none());
    }

    #[test]
    fn a_non_finite_or_out_of_range_throttle_is_tamed() {
        assert_eq!(decode(&physics(f32::NAN, 3, 6000), &statics(7500)).unwrap().throttle, 0.0);
        assert_eq!(decode(&physics(5.0, 3, 6000), &statics(7500)).unwrap().throttle, 1.0);
        assert_eq!(decode(&physics(-5.0, 3, 6000), &statics(7500)).unwrap().throttle, 0.0);
    }
}
