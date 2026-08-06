// SPDX-License-Identifier: GPL-2.0-only
//! Installing the two telemetry helpers into the places games load them
//! from, so nobody has to copy a file by hand.
//!
//! Everything else this project needs is put in place for the user by a
//! package: the driver, the daemon, the front-ends, the FFB proxy. These two
//! were the exception, because neither belongs anywhere a package can simply
//! drop it and be done:
//!
//! - [`RELAY_BIN`] is a *Windows* executable. It runs inside one specific
//!   game's Proton prefix, and there is one prefix per game, so a package
//!   can only stage a master copy and let something else place it.
//! - [`SCS_PLUGIN`] is loaded by the game process itself, out of a
//!   directory inside the game's own installation.
//!
//! So the packages install a master copy of each into
//! [`SHARED_DIR`], and this module copies from there to wherever the
//! game will look. The front-ends' Setup pages drive it.
//!
//! Resolution is pure over its inputs so it can be tested against fixture
//! trees; the `*_source()` and `install_*` wrappers feed in the real
//! environment.

use std::path::{Path, PathBuf};

/// The relay's file name, in the shared directory and in the prefix.
pub const RELAY_BIN: &str = "logi-tf-relay.exe";

/// The truck-sim plugin's file name. The game loads it by name, so this is
/// also what it must be called once installed.
pub const SCS_PLUGIN: &str = "liblogi_tf_scs.so";

/// Where packages stage their master copies.
pub const SHARED_DIR: &str = "/usr/share/logitech-trueforce";

/// The same, for a `/usr/local` install.
const SHARED_DIR_LOCAL: &str = "/usr/local/share/logitech-trueforce";

/// Where the relay is placed inside a game's wine prefix.
///
/// The drive root, so the in-prefix path is `C:\logi-tf-relay.exe`: short,
/// unambiguous, and not inside a directory a game update might replace.
const PREFIX_RELAY_DIR: &str = "drive_c";

/// Where a game built on the SCS engine looks for plugins, relative to the
/// game's installation directory. The 64-bit Linux build is the only one
/// that matters here: both titles ship native Linux binaries.
pub const SCS_PLUGIN_DIR: &str = "bin/linux_x64/plugins";

/// How far above the running executable to look for a checkout, matching
/// [`crate::helpers`]'s own walk.
const MAX_WALK_UP: usize = 8;

/// The relay's path inside a repo checkout.
const REPO_RELAY: &str = "tools/logi-tf-relay.exe";

/// The plugin's path inside a repo checkout's build output. Unlike the
/// relay, this one is built rather than committed, because every builder
/// can produce a native shared object.
const REPO_SCS: &str = "userspace/logi-wheel/target/release/liblogi_tf_scs.so";

/// Steam appids of the games that load [`SCS_PLUGIN`].
///
/// Both are native Linux titles, which is why they are listed here rather
/// than discovered through [`crate::steam::installed_games`]: that function
/// only reports games with a Proton prefix, and these have none.
pub const SCS_APPIDS: [(u32, &str); 2] =
    [(227300, "Euro Truck Simulator 2"), (270880, "American Truck Simulator")];

/// The daemon game ids fed by the in-prefix relay.
///
/// A subset of [`crate::relay::GAME_IDS`], because that list covers every
/// sender of the relay *wire format*, and two of them are not the relay:
/// Euro Truck Simulator 2 and American Truck Simulator speak it from the
/// SCS plugin, running natively inside the game. Offering to install a
/// Windows executable into a native game's non-existent Proton prefix is
/// exactly the kind of confident wrong answer worth spending a constant to
/// avoid.
pub const RELAY_GAME_IDS: [&str; 7] =
    ["iracing", "raceroom", "assetto", "acc", "ac-evo", "rf2", "lmu"];

/// Whether a game gated by `game_id` is fed by the relay.
pub fn needs_relay(game_id: &str) -> bool {
    RELAY_GAME_IDS.contains(&game_id)
}

/// Whether a game gated by `game_id` is fed by the SCS plugin.
pub fn needs_scs_plugin(game_id: &str) -> bool {
    matches!(game_id, "ets2" | "ats")
}

