// SPDX-License-Identifier: GPL-2.0-only
//! The normalized telemetry sample all parsers decode into.

/// One decoded telemetry sample, normalized across wire formats.
#[derive(Debug, Clone, Copy, PartialEq)]
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
}
