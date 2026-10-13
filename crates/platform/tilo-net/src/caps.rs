//! What this crate provides on each target, as data an app can query.

pub use tilo_platform_caps::{Availability, availability, available};

/// A feature this target, or this particular host, cannot provide.
pub type Unsupported = tilo_platform_caps::Unsupported<Feature>;

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

pub(crate) const WEB_CROSS_ORIGIN: &str = "The browser only reads responses from sites that allow it; the host can relay others through its fetch proxy.";
pub(crate) const WEB_SOCKETS: &str = "A browser page cannot open raw network connections.";
pub(crate) const WEB_PEER_TO_PEER: &str = "A browser page cannot accept connections from peers.";
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub(crate) const NO_WEBTRANSPORT: &str = "This browser has no WebTransport.";
pub(crate) const NOT_YET: &str = "Tilo cannot do this yet.";

impl tilo_platform_caps::Feature for Feature {
    /// The feature's availability on Linux, macOS and the web, in that
    /// order. WebTransport is listed for the web, but a browser without it
    /// answers [`Unsupported`] from [`availability`] and from `connect`.
    fn targets(self) -> [Availability; 3] {
        use Availability::{Available, Unsupported};
        match self {
            Self::Http | Self::WebSocket | Self::WebTransport | Self::Status => {
                [Available, Available, Available]
            }
            Self::CrossOriginHttp => [Available, Available, Unsupported(WEB_CROSS_ORIGIN)],
            Self::Sockets => [Available, Available, Unsupported(WEB_SOCKETS)],
            Self::PeerToPeer => [Available, Available, Unsupported(WEB_PEER_TO_PEER)],
            // Linux asks the session portal, which may be unavailable or refuse.
            Self::Metered => [Available, Unsupported(NOT_YET), Available],
        }
    }

    /// Whether the feature can be used on this target (and, for
    /// WebTransport in a page, in this browser).
    fn availability(self) -> Availability {
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        if self == Self::WebTransport && !crate::web::has_global("WebTransport") {
            return Availability::Unsupported(NO_WEBTRANSPORT);
        }
        self.targets()[tilo_platform_caps::COLUMN]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_platform_caps::Feature as _;

    #[test]
    fn the_desktop_reads_its_own_column() {
        assert_eq!(availability(Feature::Sockets), Availability::Available);
        assert_eq!(
            availability(Feature::CrossOriginHttp),
            Availability::Available
        );
        let web = Feature::Sockets.targets()[2];
        assert_eq!(web, Availability::Unsupported(WEB_SOCKETS));
    }
}
