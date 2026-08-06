// SPDX-License-Identifier: GPL-2.0-only
//! `logi-tf-relay`: the Wine-side shared-memory telemetry relay.
//!
//! Runs INSIDE the game's Proton prefix (it is a Windows executable), opens
//! the sim's named shared-memory section with the ordinary Win32 API - which
//! Wine implements, exactly as G HUB and SimHub do on Windows - and forwards
//! engine telemetry over localhost UDP in the relay wire format
//! (`logi_wheel_core::relay`) that `logi-tf-sim` listens for on port 20780.
//! Spec: `dev/docs/shared-memory-telemetry-plan.md`.
//!
//! Two modes:
//!
//! - `logi-tf-relay --game <id> --dump <file>`: open the
//!   section, write its first bytes to `<file>`, exit. This produces the
//!   REAL byte fixture each per-game decoder is written and unit-tested
//!   against - the same discipline the native UDP parsers follow. Run it
//!   from inside the prefix while a session is live.
//! - `logi-tf-relay --game <id>` (normal mode): stream telemetry. Not
//!   available until the game's decoder exists, which by the house rule
//!   requires a `--dump` fixture first; until then this mode explains
//!   exactly that instead of guessing at struct offsets.
//!
//! On non-Windows hosts this compiles to a stub that says to cross-compile
//! (`cargo build -p logi-tf-relay --target x86_64-pc-windows-gnu`), so the
//! workspace always builds without a Windows toolchain.

mod assettocorsa;
mod games;
mod iracing;
mod raceroom;

use std::process::ExitCode;

const USAGE: &str = "logi-tf-relay: shared-memory telemetry for logi-tf-sim (runs inside the Proton prefix)

USAGE:
  logi-tf-relay --game <id>                   stream telemetry to logi-tf-sim
  logi-tf-relay --game <id> --dump <file>     write the section's bytes to <file>
  logi-tf-relay --section <name> --dump <file>  dump any named section

Games that stream today:  iracing, raceroom, assetto-corsa
Games that need a dump:   rf2, lmu

Take the dump while a session is actually RUNNING; sitting in the menus is
not always enough. Send the dump file to the project and the game's decoder
gets written against it. The rule here is a trustworthy layout before every
decoder, never struct offsets from memory.";

/// Max bytes `--dump` writes: enough for every header + descriptor table we
/// know of (iRacing: 112-byte header + ~300 varHeaders à 144 byte ≈ 43 KiB)
/// without dragging a whole rF2 vehicle array to disk.
#[cfg(windows)]
const DUMP_LIMIT: usize = 64 * 1024;

