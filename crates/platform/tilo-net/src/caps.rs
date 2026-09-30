//! What this crate provides on each target, as data an app can query.

use std::fmt;

/// One capability of this crate, for [`availability`] and [`Unsupported`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    /// HTTP requests with streaming bodies: [`Client`](crate::Client).
    Http,
    /// Reading responses from any site. A page reads only the responses a
    /// site allows it to (CORS), unless the host provides a
    /// [`proxy`](crate::proxy).
    CrossOriginHttp,
    /// WebSocket clients: [`ws`](crate::ws).
    WebSocket,
    /// WebTransport sessions: [`webtransport`](crate::webtransport).
    WebTransport,
    /// Raw TCP, UDP and TLS over TCP: [`sockets`](crate::sockets).
    Sockets,
    /// Endpoints peers dial by ticket: [`p2p`](crate::p2p).
    PeerToPeer,
    /// Whether the device is online: [`status`](crate::status).
    Status,
    /// Whether the connection is metered: [`NetStatus::metered`](crate::status::NetStatus::metered).
    Metered,
}

/// Whether a [`Feature`] can be used on a target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Availability {
    Available,
    /// Usable after a user prompt (browser permission, macOS TCC).
    Prompt,
    /// Never on this target; the reason is user-facing.
    Unsupported(&'static str),
}

/// A feature this target, or this particular host, cannot provide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Unsupported {
    pub feature: Feature,
    /// User-facing.
    pub reason: &'static str,
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.reason)
    }
}

impl std::error::Error for Unsupported {}

pub(crate) const WEB_CROSS_ORIGIN: &str = "The browser only reads responses from sites that allow it; the host can relay others through its fetch proxy.";
pub(crate) const WEB_SOCKETS: &str = "A browser page cannot open raw network connections.";
pub(crate) const WEB_PEER_TO_PEER: &str = "A browser page cannot accept connections from peers.";
#[cfg_attr(
    not(all(target_arch = "wasm32", target_os = "unknown")),
    allow(dead_code, reason = "only a page lacks these")
)]
pub(crate) const NO_WEBTRANSPORT: &str = "This browser has no WebTransport.";
pub(crate) const NOT_YET: &str = "Tilo cannot do this yet.";

/// Every feature with its availability on Linux, macOS and the web, in that
/// order. WebTransport is listed for the web, but a browser without it
/// answers [`Unsupported`] from [`availability`] and from `connect`.
pub const CAPABILITIES: &[(Feature, [Availability; 3])] = {
    use Availability::{Available, Unsupported};
    &[
        (Feature::Http, [Available, Available, Available]),
        (
            Feature::CrossOriginHttp,
            [Available, Available, Unsupported(WEB_CROSS_ORIGIN)],
        ),
        (Feature::WebSocket, [Available, Available, Available]),
        (Feature::WebTransport, [Available, Available, Available]),
        (
            Feature::Sockets,
            [Available, Available, Unsupported(WEB_SOCKETS)],
        ),
        (
            Feature::PeerToPeer,
            [Available, Available, Unsupported(WEB_PEER_TO_PEER)],
        ),
        (Feature::Status, [Available, Available, Available]),
        // Linux asks NetworkManager, which may not be running.
        (
            Feature::Metered,
            [Available, Unsupported(NOT_YET), Available],
        ),
    ]
};

/// This target's column of [`CAPABILITIES`]. Native targets other than
/// macOS report the Linux column.
const COLUMN: usize = if cfg!(all(target_arch = "wasm32", target_os = "unknown")) {
    2
} else if cfg!(target_os = "macos") {
    1
} else {
    0
};

/// Whether `feature` can be used on this target (and, for WebTransport in a
/// page, in this browser).
pub fn availability(feature: Feature) -> Availability {
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    if feature == Feature::WebTransport && !crate::web::has_global("WebTransport") {
        return Availability::Unsupported(NO_WEBTRANSPORT);
    }
    CAPABILITIES
        .iter()
        .find(|(listed, _)| *listed == feature)
        .and_then(|(_, targets)| targets.get(COLUMN).copied())
        .unwrap_or(Availability::Unsupported(
            "This build does not know the feature.",
        ))
}

/// The error for `feature` on this target, when its column says it is
/// unsupported.
#[cfg_attr(
    any(
        not(all(target_arch = "wasm32", target_os = "unknown")),
        not(all(feature = "webtransport", feature = "sockets"))
    ),
    allow(dead_code, reason = "only a page lacks these")
)]
pub(crate) fn unsupported(feature: Feature) -> Unsupported {
    let reason = match availability(feature) {
        Availability::Unsupported(reason) => reason,
        Availability::Available | Availability::Prompt => NOT_YET,
    };
    Unsupported { feature, reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Feature; 8] = [
        Feature::Http,
        Feature::CrossOriginHttp,
        Feature::WebSocket,
        Feature::WebTransport,
        Feature::Sockets,
        Feature::PeerToPeer,
        Feature::Status,
        Feature::Metered,
    ];

    #[test]
    fn every_feature_is_listed_once() {
        for feature in ALL {
            let rows = CAPABILITIES.iter().filter(|(listed, _)| *listed == feature);
            assert_eq!(rows.count(), 1, "{feature:?}");
        }
        assert_eq!(CAPABILITIES.len(), ALL.len());
    }

    #[test]
    fn the_desktop_reads_its_own_column() {
        assert_eq!(availability(Feature::Sockets), Availability::Available);
        assert_eq!(
            availability(Feature::CrossOriginHttp),
            Availability::Available
        );
        let web = CAPABILITIES
            .iter()
            .find(|(feature, _)| *feature == Feature::Sockets)
            .map(|(_, targets)| targets[2]);
        assert_eq!(web, Some(Availability::Unsupported(WEB_SOCKETS)));
    }
}
