//! The JSON-lines protocol between `tilo-room-service` and the
//! `tilo-relay room` child it runs for each short-lived room, or for its one
//! persistent room.
//!
//! The service writes one [`OperatorCommand`] per line to the child's stdin.
//! The child writes one [`RoomEvent`] per line to its stdout, each behind
//! [`LINE_PREFIX`], so nothing else the child prints can be mistaken for one.
//! Both binaries ship in one image, so the protocol changes in one commit;
//! [`VERSION`] is announced in [`RoomEvent::Started`] and a reader refuses
//! any other. Neither side skips a line it cannot decode: an unknown
//! command, event or field is an error.

use serde::{Deserialize, Serialize};

use crate::rooms::RoomRole;

/// Version of this protocol, announced in [`RoomEvent::Started`].
pub const VERSION: u16 = 2;

/// The child reports [`RoomEvent::Online`] at most this often, and only
/// when the numbers changed, so a busy room does not flood the service.
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

/// What the room service tells its room relay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperatorCommand {
    /// Certify the pending browser request that displays this pairing code.
    /// The child answers with [`RoomEvent::MemberCertified`] or
    /// [`RoomEvent::AdmitRefused`] for the same code.
    Admit {
        /// The six-digit code the guest's browser displays.
        pairing_code: String,
    },
    /// Pre-admit one guest of a persistent room: the child signs a
    /// pre-admission with the room's owner root for `token_sha256`, issues
    /// an invitation that only `user_public_key` can redeem, and answers
    /// [`RoomEvent::PreAdmitted`] or [`RoomEvent::PreAdmitRefused`] for the
    /// same digest.
    PreAdmit {
        /// The guest's user root (hex). Binding the invitation to it means a
        /// leaked invitation and token admit nobody else, and lets the
        /// service map a later [`RoomEvent::MemberRemoved`] back to the join.
        user_public_key: String,
        /// SHA-256 of the one-time admission token (hex).
        token_sha256: String,
        /// How the room's notices name the guest.
        label: String,
        role: RoomRole,
    },
    /// Certify an in-memory browser device as the persistent room's one
    /// read-only spectator person, so a page embedding the room can watch it
    /// without minting a member per visitor. The child answers
    /// [`RoomEvent::SpectatorCertified`] or [`RoomEvent::SpectateRefused`].
    Spectate {
        /// The browser's device key (hex).
        device_public_key: String,
    },
    /// Stop the room. A struct variant, so `deny_unknown_fields` holds for
    /// it too (serde ignores extra fields on a tagged unit variant).
    Shutdown {},
    /// Keep the request of someone who asked at the door of a persistent
    /// room that takes people by invitation, without one: the address
    /// `contact` ([`crate::rooms::contact_address`]) its answer goes to. The
    /// room keeps it on its wait list for whoever may invite. The
    /// child answers [`RoomEvent::WaitListed`] or
    /// [`RoomEvent::WaitListRefused`] for the same `request`.
    WaitList {
        /// The service's name for this command's answer (hex), so two
        /// visitors leaving one address are each answered.
        request: String,
        contact: String,
    },
}

