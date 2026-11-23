//! The connector process: the one place YouTube is asked for a tilolang app whose types hold
//! its resources (`type Video (origin: youtube.video)`). The session's launcher starts it for
//! the app, in the app's sandbox (the network its manifest declares for the origin), and it
//! lives whether or not a window of the app is open.
//!
//! Each instance of the app that runs attaches over a socket the session hands both
//! ([`crate::remote::RemoteService`] at the other end). Its questions are answered one after
//! the other, in the order it asked, on a thread of its own; nothing is kept between them,
//! since the origin writes what YouTube answered into the package's records.

use std::os::unix::net::UnixStream;
use std::sync::Arc;

use crate::service::Service;
use crate::wire::{self, Ask, FromConnector};

/// The name of the origin this connects, as the launcher names it to the runtime
/// (`TILO_TILOLANG_CONNECTOR`).
pub const ORIGIN: &str = "youtube";

/// Run the YouTube connector this process was started as (`TILO_TILOLANG_CONNECTOR=youtube`),
/// on the socket the launcher handed it, until the session closes the socket: every instance that
/// attaches has its questions answered by `service`.
pub fn run(service: &Arc<dyn Service>) -> Result<(), String> {
    let socket = runtime_portal::origin_connector::take_socket()
        .ok_or_else(|| "the launcher handed the YouTube connector no socket".to_owned())?;
    let attachments =
        runtime_portal::origin_connector::attachments(socket, |failed| match failed {
            None => tracing::info!("the session closed the YouTube connector's socket"),
            Some(error) => tracing::error!(%error, "the YouTube connector's socket failed"),
        });
    serve(service, attachments);
    Ok(())
}

/// Run the connector over `service` until `attachments` ends (the session closed its socket):
/// every instance that attaches has its questions answered.
pub fn serve(service: &Arc<dyn Service>, attachments: impl Iterator<Item = UnixStream>) {
    for stream in attachments {
        let service = Arc::clone(service);
        let spawned = std::thread::Builder::new()
            .name("youtube-attached".into())
            .spawn(move || answer_each(stream, &*service));
        if let Err(error) = spawned {
            tracing::error!(%error, "an instance could not attach to the YouTube connector");
        }
    }
    tracing::info!("the session closed the YouTube connector's socket");
}

/// Answer each question the instance at `stream` asks, in the order it asks them, until it goes.
/// The instance asks one at a time: it waits for an answer before it asks the next.
fn answer_each(mut stream: UnixStream, service: &dyn Service) {
    loop {
        let ask = match wire::read::<Ask>(&mut stream) {
            Ok(Some(ask)) => ask,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, "an attached instance said something unreadable");
                return;
            }
        };
        let answer = asked(service, &ask).unwrap_or_else(|reason| FromConnector::Failed {
            request: wire::request_of(&ask),
            reason,
        });
        if let Err(error) = wire::write(&mut stream, &answer) {
            tracing::warn!(%error, "an instance left before its answer");
            return;
        }
    }
}

/// What the service says about `ask`.
fn asked(service: &dyn Service, ask: &Ask) -> Result<FromConnector, String> {
    let failed = |error: crate::service::ServiceError| error.0;
    match ask {
        Ask::Search { request, query } => {
            let page = service.search(query).map_err(failed)?;
            Ok(FromConnector::Search {
                request: *request,
                page,
            })
        }
        Ask::Channel { request, key } => {
            let page = service.channel(key).map_err(failed)?;
            Ok(FromConnector::Channel {
                request: *request,
                page,
            })
        }
        Ask::Playlist { request, key } => {
            let page = service.playlist(key).map_err(failed)?;
            Ok(FromConnector::Playlist {
                request: *request,
                page,
            })
        }
        Ask::Trending { request } => {
            let page = service.trending().map_err(failed)?;
            Ok(FromConnector::Trending {
                request: *request,
                page,
            })
        }
        Ask::Stream { request, key } => {
            let url = service.stream(key).map_err(failed)?;
            Ok(FromConnector::Stream {
                request: *request,
                url,
            })
        }
    }
}
