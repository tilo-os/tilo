//! The host's WebTransport listener: how a page reaches it, with the
//! browser's own transport.
//!
//! HTTP/3 beside the HTTPS listener, on the same certificate. A device opens
//! one session per thing it asks, at that thing's path
//! ([`tilo_server_protocol::web::WebLink`]), and each is served by the same
//! code its iroh ALPN is:
//!
//! - the session path: every stream a member session, as on the relay ALPN
//!   ([`RelayProtocol::serve_link`]);
//! - the certification path: one request or approver command, as on the
//!   certification ALPN;
//! - the live session path: the workspace's live session, offered and answered as
//!   `moq-lite`, after the device proves itself on the first stream.

use tilo_net::webtransport::Answer;
use tilo_server_client::link::Link;
use tilo_server_protocol::web::{LINK_CERTIFY_PATH, LINK_LIVE_SESSION_PATH, LINK_SESSION_PATH};

use crate::certification::CertificationProtocol;
use crate::host::RelayProtocol;

/// What the listener serves.
#[derive(Clone)]
pub(crate) struct Serving {
    pub(crate) relay: RelayProtocol,
    pub(crate) certification: CertificationProtocol,
    pub(crate) live_origin: tilo_live::relay::Relay,
}

/// Which paths are served, and the subprotocol each answers: the live session's
/// session is `moq-lite`, as its ALPN says over iroh.
///
/// A browser that negotiates no subprotocols offers none at all (Chrome
/// before it shipped `WebTransport.protocol`), and is still answered at the
/// live session's path, which speaks that one protocol whatever was negotiated
/// ([`live_session`]). One that offers only others is refused.
fn answer(path: &str, offered: &[String]) -> Answer {
    match path {
        LINK_SESSION_PATH | LINK_CERTIFY_PATH => Answer::Accept,
        LINK_LIVE_SESSION_PATH
            if offered
                .iter()
                .any(|protocol| protocol == tilo_live::preface::PROTOCOL) =>
        {
            Answer::AcceptAs(tilo_live::preface::PROTOCOL.to_owned())
        }
        LINK_LIVE_SESSION_PATH if offered.is_empty() => Answer::Accept,
        _ => Answer::Refuse,
    }
}

/// A session accepted at the live session's path, as the protocol that path speaks:
/// the one negotiated, else the one the path names.
fn live_session(session: tilo_net::webtransport::Session) -> tilo_net::webtransport::Session {
    session.speaking(tilo_live::preface::PROTOCOL)
}

/// Serve `listener` until it is dropped.
pub(crate) async fn serve(listener: tilo_net::webtransport::Listener, serving: Serving) {
    listener
        .serve(answer, move |path, session| {
            let serving = serving.clone();
            async move {
                match path.as_str() {
                    LINK_SESSION_PATH => serving.relay.serve_link(Link::web(session)).await,
                    LINK_CERTIFY_PATH => {
                        if let Err(error) = serving.certification.serve(&Link::web(session)).await {
                            tracing::debug!(%error, "certification over WebTransport ended");
                        }
                    }
                    LINK_LIVE_SESSION_PATH => {
                        super::live::serve_web(
                            &serving.live_origin,
                            &serving.relay,
                            live_session(session),
                        )
                        .await;
                    }
                    _ => {}
                }
            }
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_live_session_path_answers_a_browser_that_negotiates_no_subprotocol() {
        let moq = vec![tilo_live::preface::PROTOCOL.to_owned()];
        assert_eq!(
            answer(LINK_LIVE_SESSION_PATH, &moq),
            Answer::AcceptAs(tilo_live::preface::PROTOCOL.to_owned()),
            "an offered moq-lite is negotiated"
        );
        assert_eq!(
            answer(LINK_LIVE_SESSION_PATH, &[]),
            Answer::Accept,
            "a browser that offers no subprotocols (Chrome before WebTransport.protocol) is still answered"
        );
        assert_eq!(
            answer(LINK_LIVE_SESSION_PATH, &["moq-lite-01".to_owned()]),
            Answer::Refuse,
            "one that offers only other protocols speaks none the live session does"
        );
        assert_eq!(answer(LINK_SESSION_PATH, &[]), Answer::Accept);
        assert_eq!(answer("/elsewhere", &[]), Answer::Refuse);
    }
}