#[derive(Debug)]
struct Args {
    section: Option<String>,
    /// The second section this game's decoder needs, when it needs one.
    /// Only Assetto Corsa does: its redline lives in a different block from
    /// its engine speed.
    aux: Option<String>,
    dump: Option<String>,
    /// Which known game was named, when one was. `--section` alone leaves
    /// this `None`: an arbitrary section can be dumped but not decoded,
    /// because a decoder is per format, not per section name.
    game: Option<&'static str>,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut section = None;
    let mut aux = None;
    let mut dump = None;
    let mut game_id = None;
    let mut it = argv.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--game" => {
                let id = it.next().ok_or("--game needs a game id")?;
                // The known-ids list is derived rather than written out: a
                // hardcoded copy went stale the first time a game was added.
                let game = games::by_id(id).ok_or_else(|| {
                    let known: Vec<&str> = games::GAMES.iter().map(|g| g.id).collect();
                    format!("unknown game {id:?} (known: {})", known.join(", "))
                })?;
                if let Some(prerequisite) = game.prerequisite {
                    eprintln!("logi-tf-relay: note for {}: {}", game.name, prerequisite);
                }
                section = Some(game.section.to_string());
                aux = game.aux_section.map(str::to_string);
                game_id = Some(game.id);
            }
            "--section" => {
                section = Some(it.next().ok_or("--section needs a name")?.clone());
                // An explicitly named section is dumped, never decoded, so
                // whatever a preceding --game set up no longer applies.
                aux = None;
                game_id = None;
            }
            "--dump" => {
                dump = Some(it.next().ok_or("--dump needs a file path")?.clone());
            }
            "--help" | "-h" => return Err(String::new()),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(Args { section, aux, dump, game: game_id })
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(msg) => {
            if !msg.is_empty() {
                eprintln!("logi-tf-relay: {msg}\n");
            }
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    let Some(section) = args.section else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };

    let decodable = args.game.and_then(games::by_id).is_some_and(|g| g.decodable);
    match args.dump {
        Some(path) => run_dump(&section, &path),
        // Each decodable game earned that status a different way; see the
        // `decodable` field in `games` and each decoder's module docs.
        None if decodable => run_stream(&section, args.aux.as_deref(), args.game.unwrap_or("")),
        None => {
            // The rest are fixed-layout structs with nothing in-band to
            // catch a wrong offset, so their decoders wait for a real
            // fixture. Being honest here beats streaming garbage.
            eprintln!(
                "logi-tf-relay: streaming for {section:?} is not built yet. \
                 It needs a dump fixture from a live session first.\n\
                 Run: logi-tf-relay --section \"{section}\" --dump dump.bin\n\
                 then send dump.bin to the project."
            );
            ExitCode::FAILURE
        }
    }
}

