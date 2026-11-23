//! Why a call to a server failed, over iroh or over the HTTPS calls that come
//! before a session: a link's answer, and why a certification produced no
//! certificate.

/// Why a link or request failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    /// The server was not reached, or answered with a server error.
    #[error("the server could not be reached: {0}")]
    Unavailable(String),
    /// The server refused this device, giving no reason the device can act
    /// on (a route's `401`/`403` text).
    #[error("the server refused: {0}")]
    Refused(String),
    /// Another server answers here than the one this device pinned: the server
    /// was set up again.
    #[error("the server is not the one this computer knew: it was set up again")]
    ServerChanged,
    /// A body did not decode: something between here and the server (a proxy's
    /// error page), or a server speaking another version, answered.
    #[error("malformed answer: {0}")]
    Malformed(String),
    /// The signer failed.
    #[error("signer: {0}")]
    Sign(String),
    /// The server refused this device because its person removed it from
    /// their devices, and said so under its own signature
    /// (`tilo_server_protocol::auth::HelloRefusal::DeviceRevoked`). Nothing
    /// this device asks again changes it.
    #[error("this device was removed from its person's devices")]
    DeviceRevoked,
    /// The server refused the certificate this device showed, and said so
    /// under its own signature
    /// (`tilo_server_protocol::auth::HelloRefusal::CertificateRejected`): a
    /// device whose kept certificate lapsed certifies again, and gets in no
    /// other way.
    #[error("the server refused this device's certificate")]
    CertificateRejected,
    /// The server speaks peer protocol `theirs`, not this device's: they do not
    /// sync until the one behind has updated
    /// (docs/architecture/compatibility.md).
    #[error(
        "the server speaks peer protocol {theirs}, this device {}",
        tilo_server_protocol::auth::PEER_PROTOCOL_VERSION
    )]
    PeerVersion {
        /// The server's version.
        theirs: u32,
    },
}

/// Why a certification did not produce a certificate: the request could not
/// be made, the conversation with the server failed, or whoever holds the
/// person's root did not certify the device.
#[derive(Debug, thiserror::Error)]
pub enum CertifyError {
    /// The request could not be built or signed.
    #[error("the request could not be made: {0}")]
    Request(String),
    /// The server could not be reached or answered badly.
    #[error("{0}")]
    Transport(#[from] LinkError),
    /// An approver declined, or the server refused.
    #[error("{0}")]
    Denied(String),
    /// No approver answered in time.
    #[error("no approving device answered in time")]
    Expired,
    /// Iroh dial or bi-stream.
    #[error("iroh: {0}")]
    Iroh(String),
    /// Postcard encode/decode.
    #[error("codec: {0}")]
    Codec(String),
    /// The server's or the device's handshake did not verify.
    #[error("auth: {0}")]
    Auth(#[from] tilo_server_protocol::auth::AuthError),
    /// I/O.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The device signer refused or failed.
    #[error("signer: {0}")]
    Sign(String),
}
