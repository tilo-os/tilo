//! The JSON-lines protocol between a workspace's operator and the
//! `tilo-relay room` child it runs for it — a short-lived workspace, or one
//! persistent workspace.
//!
//! The service writes one [`OperatorCommand`] per line to the child's stdin.
//! The child writes one [`OperatorEvent`] per line to its stdout, each behind
//! [`LINE_PREFIX`], so nothing else the child prints can be mistaken for one.
//! Operator and workspace relay are built from one tree, so the protocol changes
//! in one commit;
//! [`VERSION`] is announced in [`OperatorEvent::Started`] and a reader refuses
//! any other. Neither side skips a line it cannot decode: an unknown
//! command, event or field is an error.

use serde::{Deserialize, Serialize};

/// Version of this protocol, announced in [`OperatorEvent::Started`].
pub const VERSION: u16 = 6;

/// The child reports [`OperatorEvent::Online`] at most this often, and only
/// when the numbers changed, so a busy workspace does not flood the service.
pub const ONLINE_EVERY_SECS: u64 = 2;

/// Decodes a 32-byte key or digest written as 64 lowercase hex characters,
/// the only form this protocol carries.
#[must_use]
pub fn hex32(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    hex::decode(text).ok()?.try_into().ok()
}

/// Prefix of every protocol line on the child's stdout.
pub const LINE_PREFIX: &str = "tilo-room-operator ";

/// How long the child holds an admitted pairing code with no matching
/// certification request before it answers [`RefusalReason::NoMatchingRequest`].
pub const ADMIT_WAIT_SECS: u64 = 60;

/// What the workspace service tells its workspace relay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperatorCommand {
    /// Certify the pending browser request that displays this pairing code.
    /// The child answers with [`OperatorEvent::MemberCertified`] or
    /// [`OperatorEvent::AdmitRefused`] for the same code.
    Admit {
        /// The six-digit code the guest's browser displays.
        pairing_code: String,
    },
    /// Stop the workspace. A struct variant, so `deny_unknown_fields` holds for
    /// it too (serde ignores extra fields on a tagged unit variant).
    Shutdown {},
}

/// What the workspace relay reports to the workspace service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperatorEvent {
    /// The relay serves browsers. Always the first event.
    Started {
        /// [`VERSION`] of the child.
        version: u16,
        /// `https://host:port` of the browser listener.
        origin: String,
        /// The plain loopback listener, if any.
        plain_origin: Option<String>,
        /// The workspace's throwaway user root (hex).
        user_public_key: String,
        /// SHA-256 of a development certificate, for a client that pins it.
        certificate_sha256: Option<String>,
        /// Page origins allowed to call the workspace cross-origin.
        allowed_origins: Vec<String>,
        /// The governed workspace's UUID; `None` for a short-lived workspace.
        #[serde(rename = "room")]
        workspace: Option<String>,
    },
    /// The browser that displayed `pairing_code` was certified.
    MemberCertified {
        /// The certified device key (hex).
        device: String,
        /// The device label the browser asked for.
        label: String,
        pairing_code: String,
    },
    /// An admitted pairing code certified nobody.
    AdmitRefused {
        /// The admitted pairing code.
        pairing_code: String,
        /// Why it certified nobody.
        reason: RefusalReason,
    },
    /// Who has an open browser session now.
    Online {
        /// Distinct people (user roots) with an open browser session.
        people: u32,
        /// Some of the members online, for the site's directory of public
        /// workspaces (at most [`crate::directory::MAX_FACES`]).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        here: Vec<crate::directory::Face>,
        /// How many people belong to the workspace.
        #[serde(default, skip_serializing_if = "is_zero")]
        members: u32,
        /// Every person counted in `people` (user roots, hex): the door
        /// seats people, so a person already here is already seated, and
        /// another device of theirs coming in takes no seat a newcomer
        /// could have.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        online: Vec<String>,
    },
    /// A member was removed from the persistent workspace.
    MemberRemoved {
        /// Their user root (hex).
        user_public_key: String,
        /// The removal is a ban.
        banned: bool,
    },
}

/// Why an admitted pairing code certified nobody.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    /// No pending request displayed the code within [`ADMIT_WAIT_SECS`].
    NoMatchingRequest,
    /// More than one pending request displayed the code; none was certified,
    /// since the operator cannot tell which one the guest meant.
    AmbiguousCode,
    /// The certification policy refused the matching request.
    PolicyDenied,
}

