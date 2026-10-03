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

/// # Errors
/// Returns the precise policy denial when the token creation facts are unsafe.
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

/// # Errors
/// Returns the precise policy denial when an activation request is stale or mismatched.
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
        // Phase `focused_recent_user_launch_is_allowed`.
        assert_eq!(evaluate_token_creation(valid()), Ok(()));
        // Phase `background_and_forged_requests_are_denied_independently`.
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
        // Phase `recent_serial_delivered_to_another_client_is_denied`.
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
        // Phase `stale_foreign_or_mismatched_target_is_denied`.
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
        // Phase `desktop_suffix_is_the_only_app_id_normalisation`.
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
}
