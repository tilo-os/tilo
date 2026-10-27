//! What this crate can do on the running target.

pub use tilo_platform_caps::{Availability, availability, available};
/// A capability of this crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    /// [`crate::MediaEngine`] decodes real media (not the synthesized
    /// pattern).
    Playback,
    /// [`crate::MediaEngine`] plays adaptive live streams (HLS), not only
    /// one progressive file, wherever [`Feature::Playback`] is built in.
    /// It reports the target rather than the build, so a native build
    /// without decoders does not claim live streams need another app.
    LiveStreams,
    /// Decoded pictures stay on the GPU and are imported without a copy
    /// (dmabuf). Where this is unsupported, frames arrive as RGBA or planar
    /// YUV in system memory and are uploaded; consumers fall back to that.
    ZeroCopyVideo,
    /// The system's now-playing controls and media keys
    /// ([`crate::MediaEngine::set_now_playing`]).
    NowPlaying,
    /// Low-latency encode and decode for calls and screen sharing
    /// ([`crate::realtime`]).
    Realtime,
    /// Encoding for export ([`crate::encode`]).
    Encode,
    /// The system audio graph: devices, per-app streams and routing
    /// ([`crate::audio`]).
    AudioGraph,
}

const NO_DECODE: &str = "built without the `decode` feature";
const NO_REALTIME: &str = "built without the `realtime` feature";
const NO_ENCODE: &str = "built without the `encode` feature";
const WEB_READBACK: &str = "the page reads pictures back through a canvas";
const WEB_NO_REALTIME: &str = "WebCodecs calls are not wired to the engine yet";
const WEB_NO_ENCODE: &str = "no encoder on the page yet";
const WEB_NO_LIVE: &str = "the page's media element plays one progressive file, not HLS";
const NO_GRAPH: &str = "no PipeWire graph on this platform";

impl tilo_platform_caps::Feature for Feature {
    /// The feature's availability on Linux, macOS and the web, in that order,
    /// for a build with the native backends enabled.
    fn targets(self) -> [Availability; 3] {
        use Availability::{Available, Unsupported};
        match self {
            Self::Playback | Self::NowPlaying => [Available, Available, Available],
            Self::LiveStreams => [Available, Available, Unsupported(WEB_NO_LIVE)],
            Self::ZeroCopyVideo => [
                Available,
                Unsupported("no dmabuf import on macOS; frames are uploaded"),
                Unsupported(WEB_READBACK),
            ],
            Self::Realtime => [Available, Available, Unsupported(WEB_NO_REALTIME)],
            Self::Encode => [Available, Available, Unsupported(WEB_NO_ENCODE)],
            Self::AudioGraph => [Available, Unsupported(NO_GRAPH), Unsupported(NO_GRAPH)],
        }
    }

    /// Whether the feature works on the running target, in this build.
    fn availability(self) -> Availability {
        let native = !cfg!(target_family = "wasm");
        let built = match self {
            Self::Playback | Self::ZeroCopyVideo if native && !cfg!(gst_decode) => Some(NO_DECODE),
            Self::Realtime if native && !cfg!(gst_realtime) => Some(NO_REALTIME),
            Self::Encode if native && !cfg!(any(gst_encode, gst_decode)) => Some(NO_ENCODE),
            Self::Playback if !native && !cfg!(web_page) => Some("no page to play in"),
            _ => None,
        };
        match built {
            Some(reason) => Availability::Unsupported(reason),
            None => self.targets()[tilo_platform_caps::COLUMN],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_platform_caps::Feature as _;

    #[test]
    fn features_left_out_of_the_build_are_unsupported() {
        if !cfg!(gst_decode) {
            assert_eq!(
                availability(Feature::Playback),
                Availability::Unsupported(NO_DECODE)
            );
        }
        assert_eq!(
            availability(Feature::AudioGraph),
            Feature::AudioGraph.targets()[tilo_platform_caps::COLUMN]
        );
    }
}
