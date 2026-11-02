//! The page's way into the live plane.
//!
//! A joined page is a participant like a device: it opens one `moq-lite`
//! session to the relay that serves its origin, sealed as its own device (not
//! as any device that bridges anything), and publishes and reads the same
//! tracks a device does. The plane itself is `tilo_live::plane::Plane`: this
//! module is only the one step that differs in a browser — opening the
//! WebTransport session ([`tilo_net::webtransport`]) and proving the page's
//! grant on it.
//!
//! A browser without WebTransport, or without WebTransport subprotocol
//! negotiation, has no way to join — there is no polling fallback — so
//! [`join`] answers [`LiveError::Unsupported`], whose message is what the page
//! shows.

use tilo_call_lane::Roster;
use tilo_live::admit::{self, AdmissionError, Dial, Dialling};
use tilo_live::plane::{Join, Plane, PlaneError};
use tilo_live::scope::Scope;
use tilo_relay_protocol::web::LiveGrant;

use crate::ChannelContentKeys;

/// The message a page shows when its browser cannot join live.
pub const UNSUPPORTED: &str = "this browser cannot join live";

/// Why the page could not join the live plane.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LiveError {
    /// The browser has no WebTransport, or no WebTransport subprotocol
    /// negotiation. There is no fallback: the page joins nothing.
    #[error("{UNSUPPORTED}")]
    Unsupported,
    /// The host refused the page's grant, or its authority lapsed.
    #[error("the host refused the page's live grant")]
    Denied,
    /// The carrier could not be reached, or did not finish its setup.
    #[error("the live carrier is unavailable")]
    Unavailable,
    /// The transport does not speak the live carrier's protocol.
    #[error("the transport does not speak the live carrier's protocol")]
    WrongProtocol,
    /// The scope's keys cannot seal or open this page's tracks.
    #[error("the page holds no media key for this scope")]
    NoKey,
    /// The plane refused the page's namespace or track.
    #[error("the live plane refused the page's namespace")]
    Refused,
}

impl From<PlaneError> for LiveError {
    fn from(error: PlaneError) -> Self {
        match error {
            PlaneError::Denied | PlaneError::Ended => Self::Denied,
            PlaneError::Unavailable => Self::Unavailable,
            PlaneError::Refused(
                tilo_call_lane::Refusal::NoKey(_) | tilo_call_lane::Refusal::Primitive,
            ) => Self::NoKey,
            PlaneError::Refused(_) | PlaneError::Invalid => Self::Refused,
        }
    }
}

/// Join `scope`'s live plane as this page's own device, through the relay
/// listener at `origin` (the [`LiveInfo::webtransport_origin`] that `/live`
/// answered), accepting exactly the certificate whose SHA-256 is
/// `certificate_sha256` (base64, as that route reports it), admitted by
/// `grant` (the page's session token and workspace).
///
/// # Errors
///
/// [`LiveError::Unsupported`] in a browser without WebTransport or without
/// subprotocol negotiation, [`LiveError::WrongProtocol`] from a carrier that
/// speaks another protocol, [`LiveError::Denied`] when the host refuses the
/// grant, [`LiveError::Unavailable`] when the carrier fails first, and
/// [`LiveError::NoKey`] when `keys` cannot seal this device's tracks.
///
/// [`LiveInfo::webtransport_origin`]: tilo_relay_protocol::web::LiveInfo
pub async fn join(
    origin: &str,
    certificate_sha256: &str,
    grant: LiveGrant,
    scope: Scope,
    device: [u8; 32],
    keys: ChannelContentKeys,
    roster: Roster,
) -> Result<Plane, LiveError> {
    let certificate = decode_sha256(certificate_sha256).ok_or(LiveError::Unavailable)?;
    let options = tilo_net::webtransport::Options::pinned(vec![certificate])
        .with_protocols(vec![tilo_live::preface::PROTOCOL.to_owned()]);
    let transport = tilo_net::webtransport::connect(&dial_url(origin), options)
        .await
        .map_err(|error| match error {
            tilo_net::NetError::Unsupported(_) => LiveError::Unsupported,
            _ => LiveError::Unavailable,
        })?;
    protocol_gate(transport.protocol())?;
    let opened = Opened {
        transport,
        grant: grant.encode().to_vec(),
    };
    Plane::join(
        &opened,
        &opened.grant,
        Join {
            scope,
            device,
            keys,
            roster,
            publish: true,
        },
    )
    .await
    .map_err(LiveError::from)
}

/// A page's WebTransport session, with the grant it proves itself by: the one
/// transport-specific step of joining.
struct Opened<S> {
    transport: S,
    grant: Vec<u8>,
}

impl<S> Dial for Opened<S>
where
    S: admit::Carrier + Send + Sync + 'static,
    S::SendStream: Send,
    S::RecvStream: Send,
{
    fn dial<'a>(
        &'a self,
        grant: &'a [u8],
        publish: Option<moq_net::origin::Consumer>,
        subscribe: Option<moq_net::origin::Producer>,
        timeout: std::time::Duration,
    ) -> Dialling<'a> {
        Box::pin(async move {
            n0_future::time::timeout(
                timeout,
                admit::start(self.transport.clone(), grant, publish, subscribe),
            )
            .await
            .map_err(|_| AdmissionError::Unavailable)?
        })
    }
}

/// Where a page dials a relay's live listener: the origin `/live` answered,
/// on the carrier's one path.
fn dial_url(origin: &str) -> String {
    format!(
        "{}{}",
        origin.trim_end_matches('/'),
        tilo_live::preface::PATH
    )
}

/// The gate on the session's negotiated subprotocol: the page joins only a
/// carrier that speaks the live plane, and a browser that negotiated none has
/// no fallback.
fn protocol_gate(protocol: Option<&str>) -> Result<(), LiveError> {
    match protocol {
        Some(tilo_live::preface::PROTOCOL) => Ok(()),
        None => Err(LiveError::Unsupported),
        Some(_) => Err(LiveError::WrongProtocol),
    }
}

/// The 32 bytes a base64 SHA-256 digest names.
fn decode_sha256(digest: &str) -> Option<[u8; 32]> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(digest.trim())
        .ok()?;
    bytes.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::{LiveError, UNSUPPORTED, dial_url, protocol_gate};

    #[test]
    fn a_page_dials_the_carriers_one_path() {
        assert_eq!(
            dial_url("https://relay.example:4443"),
            "https://relay.example:4443/moq/voice"
        );
        assert_eq!(
            dial_url("https://relay.example:4443/"),
            "https://relay.example:4443/moq/voice"
        );
    }

    #[test]
    fn a_browser_that_negotiates_no_subprotocol_cannot_join() {
        assert_eq!(protocol_gate(None), Err(LiveError::Unsupported));
        assert_eq!(LiveError::Unsupported.to_string(), UNSUPPORTED);
        assert_eq!(
            protocol_gate(Some("moq-lite-05")),
            Err(LiveError::WrongProtocol)
        );
        assert_eq!(protocol_gate(Some(tilo_live::preface::PROTOCOL)), Ok(()));
    }
}
