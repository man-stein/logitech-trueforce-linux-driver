// SPDX-License-Identifier: GPL-2.0-only
//! The normalized telemetry sample all parsers decode into.

/// One decoded telemetry sample, normalized across wire formats.
/// `Default` is the inert sample: engine stopped, nothing intervening,
/// nothing hit. Parsers build on it with `..Default::default()` so adding a
/// field cannot silently give an existing source a wrong value, only a
/// quiet one.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Telemetry {
    /// Engine speed in revolutions per minute.
    pub rpm: f32,
    /// Engine redline in revolutions per minute (> 0 for a valid sample).
    pub max_rpm: f32,
    /// Throttle position, 0.0 to 1.0.
    pub throttle: f32,
    /// Vehicle speed in meters per second.
    pub speed: f32,
    /// Whether the car's pit-limiter is engaged, where the wire format says
    /// so. Formats that carry no such field leave this false, which is the
    /// same thing as "not limiting" for every consumer of it.
    ///
    /// Only the rev lights use this: G Hub renders a pit limiter by
    /// flashing the whole strip rather than by any device-side effect (see
    /// `docs/PROTOCOL_SPECIFICATION.md` 12.4), so it is reproduced here
    /// rather than in the driver.
    pub pit_limiter: bool,

    // ---------------------------------------------------------------
    // Inputs for the effects in `crate::effects`.
    //
    // Every one defaults to the value that means "nothing happening", so a
    // format that does not carry a field leaves its effects silent rather
    // than wrong. That is the same contract `pit_limiter` already has: a
    // source that never reports a limiter is indistinguishable from a car
    // without one, and it is why an effect can ship before every format can
    // feed it.
    // ---------------------------------------------------------------
    /// Selected gear: negative reverse, 0 neutral, 1.. forward. Drives the
    /// gear-shift effect, which fires on a change rather than a value.
    pub gear: i8,
    /// Brake pedal, 0..1.
    pub brake: f32,
    /// Clutch pedal, 0..1.
    pub clutch: f32,
    /// ABS is actively modulating, not merely fitted.
    pub abs_active: bool,
    /// Traction or stability control is intervening.
    pub traction_control: bool,
    /// Driven-wheel slip, 0 none .. 1 fully broken away. Sources that only
    /// report a warning lamp set this to a nominal level instead.
    pub wheel_slip: f32,
    /// All wheels off the ground.
    pub airborne: bool,
    /// Surface roughness underneath, 0 smooth .. 1 coarse.
    pub surface_roughness: f32,
    /// Impact magnitude in g for this sample, 0 when nothing was hit.
    pub impact_g: f32,
    /// DRS (or any push-to-pass equivalent) is open.
    pub drs_active: bool,
}
