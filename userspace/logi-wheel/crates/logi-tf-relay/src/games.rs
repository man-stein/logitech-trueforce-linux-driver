// SPDX-License-Identifier: GPL-2.0-only
//! The shared-memory sims the relay knows how to reach.
//!
//! Section names per `dev/docs/shared-memory-telemetry-plan.md`. iRacing
//! publishes under the session-local `Local\` namespace; the rF2 family
//! (rFactor 2 and Le Mans Ultimate share the engine and the community
//! `rF2SharedMemoryMapPlugin`) publishes globally named `$...$` sections.
//!
//! Decoders are deliberately absent: the project rule is a real captured
//! byte fixture before any wire-format decoder ships, and no `--dump` from
//! a live Proton session exists yet for any of these. `--dump` is exactly
//! how that fixture gets made; each game's decoder lands here once its
//! dump does.

/// One shared-memory sim the relay can open.
pub struct Game {
    /// CLI name (`--game <id>`).
    pub id: &'static str,
    /// Display name for messages.
    pub name: &'static str,
    /// The Windows named-section name to open.
    pub section: &'static str,
    /// Extra prerequisite the user must have installed, if any.
    pub prerequisite: Option<&'static str>,
}

/// Every game the relay knows the section name for.
pub const GAMES: &[Game] = &[
    Game {
        id: "iracing",
        name: "iRacing",
        section: "Local\\IRSDKMemMapFileName",
        prerequisite: None,
    },
    Game {
        id: "lmu",
        name: "Le Mans Ultimate",
        section: "$rFactor2SMMP_Telemetry$",
        prerequisite: Some(
            "needs the community rF2SharedMemoryMapPlugin in the game's Plugins directory",
        ),
    },
    Game {
        id: "rf2",
        name: "rFactor 2",
        section: "$rFactor2SMMP_Telemetry$",
        prerequisite: Some(
            "needs the community rF2SharedMemoryMapPlugin in the game's Plugins directory",
        ),
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
}
