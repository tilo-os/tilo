//! `xdg_activation_v1` focus-stealing policy.
//!
//! Tokens are capabilities minted from a recent user interaction.  A client
//! may hand that capability to a process it launched, but a background client
//! cannot mint one from an old serial or an unfocused surface.  Keeping the
//! decision pure makes the security boundary deterministic to test; Smithay's
//! protocol handler in [`crate::compositor::state`] supplies the live facts.

use std::time::Duration;

/// Mainstream launch latency fits comfortably inside this window while old
/// tokens stop being a standing permission to steal focus.
pub const MAX_ACTIVATION_AGE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationDenial {
    Expired,
    UnknownSerial,
    WrongSeat,
    MissingSource,
    WrongSourceClient,
    SourceNotFocused,
    UnknownTarget,
    AppIdMismatch,
}

/// Facts that do not require protocol objects. `app_id_matches` is true when
/// no app id was supplied: the token itself is still an unguessable,
/// single-use capability tied to its focused source and input serial.
#[allow(
    clippy::struct_excessive_bools,
    reason = "Each independently auditable protocol fact maps to a distinct activation-denial reason; collapsing them would obscure the security decision recorded in logs and tests."
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivationFacts {
    pub age: Duration,
    pub serial_belongs_to_source: bool,
    pub seat_is_owned: bool,
    pub source_exists: bool,
    pub source_belongs_to_creator: bool,
    pub source_was_focused: bool,
}

pub fn evaluate_token_creation(facts: ActivationFacts) -> Result<(), ActivationDenial> {
    if facts.age > MAX_ACTIVATION_AGE {
        return Err(ActivationDenial::Expired);
    }
    if !facts.serial_belongs_to_source {
        return Err(ActivationDenial::UnknownSerial);
    }
    if !facts.seat_is_owned {
        return Err(ActivationDenial::WrongSeat);
    }
    if !facts.source_exists {
        return Err(ActivationDenial::MissingSource);
    }
    if !facts.source_belongs_to_creator {
        return Err(ActivationDenial::WrongSourceClient);
    }
    if !facts.source_was_focused {
        return Err(ActivationDenial::SourceNotFocused);
    }
    Ok(())
}

pub fn evaluate_activation_request(
    age: Duration,
    target_is_toplevel: bool,
    app_id_matches: bool,
) -> Result<(), ActivationDenial> {
    if age > MAX_ACTIVATION_AGE {
        return Err(ActivationDenial::Expired);
    }
    if !target_is_toplevel {
        return Err(ActivationDenial::UnknownTarget);
    }
    if !app_id_matches {
        return Err(ActivationDenial::AppIdMismatch);
    }
    Ok(())
}

/// The protocol uses a desktop-file id while some launchers include its
/// conventional suffix. They name the same application; no fuzzy or
/// case-insensitive matching is done.
pub fn app_ids_match(requested: &str, actual: &str) -> bool {
    fn canonical(id: &str) -> &str {
        id.strip_suffix(".desktop").unwrap_or(id)
    }
    !requested.is_empty() && canonical(requested) == canonical(actual)
}

/// The person's own presses (a click, a key, a tap) delivered to an app, by
/// the Wayland client that received them: the same input serials an
/// `xdg_activation_v1` token is minted from ([`evaluate_token_creation`]),
/// recorded as they are delivered so that an app's request to open
/// something (a link, a file, another app's action) can show it follows the
/// person's input. Input an agent or a remote participant sends through the
/// shared seat is never recorded here. Each act proves one open: taking it
/// uses it up, so an app that keeps opening things after the person's click
/// opens the rest aside.
#[derive(Default)]
pub struct PersonInput {
    acts: std::sync::Mutex<std::collections::VecDeque<(u32, std::time::Instant)>>,
}

/// How many recent presses are kept: enough for every client the person
/// can reach within [`MAX_ACTIVATION_AGE`].
const MAX_PERSON_ACTS: usize = 64;

impl PersonInput {
    /// The session's record, shared by the compositor thread that delivers
    /// input and the threads that serve apps' open requests.
    pub fn session() -> &'static Self {
        static SESSION: std::sync::OnceLock<PersonInput> = std::sync::OnceLock::new();
        SESSION.get_or_init(Self::default)
    }

    /// The person pressed something in a surface of client `client` at `at`.
    pub fn note(&self, client: u32, at: std::time::Instant) {
        let Ok(mut acts) = self.acts.lock() else {
            return;
        };
        acts.retain(|(seen, when)| {
            *seen != client && at.saturating_duration_since(*when) <= MAX_ACTIVATION_AGE
        });
        acts.push_back((client, at));
        while acts.len() > MAX_PERSON_ACTS {
            let _ = acts.pop_front();
        }
    }

    /// Whether one of `clients` received the person's press within
    /// [`MAX_ACTIVATION_AGE`] of `now`; taking it uses it up.
    pub fn take(&self, clients: &[u32], now: std::time::Instant) -> bool {
        let Ok(mut acts) = self.acts.lock() else {
            return false;
        };
        acts.retain(|(_, when)| now.saturating_duration_since(*when) <= MAX_ACTIVATION_AGE);
        let before = acts.len();
        acts.retain(|(client, _)| !clients.contains(client));
        acts.len() != before
    }
}