impl RefusalReason {
    /// A sentence for the guest.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::NoMatchingRequest => "no device showed this pairing code in time",
            Self::AmbiguousCode => {
                "more than one device showed this pairing code; reload the page and try again"
            }
            Self::PolicyDenied => "the room refused to certify this device",
        }
    }
}

/// Why a protocol line could not be read.
#[derive(Debug, thiserror::Error)]
pub enum LineError {
    /// A stdout line without [`LINE_PREFIX`].
    #[error("not a room operator line")]
    NotProtocol,
    /// A line that does not decode.
    #[error("malformed room operator line: {0}")]
    Malformed(#[from] serde_json::Error),
}

impl OperatorCommand {
    /// The command as one stdin line, newline included.
    #[must_use]
    pub fn to_line(&self) -> String {
        // Every variant is plain strings, which always serialise.
        let mut line = serde_json::to_string(self).unwrap_or_default();
        line.push('\n');
        line
    }

    /// Decodes one stdin line.
    ///
    /// # Errors
    ///
    /// [`LineError::Malformed`] for anything that is not exactly one command.
    pub fn from_line(line: &str) -> Result<Self, LineError> {
        Ok(serde_json::from_str(line.trim())?)
    }
}

impl OperatorEvent {
    /// The event as one stdout line, prefix included and newline excluded
    /// (`println!` adds it).
    #[must_use]
    pub fn to_line(&self) -> String {
        // Every variant is plain strings and numbers, which always serialise.
        format!(
            "{LINE_PREFIX}{}",
            serde_json::to_string(self).unwrap_or_default()
        )
    }

