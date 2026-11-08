//! Why a call to a host failed, over iroh or over the HTTPS calls that come
//! before a session.

/// Why a link or request failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    /// The host was not reached, or answered with a server error.
    #[error("host unavailable: {0}")]
    Unavailable(String),
    /// The host refused this device, giving no reason the device can act
    /// on (a route's `401`/`403` text).
    #[error("host refused: {0}")]
    Refused(String),
    /// Another host answers here than the one this device pinned: the host
    /// was set up again.
    #[error("host is not the pinned device")]
    HostChanged,
    /// A body did not decode: something between here and the host (a proxy's
    /// error page), or a host speaking another version, answered.
    #[error("malformed answer: {0}")]
    Malformed(String),
    /// The signer failed.
    #[error("signer: {0}")]
    Sign(String),
    /// The host speaks peer protocol `theirs`, not this device's: they do not
    /// sync until the one behind has updated
    /// (docs/architecture/compatibility.md).
    #[error(
        "the host speaks peer protocol {theirs}, this device {}",
        tilo_relay_protocol::auth::PEER_PROTOCOL_VERSION
    )]
    PeerVersion {
        /// The host's version.
        theirs: u32,
    },
}
