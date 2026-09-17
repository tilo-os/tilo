//! Transport-neutral application contract for active realtime-session handoff.
//!
//! These values contain semantic room and control intent only. In particular,
//! they cannot represent capture objects, device identities, transport
//! tickets, credentials, or membership assertions. The local shell binds the
//! command to the certified app/workspace route and owns all device routing.

use serde::{Deserialize, Serialize};

pub const REALTIME_PROTOCOL_VERSION: u16 = 1;
pub const MAX_REALTIME_WIRE_BYTES: usize = 8 * 1024;
pub const MAX_REALTIME_TOKEN_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent media facet on the stable cross-language JSON contract"
)]
pub struct RealtimeMediaFacets {
    pub microphone: bool,
    pub camera: bool,
    pub screen_share: bool,
    pub share_audio: bool,
}

impl RealtimeMediaFacets {
    #[must_use]
    pub const fn valid(self) -> bool {
        (self.microphone || self.camera || self.screen_share)
            && (!self.share_audio || self.screen_share)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent control intent on the stable cross-language JSON contract"
)]
pub struct RealtimeControlIntent {
    pub microphone_muted: bool,
    pub camera_muted: bool,
    pub screen_share_paused: bool,
    pub share_audio_muted: bool,
}

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
    Register { intent: RealtimeResumeIntent },
    Propose { session: String, target: String },
    Accept { session: String },
    MediaReady { session: String },
    TargetFailed { session: String },
}

impl RealtimeCommand {
    #[must_use]
    pub fn valid(&self) -> bool {
        match self {
            Self::Register { intent } => intent.valid(),
            Self::Propose { session, target } => {
                opaque_token(session) && portable_identifier(target, 256)
            }
            Self::Accept { session }
            | Self::MediaReady { session }
            | Self::TargetFailed { session } => opaque_token(session),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RealtimeReply {
    Registered { session: String },
    Proposed,
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
            Self::StopSource { session }
            | Self::RolledBack { session }
            | Self::Ended { session } => opaque_token(session),
        }
    }
}

fn opaque_token(value: &str) -> bool {
    portable_identifier(value, MAX_REALTIME_TOKEN_BYTES)
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
    fn semantic_intents_reject_transport_and_secret_shaped_values() {
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

    #[test]
    fn share_audio_requires_a_screen_share() {
        assert!(
            !RealtimeMediaFacets {
                microphone: true,
                share_audio: true,
                ..RealtimeMediaFacets::default()
            }
            .valid()
        );
    }

    #[test]
    fn command_reply_and_event_json_round_trip_without_extension_fields() {
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
            serde_json::from_slice::<RealtimeReply>(&serde_json::to_vec(&reply).unwrap()).unwrap(),
            reply
        );
        let event = RealtimeEvent::StopSource {
            session: "restart-scoped-1".into(),
        };
        assert_eq!(
            serde_json::from_slice::<RealtimeEvent>(&serde_json::to_vec(&event).unwrap()).unwrap(),
            event
        );

        let malformed = br#"{"kind":"register","intent":{"provider":"tilo.chat","room_id":"voice-42","semantic_position":null,"media":{"microphone":true,"camera":false,"screen_share":false,"share_audio":false},"controls":{"microphone_muted":false,"camera_muted":false,"screen_share_paused":false,"share_audio_muted":false},"credential":"secret"}}"#;
        assert!(serde_json::from_slice::<RealtimeCommand>(malformed).is_err());
    }
}
