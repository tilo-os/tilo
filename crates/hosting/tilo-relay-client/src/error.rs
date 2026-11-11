//! Why a call to a host failed, over iroh or over the HTTPS calls that come
//! before a session: a link's answer, and why a certification produced no
//! certificate.

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
    /// The door this device entered a workspace by, or the workspace, did
    /// not let it in ([`tilo_relay_protocol::entering`]).
    #[error("{}", .0.reason())]
    Entry(tilo_relay_protocol::entering::EnterRefusal),
}

/// Why a certification did not produce a certificate: the request could not
/// be made, the conversation with the host failed, or whoever holds the
/// person's root did not certify the device.
#[derive(Debug, thiserror::Error)]
pub enum CertifyError {
    /// The request could not be built or signed.
    #[error("certification request: {0}")]
    Request(String),
    /// The host could not be reached or answered badly.
    #[error("certification transport: {0}")]
    Transport(#[from] LinkError),
    /// An approver declined, or the host refused.
    #[error("certification denied: {0}")]
    Denied(String),
    /// The host is busy with this person's changes: nothing ran, and the
    /// same request works once `retry_after_ms` passed.
    #[error("the host takes this person's changes more slowly; retry in {retry_after_ms} ms")]
    RateLimited {
        /// How long the host asks to wait, in milliseconds.
        retry_after_ms: u64,
    },
    /// No approver answered in time.
    #[error("no approver answered the certification request")]
    Expired,
    /// Iroh dial or bi-stream.
    #[error("iroh: {0}")]
    Iroh(String),
    /// Postcard encode/decode.
    #[error("codec: {0}")]
    Codec(String),
    /// The host's or the device's handshake did not verify.
    #[error("auth: {0}")]
    Auth(#[from] tilo_relay_protocol::auth::AuthError),
    /// I/O.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The device signer refused or failed.
    #[error("signer: {0}")]
    Sign(String),
}
