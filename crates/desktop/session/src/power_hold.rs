//! "Log out?" while an app holds work that is not saved.
//!
//! Logging out and shutting down end every app in the session. An app that keeps work the person
//! has not saved holds the system's logout inhibitor ([`tilo_power_mgmt::inhibit::INHIBIT_LOGOUT`]),
//! naming what it holds in its own words. When one does, the effect waits for the person on the
//! question of [`tilo_sheets::power_hold`]: what is about to happen, the apps that would lose work,
//! and the two answers. Carrying on is their answer, never a silent loss; staying leaves
//! everything as it is.
//!
//! The holds are the machine's own, read here on the pass that asks: nothing here keeps a copy of
//! what an app says it holds.

use std::cell::Cell;

use tilo_power_mgmt::inhibit::INHIBIT_LOGOUT;
use tilo_sheets::SheetState as _;
use tilo_sheets::power_hold::{PowerHoldEffect, PowerHoldState, Question};

use crate::power_menu::Effect;

/// What the person asked for while the question about unsaved work stands.
#[derive(Default)]
pub(crate) struct Hold {
    /// The effect waiting for their answer.
    waiting: Cell<Option<Effect>>,
}

impl Hold {
    /// Ask about `effect` when an app holds work the person has not saved, naming them by what
    /// they say they hold: whether the effect now waits for the person.
    pub(crate) fn ask(&self, effect: Effect, state: &PowerHoldState, apps: Vec<String>) -> bool {
        if apps.is_empty() {
            return false;
        }
        state.ask(Question {
            effect: effect.name().to_owned(),
            apps,
        });
        if !state.presentation().is_drawn() {
            state.presentation().open(false);
        }
        self.waiting.set(Some(effect));
        true
    }

    /// What the person answered since the last pass, as the effect to carry out now: `Some` when
    /// they chose to go ahead, and nothing when they stayed.
    pub(crate) fn follow(&self, state: &PowerHoldState) -> Option<Effect> {
        let mut carrying = None;
        for answer in state.take_asked() {
            match answer {
                PowerHoldEffect::Cancel => self.waiting.set(None),
                PowerHoldEffect::Carry => carrying = self.waiting.take(),
            }
            if state.presentation().is_drawn() {
                state.presentation().close();
            }
        }
        carrying
    }

    /// Whether a question stands now.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn asking(&self) -> bool {
        self.waiting.get().is_some()
    }
}

/// What the apps say they are holding back from logging out, in their own words: empty when none
/// does, and the effect may go ahead.
#[must_use]
pub(crate) fn held_by_apps(power: Option<&tilo_session_power::PowerLink>) -> Vec<String> {
    let Some(link) = power else {
        return Vec::new();
    };
    let mut held = link.registry().active_reasons_for(INHIBIT_LOGOUT);
    held.sort_unstable();
    held.dedup();
    held
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The holds an app takes, as the effect's question lists them: each app's own words, and
    /// nothing when an app holds none. A hold on something else — a download keeping the machine
    /// awake — is not a reason to ask about logging out.
    #[test]
    fn what_the_apps_hold_is_what_the_question_lists() {
        let link = tilo_session_power::PowerLink::new(tilo_power_mgmt::PowerPolicy::default());
        assert!(
            held_by_apps(Some(&link)).is_empty(),
            "a machine whose apps hold nothing may log out at once"
        );
        assert!(
            held_by_apps(None).is_empty(),
            "a session with no power holds nothing"
        );
        let notes = link
            .registry()
            .acquire_for("Notes: 2 notes are not saved", INHIBIT_LOGOUT);
        let _writer = link
            .registry()
            .acquire_for("Writer: an article is not saved", INHIBIT_LOGOUT);
        let _download = link.registry().acquire_for(
            "Files: a download is running",
            tilo_power_mgmt::inhibit::INHIBIT_SUSPEND,
        );
        assert_eq!(
            held_by_apps(Some(&link)),
            vec![
                "Notes: 2 notes are not saved".to_owned(),
                "Writer: an article is not saved".to_owned(),
            ]
        );
        // An app that saved its work lets go, and is no longer asked about.
        drop(notes);
        assert_eq!(
            held_by_apps(Some(&link)),
            vec!["Writer: an article is not saved".to_owned()]
        );
    }

    /// An effect waits for the person only when an app holds something: with nothing held it is
    /// not asked about at all.
    #[test]
    fn an_effect_nobody_holds_anything_back_from_is_not_asked_about() {
        let state = PowerHoldState::default();
        let hold = Hold::default();
        assert!(!hold.ask(Effect::LogOut, &state, Vec::new()));
        assert!(!hold.asking(), "nothing waits for an answer");
        assert!(hold.ask(
            Effect::ShutDown,
            &state,
            vec!["Notes: 2 notes are not saved".to_owned()]
        ));
        assert!(hold.asking(), "the effect waits for the person's answer");
        assert_eq!(state.question().effect(), "shut-down");
        assert!(
            state
                .question()
                .apps()
                .contains(&"Notes: 2 notes are not saved".to_owned())
        );
    }
}
