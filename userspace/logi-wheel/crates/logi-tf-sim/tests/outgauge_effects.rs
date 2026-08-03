// SPDX-License-Identifier: GPL-2.0-only
//! End-to-end: real OutGauge bytes in, haptic samples out.
//!
//! The unit tests either side of this cover the decoder and the effects
//! separately, each against its own fixtures. This one runs the whole path
//! a game actually drives: a packet laid out from the published OutGauge
//! spec, through the decoder, into the mixer, and asserts that the right
//! layer moved.
//!
//! The packet is built here from the spec rather than with the decoder's own
//! test helper on purpose. A field offset that was wrong in both the parser
//! and the helper that feeds it would agree with itself forever; an
//! independent second construction is what catches that.

use logi_tf_sim::beamng;
use logi_tf_sim::effects::{EffectGains, EffectId, Mixer};
use logi_tf_sim::synth::DEFAULT_CYLINDERS;

/// OutGauge, from the Live for Speed specification BeamNG implements.
const LEN: usize = 92;
const OFF_GEAR: usize = 10;
const OFF_SPEED: usize = 12;
const OFF_RPM: usize = 16;
const OFF_SHOW_LIGHTS: usize = 44;
const OFF_THROTTLE: usize = 48;
const OFF_BRAKE: usize = 52;

const DL_PITSPEED: u32 = 8;
const DL_TC: u32 = 16;
const DL_ABS: u32 = 1024;

struct Packet([u8; LEN]);

impl Packet {
    /// A car under way: third gear, half throttle, no lamps.
    fn driving() -> Self {
        let mut p = Packet([0u8; LEN]);
        p.f32_at(OFF_SPEED, 30.0);
        p.f32_at(OFF_RPM, 4000.0);
        p.f32_at(OFF_THROTTLE, 0.5);
        // OutGauge counts 0 R, 1 N, 2 first: wire 4 is third.
        p.0[OFF_GEAR] = 4;
        p
    }

    fn f32_at(&mut self, off: usize, v: f32) {
        self.0[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn gear(mut self, wire: u8) -> Self {
        self.0[OFF_GEAR] = wire;
        self
    }

    fn lamps(mut self, bits: u32) -> Self {
        self.0[OFF_SHOW_LIGHTS..OFF_SHOW_LIGHTS + 4].copy_from_slice(&bits.to_le_bytes());
        self
    }

    fn brake(mut self, v: f32) -> Self {
        self.f32_at(OFF_BRAKE, v);
        self
    }
}

fn only(id: EffectId) -> EffectGains {
    let mut g = EffectGains::default();
    for e in EffectId::ALL {
        g.set(e, if e == id { 100 } else { 0 });
    }
    g
}

fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

/// Feed `packets` through a decoder and mixer, one millisecond per packet,
/// and return the loudest sample produced.
fn drive(layer: EffectId, packets: &[Packet]) -> f32 {
    let mut decoder = beamng::Decoder::default();
    let mut mixer = Mixer::new(DEFAULT_CYLINDERS, 1.0, only(layer));
    let mut out = Vec::new();
    let mut loudest = 0.0f32;
    for pkt in packets {
        let (_, tel) = decoder.parse(&pkt.0).expect("the decoder accepted the packet");
        mixer.render(&tel, 1.0, 1, &mut out);
        loudest = loudest.max(peak(&out));
    }
    loudest
}

/// Hold one packet for `ms` milliseconds.
fn hold(pkt: Packet, ms: usize) -> Vec<Packet> {
    (0..ms).map(|_| Packet(pkt.0)).collect()
}

#[test]
fn a_gear_change_on_the_wire_is_felt() {
    let mut run = hold(Packet::driving(), 50);
    run.extend(hold(Packet::driving().gear(5), 200));
    assert!(drive(EffectId::GearShift, &run) > 0.5, "the shift was not felt");
}

#[test]
fn holding_a_gear_is_felt_as_nothing() {
    let run = hold(Packet::driving(), 400);
    assert_eq!(drive(EffectId::GearShift, &run), 0.0, "a steady gear produced a thump");
}

#[test]
fn the_abs_lamp_on_the_wire_reaches_the_pump_effect() {
    let run = hold(Packet::driving().lamps(DL_ABS).brake(1.0), 300);
    assert!(drive(EffectId::Abs, &run) > 0.3, "the ABS lamp produced nothing");
}

#[test]
fn the_traction_lamp_on_the_wire_reaches_the_slip_effect() {
    let run = hold(Packet::driving().lamps(DL_TC), 400);
    assert!(drive(EffectId::TractionLoss, &run) > 0.1, "the traction lamp produced nothing");
}

#[test]
fn the_pit_lamp_on_the_wire_reaches_the_limiter_effect() {
    let run = hold(Packet::driving().lamps(DL_PITSPEED), 300);
    assert!(drive(EffectId::PitLimiter, &run) > 0.3, "the pit lamp produced nothing");
}

#[test]
fn an_unrelated_lamp_moves_nothing() {
    // DL_FULLBEAM. Nothing here should answer to it.
    let run = hold(Packet::driving().lamps(2), 400);
    for layer in [EffectId::Abs, EffectId::TractionLoss, EffectId::PitLimiter] {
        assert_eq!(drive(layer, &run), 0.0, "{} answered to the headlights", layer.key());
    }
}

#[test]
fn a_quiet_lap_stays_quiet_on_every_layer_no_source_feeds() {
    // Surface, impacts, airborne and DRS have no OutGauge field. They must
    // be silent, not merely quiet: this is the guarantee that lets them ship
    // before a decoder can feed them.
    let run = hold(Packet::driving().lamps(DL_ABS | DL_TC | DL_PITSPEED).brake(1.0), 400);
    for layer in
        [EffectId::RoadBumps, EffectId::Airborne, EffectId::Collision, EffectId::Drs]
    {
        assert_eq!(drive(layer, &run), 0.0, "{} was not silent", layer.key());
    }
}

#[test]
fn the_engine_layer_still_carries_the_lap() {
    let run = hold(Packet::driving(), 200);
    assert!(drive(EffectId::Engine, &run) > 0.1, "no engine note from a running engine");
}
