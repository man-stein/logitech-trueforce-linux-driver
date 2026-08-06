// SPDX-License-Identifier: GPL-2.0-only
//! The shared-memory sims the relay knows how to reach.
//!
//! Section names per `dev/docs/shared-memory-telemetry-plan.md`. iRacing
//! publishes under the session-local `Local\` namespace; the rF2 family
//! (rFactor 2 and Le Mans Ultimate share the engine and the community
//! `rF2SharedMemoryMapPlugin`) publishes globally named `$...$` sections.
//!
//! A game gets a decoder here once its layout can be trusted, which happens
//! one of three ways: the format is self-describing (iRacing), the vendor
//! publishes the layout and owns both ends of it (RaceRoom), or the fields
//! needed sit in a part of the struct that provably has not moved and the
//! risky part carries an in-band check (Assetto Corsa). Anything else waits
//! for a real captured fixture, which is what `--dump` is for.

/// One shared-memory sim the relay can open.
pub struct Game {
    /// CLI name (`--game <id>`).
    pub id: &'static str,
    /// Display name for messages.
    pub name: &'static str,
    /// The Windows named-section name to open.
    pub section: &'static str,
    /// A second section this game's decoder also needs, if any.
    ///
    /// Assetto Corsa is the reason this exists: engine speed is in its
    /// per-tick physics block but the redline is in its per-session static
    /// block, and a sample needs both.
    pub aux_section: Option<&'static str>,
    /// Extra prerequisite the user must have installed, if any.
    pub prerequisite: Option<&'static str>,
    /// Whether a decoder exists, or `--dump` is all this game can do yet.
    pub decodable: bool,
}

/// Every game the relay knows the section name for.
pub const GAMES: &[Game] = &[
    Game {
        id: "iracing",
        name: "iRacing",
        section: "Local\\IRSDKMemMapFileName",
        aux_section: None,
        prerequisite: None,
        decodable: true,
    },
    Game {
        id: crate::raceroom::ID,
        name: "RaceRoom Racing Experience",
        section: crate::raceroom::SECTION,
        aux_section: None,
        prerequisite: None,
        decodable: true,
    },
    Game {
        id: crate::assettocorsa::ID,
        name: "Assetto Corsa",
        section: crate::assettocorsa::SECTION_PHYSICS,
        aux_section: Some(crate::assettocorsa::SECTION_STATIC),
        prerequisite: None,
        decodable: true,
    },
    Game {
        id: "lmu",
        name: "Le Mans Ultimate",
        section: "$rFactor2SMMP_Telemetry$",
        aux_section: None,
        prerequisite: Some(
            "needs the community rF2SharedMemoryMapPlugin in the game's Plugins directory",
        ),
        decodable: false,
    },
    Game {
        id: "rf2",
        name: "rFactor 2",
        section: "$rFactor2SMMP_Telemetry$",
        aux_section: None,
        prerequisite: Some(
            "needs the community rF2SharedMemoryMapPlugin in the game's Plugins directory",
        ),
        decodable: false,
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
        assert!(by_id("acc").is_none());
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

    /// Only Assetto Corsa needs two sections, and it must actually name the
    /// second: without the static block there is no redline, and the
    /// decoder would refuse every sample.
    #[test]
    fn assetto_corsa_is_the_only_two_section_game() {
        for game in GAMES {
            let expected = game.id == crate::assettocorsa::ID;
            assert_eq!(game.aux_section.is_some(), expected, "{}", game.name);
        }
    }
}
