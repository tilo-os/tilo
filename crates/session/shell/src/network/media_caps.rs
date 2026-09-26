//! What each peer can encode and decode, from their
//! [`NeighbourMsg::MediaCapabilities`](crate::network::NeighbourMsg) adverts.
//!
//! Peers advertise with every heartbeat (every 5 s). An advert older than
//! [`STALE_AFTER`] is forgotten, which is how an evicted peer drops out: the
//! registry needs no eviction hook. A peer with no fresh advert is treated as
//! [`CodecCapabilities::legacy_peer`] — H.264 only — which is what every
//! build that predates the advert can decode.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use media::realtime::CodecCapabilities;

use crate::network::protocol::{NeighbourMsg, PeerId};

/// Three missed heartbeats, the same horizon as peer eviction.
pub const STALE_AFTER: Duration = Duration::from_secs(15);

#[derive(Default)]
struct Registry {
    peers: HashMap<PeerId, (CodecCapabilities, Instant)>,
    /// Bumped whenever a peer's advertised capabilities change, so a share
    /// can tell cheaply whether to renegotiate.
    generation: u64,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

/// Open sessions whose frames end up in a decoder that takes H.264 only.
static H264_ONLY_VIEWERS: AtomicUsize = AtomicUsize::new(0);

/// Held while frames this peer receives are relayed to an H.264-only
/// decoder (workspace control's Remote app). While any exists, this peer
/// advertises H.264-only decode, which is the truth about what it can view.
#[derive(Debug)]
pub struct H264OnlyViewer(());

impl H264OnlyViewer {
    pub fn new() -> Self {
        H264_ONLY_VIEWERS.fetch_add(1, Ordering::AcqRel);
        Self(())
    }
}

impl Default for H264OnlyViewer {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for H264OnlyViewer {
    fn drop(&mut self) {
        H264_ONLY_VIEWERS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// This machine's capabilities as a wire advert from `peer`.
pub fn local_advert(peer: PeerId) -> NeighbourMsg {
    let mut caps = media::realtime::local_capabilities();
    if H264_ONLY_VIEWERS.load(Ordering::Acquire) > 0 {
        let legacy = CodecCapabilities::legacy_peer();
        caps.hardware_decode = legacy.hardware_decode;
        caps.software_decode = legacy.software_decode;
    }
    NeighbourMsg::MediaCapabilities {
        peer,
        hardware_decode: CodecCapabilities::labels(&caps.hardware_decode),
        software_decode: CodecCapabilities::labels(&caps.software_decode),
        hardware_encode: CodecCapabilities::labels(&caps.hardware_encode),
        software_encode: CodecCapabilities::labels(&caps.software_encode),
    }
}

/// Record an authenticated peer's advert. Labels this build does not know
/// are ignored.
pub fn record(
    peer: PeerId,
    hardware_decode: &[String],
    software_decode: &[String],
    hardware_encode: &[String],
    software_encode: &[String],
) {
    record_caps(
        peer,
        CodecCapabilities {
            hardware_decode: CodecCapabilities::parse_labels(hardware_decode),
            software_decode: CodecCapabilities::parse_labels(software_decode),
            hardware_encode: CodecCapabilities::parse_labels(hardware_encode),
            software_encode: CodecCapabilities::parse_labels(software_encode),
        },
    );
}

/// Record parsed capabilities for `peer`, stamped now.
pub fn record_caps(peer: PeerId, caps: CodecCapabilities) {
    let mut reg = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let changed = reg.peers.get(&peer).is_none_or(|(old, _)| *old != caps);
    reg.peers.insert(peer, (caps, Instant::now()));
    if changed {
        reg.generation = reg.generation.wrapping_add(1);
    }
}

/// What `peer` can do, or [`CodecCapabilities::legacy_peer`] without a
/// fresh advert.
pub fn capabilities(peer: PeerId) -> CodecCapabilities {
    let reg = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    reg.peers
        .get(&peer)
        .filter(|(_, seen)| seen.elapsed() < STALE_AFTER)
        .map_or_else(CodecCapabilities::legacy_peer, |(caps, _)| caps.clone())
}

/// Advances whenever some peer's capabilities changed.
pub fn generation() -> u64 {
    registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .generation
}

#[cfg(test)]
mod tests {
    use super::*;
    use media::StreamCodec;

    #[test]
    fn media_capability_adverts_replace_the_h264_default() {
        // Phase `an_unknown_peer_is_h264_only_and_an_advert_replaces_that`.
        {
            const PEER: PeerId = 0x5eed_0001;
            assert_eq!(capabilities(PEER), CodecCapabilities::legacy_peer());
            let before = generation();
            record(
                PEER,
                &["av1".into(), "h266".into()],
                &["h264".into()],
                &[],
                &[],
            );
            let caps = capabilities(PEER);
            assert!(caps.hardware_decode.contains(&StreamCodec::Av1));
            assert_eq!(caps.hardware_decode.len(), 1, "unknown labels are ignored");
            assert_ne!(generation(), before);
            let after = generation();
            record(PEER, &["av1".into()], &["h264".into()], &[], &[]);
            assert_eq!(generation(), after, "an unchanged advert is not a change");
        }
        // Phase `the_local_advert_names_this_peer`.
        match local_advert(7) {
            NeighbourMsg::MediaCapabilities { peer, .. } => assert_eq!(peer, 7),
            other => panic!("expected a capabilities advert, got {other:?}"),
        }
    }
}
