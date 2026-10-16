//! Transport-neutral application contract for active realtime-session handoff.
//!
//! Resume intents and lifecycle events contain semantic room/control state
//! only. An `Accept` command may additionally carry bounded opaque
//! target-local capture authorizations; they cross only verified local IPC and
//! never enter continuation state or the device fabric. Capture objects,
//! device identities, transport tickets, credentials, and membership
//! assertions have no representation. The local shell binds each command to
//! the certified app/workspace route and owns all device routing.

use serde::{Deserialize, Serialize};

pub const MAX_REALTIME_WIRE_BYTES: usize = 8 * 1024;
pub const MAX_REALTIME_TOKEN_BYTES: usize = 128;
pub const MAX_CAPTURE_AUTHORIZATIONS: usize = 4;
pub const OPAQUE_CAPTURE_AUTHORIZATION_BYTES: usize = 32;

pub use collab_types::realtime::{RealtimeControlIntent, RealtimeMediaFacets};

/// Safe app projection registered with the local broker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealtimeResumeIntent {
    pub provider: String,
    pub room_id: String,
    pub semantic_position: Option<String>,
    pub media: RealtimeMediaFacets,
    pub controls: RealtimeControlIntent,
}

impl RealtimeResumeIntent {
    #[must_use]
    pub fn valid(&self) -> bool {
        portable_identifier(&self.provider, 128)
            && portable_identifier(&self.room_id, 512)
            && self
                .semantic_position
                .as_deref()
                .is_none_or(|value| portable_identifier(value, 512))
            && self.media.valid()
    }
}

/// Application-to-shell realtime control command. Session tokens are opaque,
/// restart-scoped broker capabilities.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RealtimeCommand {
    Register {
        intent: RealtimeResumeIntent,
    },
    Accept {
        session: String,
        /// Fresh target-local `CaptureBroker` capabilities. These cross only
        /// the verified app-to-shell IPC and never enter continuation state or
        /// the device fabric.
        #[serde(default)]
        capture_authorizations: Vec<String>,
    },
    MediaReady {
        session: String,
    },
    TargetFailed {
        session: String,
    },
}