    /// Decodes one stdout line.
    ///
    /// # Errors
    ///
    /// [`LineError::NotProtocol`] for a line without [`LINE_PREFIX`] (a
    /// reader passes over those), [`LineError::Malformed`] for a protocol
    /// line that does not decode (a reader must not pass over those).
    pub fn from_line(line: &str) -> Result<Self, LineError> {
        let json = line
            .trim_end()
            .strip_prefix(LINE_PREFIX)
            .ok_or(LineError::NotProtocol)?;
        Ok(serde_json::from_str(json)?)
    }
}

/// Whether a count is nothing, which the line leaves out.
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the field by reference"
)]
fn is_zero(count: &u32) -> bool {
    *count == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events() -> Vec<OperatorEvent> {
        vec![
            OperatorEvent::Started {
                version: VERSION,
                origin: "https://127.0.0.1:9443".into(),
                plain_origin: Some("http://localhost:9543".into()),
                user_public_key: "ab".repeat(32),
                certificate_sha256: None,
                allowed_origins: vec!["https://rooms.example".into()],
                workspace: None,
            },
            OperatorEvent::MemberCertified {
                device: "cd".repeat(32),
                label: "Firefox on Linux".into(),
                pairing_code: "012345".into(),
            },
            OperatorEvent::AdmitRefused {
                pairing_code: "012345".into(),
                reason: RefusalReason::NoMatchingRequest,
            },
            OperatorEvent::AdmitRefused {
                pairing_code: "012345".into(),
                reason: RefusalReason::AmbiguousCode,
            },
            OperatorEvent::AdmitRefused {
                pairing_code: "012345".into(),
                reason: RefusalReason::PolicyDenied,
            },
        ]
    }

    fn current_events() -> Vec<OperatorEvent> {
        vec![
            OperatorEvent::Started {
                version: VERSION,
                origin: "https://rooms.example".into(),
                plain_origin: None,
                user_public_key: "ab".repeat(32),
                certificate_sha256: None,
                allowed_origins: vec![],
                workspace: Some("6f1c3b0e-58a1-4c2e-9d55-0b3c1f6a2e10".into()),
            },
            OperatorEvent::Online {
                people: 3,
                here: vec![crate::directory::Face {
                    user_public_key: "cd".repeat(32),
                    name: "Ada".into(),
                }],
                members: 40,
                online: vec!["cd".repeat(32), "ab".repeat(32)],
            },
            OperatorEvent::MemberRemoved {
                user_public_key: "cd".repeat(32),
                banned: true,
            },
        ]
    }

    #[test]
    fn protocol_six_round_trips_and_pins_its_wire_form() {
        assert_eq!(VERSION, 6);
        for event in current_events() {
            assert_eq!(OperatorEvent::from_line(&event.to_line()).unwrap(), event);
        }
        assert_eq!(
            OperatorEvent::Online {
                people: 7,
                here: Vec::new(),
                members: 0,
                online: Vec::new(),
            }
            .to_line(),
            "tilo-room-operator {\"event\":\"online\",\"people\":7}"
        );
    }

    #[test]
    fn hex32_accepts_exactly_sixty_four_lowercase_hex_characters() {
        assert_eq!(hex32(&"ab".repeat(32)), Some([0xab; 32]));
        assert_eq!(hex32(&"AB".repeat(32)), None);
        assert_eq!(hex32(&"ab".repeat(31)), None);
        assert_eq!(hex32(&format!("{} ", "ab".repeat(32))), None);
    }

    #[test]
    fn operator_round_trip() {
        for command in [
            OperatorCommand::Admit {
                pairing_code: "012345".into(),
            },
            OperatorCommand::Shutdown {},
        ] {
            let line = command.to_line();
            assert!(line.ends_with('\n') && !line.trim_end().contains('\n'));
            assert_eq!(OperatorCommand::from_line(&line).unwrap(), command);
        }
        for event in events() {
            let line = event.to_line();
            assert!(line.starts_with(LINE_PREFIX) && !line.contains('\n'));
            assert_eq!(OperatorEvent::from_line(&line).unwrap(), event);
        }
    }

    #[test]
    fn operator_wire_form_is_pinned() {
        assert_eq!(
            OperatorCommand::Admit {
                pairing_code: "012345".into()
            }
            .to_line(),
            "{\"op\":\"admit\",\"pairing_code\":\"012345\"}\n"
        );
        assert_eq!(
            OperatorCommand::Shutdown {}.to_line(),
            "{\"op\":\"shutdown\"}\n"
        );
        assert_eq!(
            OperatorEvent::AdmitRefused {
                pairing_code: "012345".into(),
                reason: RefusalReason::AmbiguousCode,
            }
            .to_line(),
            "tilo-room-operator {\"event\":\"admit_refused\",\"pairing_code\":\"012345\",\"reason\":\"ambiguous_code\"}"
        );
    }

    #[test]
    fn operator_rejects_unknown_tags_fields_and_the_old_text_form() {
        for line in [
            "admit 012345",
            "shutdown",
            "{\"op\":\"admit\",\"pairingCode\":\"012345\"}",
            "{\"op\":\"admit\",\"pairing_code\":\"012345\",\"extra\":1}",
            "{\"op\":\"kick\",\"pairing_code\":\"012345\"}",
            "{\"op\":\"shutdown\",\"now\":true}",
            "{\"op\":\"pre_admit\",\"user_public_key\":\"aa\",\"token_sha256\":\"bb\",\"label\":\"x\",\"role\":\"Owner\"}",
            "{\"op\":\"pre_admit\",\"token_sha256\":\"bb\",\"label\":\"x\",\"role\":\"Editor\"}",
            "{\"op\":\"spectate\"}",
            "",
        ] {
            assert!(
                matches!(
                    OperatorCommand::from_line(line),
                    Err(LineError::Malformed(_))
                ),
                "{line:?} decoded"
            );
        }
        for json in [
            "{\"event\":\"member_certified\",\"device\":\"cd\",\"label\":\"x\",\"pairingCode\":\"012345\"}",
            "{\"event\":\"member_certified\",\"device\":\"cd\",\"label\":\"x\",\"pairing_code\":\"1\",\"at\":1}",
            "{\"event\":\"member_left\",\"device\":\"cd\"}",
            "{\"event\":\"admit_refused\",\"pairing_code\":\"1\",\"reason\":\"tired\"}",
            "{\"userPublicKey\":\"ab\"}",
        ] {
            assert!(
                matches!(
                    OperatorEvent::from_line(&format!("{LINE_PREFIX}{json}")),
                    Err(LineError::Malformed(_))
                ),
                "{json:?} decoded"
            );
        }
    }

    #[test]
    fn a_line_without_the_prefix_is_not_a_protocol_line() {
        let started = events().remove(0);
        let bare = started.to_line().replacen(LINE_PREFIX, "", 1);
        assert!(matches!(
            OperatorEvent::from_line(&bare),
            Err(LineError::NotProtocol)
        ));
        assert!(matches!(
            OperatorEvent::from_line("2026-10-08T00:00:00Z WARN relay: {\"userPublicKey\":\"ab\"}"),
            Err(LineError::NotProtocol)
        ));
    }
}
