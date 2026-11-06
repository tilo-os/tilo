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
pub const VERSION: u16 = 5;

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
    /// Keep the request of someone who asked at the door of a persistent
    /// workspace that takes people by invitation, without one: the address
    /// `contact` ([`crate::workspaces::contact_address`]) its answer goes to. The
    /// workspace keeps it on its wait list for whoever may invite. The
    /// child answers [`OperatorEvent::WaitListed`] or
    /// [`OperatorEvent::WaitListRefused`] for the same `request`.
    WaitList {
        /// The service's name for this command's answer (hex), so two
        /// visitors leaving one address are each answered.
        request: String,
        contact: String,
        /// SHA-256 (hex) of the secret the service hands the visitor
        /// ([`crate::workspaces::WAIT_LIST_REMOVAL_BYTES`]): whoever holds it takes
        /// the entry off again ([`OperatorCommand::Unlist`]). The secret
        /// itself never reaches the workspace.
        removal_sha256: String,
        /// SHA-256 (hex) of the secret the confirmation mail carries, when
        /// the service sends mail: the entry stays unconfirmed, and hidden
        /// from inviters, until it is confirmed ([`OperatorCommand::Confirm`]),
        /// and expires unconfirmed. Without one the entry is listed at once.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confirm_sha256: Option<String>,
        /// The language the visitor read the door in (`en`, `de`), for the
        /// mails the entry gets later.
        language: String,
    },
    /// Take the entry whose removal secret hashes to `removal_sha256` off
    /// the wait list, for whoever left it. The child answers
    /// [`OperatorEvent::Unlisted`] for the same `request` whether or not an
    /// entry had that secret, or [`OperatorEvent::WaitListFailed`].
    Unlist {
        /// The service's name for this command's answer (hex).
        request: String,
        /// SHA-256 (hex) of the removal secret.
        removal_sha256: String,
    },
    /// Confirm the entry whose confirmation secret hashes to
    /// `confirm_sha256`. The child answers [`OperatorEvent::Confirmed`] or
    /// [`OperatorEvent::WaitListFailed`] for the same `request`.
    Confirm {
        /// The service's name for this command's answer (hex).
        request: String,
        /// SHA-256 (hex) of the confirmation secret.
        confirm_sha256: String,
    },
    /// Where the entry whose removal secret hashes to `removal_sha256`
    /// stands, for whoever left it. The child answers [`OperatorEvent::Standing`]
    /// or [`OperatorEvent::WaitListFailed`] for the same `request`.
    Standing {
        /// The service's name for this command's answer (hex).
        request: String,
        /// SHA-256 (hex) of the removal secret.
        removal_sha256: String,
    },
    /// Acknowledge durable enqueue of an invitation, not provider delivery.
    InviteMailQueued { request: String, queued: bool },
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
    /// The workspace keeps the request of an [`OperatorCommand::WaitList`]:
    /// `added`, or it kept one for that address already.
    WaitListed {
        /// The command's `request`.
        request: String,
        added: bool,
        /// Mail the address the confirmation link now: the command carried
        /// a confirmation digest, and the address was not mailed within the
        /// hour.
        #[serde(default, skip_serializing_if = "is_false")]
        mail: bool,
    },
    /// An [`OperatorCommand::WaitList`] the workspace did not keep.
    WaitListRefused {
        /// The command's `request`.
        request: String,
        /// Why, for the operator's log.
        reason: String,
        /// The workspace keeps as many such requests as it takes already: the
        /// door says so, rather than that it failed.
        #[serde(default, skip_serializing_if = "is_false")]
        full: bool,
    },
    /// An [`OperatorCommand::Unlist`] was done: no entry has that removal
    /// secret now. Whether one had it is not said to whoever asked.
    Unlisted {
        /// The command's `request`.
        request: String,
        /// Where to tell that the address left the list: an entry had that
        /// secret, and its address had been confirmed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        told: Option<WaitListMailbox>,
        /// Relay-private SHA-256 of `workspaces::contact_key`, even for a pending
        /// entry, so the door can cancel its queued mail and receipts. Never
        /// returned by the door's public removal endpoint.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        contact_sha256: Option<String>,
    },
    /// An [`OperatorCommand::Unlist`], [`OperatorCommand::Confirm`] or
    /// [`OperatorCommand::Standing`] the workspace could not do.
    WaitListFailed {
        /// The command's `request`.
        request: String,
        /// Why, for the operator's log.
        reason: String,
    },
    /// The answer to an [`OperatorCommand::Confirm`]: whether an entry had
    /// that confirmation secret (it is listed now), or none did (it was
    /// taken off, or not confirmed in time).
    Confirmed {
        /// The command's `request`.
        request: String,
        confirmed: bool,
    },
    /// The answer to an [`OperatorCommand::Standing`].
    Standing {
        /// The command's `request`.
        request: String,
        standing: WaitListStanding,
    },
    /// Someone who may invite asked the workspace to mail an invitation to a
    /// confirmed wait-list entry: send it, from the workspace's door.
    InviteMail {
        request: String,
        to: WaitListMailbox,
        /// The invitation's web link, on the workspace's own door.
        link: String,
        /// A secret (hex) that takes the entry off the list, for the mail's
        /// removal link; the workspace keeps only its SHA-256.
        removal: String,
    },
}

