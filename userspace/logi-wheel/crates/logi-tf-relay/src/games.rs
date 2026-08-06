// SPDX-License-Identifier: GPL-2.0-only
//! The shared-memory sims the relay knows how to reach.
//!
//! Section names per `dev/docs/shared-memory-telemetry-plan.md`. iRacing
//! publishes under the session-local `Local\` namespace; the rF2 family
//! (rFactor 2 and Le Mans Ultimate share the engine and the community
//! `rF2SharedMemoryMapPlugin`) publishes globally named `$...$` sections.
//!
//! A game gets a decoder here once its layout can be trusted, which has so
//! far happened four ways: the format is self-describing (iRacing), the
//! vendor publishes the layout and owns both ends of it (RaceRoom), the
//! fields needed sit in a part of the struct that provably has not moved and
//! the risky part carries an in-band check (Assetto Corsa), or the format
//! carries enough state to tell a good read from a bad one (the rF2 family's
//! version counters and cross-buffer id match). A format with none of those
//! waits for a real captured fixture, which is what `--dump` is for.

/// One shared-memory sim the relay can open.
pub struct Game {
    /// CLI name (`--game <id>`).
    pub id: &'static str,
    /// Display name for messages.
    pub name: &'static str,
    /// The Windows named-section name to open.
    pub section: &'static str,
    /// A second section this game's decoder also needs, and how much of it
    /// to read.
    ///
    /// Assetto Corsa is the reason this exists: engine speed is in its
    /// per-tick physics block but the redline is in its per-session static
    /// block, and a sample needs both. The rF2 family needs one too, for
    /// "which car is the player".
    ///
    /// The size travels with the name because the two sections are not the
    /// same size: rF2's scoring buffer is 75 KiB against telemetry's 236
    /// KiB. Reading one with the other's length is what left `SCORING_LEN`
    /// declared and unused, which only the Windows build ever noticed.
    pub aux_section: Option<(&'static str, usize)>,
    /// Extra prerequisite the user must have installed, if any.
    pub prerequisite: Option<&'static str>,
    /// Whether a decoder exists, or `--dump` is all this game can do yet.
    pub decodable: bool,
    /// How many bytes to read from the section each tick.
    ///
    /// Per game because the sizes are not close: iRacing needs its header
    /// and descriptor table, Assetto Corsa a few hundred bytes, and the rF2
    /// family a 236 KiB array of every car in the session. Reading a fixed
    /// small amount would silently truncate the last of those.
    pub read_len: usize,
}

/// Bytes read per tick for a game whose section has no fixed size, notably
/// iRacing's header plus its variable-descriptor table (112 bytes plus about
/// 300 descriptors of 144 bytes).
pub const DEFAULT_READ_LEN: usize = 64 * 1024;

/// The prerequisite both rF2-family titles share.
const RF2_PLUGIN: &str =
    "needs the community rF2SharedMemoryMapPlugin in the game's Plugins directory";

/// Every game the relay knows the section name for.
pub const GAMES: &[Game] = &[
    Game {
        id: "iracing",
        name: "iRacing",
        section: "Local\\IRSDKMemMapFileName",
        aux_section: None,
        prerequisite: None,
        decodable: true,
        read_len: DEFAULT_READ_LEN,
    },
    Game {
        id: crate::raceroom::ID,
        name: "RaceRoom Racing Experience",
        section: crate::raceroom::SECTION,
        aux_section: None,
        prerequisite: None,
        decodable: true,
        read_len: DEFAULT_READ_LEN,
    },
    Game {
        id: crate::assettocorsa::ID,
        name: "Assetto Corsa",
        section: crate::assettocorsa::SECTION_PHYSICS,
        aux_section: Some((crate::assettocorsa::SECTION_STATIC, DEFAULT_READ_LEN)),
        prerequisite: None,
        decodable: true,
        read_len: DEFAULT_READ_LEN,
    },
    Game {
        id: crate::assettocorsa::ID_ACC,
        name: "Assetto Corsa Competizione",
        // Identical section names and identical layout to Assetto Corsa, so
        // the same decoder reads it; only the settings id differs.
        section: crate::assettocorsa::SECTION_PHYSICS,
        aux_section: Some((crate::assettocorsa::SECTION_STATIC, DEFAULT_READ_LEN)),
        prerequisite: None,
        decodable: true,
        read_len: DEFAULT_READ_LEN,
    },
    Game {
        id: crate::assettocorsa::ID_EVO,
        name: "Assetto Corsa EVO",
        // EVO renamed its sections and moved the redline into the physics
        // block, so unlike Competizione it needs only one section.
        section: crate::assettocorsa::SECTION_PHYSICS_EVO,
        aux_section: None,
        prerequisite: None,
        decodable: true,
        read_len: DEFAULT_READ_LEN,
    },
    Game {
        id: crate::rfactor2::ID_LMU,
        name: "Le Mans Ultimate",
        section: crate::rfactor2::SECTION_TELEMETRY,
        aux_section: Some((crate::rfactor2::SECTION_SCORING, crate::rfactor2::SCORING_LEN)),
        prerequisite: Some(RF2_PLUGIN),
        decodable: true,
        read_len: crate::rfactor2::TELEMETRY_LEN,
    },
    Game {
        id: crate::rfactor2::ID_RF2,
        name: "rFactor 2",
        section: crate::rfactor2::SECTION_TELEMETRY,
        aux_section: Some((crate::rfactor2::SECTION_SCORING, crate::rfactor2::SCORING_LEN)),
        prerequisite: Some(RF2_PLUGIN),
        decodable: true,
        read_len: crate::rfactor2::TELEMETRY_LEN,
    },
];

/// Look up a game by its CLI id.
pub fn by_id(id: &str) -> Option<&'static Game> {
    GAMES.iter().find(|g| g.id.eq_ignore_ascii_case(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_is_case_insensitive_and_misses_are_none() {
        assert_eq!(by_id("iRacing").unwrap().id, "iracing");
        assert_eq!(by_id("LMU").unwrap().name, "Le Mans Ultimate");
        assert_eq!(by_id("ACC").unwrap().name, "Assetto Corsa Competizione");
        assert!(by_id("gran turismo").is_none());
    }

    #[test]
    fn rf2_and_lmu_share_the_section_but_not_the_identity() {
        let lmu = by_id("lmu").unwrap();
        let rf2 = by_id("rf2").unwrap();
        assert_eq!(lmu.section, rf2.section, "same engine, same plugin, same section");
        assert_ne!(lmu.name, rf2.name);
        assert!(lmu.prerequisite.is_some(), "the rF2 plugin prerequisite must reach the help text");
    }

    #[test]
    fn iracing_section_is_session_local() {
        assert!(by_id("iracing").unwrap().section.starts_with("Local\\"));
    }

    /// A game the relay can decode sends samples tagged with its id, and the
    /// daemon drops any id it does not know. Adding a decoder here without
    /// adding the id there produces a relay that streams into a void, which
    /// looks exactly like a game that is not publishing.
    #[test]
    fn every_decodable_game_has_an_id_the_daemon_accepts() {
        for game in GAMES.iter().filter(|g| g.decodable) {
            assert!(
                logi_wheel_core::relay::GAME_IDS.contains(&game.id),
                "{} decodes but its id {:?} is not in relay::GAME_IDS",
                game.name,
                game.id
            );
        }
    }

    /// A game that names a second section needs it: Assetto Corsa's redline
    /// and the rF2 family's `mIsPlayer` both live outside the primary
    /// section, and without them their decoders refuse every sample.
    #[test]
    fn the_two_section_games_are_the_ones_that_need_a_second() {
        for game in GAMES {
            let needs_two = matches!(
                game.id,
                crate::assettocorsa::ID
                    | crate::assettocorsa::ID_ACC
                    | crate::rfactor2::ID_RF2
                    | crate::rfactor2::ID_LMU
            );
            assert_eq!(game.aux_section.is_some(), needs_two, "{}", game.name);
        }
    }

    /// The rF2 family's telemetry mapping is an array of every car in the
    /// session. Reading the default amount would take the first 34 of 128
    /// rows and silently miss a player further down the grid.
    #[test]
    fn the_rf2_family_reads_its_whole_mapping() {
        for game in GAMES.iter().filter(|g| g.section == crate::rfactor2::SECTION_TELEMETRY) {
            assert_eq!(game.read_len, crate::rfactor2::TELEMETRY_LEN, "{}", game.name);
            assert!(game.read_len > DEFAULT_READ_LEN, "{} needs more than the default", game.name);
        }
    }
}
