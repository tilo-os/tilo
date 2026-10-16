//! A realtime session's media and controls, as both the toolkit's handoff
//! contract and an app manifest's continuity schema carry them.

use serde::{Deserialize, Serialize};

/// Media a realtime session uses.
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