impl RealtimeCommand {
    #[must_use]
    pub fn valid(&self) -> bool {
        match self {
            Self::Register { intent } => intent.valid(),
            Self::Accept {
                session,
                capture_authorizations,
            } => {
                opaque_token(session)
                    && !capture_authorizations.is_empty()
                    && capture_authorizations.len() <= MAX_CAPTURE_AUTHORIZATIONS
                    && capture_authorizations
                        .iter()
                        .all(|authorization| opaque_capture_authorization(authorization))
                    && capture_authorizations
                        .iter()
                        .enumerate()
                        .all(|(index, authorization)| {
                            capture_authorizations
                                .get(..index)
                                .is_some_and(|prior| !prior.contains(authorization))
                        })
            }
            Self::MediaReady { session } | Self::TargetFailed { session } => opaque_token(session),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RealtimeReply {
    Registered { session: String },
    Accepted,
    MediaReady,
    RolledBack,
    Rejected { code: String },
}

/// Shell-to-application lifecycle edge. `StopSource` is emitted only after the
/// exact target's explicit `MediaReady` succeeds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RealtimeEvent {
    Offer {
        session: String,
        intent: RealtimeResumeIntent,
    },
    /// The compositor-owned consent sheet selected concrete target-local
    /// capture sources. These opaque, one-shot capabilities travel only over
    /// the verified local app IPC; device identities and source handles stay
    /// in the shell.
    CaptureAuthorized {
        session: String,
        capture_authorizations: Vec<String>,
    },
    StopSource {
        session: String,
    },
    RolledBack {
        session: String,
    },
    Ended {
        session: String,
    },
}

impl RealtimeEvent {
    #[must_use]
    pub fn valid(&self) -> bool {
        match self {
            Self::Offer { session, intent } => opaque_token(session) && intent.valid(),
            Self::CaptureAuthorized {
                session,
                capture_authorizations,
            } => {
                opaque_token(session)
                    && !capture_authorizations.is_empty()
                    && capture_authorizations.len() <= MAX_CAPTURE_AUTHORIZATIONS
                    && capture_authorizations
                        .iter()
                        .all(|authorization| opaque_capture_authorization(authorization))
            }
            Self::StopSource { session }
            | Self::RolledBack { session }
            | Self::Ended { session } => opaque_token(session),
        }
    }
}

fn opaque_token(value: &str) -> bool {
    portable_identifier(value, MAX_REALTIME_TOKEN_BYTES)
}

fn opaque_capture_authorization(value: &str) -> bool {
    value.len() == OPAQUE_CAPTURE_AUTHORIZATION_BYTES.saturating_mul(2)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn portable_identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && !value.chars().any(char::is_control)
        && !value.contains("://")
        && !value.contains('?')
        && !value.contains(['/', '\\'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn realtime_intents_reject_unsafe_values_and_require_their_preconditions() {
        // Case `semantic_intents_reject_transport_and_secret_shaped_values`.
        {
            let safe = RealtimeResumeIntent {
                provider: "tilo.chat".into(),
                room_id: "voice-42".into(),
                semantic_position: Some("workspace-acme".into()),
                media: RealtimeMediaFacets {
                    microphone: true,
                    ..RealtimeMediaFacets::default()
                },
                controls: RealtimeControlIntent::default(),
            };
            assert!(safe.valid());
            for room in [
                "https://join.invalid/x",
                "voice?token=secret",
                "/run/pipewire-0",
            ] {
                let mut unsafe_intent = safe.clone();
                unsafe_intent.room_id = room.into();
                assert!(!unsafe_intent.valid());
            }
        }

        // Case `share_audio_requires_a_screen_share`.
        {
            assert!(
                !RealtimeMediaFacets {
                    microphone: true,
                    share_audio: true,
                    ..RealtimeMediaFacets::default()
                }
                .valid()
            );
        }

        // Case `accept_requires_bounded_unique_target_local_authorizations`.
        {
            let authorization = "ab".repeat(OPAQUE_CAPTURE_AUTHORIZATION_BYTES);
            let valid = RealtimeCommand::Accept {
                session: "restart-scoped-1".into(),
                capture_authorizations: vec![authorization.clone()],
            };
            assert!(valid.valid());
            assert!(
                !RealtimeCommand::Accept {
                    session: "restart-scoped-1".into(),
                    capture_authorizations: Vec::new(),
                }
                .valid()
            );
            assert!(
                !RealtimeCommand::Accept {
                    session: "restart-scoped-1".into(),
                    capture_authorizations: vec![authorization.clone(), authorization],
                }
                .valid()
            );
            assert!(
                !RealtimeCommand::Accept {
                    session: "restart-scoped-1".into(),
                    capture_authorizations: vec!["not-a-capability".into()],
                }
                .valid()
            );
            let legacy: RealtimeCommand =
                serde_json::from_slice(br#"{"kind":"accept","session":"restart-scoped-1"}"#)
                    .unwrap();
            assert!(
                !legacy.valid(),
                "legacy boolean-era accept parses but must fail closed"
            );
        }
    }

    #[test]
    fn realtime_wire_json_round_trips_without_extension_fields() {
        // Case `command_reply_and_event_json_round_trip_without_extension_fields`.
        {
            let command = RealtimeCommand::Register {
                intent: RealtimeResumeIntent {
                    provider: "tilo.chat".into(),
                    room_id: "voice-42".into(),
                    semantic_position: Some("workspace-acme".into()),
                    media: RealtimeMediaFacets {
                        microphone: true,
                        camera: true,
                        ..RealtimeMediaFacets::default()
                    },
                    controls: RealtimeControlIntent {
                        microphone_muted: true,
                        ..RealtimeControlIntent::default()
                    },
                },
            };
            let bytes = serde_json::to_vec(&command).unwrap();
            assert!(bytes.len() < MAX_REALTIME_WIRE_BYTES);
            assert_eq!(
                serde_json::from_slice::<RealtimeCommand>(&bytes).unwrap(),
                command
            );

            let reply = RealtimeReply::Registered {
                session: "restart-scoped-1".into(),
            };
            assert_eq!(
                serde_json::from_slice::<RealtimeReply>(&serde_json::to_vec(&reply).unwrap())
                    .unwrap(),
                reply
            );
            let event = RealtimeEvent::StopSource {
                session: "restart-scoped-1".into(),
            };
            assert_eq!(
                serde_json::from_slice::<RealtimeEvent>(&serde_json::to_vec(&event).unwrap())
                    .unwrap(),
                event
            );
            let event = RealtimeEvent::CaptureAuthorized {
                session: "restart-scoped-1".into(),
                capture_authorizations: vec!["ab".repeat(OPAQUE_CAPTURE_AUTHORIZATION_BYTES)],
            };
            assert!(event.valid());
            assert_eq!(
                serde_json::from_slice::<RealtimeEvent>(&serde_json::to_vec(&event).unwrap())
                    .unwrap(),
                event
            );

            let malformed = br#"{"kind":"register","intent":{"provider":"tilo.chat","room_id":"voice-42","semantic_position":null,"media":{"microphone":true,"camera":false,"screen_share":false,"share_audio":false},"controls":{"microphone_muted":false,"camera_muted":false,"screen_share_paused":false,"share_audio_muted":false},"credential":"secret"}}"#;
            assert!(serde_json::from_slice::<RealtimeCommand>(malformed).is_err());
        }
    }
}