/// Find `name` in the staged locations, then in a repo checkout.
///
/// `exe` is the running executable's path, used to locate the checkout when
/// running from a build tree rather than a package.
fn resolve(name: &str, repo_rel: &str, roots: &[&Path], exe: Option<&Path>) -> Option<PathBuf> {
    for root in roots {
        let candidate = root.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let mut dir = exe?.parent()?;
    for _ in 0..MAX_WALK_UP {
        let candidate = dir.join(repo_rel);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    None
}

/// Locate the master copy of the relay.
pub fn resolve_relay(roots: &[&Path], exe: Option<&Path>) -> Option<PathBuf> {
    resolve(RELAY_BIN, REPO_RELAY, roots, exe)
}

/// Locate the master copy of the truck-sim plugin.
pub fn resolve_scs(roots: &[&Path], exe: Option<&Path>) -> Option<PathBuf> {
    resolve(SCS_PLUGIN, REPO_SCS, roots, exe)
}

fn default_roots() -> [&'static Path; 2] {
    [Path::new(SHARED_DIR), Path::new(SHARED_DIR_LOCAL)]
}

/// [`resolve_relay`] over the real environment.
pub fn relay_source() -> Option<PathBuf> {
    resolve_relay(&default_roots(), std::env::current_exe().ok().as_deref())
}

/// [`resolve_scs`] over the real environment.
pub fn scs_source() -> Option<PathBuf> {
    resolve_scs(&default_roots(), std::env::current_exe().ok().as_deref())
}

/// Where the relay goes in `prefix`.
pub fn relay_target(prefix: &Path) -> PathBuf {
    prefix.join(PREFIX_RELAY_DIR).join(RELAY_BIN)
}

/// Where the plugin goes in a game installation at `game_dir`.
pub fn scs_target(game_dir: &Path) -> PathBuf {
    game_dir.join(SCS_PLUGIN_DIR).join(SCS_PLUGIN)
}

/// Whether the relay is already installed in `prefix`.
pub fn relay_installed_in(prefix: &Path) -> bool {
    relay_target(prefix).is_file()
}

/// Whether the plugin is already installed in `game_dir`.
pub fn scs_installed_in(game_dir: &Path) -> bool {
    scs_target(game_dir).is_file()
}

/// What an install attempt did, so a front-end can say something true
/// rather than just "done".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// Copied for the first time.
    Fresh,
    /// Replaced an existing copy, which is what an upgrade looks like.
    Replaced,
}

/// Copy `source` to `target`, creating the parent directory.
///
/// Always overwrites: the master copy travels with the package, so if it
/// differs from what is in place, what is in place is the older one. Nothing
/// here is user-authored, so there is nothing to preserve.
fn place(source: &Path, target: &Path) -> std::io::Result<Installed> {
    let existed = target.is_file();
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(source, target)?;
    Ok(if existed { Installed::Replaced } else { Installed::Fresh })
}

/// Install the relay into a game's wine prefix.
pub fn install_relay(source: &Path, prefix: &Path) -> std::io::Result<Installed> {
    place(source, &relay_target(prefix))
}

/// Install the truck-sim plugin into a game's installation.
///
/// The `plugins` directory usually does not exist: the game looks for it but
/// does not ship one, so it is created.
pub fn install_scs(source: &Path, game_dir: &Path) -> std::io::Result<Installed> {
    place(source, &scs_target(game_dir))
}

/// The command line that runs an installed relay for `game_id` in `prefix`.
///
/// Returned rather than executed: a front-end may want to show it, and the
/// person running it may want to change the port or add their own
/// environment. Keeping it one place stops the app and the docs disagreeing.
pub fn relay_command(prefix: &Path, game_id: &str) -> String {
    format!(
        "WINEPREFIX={} wine c:\\\\{} --game {}",
        prefix.display(),
        RELAY_BIN,
        game_id
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tempdir() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "tf-helpers-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn a_packaged_copy_is_preferred_over_a_checkout() {
        let root = tempdir();
        let shared = root.join("share");
        touch(&shared.join(RELAY_BIN), "packaged");
        let exe = root.join("repo/userspace/logi-wheel/target/release/logi-wheel-gui");
        touch(&exe, "");
        touch(&root.join("repo").join(REPO_RELAY), "checkout");

        let found = resolve_relay(&[shared.as_path()], Some(&exe)).expect("found");
        assert_eq!(fs::read_to_string(found).unwrap(), "packaged");
    }

    /// Running from a build tree has to work too, or every contributor is
    /// told to install a package before they can test the Setup page.
    #[test]
    fn a_checkout_is_found_when_nothing_is_packaged() {
        let root = tempdir();
        let exe = root.join("repo/userspace/logi-wheel/target/release/logi-wheel-gui");
        touch(&exe, "");
        touch(&root.join("repo").join(REPO_RELAY), "checkout");

        let missing = root.join("nowhere");
        let found = resolve_relay(&[missing.as_path()], Some(&exe)).expect("found");
        assert_eq!(fs::read_to_string(found).unwrap(), "checkout");

        touch(&root.join("repo").join(REPO_SCS), "plugin");
        assert!(resolve_scs(&[missing.as_path()], Some(&exe)).is_some());
    }

    #[test]
    fn nothing_anywhere_resolves_to_none() {
        let root = tempdir();
        let exe = root.join("bin/logi-wheel-gui");
        touch(&exe, "");
        assert!(resolve_relay(&[root.join("nope").as_path()], Some(&exe)).is_none());
    }

    /// The relay lands at the drive root, so the in-prefix path is short and
    /// is not inside a directory a game update might replace.
    #[test]
    fn the_relay_goes_to_the_drive_root_of_the_prefix() {
        let prefix = Path::new("/games/compatdata/805550/pfx");
        assert_eq!(
            relay_target(prefix),
            Path::new("/games/compatdata/805550/pfx/drive_c/logi-tf-relay.exe")
        );
    }

    /// The game looks for a `plugins` directory but does not ship one, so
    /// installing has to create it.
    #[test]
    fn installing_the_plugin_creates_the_directory_the_game_looks_in() {
        let root = tempdir();
        let source = root.join(SCS_PLUGIN);
        touch(&source, "plugin");
        let game = root.join("common/Euro Truck Simulator 2");
        fs::create_dir_all(&game).unwrap();
        assert!(!scs_installed_in(&game));

        assert_eq!(install_scs(&source, &game).unwrap(), Installed::Fresh);
        assert!(scs_installed_in(&game));
        assert_eq!(scs_target(&game), game.join("bin/linux_x64/plugins/liblogi_tf_scs.so"));
        assert_eq!(fs::read_to_string(scs_target(&game)).unwrap(), "plugin");
    }

    /// Reinstalling must overwrite. The copy in place is not user-authored,
    /// so an older one is simply stale, and the common reason to reinstall
    /// is that the package brought a newer helper.
    #[test]
    fn reinstalling_replaces_the_older_copy_and_says_so() {
        let root = tempdir();
        let source = root.join(RELAY_BIN);
        touch(&source, "new");
        let prefix = root.join("pfx");
        touch(&relay_target(&prefix), "old");

        assert!(relay_installed_in(&prefix));
        assert_eq!(install_relay(&source, &prefix).unwrap(), Installed::Replaced);
        assert_eq!(fs::read_to_string(relay_target(&prefix)).unwrap(), "new");
    }

    /// The command the app shows and the command the docs give must be the
    /// same command, which is why it is generated rather than written twice.
    #[test]
    fn the_relay_command_names_the_prefix_the_binary_and_the_game() {
        let cmd = relay_command(Path::new("/games/pfx"), "acc");
        assert!(cmd.contains("WINEPREFIX=/games/pfx"), "{cmd}");
        assert!(cmd.contains("--game acc"), "{cmd}");
        assert!(cmd.contains(RELAY_BIN), "{cmd}");
    }

    /// Every relay-fed id must be a real wire id, or the app offers to set
    /// up a game the daemon would ignore.
    #[test]
    fn every_relay_game_id_is_a_real_wire_id() {
        for id in RELAY_GAME_IDS {
            assert!(
                crate::relay::GAME_IDS.contains(&id),
                "{id} is offered the relay but is not a relay wire id"
            );
        }
    }

    /// The truck sims speak the relay wire format from a native plugin, not
    /// from the relay. Offering them a Windows executable would send someone
    /// looking for a Proton prefix that does not exist.
    #[test]
    fn the_truck_sims_are_plugin_fed_not_relay_fed() {
        for id in ["ets2", "ats"] {
            assert!(crate::relay::GAME_IDS.contains(&id), "{id} is a wire id");
            assert!(!needs_relay(id), "{id} must not be offered the relay");
            assert!(needs_scs_plugin(id), "{id} is fed by the plugin");
        }
        assert!(!needs_scs_plugin("acc"));
        assert!(needs_relay("acc"));
    }

    /// A UDP game needs neither helper: the daemon hears it directly.
    #[test]
    fn a_udp_game_needs_no_helper_at_all() {
        for id in ["dirt-rally-2", "codemasters", "ams2-pcars2", "f1", "beamng", "ea-wrc"] {
            assert!(!needs_relay(id), "{id} is heard over UDP");
            assert!(!needs_scs_plugin(id), "{id} is heard over UDP");
        }
    }

    /// Both truck sims are native Linux titles, so they never appear in the
    /// Proton-prefix scan and have to be listed.
    #[test]
    fn the_truck_sims_are_listed_because_nothing_else_would_find_them() {
        let ids: Vec<u32> = SCS_APPIDS.iter().map(|(id, _)| *id).collect();
        assert!(ids.contains(&227300), "Euro Truck Simulator 2");
        assert!(ids.contains(&270880), "American Truck Simulator");
    }
}
