//! What this crate can do on the running target.

/// A capability of this crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    /// [`crate::MediaEngine`] decodes real media (not the synthesized
    /// pattern).
    Playback,
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

/// Whether a feature works on this target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    Available,
    Unsupported(&'static str),
}

const NO_DECODE: &str = "built without the `decode` feature";
const NO_REALTIME: &str = "built without the `realtime` feature";
const NO_ENCODE: &str = "built without the `encode` feature";
const WEB_READBACK: &str = "the page reads pictures back through a canvas";
const WEB_NO_REALTIME: &str = "WebCodecs calls are not wired to the engine yet";
const WEB_NO_ENCODE: &str = "no encoder on the page yet";
const NO_GRAPH: &str = "no PipeWire graph on this platform";

/// Each feature's availability on Linux, macOS and the web, in that order,
/// for a build with the native backends enabled.
pub const CAPABILITIES: &[(Feature, [Availability; 3])] = &[
    (
        Feature::Playback,
        [
            Availability::Available,
            Availability::Available,
            Availability::Available,
        ],
    ),
    (
        Feature::ZeroCopyVideo,
        [
            Availability::Available,
            Availability::Unsupported("no dmabuf import on macOS; frames are uploaded"),
            Availability::Unsupported(WEB_READBACK),
        ],
    ),
    (
        Feature::NowPlaying,
        [
            Availability::Available,
            Availability::Available,
            Availability::Available,
        ],
    ),
    (
        Feature::Realtime,
        [
            Availability::Available,
            Availability::Available,
            Availability::Unsupported(WEB_NO_REALTIME),
        ],
    ),
    (
        Feature::Encode,
        [
            Availability::Available,
            Availability::Available,
            Availability::Unsupported(WEB_NO_ENCODE),
        ],
    ),
    (
        Feature::AudioGraph,
        [
            Availability::Available,
            Availability::Unsupported(NO_GRAPH),
            Availability::Unsupported(NO_GRAPH),
        ],
    ),
];

fn column() -> usize {
    if cfg!(target_family = "wasm") {
        2
    } else {
        usize::from(cfg!(target_os = "macos"))
    }
}

/// Whether `feature` works on the running target, in this build.
#[must_use]
pub fn availability(feature: Feature) -> Availability {
    let native = !cfg!(target_family = "wasm");
    let built = match feature {
        Feature::Playback | Feature::ZeroCopyVideo if native && !cfg!(gst_decode) => {
            Some(NO_DECODE)
        }
        Feature::Realtime if native && !cfg!(gst_realtime) => Some(NO_REALTIME),
        Feature::Encode if native && !cfg!(any(gst_encode, gst_decode)) => Some(NO_ENCODE),
        Feature::Playback if !native && !cfg!(web_page) => Some("no page to play in"),
        _ => None,
    };
    if let Some(reason) = built {
        return Availability::Unsupported(reason);
    }
    CAPABILITIES
        .iter()
        .find(|(f, _)| *f == feature)
        .and_then(|(_, row)| row.get(column()).copied())
        .unwrap_or(Availability::Unsupported("unknown feature"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_feature_has_a_row() {
        for feature in [
            Feature::Playback,
            Feature::ZeroCopyVideo,
            Feature::NowPlaying,
            Feature::Realtime,
            Feature::Encode,
            Feature::AudioGraph,
        ] {
            assert!(CAPABILITIES.iter().any(|(f, _)| *f == feature));
        }
    }

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
            CAPABILITIES
                .iter()
                .find(|(f, _)| *f == Feature::AudioGraph)
                .map(|(_, row)| row[column()])
                .unwrap()
        );
    }
}