/// Stream decoded telemetry to the daemon until interrupted.
///
/// Re-reads the section every tick rather than holding a mapped view: the
/// section is small, the rate is low, and a fresh read cannot observe a
/// half-updated buffer the way a retained pointer can. Undecodable ticks
/// are skipped silently, since a menu, a replay or a paused session all
/// legitimately produce them.
#[cfg(windows)]
fn run_stream(section: &str, aux: Option<&str>, game: &str) -> ExitCode {
    use std::net::UdpSocket;

    let port = std::env::var("LOGI_TF_SIM_RELAY_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|p| *p != 0)
        .unwrap_or(logi_wheel_core::relay::DEFAULT_PORT);

    let Ok(socket) = UdpSocket::bind("127.0.0.1:0") else {
        eprintln!("logi-tf-relay: could not open a local UDP socket");
        return ExitCode::FAILURE;
    };
    if socket.connect(("127.0.0.1", port)).is_err() {
        eprintln!("logi-tf-relay: could not target 127.0.0.1:{port}");
        return ExitCode::FAILURE;
    }

    eprintln!("logi-tf-relay: streaming {section:?} to 127.0.0.1:{port} (ctrl-c to stop)");
    let mut warned = false;
    loop {
        match win::read_section(section, DUMP_LIMIT) {
            Ok(bytes) => {
                warned = false;
                // Assetto Corsa's redline is in a second section, read on
                // the same tick so a car change cannot pair a new engine
                // speed with the previous car's redline.
                let aux_bytes = match aux {
                    Some(name) => win::read_section(name, DUMP_LIMIT).ok(),
                    None => None,
                };
                let sample = match game {
                    raceroom::ID => raceroom::decode(&bytes),
                    assettocorsa::ID => {
                        aux_bytes.and_then(|s| assettocorsa::decode(&bytes, &s))
                    }
                    _ => iracing::decode(&bytes),
                };
                if let Some(sample) = sample {
                    let _ = socket.send(&logi_wheel_core::relay::encode(&sample));
                }
            }
            Err(err) => {
                // The game not being up yet is the normal case on startup,
                // so say it once and keep trying rather than exiting.
                if !warned {
                    eprintln!("logi-tf-relay: {section:?} not readable yet ({err}); waiting");
                    warned = true;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
}

/// Linux stub: the relay only means anything inside the prefix.
#[cfg(not(windows))]
fn run_stream(_section: &str, _aux: Option<&str>, _game: &str) -> ExitCode {
    eprintln!(
        "logi-tf-relay: this is the Linux stub. The relay has to be \
         cross-compiled and run inside the game's Proton prefix:\n  \
         rustup target add x86_64-pc-windows-gnu\n  \
         cargo build --release -p logi-tf-relay --target x86_64-pc-windows-gnu"
    );
    ExitCode::FAILURE
}

#[cfg(windows)]
fn run_dump(section: &str, path: &str) -> ExitCode {
    match win::read_section(section, DUMP_LIMIT) {
        Ok(bytes) => {
            if let Err(err) = std::fs::write(path, &bytes) {
                eprintln!("logi-tf-relay: could not write {path:?}: {err}");
                return ExitCode::FAILURE;
            }
            println!(
                "logi-tf-relay: wrote {} bytes from {section:?} to {path:?}",
                bytes.len()
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!(
                "logi-tf-relay: could not open {section:?}: {err}\n\
                 Check that the game is running with a session actually live \
                 (sitting in the menus is not always enough), and that the \
                 relay runs in the SAME Proton prefix as the game."
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn run_dump(_section: &str, _path: &str) -> ExitCode {
    eprintln!(
        "logi-tf-relay: this is the Linux stub. The relay has to be \
         cross-compiled and run inside the game's Proton prefix:\n  \
         rustup target add x86_64-pc-windows-gnu\n  \
         cargo build --release -p logi-tf-relay --target x86_64-pc-windows-gnu"
    );
    ExitCode::FAILURE
}

/// The Win32 side: open a named section read-only and copy out its bytes.
/// Isolated so everything unsafe lives in one place with one contract.
#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
    use windows_sys::Win32::System::Memory::{
        MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualQuery, FILE_MAP_READ,
        MEMORY_BASIC_INFORMATION,
    };

    /// Open `section`, map it read-only, and return up to `limit` bytes.
    /// The copy is byte-for-byte; a live game keeps writing while we read,
    /// which is fine for a dump fixture (the header fields we care about
    /// are static once a session is up).
    pub fn read_section(section: &str, limit: usize) -> Result<Vec<u8>, String> {
        let wide: Vec<u16> = section.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let mapping = OpenFileMappingW(FILE_MAP_READ, 0, wide.as_ptr());
            if mapping.is_null() {
                return Err(format!("OpenFileMappingW: fel {}", GetLastError()));
            }
            let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0);
            if view.Value.is_null() {
                let err = GetLastError();
                CloseHandle(mapping);
                return Err(format!("MapViewOfFile: fel {err}"));
            }

            // The section size: VirtualQuery on the view gives the region length.
            let mut info: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
            let size = if VirtualQuery(
                view.Value,
                &mut info,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            ) != 0
            {
                info.RegionSize.min(limit)
            } else {
                limit
            };

            let bytes = std::slice::from_raw_parts(view.Value as *const u8, size).to_vec();
            UnmapViewOfFile(view);
            CloseHandle(mapping);
            Ok(bytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn game_id_resolves_to_its_section() {
        let a = parse_args(&args(&["--game", "iracing", "--dump", "d.bin"])).unwrap();
        assert_eq!(a.section.as_deref(), Some("Local\\IRSDKMemMapFileName"));
        assert_eq!(a.dump.as_deref(), Some("d.bin"));
    }

    #[test]
    fn explicit_section_wins_over_nothing() {
        let a = parse_args(&args(&["--section", "$R3E", "--dump", "x"])).unwrap();
        assert_eq!(a.section.as_deref(), Some("$R3E"));
    }

    #[test]
    fn unknown_game_and_missing_values_are_errors() {
        assert!(parse_args(&args(&["--game", "acc"])).is_err());
        assert!(parse_args(&args(&["--game"])).is_err());
        assert!(parse_args(&args(&["--dump"])).is_err());
        assert!(parse_args(&args(&["--frobnicate"])).is_err());
    }

    #[test]
    fn help_is_the_empty_error() {
        assert_eq!(parse_args(&args(&["--help"])).unwrap_err(), "");
    }
}