/// Whether the person's own input is behind a request app `app_id` (a
/// package id, or a host application's window identity) makes now: a
/// press reached one of its windows in `tree` within the activation window
/// and has not been used for another open. Uses it up when it has.
pub fn person_initiated(tree: &crate::shell::state::WorkspaceTree, app_id: &str) -> bool {
    let clients: Vec<u32> = tree
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .filter_map(|tab| match &tab.tile {
            crate::shell::state::TileContent::Foreign {
                app_id: window,
                surface_id,
            } if crate::shell::launcher::app_id_matches_package(window, app_id)
                || app_ids_match(app_id, window) =>
            {
                Some(crate::compositor::state::client_index_of(*surface_id))
            }
            _ => None,
        })
        .collect();
    !clients.is_empty() && PersonInput::session().take(&clients, std::time::Instant::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> ActivationFacts {
        ActivationFacts {
            age: Duration::from_millis(200),
            serial_belongs_to_source: true,
            seat_is_owned: true,
            source_exists: true,
            source_belongs_to_creator: true,
            source_was_focused: true,
        }
    }

    #[test]
    fn activation_is_granted_only_for_a_fresh_focused_user_launch() {
        assert_eq!(evaluate_token_creation(valid()), Ok(()));
        {
            let cases = [
                (
                    ActivationFacts {
                        age: Duration::from_secs(11),
                        ..valid()
                    },
                    ActivationDenial::Expired,
                ),
                (
                    ActivationFacts {
                        serial_belongs_to_source: false,
                        ..valid()
                    },
                    ActivationDenial::UnknownSerial,
                ),
                (
                    ActivationFacts {
                        seat_is_owned: false,
                        ..valid()
                    },
                    ActivationDenial::WrongSeat,
                ),
                (
                    ActivationFacts {
                        source_exists: false,
                        ..valid()
                    },
                    ActivationDenial::MissingSource,
                ),
                (
                    ActivationFacts {
                        source_belongs_to_creator: false,
                        ..valid()
                    },
                    ActivationDenial::WrongSourceClient,
                ),
                (
                    ActivationFacts {
                        source_was_focused: false,
                        ..valid()
                    },
                    ActivationDenial::SourceNotFocused,
                ),
            ];
            for (facts, denial) in cases {
                assert_eq!(evaluate_token_creation(facts), Err(denial));
            }
        }
        {
            let facts = ActivationFacts {
                serial_belongs_to_source: false,
                ..valid()
            };
            assert_eq!(
                evaluate_token_creation(facts),
                Err(ActivationDenial::UnknownSerial)
            );
        }
        assert_eq!(
            evaluate_activation_request(Duration::from_secs(11), true, true),
            Err(ActivationDenial::Expired)
        );
        assert_eq!(
            evaluate_activation_request(Duration::from_millis(5), false, true),
            Err(ActivationDenial::UnknownTarget)
        );
        assert_eq!(
            evaluate_activation_request(Duration::from_millis(5), true, false),
            Err(ActivationDenial::AppIdMismatch)
        );
        assert!(app_ids_match(
            "org.example.Editor.desktop",
            "org.example.Editor"
        ));
        assert!(app_ids_match(
            "org.example.Editor",
            "org.example.Editor.desktop"
        ));
        assert!(!app_ids_match("org.example.editor", "org.example.Editor"));
        assert!(!app_ids_match("", "org.example.Editor"));
    }

    // A press of the person's in an app proves one open of that app's,
    // within the activation window; an app with no press of the person's, a
    // stale one or one already used opens aside. (The interim proof, until
    // activation tokens travel with the requests.)
    #[test]
    fn a_press_of_the_persons_proves_one_open_of_that_app_and_no_other() {
        let record = PersonInput::default();
        let start = std::time::Instant::now();
        let at = |ms: u64| {
            start
                .checked_add(Duration::from_millis(ms))
                .expect("a near instant")
        };
        record.note(7, at(0));
        assert!(!record.take(&[8], at(10)), "another app had no press");
        assert!(record.take(&[7], at(20)), "the app the person pressed in");
        assert!(!record.take(&[7], at(30)), "one press proves one open");
        record.note(7, at(40));
        assert!(
            !record.take(
                &[7],
                at(41).checked_add(MAX_ACTIVATION_AGE).expect("an instant")
            ),
            "a press older than the activation window proves nothing"
        );
    }
}
