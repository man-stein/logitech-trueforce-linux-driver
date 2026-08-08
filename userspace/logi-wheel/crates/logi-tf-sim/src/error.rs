// SPDX-License-Identifier: GPL-2.0-only
//! Crate error type.

use std::fmt;

/// Errors surfaced by the daemon, the sweep mode, and the stream wrapper.
#[derive(Debug)]
pub enum Error {
    /// No force-feedback session could be held open, so a direct-drive
    /// wheel would move unpredictably. See `ffb_keepalive`.
    Unstabilised,
    /// An OS-level failure, with context (what was being attempted).
    Io(String, std::io::Error),
    /// No supported wheel was found by libtrueforce discovery.
    NoWheel,
    /// A libtrueforce call failed: (function, LOGITF_* return code).
    Stream(String, i32),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unstabilised => write!(
                f,
                "refusing to run: no force-feedback session could be opened, and \
                 without one a direct-drive wheel can drive itself into its stops. \
                 Check that you can write to the wheel's /dev/input/event* node \
                 (the udev rules grant this); see issue #57"
            ),
            Error::Io(what, e) => write!(f, "{what}: {e}"),
            Error::NoWheel => write!(f, "no supported wheel found"),
            Error::Stream(func, rc) => write!(f, "{func} failed (rc {rc})"),
        }
    }
}

impl std::error::Error for Error {}

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn the_refusal_says_what_to_do_about_it() {
        let msg = Error::Unstabilised.to_string();
        assert!(msg.contains("refusing to run"), "says it refused: {msg}");
        assert!(msg.contains("force-feedback"), "names the cause: {msg}");
        assert!(msg.contains("/dev/input/event"), "names what to check: {msg}");
        assert!(msg.contains("#57"), "points at the explanation: {msg}");
    }
}
