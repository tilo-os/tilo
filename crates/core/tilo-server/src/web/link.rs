//! The host's WebTransport listener: how a page reaches it, with the
//! browser's own transport.
//!
//! HTTP/3 beside the HTTPS listener, on the same certificate. A device opens
//! one session per thing it asks, at that thing's path
//! ([`tilo_server_protocol::web::WebLink`]), and each is served by the same
//! code its iroh ALPN is:
//!
//! - the session path: every stream a member session, as on the server ALPN
//!   ([`ServerProtocol::serve_link`]);
//! - the live session path: the workspace's live session, offered and answered as
//!   `moq-lite`, after the device proves itself on the first stream;
//! - the page path: a workspace's apps as web pages ([`super::page`]), after
//!   the device proves itself on the first stream;
//! - the records path: a device's link to the workspace's records
//!   ([`tilo_peer_state::direct::Serving::serve_link`]), after the device
//!   proves itself on the first stream;
//! - the actions path: devices asking the workspace to run actions
//!   ([`tilo_peer_state::actions::ActionService`]), each request standing on
//!   the signature of the device that made it, since a page's link proves
//!   nothing of its opener.

use std::sync::Arc;

use tilo_net::webtransport::Answer;
use tilo_server_client::link::Link;
use tilo_server_protocol::web::{
    LINK_ACTIONS_PATH, LINK_LIVE_SESSION_PATH, LINK_PAGE_PATH, LINK_RECORDS_PATH,
};

use crate::service::ServerProtocol;

/// What the listener serves.
#[derive(Clone)]
pub(crate) struct Serving {
    pub(crate) server: ServerProtocol,
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
        LINK_PAGE_PATH | LINK_RECORDS_PATH | LINK_ACTIONS_PATH => Answer::Accept,
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

/// The session's first stream, proving which member device opened it, as the
/// session path's handshake does, then the same session as the link every
/// stream after it is: `None` when the device did not prove itself, with the
/// session closed.
async fn proved(
    protocol: &ServerProtocol,
    session: &tilo_net::webtransport::Session,
) -> Option<Link> {
    let opening = Link::web(session.clone());
    let Ok((mut send, mut recv)) = opening.accept_bi().await else {
        return None;
    };
    let Ok(device) = protocol.authenticate_member(&mut recv, &mut send).await else {
        opening.close("the device did not prove itself");
        return None;
    };
    // The session names the device that proved itself on it from here on: it is
    // the transport's remote, and the admission above every frame decides by.
    Some(Link::web_as(session.clone(), device.device_public_key))
}

/// Serve `listener` until it is dropped.
pub(crate) async fn serve(listener: tilo_net::webtransport::Listener, serving: Serving) {
    listener
        .serve(answer, move |path, session| {
            let serving = serving.clone();
            async move {
                match path.as_str() {
                    LINK_LIVE_SESSION_PATH => {
                        super::live::serve_web(
                            &serving.live_origin,
                            &serving.server,
                            live_session(session),
                        )
                        .await;
                    }
                    LINK_PAGE_PATH => {
                        super::page::serve_web(&serving.server, session).await;
                    }
                    LINK_RECORDS_PATH => {
                        let Some(link) = proved(&serving.server, &session).await else {
                            return;
                        };
                        let Some(direct) = serving.server.direct() else {
                            link.close("no workspace is served here");
                            return;
                        };
                        if let Err(error) = direct.serve_link(&link).await {
                            tracing::debug!(%error, "a records link over WebTransport ended");
                        }
                    }
                    LINK_ACTIONS_PATH => {
                        let Some(actions) = serving.server.actions() else {
                            session.close(0, "no workspace is served here");
                            return;
                        };
                        // A page's link proves nothing of its opener: the requests it carries
                        // stand on the signature of the device that made each ([`ActionService`]).
                        actions.serve_link(Arc::new(Link::web(session)));
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
        assert_eq!(answer("/elsewhere", &[]), Answer::Refuse);
    }

    #[test]
    fn a_devices_link_to_its_workspace_is_answered_at_its_paths() {
        assert_eq!(
            answer(LINK_RECORDS_PATH, &[]),
            Answer::Accept,
            "a device's link to the workspace's records is answered"
        );
        assert_eq!(
            answer(LINK_ACTIONS_PATH, &[]),
            Answer::Accept,
            "the same device asking the workspace for an action is answered"
        );
    }
}
