//! A realtime session's media and controls, as an app manifest's continuity
//! schema carries them.

use serde::{Deserialize, Serialize};

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
    /// At least one medium, and audio is shared only with a shared screen.
    #[must_use]
    pub const fn valid(self) -> bool {
        (self.microphone || self.camera || self.screen_share)
            && (!self.share_audio || self.screen_share)
    }
}

/// Controls the person had set in a realtime session.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A session shares some medium, and its audio only with a shared screen.
    #[test]
    fn a_session_shares_a_medium_and_audio_only_with_its_screen() {
        let microphone = RealtimeMediaFacets {
            microphone: true,
            ..RealtimeMediaFacets::default()
        };
        assert!(microphone.valid());
        assert!(!RealtimeMediaFacets::default().valid());
        assert!(
            !RealtimeMediaFacets {
                share_audio: true,
                ..microphone
            }
            .valid()
        );
        assert!(
            RealtimeMediaFacets {
                screen_share: true,
                share_audio: true,
                ..microphone
            }
            .valid()
        );
    }
}
