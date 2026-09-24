//! Per-kind connection bounds on the one shared QUIC endpoint.
//!
//! The endpoint serves plain HTTP/3 delivery, WebTransport voice (MoQ media)
//! and WebTransport LIVE_V1. A connection first holds a short handshake
//! permit until its first request stream classifies it, then a permit of its
//! own kind for its lifetime. One kind can therefore never starve another
//! (reconnect churn of HTTP/3 data keeps voice and live admissible), and
//! unclassified connections cannot pin serving capacity. Every connection is
//! also charged `Ingress::reserve_media_transport` bytes by the caller.
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Connections still in QUIC/TLS/HTTP-3 setup, not yet classified.
pub(crate) const HANDSHAKE_CONNECTIONS: usize = 32;
/// Plain HTTP/3 delivery connections (each carries its own stream bound).
pub(crate) const HTTP_CONNECTIONS: usize = 64;
/// WebTransport MoQ media (voice) sessions.
pub(crate) const MEDIA_SESSIONS: usize = 32;
/// WebTransport LIVE_V1 sessions.
pub(crate) const LIVE_SESSIONS: usize = 64;
/// Upper bound on concurrently running connection tasks.
pub(crate) const MAX_TASKS: usize =
    HANDSHAKE_CONNECTIONS + HTTP_CONNECTIONS + MEDIA_SESSIONS + LIVE_SESSIONS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Http,
    Media,
    Live,
}

#[derive(Clone)]
pub(crate) struct EndpointLimits {
    handshake: Arc<Semaphore>,
    http: Arc<Semaphore>,
    media: Arc<Semaphore>,
    live: Arc<Semaphore>,
}

impl EndpointLimits {
    pub(crate) fn new() -> Self {
        Self::with(
            HANDSHAKE_CONNECTIONS,
            HTTP_CONNECTIONS,
            MEDIA_SESSIONS,
            LIVE_SESSIONS,
        )
    }
    fn with(handshake: usize, http: usize, media: usize, live: usize) -> Self {
        Self {
            handshake: Arc::new(Semaphore::new(handshake)),
            http: Arc::new(Semaphore::new(http)),
            media: Arc::new(Semaphore::new(media)),
            live: Arc::new(Semaphore::new(live)),
        }
    }
    /// Admit one unclassified connection, or `None` to refuse it now.
    pub(crate) fn handshake(&self) -> Option<OwnedSemaphorePermit> {
        self.handshake.clone().try_acquire_owned().ok()
    }
    /// Classify an admitted connection; `None` means its kind is full.
    /// The handshake permit is released either way.
    pub(crate) fn classify(
        &self,
        handshake: OwnedSemaphorePermit,
        kind: Kind,
    ) -> Option<OwnedSemaphorePermit> {
        drop(handshake);
        let semaphore = match kind {
            Kind::Http => &self.http,
            Kind::Media => &self.media,
            Kind::Live => &self.live,
        };
        semaphore.clone().try_acquire_owned().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_are_bounded_independently_and_handshakes_release_on_classification() {
        let limits = EndpointLimits::with(2, 1, 1, 1);
        let a = limits.handshake().unwrap();
        let b = limits.handshake().unwrap();
        assert!(limits.handshake().is_none(), "handshake bound");
        let http = limits.classify(a, Kind::Http).unwrap();
        // The classified connection freed its handshake slot.
        let c = limits.handshake().unwrap();
        // A full HTTP kind never consumes voice or live capacity.
        assert!(limits.classify(b, Kind::Http).is_none());
        let live = limits.classify(c, Kind::Live).unwrap();
        let d = limits.handshake().unwrap();
        let media = limits.classify(d, Kind::Media).unwrap();
        assert!(
            limits
                .classify(limits.handshake().unwrap(), Kind::Media)
                .is_none()
        );
        drop((http, live, media));
        assert!(
            limits
                .classify(limits.handshake().unwrap(), Kind::Http)
                .is_some()
        );
    }

    #[test]
    fn production_bounds_exceed_the_former_shared_sixteen() {
        assert!(HTTP_CONNECTIONS > 16 && LIVE_SESSIONS > 16 && MEDIA_SESSIONS >= 16);
        assert_eq!(MAX_TASKS, 192);
    }
}
