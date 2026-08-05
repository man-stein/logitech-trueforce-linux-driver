//! Putting text on the system clipboard, best-effort.
//!
//! Shared by both front-ends so a copy button and a copy key behave the
//! same way, and so there is one place to teach about a clipboard tool
//! rather than two that can drift.

use std::io::Write;
use std::process::{Command, Stdio};

/// Copy `text` to the clipboard: try `wl-copy` (Wayland), then
/// `xclip -selection clipboard` (X11).
///
/// Every failure is ignored, and deliberately. There may be no clipboard
/// tool installed, no display server at all (a terminal over SSH), or a
/// hung helper; none of that is worth an error dialog, because both
/// front-ends show the text they are copying and it can be selected by
/// hand. Returns whether a tool accepted it, for callers that want to say
/// so.
///
/// Call this off the UI thread. A missing or hanging clipboard tool must
/// never stall a window.
pub fn copy(text: &str) -> bool {
    if Command::new("wl-copy").arg(text).status().is_ok_and(|s| s.success()) {
        return true;
    }
    let Ok(mut child) = Command::new("xclip")
        .args(["-selection", "clipboard"])
        .stdin(Stdio::piped())
        .spawn()
    else {
        return false;
    };
    if let Some(stdin) = child.stdin.as_mut() {
        if stdin.write_all(text.as_bytes()).is_err() {
            return false;
        }
    }
    child.wait().is_ok_and(|s| s.success())
}
