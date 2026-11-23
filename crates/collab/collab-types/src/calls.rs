//! What a workspace allows in each of its calls.
//!
//! A call is the people who say in their presence that they are in a voice
//! channel; what the call allows is a rule the workspace keeps per channel,
//! which the owner and whoever the workspace grants `call` change.
//! Every member's live session reads it: a call that was ended carries
//! nobody, and a person denied a call is neither heard in it nor listed in it.
//! A channel nobody changed allows everyone everything.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The longest channel id a call's row is kept under.
pub const MAX_CHANNEL_BYTES: usize = 128;

/// The most people one call denies anything.
pub const MAX_DENIED: usize = 128;

/// The tag a channel's call media is named by on the wire: the first 16 hex digits of the
/// channel id's BLAKE3 digest, so any channel id names a track and a relay finds the call's rule
/// from the name alone.
#[must_use]
pub fn tag(channel: &str) -> String {
    let digest = blake3::hash(channel.as_bytes()).to_hex();
    digest.as_str().get(..16).unwrap_or_default().to_owned()
}

/// What someone in a call does: speak (and show pictures) in it, and hear it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Directions {
    /// Speak and show pictures.
    pub publish: bool,
    /// Hear and see the others.
    pub subscribe: bool,
}

impl Directions {
    /// Both directions.
    pub const BOTH: Self = Self {
        publish: true,
        subscribe: true,
    };
    /// Neither direction.
    pub const NONE: Self = Self {
        publish: false,
        subscribe: false,
    };

    /// Whether this allows anything at all: someone allowed neither is not in the call.
    #[must_use]
    pub const fn any(self) -> bool {
        self.publish || self.subscribe
    }
}

impl Default for Directions {
    fn default() -> Self {
        Self::BOTH
    }
}

/// What a call allows, as the workspace keeps it for one channel.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRule {
    /// The call was ended: nobody is in it until someone restarts it.
    pub ended: bool,
    /// What everyone in it may do.
    pub allowed: Directions,
    /// What each person (their root as hex) may no longer do in it.
    pub denied: BTreeMap<String, Directions>,
}

impl CallRule {
    /// What `person` (their root as hex, when known) may do in the call now: nothing in an ended
    /// call, and what the call allows less what they were denied. Someone nobody named gets
    /// the call's own directions.
    #[must_use]
    pub fn may(&self, person: Option<&str>) -> Directions {
        if self.ended {
            return Directions::NONE;
        }
        let denied = person
            .and_then(|person| self.denied.get(person))
            .copied()
            .unwrap_or(Directions::NONE);
        Directions {
            publish: self.allowed.publish && !denied.publish,
            subscribe: self.allowed.subscribe && !denied.subscribe,
        }
    }

    /// This rule with `change` made, or why it cannot be.
    ///
    /// # Errors
    /// A change that would deny more people than one call keeps.
    pub fn changed(&self, change: &CallChange) -> Result<Self, String> {
        let mut rule = self.clone();
        match change {
            CallChange::Start { allowed } => {
                rule.ended = false;
                rule.allowed = *allowed;
            }
            CallChange::End => rule.ended = true,
            CallChange::Restart => rule.ended = false,
            CallChange::SetDirections { allowed } => rule.allowed = *allowed,
            CallChange::Deny { person, denied } => {
                let person = hex::encode(person);
                if denied.any() {
                    rule.denied.insert(person, *denied);
                } else {
                    rule.denied.remove(&person);
                }
                if rule.denied.len() > MAX_DENIED {
                    return Err(format!("a call denies {MAX_DENIED} people at most"));
                }
            }
            CallChange::Allow { person } => {
                rule.denied.remove(&hex::encode(person));
            }
        }
        Ok(rule)
    }
}

/// A change to what one call allows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CallChange {
    /// Open the call to everyone with `allowed`, restarting it when it was ended.
    Start { allowed: Directions },
    /// End the call for everyone in it.
    End,
    /// Let people into an ended call again, as it allowed before.
    Restart,
    /// What everyone in the call may do from now on.
    SetDirections { allowed: Directions },
    /// What `person` (their root) may no longer do in the call; denying neither lifts it.
    Deny {
        person: [u8; 32],
        denied: Directions,
    },
    /// Lift what `person` was denied.
    Allow { person: [u8; 32] },
}

/// Whether `channel` names a call's row: some text, not too long, no control characters.
///
/// # Errors
/// What is wrong with it.
pub fn validate_channel(channel: &str) -> Result<(), String> {
    if channel.is_empty()
        || channel.len() > MAX_CHANNEL_BYTES
        || channel.chars().any(char::is_control)
    {
        return Err(format!(
            "a call's channel is 1 to {MAX_CHANNEL_BYTES} bytes of text"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ended call allows nobody anything, a denial takes from one person what the call
    /// allows everyone, and restarting lets everyone back as the call allowed them.
    #[test]
    fn a_call_allows_what_its_moderators_left_it() {
        let bob = [0xb0; 32];
        let open = CallRule::default();
        assert_eq!(open.may(Some("ada")), Directions::BOTH);
        let denied = open
            .changed(&CallChange::Deny {
                person: bob,
                denied: Directions::BOTH,
            })
            .unwrap();
        assert_eq!(denied.may(Some(&hex::encode(bob))), Directions::NONE);
        assert_eq!(denied.may(Some("ada")), Directions::BOTH);
        let listening = denied
            .changed(&CallChange::SetDirections {
                allowed: Directions {
                    publish: false,
                    subscribe: true,
                },
            })
            .unwrap();
        assert!(!listening.may(Some("ada")).publish && listening.may(Some("ada")).subscribe);
        let ended = listening.changed(&CallChange::End).unwrap();
        assert_eq!(ended.may(Some("ada")), Directions::NONE);
        let restarted = ended.changed(&CallChange::Restart).unwrap();
        assert_eq!(restarted.may(Some("ada")), listening.may(Some("ada")));
        let allowed = restarted
            .changed(&CallChange::Allow { person: bob })
            .unwrap();
        assert!(allowed.may(Some(&hex::encode(bob))).subscribe);
    }
}