/// Where a mail about a wait-list entry goes.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitListMailbox {
    /// The address, as typed.
    pub contact: String,
    /// The language the mail is in (`en` or `de`).
    pub language: String,
}

impl std::fmt::Debug for WaitListMailbox {
    /// Without the address, which no log names.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaitListMailbox")
            .field("language", &self.language)
            .finish_non_exhaustive()
    }
}

/// Where a request left at the door stands, for whoever left it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WaitListStanding {
    /// Left, its address not confirmed yet.
    Pending,
    /// On the list.
    Listed,
    /// On the list, and invited.
    Invited,
    /// Not on the list (taken off, joined, or never confirmed).
    Gone,
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
            OperatorEvent::WaitListed {
                request: "12".repeat(32),
                added: true,
                mail: true,
            },
            OperatorEvent::WaitListRefused {
                request: "12".repeat(32),
                reason: "the wait list is full".into(),
                full: true,
            },
            OperatorEvent::Unlisted {
                contact_sha256: None,
                request: "12".repeat(32),
                told: Some(WaitListMailbox {
                    contact: "Ada@example.org".into(),
                    language: "de".into(),
                }),
            },
            OperatorEvent::WaitListFailed {
                request: "12".repeat(32),
                reason: "disk full".into(),
            },
            OperatorEvent::Confirmed {
                request: "12".repeat(32),
                confirmed: true,
            },
            OperatorEvent::Standing {
                request: "12".repeat(32),
                standing: WaitListStanding::Invited,
            },
            OperatorEvent::InviteMail {
                request: "ab".repeat(32),
                to: WaitListMailbox {
                    contact: "Ada@example.org".into(),
                    language: "en".into(),
                },
                link: "https://tilo.example/?invite=AAEC".into(),
                removal: "cd".repeat(32),
            },
        ]
    }

    #[test]
    fn protocol_five_round_trips_and_pins_its_wire_form() {
        assert_eq!(VERSION, 5);
        let wait = OperatorCommand::WaitList {
            request: "12".repeat(32),
            contact: "Ada@example.org".into(),
            removal_sha256: "ab".repeat(32),
            confirm_sha256: Some("cd".repeat(32)),
            language: "de".into(),
        };
        assert_eq!(
            wait.to_line(),
            format!(
                "{{\"op\":\"wait_list\",\"request\":\"{}\",\"contact\":\"Ada@example.org\",\"removal_sha256\":\"{}\",\"confirm_sha256\":\"{}\",\"language\":\"de\"}}\n",
                "12".repeat(32),
                "ab".repeat(32),
                "cd".repeat(32)
            )
        );
        assert_eq!(OperatorCommand::from_line(&wait.to_line()).unwrap(), wait);
        let unlist = OperatorCommand::Unlist {
            request: "12".repeat(32),
            removal_sha256: "ab".repeat(32),
        };
        assert_eq!(
            unlist.to_line(),
            format!(
                "{{\"op\":\"unlist\",\"request\":\"{}\",\"removal_sha256\":\"{}\"}}\n",
                "12".repeat(32),
                "ab".repeat(32)
            )
        );
        assert_eq!(
            OperatorCommand::from_line(&unlist.to_line()).unwrap(),
            unlist
        );
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
    fn wait_list_confirmations_and_standings_round_trip_and_name_no_address_in_a_log() {
        for command in [
            OperatorCommand::InviteMailQueued {
                request: "12".repeat(32),
                queued: false,
            },
            OperatorCommand::InviteMailQueued {
                request: "12".repeat(32),
                queued: true,
            },
            OperatorCommand::Confirm {
                request: "12".repeat(32),
                confirm_sha256: "cd".repeat(32),
            },
            OperatorCommand::Standing {
                request: "12".repeat(32),
                removal_sha256: "ab".repeat(32),
            },
        ] {
            assert_eq!(
                OperatorCommand::from_line(&command.to_line()).unwrap(),
                command
            );
        }
        assert_eq!(
            OperatorEvent::Standing {
                request: "12".repeat(32),
                standing: WaitListStanding::Pending,
            }
            .to_line(),
            format!(
                "tilo-room-operator {{\"event\":\"standing\",\"request\":\"{}\",\"standing\":\"pending\"}}",
                "12".repeat(32)
            )
        );
        let mailbox = WaitListMailbox {
            contact: "Ada@example.org".into(),
            language: "en".into(),
        };
        assert!(
            !format!("{mailbox:?}").contains("Ada"),
            "a mailbox is never logged with its address"
        );
    }

    #[test]
    fn pending_removal_contact_digest_round_trips_only_on_the_private_operator_wire() {
        let event = OperatorEvent::Unlisted {
            request: "12".repeat(32),
            told: None,
            contact_sha256: Some("ab".repeat(32)),
        };
        assert_eq!(OperatorEvent::from_line(&event.to_line()).unwrap(), event);
        let old = format!(
            "tilo-room-operator {{\"event\":\"unlisted\",\"request\":\"{}\"}}",
            "12".repeat(32)
        );
        assert_eq!(
            OperatorEvent::from_line(&old).unwrap(),
            OperatorEvent::Unlisted {
                request: "12".repeat(32),
                told: None,
                contact_sha256: None,
            }
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
