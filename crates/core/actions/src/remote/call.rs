//! The call lane an app's connection carries: one outside service's call, carried into a
//! workspace's call.
//!
//! A bridge app — a chat service's, a mail torrent's, any service whose call is not Tilo's —
//! holds a connection to the device and asks it to carry one call of its service into one of
//! the workspace's voice channels. The device joins the workspace's live session as the bridge,
//! publishes each outside person as a participant of their own, hands the app their frames and
//! the workspace's frames back, and holds everything to the call's rule. Nothing here names a
//! service: `service` is the app's own word for the one it reaches.

use serde::{Deserialize, Serialize};

/// The most bytes one Opus frame may carry: a 20 ms frame, which a bridge never re-encodes.
pub const MAX_CALL_AUDIO_BYTES: usize = 4096;
/// The longest a service's account or name may be, as a call's participants are shown.
pub const MAX_CALL_TEXT_BYTES: usize = 256;

/// One outside call and the workspace call it is carried into.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BridgedCall {
    /// The service's id of its call (a voice channel of its service).
    pub outside: String,
    /// The workspace's voice channel id it is one call with.
    pub tilo: String,
}

/// Who is in one outside call, as the service says.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutsideCall {
    /// The service's id of the call.
    pub call: String,
    /// Who is in it, the bridge's own account left out.
    #[serde(default)]
    pub people: Vec<OutsidePerson>,
}

/// One person in an outside call.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutsidePerson {
    /// Their account on the service.
    pub account: String,
    /// The name the service shows for them there.
    #[serde(default)]
    pub name: String,
    /// Where the service serves their picture, when it has one. A bridge that keeps it in the
    /// workspace's content store names the hash there instead, once it does.
    #[serde(default)]
    pub avatar_url: Option<String>,
    /// BLAKE3 hex of their picture, when the bridge kept it in the workspace's content store.
    #[serde(default)]
    pub avatar: Option<String>,
    /// Whether their microphone is off.
    #[serde(default)]
    pub muted: bool,
    /// Whether they hear nothing of the call. A service that deafens somebody mutes them for
    /// the call as well.
    #[serde(default)]
    pub deafened: bool,
}

/// The bridge's own place in the outside calls.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "link", rename_all = "kebab-case")]
pub enum CallLink {
    /// In no call.
    #[default]
    Idle,
    /// Joining `call`.
    Joining { call: String },
    /// In `call`, carrying audio both ways.
    Joined { call: String },
    /// The service will not let the bridge into `call`, for `reason` (a missing permission names
    /// it): said as it is, never retried until asked again.
    Refused { call: String, reason: String },
}

/// What the bridge says of its calls: who is in each, where it stands, and the line its people
/// see there about who is on the workspace's side.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallReport {
    #[serde(default)]
    pub calls: Vec<OutsideCall>,
    #[serde(default)]
    pub link: CallLink,
    /// What the neighbourhood of its own call shows in the service's own words: who here is
    /// listening, how many. Empty leaves the service's line as it was.
    #[serde(default)]
    pub status: String,
}

/// One Opus frame a person said, with the account it is theirs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallAudio {
    /// Their account on the service.
    pub speaker: String,
    /// The 20 ms Opus frame (48 kHz), untouched.
    pub opus: Vec<u8>,
}

/// What the device says about the call it carries, for the service's own line there.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallStatus {
    /// The outside call this is about, or none.
    #[serde(default)]
    pub call: Option<String>,
    /// The line to show there.
    #[serde(default)]
    pub status: Option<String>,
}

impl CallAudio {
    /// Whether this frame is one a bridge may carry: a speaker named, and a frame inside the
    /// bound.
    #[must_use]
    pub fn valid(&self) -> bool {
        !self.speaker.is_empty()
            && self.speaker.len() <= MAX_CALL_TEXT_BYTES
            && !self.opus.is_empty()
            && self.opus.len() <= MAX_CALL_AUDIO_BYTES
    }
}

impl CallReport {
    /// Whether this report is one a bridge may make.
    #[must_use]
    pub fn valid(&self) -> bool {
        self.calls.iter().all(|call| {
            !call.call.is_empty()
                && call.call.len() <= MAX_CALL_TEXT_BYTES
                && call.people.iter().all(|person| {
                    !person.account.is_empty()
                        && person.account.len() <= MAX_CALL_TEXT_BYTES
                        && !person.name.trim().is_empty()
                        && person.name.chars().count() <= 80
                        && person
                            .avatar_url
                            .as_deref()
                            .is_none_or(|url| url.len() <= 1024 && !url.trim().is_empty())
                        && person.avatar.as_deref().is_none_or(|hash| {
                            hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
                        })
                })
        })
    }
}

impl BridgedCall {
    /// Whether this is a pair a bridge may ask the device to carry.
    #[must_use]
    pub fn valid(&self) -> bool {
        !self.outside.is_empty()
            && self.outside.len() <= MAX_CALL_TEXT_BYTES
            && !self.tilo.is_empty()
            && self.tilo.len() <= MAX_CALL_TEXT_BYTES
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "in a test these are the assertion"
    )]

    use super::*;

    #[test]
    fn a_pair_names_two_calls_and_nothing_else() {
        assert!(
            BridgedCall {
                outside: "458".into(),
                tilo: "voice-tl".into()
            }
            .valid()
        );
        assert!(
            !BridgedCall {
                outside: String::new(),
                tilo: "voice-tl".into()
            }
            .valid()
        );
        assert!(
            !BridgedCall {
                outside: "x".repeat(MAX_CALL_TEXT_BYTES + 1),
                tilo: "voice-tl".into()
            }
            .valid()
        );
    }

    #[test]
    fn a_report_names_people_and_their_place() {
        let report = CallReport {
            calls: vec![OutsideCall {
                call: "458".into(),
                people: vec![OutsidePerson {
                    account: "7001".into(),
                    name: "Dana".into(),
                    avatar_url: Some("https://media.example/a.png".into()),
                    avatar: None,
                    muted: true,
                    deafened: false,
                }],
            }],
            link: CallLink::Joined { call: "458".into() },
            status: String::new(),
        };
        assert!(report.valid());
        let mut wrong = report.clone();
        wrong.calls[0].people[0].avatar = Some("not a hash".into());
        assert!(!wrong.valid(), "a kept picture is named by its BLAKE3 hex");
        let mut wrong = report;
        wrong.calls[0].people[0].account = String::new();
        assert!(!wrong.valid(), "a participant is named by an account");
    }

    #[test]
    fn a_frame_is_bounded_and_named() {
        let frame = CallAudio {
            speaker: "7001".into(),
            opus: vec![1, 2, 3],
        };
        assert!(frame.valid());
        assert!(
            !CallAudio {
                speaker: String::new(),
                opus: vec![1]
            }
            .valid()
        );
        assert!(
            !CallAudio {
                speaker: "7001".into(),
                opus: vec![0; MAX_CALL_AUDIO_BYTES + 1]
            }
            .valid()
        );
    }
}