/// What the room relay reports to the room service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoomEvent {
    /// The relay serves browsers. Always the first event.
    Started {
        /// [`VERSION`] of the child.
        version: u16,
        /// `https://host:port` of the browser listener.
        origin: String,
        /// The plain loopback listener, if any.
        plain_origin: Option<String>,
        /// The room's throwaway user root (hex).
        user_public_key: String,
        /// SHA-256 of a development certificate, for WebTransport pinning.
        certificate_sha256: Option<String>,
        /// Page origins allowed to call the room cross-origin.
        allowed_origins: Vec<String>,
        /// The governed room's UUID; `None` for a short-lived room.
        room: Option<String>,
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
    /// The invitation for a [`OperatorCommand::PreAdmit`].
    PreAdmitted {
        /// The pre-admission's token digest (hex).
        token_sha256: String,
        /// The signed invitation, unpadded base64url.
        invitation: String,
    },
    /// A [`OperatorCommand::PreAdmit`] that issued nothing. Answers the
    /// waiting join at once instead of letting it time out.
    PreAdmitRefused {
        /// The pre-admission's token digest (hex).
        token_sha256: String,
        /// Why, for the operator's log.
        reason: String,
        /// The person was removed from the room within the day: the door
        /// tells them so, as it tells anyone from the address they joined from.
        removed: bool,
        /// The room takes people by invitation only and the person is not a
        /// member: the door tells them they need an invitation.
        #[serde(default, skip_serializing_if = "is_false")]
        invite_only: bool,
    },
    /// The certificate for a [`OperatorCommand::Spectate`].
    SpectatorCertified {
        /// The certified device key (hex).
        device_public_key: String,
        /// The device certificate, in the form the browser runtime takes.
        certificate: tilo_user_identity::DeviceCert,
    },
    /// A [`OperatorCommand::Spectate`] that certified nothing.
    SpectateRefused {
        /// The device key (hex).
        device_public_key: String,
        /// Why, for the operator's log.
        reason: String,
    },
    /// Who has an open browser session now.
    Online {
        /// Distinct people (user roots), the spectator person excluded.
        people: u32,
        /// Open spectator sessions.
        spectators: u32,
        /// Some of the members online, for the site's directory of public
        /// workspaces (at most [`crate::directory::MAX_FACES`]).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        here: Vec<crate::directory::Face>,
        /// How many people belong to the room.
        #[serde(default, skip_serializing_if = "is_zero")]
        members: u32,
    },
    /// A member was removed from the persistent room.
    MemberRemoved {
        /// Their user root (hex).
        user_public_key: String,
        /// The removal is a ban.
        banned: bool,
    },
    /// The room keeps the request of an [`OperatorCommand::WaitList`]:
    /// `added`, or it kept one for that address already.
    WaitListed {
        /// The command's `request`.
        request: String,
        added: bool,
    },
    /// An [`OperatorCommand::WaitList`] the room did not keep.
    WaitListRefused {
        /// The command's `request`.
        request: String,
        /// Why, for the operator's log.
        reason: String,
        /// The room keeps as many such requests as it takes already: the
        /// door says so, rather than that it failed.
        #[serde(default, skip_serializing_if = "is_false")]
        full: bool,
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

impl RoomEvent {
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

/// Whether a flag is unset, which the line leaves out.
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the field by reference"
)]
fn is_false(flag: &bool) -> bool {
    !*flag
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events() -> Vec<RoomEvent> {
        vec![
            RoomEvent::Started {
                version: VERSION,
                origin: "https://127.0.0.1:9443".into(),
                plain_origin: Some("http://localhost:9543".into()),
                user_public_key: "ab".repeat(32),
                certificate_sha256: None,
                allowed_origins: vec!["https://rooms.example".into()],
                room: None,
            },
            RoomEvent::MemberCertified {
                device: "cd".repeat(32),
                label: "Firefox on Linux".into(),
                pairing_code: "012345".into(),
            },
            RoomEvent::AdmitRefused {
                pairing_code: "012345".into(),
                reason: RefusalReason::NoMatchingRequest,
            },
            RoomEvent::AdmitRefused {
                pairing_code: "012345".into(),
                reason: RefusalReason::AmbiguousCode,
            },
            RoomEvent::AdmitRefused {
                pairing_code: "012345".into(),
                reason: RefusalReason::PolicyDenied,
            },
        ]
    }

    fn v2_events() -> Vec<RoomEvent> {
        vec![
            RoomEvent::Started {
                version: VERSION,
                origin: "https://rooms.example".into(),
                plain_origin: None,
                user_public_key: "ab".repeat(32),
                certificate_sha256: None,
                allowed_origins: vec![],
                room: Some("6f1c3b0e-58a1-4c2e-9d55-0b3c1f6a2e10".into()),
            },
            RoomEvent::PreAdmitted {
                token_sha256: "ab".repeat(32),
                invitation: "AAECAw".into(),
            },
            RoomEvent::PreAdmitRefused {
                token_sha256: "ab".repeat(32),
                reason: "too many pre-admissions are open".into(),
                removed: false,
                invite_only: false,
            },
            RoomEvent::PreAdmitRefused {
                token_sha256: "ab".repeat(32),
                reason: "the room takes people by invitation only".into(),
                removed: false,
                invite_only: true,
            },
            RoomEvent::SpectatorCertified {
                device_public_key: "ef".repeat(32),
                certificate: tilo_user_identity::DeviceCert {
                    user_public_key: [1; 32],
                    device_public_key: [0xef; 32],
                    device_label: "Watching visitor".into(),
                    capabilities: tilo_user_identity::DeviceCapabilities::member(),
                    issued_at_ns: 1,
                    expires_at_ns: Some(2),
                    signature: [3; 64],
                },
            },
            RoomEvent::SpectateRefused {
                device_public_key: "ef".repeat(32),
                reason: "no".into(),
            },
            RoomEvent::Online {
                people: 3,
                spectators: 9,
                here: vec![crate::directory::Face {
                    user_public_key: "cd".repeat(32),
                    name: "Ada".into(),
                }],
                members: 40,
            },
            RoomEvent::MemberRemoved {
                user_public_key: "cd".repeat(32),
                banned: true,
            },
            RoomEvent::WaitListed {
                request: "12".repeat(32),
                added: true,
            },
            RoomEvent::WaitListRefused {
                request: "12".repeat(32),
                reason: "the wait list is full".into(),
                full: true,
            },
        ]
    }

    #[test]
    fn protocol_two_round_trips_and_pins_its_wire_form() {
        assert_eq!(VERSION, 2);
        let pre_admit = OperatorCommand::PreAdmit {
            user_public_key: "cd".repeat(32),
            token_sha256: "ab".repeat(32),
            label: "Ada".into(),
            role: RoomRole::Editor,
        };
        assert_eq!(
            pre_admit.to_line(),
            format!(
                "{{\"op\":\"pre_admit\",\"user_public_key\":\"{}\",\"token_sha256\":\"{}\",\"label\":\"Ada\",\"role\":\"Editor\"}}\n",
                "cd".repeat(32),
                "ab".repeat(32)
            )
        );
        assert_eq!(
            OperatorCommand::from_line(&pre_admit.to_line()).unwrap(),
            pre_admit
        );
        let spectate = OperatorCommand::Spectate {
            device_public_key: "ef".repeat(32),
        };
        assert_eq!(
            OperatorCommand::from_line(&spectate.to_line()).unwrap(),
            spectate
        );
        let wait = OperatorCommand::WaitList {
            request: "12".repeat(32),
            contact: "ada@example.org".into(),
        };
        assert_eq!(
            wait.to_line(),
            format!(
                "{{\"op\":\"wait_list\",\"request\":\"{}\",\"contact\":\"ada@example.org\"}}\n",
                "12".repeat(32)
            )
        );
        assert_eq!(OperatorCommand::from_line(&wait.to_line()).unwrap(), wait);
        for event in v2_events() {
            assert_eq!(RoomEvent::from_line(&event.to_line()).unwrap(), event);
        }
        assert_eq!(
            RoomEvent::Online {
                people: 7,
                spectators: 2,
                here: Vec::new(),
                members: 0,
            }
            .to_line(),
            "tilo-room-operator {\"event\":\"online\",\"people\":7,\"spectators\":2}"
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
    fn room_operator_round_trip() {
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
            assert_eq!(RoomEvent::from_line(&line).unwrap(), event);
        }
    }

    #[test]
    fn room_operator_wire_form_is_pinned() {
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
            RoomEvent::AdmitRefused {
                pairing_code: "012345".into(),
                reason: RefusalReason::AmbiguousCode,
            }
            .to_line(),
            "tilo-room-operator {\"event\":\"admit_refused\",\"pairing_code\":\"012345\",\"reason\":\"ambiguous_code\"}"
        );
    }

    #[test]
    fn room_operator_rejects_unknown_tags_fields_and_the_old_text_form() {
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
                    RoomEvent::from_line(&format!("{LINE_PREFIX}{json}")),
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
            RoomEvent::from_line(&bare),
            Err(LineError::NotProtocol)
        ));
        assert!(matches!(
            RoomEvent::from_line("2026-10-08T00:00:00Z WARN relay: {\"userPublicKey\":\"ab\"}"),
            Err(LineError::NotProtocol)
        ));
    }
}
