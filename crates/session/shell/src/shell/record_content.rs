//! The session's side of a record's content: the device content store the
//! filesystem daemon keeps, reached per record scope, and the shell network
//! as the route that carries content between the record's devices.

use std::sync::{Arc, Mutex};

use tilo_action_broker::record_content::{
    ContentPeer, ContentPeers, ContentPiece, ContentScope, ContentShelf, RouteContent,
};
use tilo_action_broker::route::CrdtRoute;

use super::network::{InboundRecordContent, ShellNetwork};

/// The filesystem daemon's content store, read and written through the
/// scope a record's content lives under — the same partition the record's
/// app stores its pictures in.
struct SessionShelf;

impl ContentShelf for SessionShelf {
    fn read(&self, scope: &ContentScope, hash: &str) -> Option<Vec<u8>> {
        use runtime_fs::blobs::ReadOutcome;

        // Only what this device keeps: asking for missing content would set
        // the daemon fetching it, and a request from a peer must not.
        let result = super::broker_services::with_session_blobs(|client| {
            let scoped = client.scope(&scope.room, &scope.app)?;
            if !scoped.has(hash)? {
                return Ok(None);
            }
            Ok(match scoped.read_all(hash)? {
                ReadOutcome::Ready { bytes, .. } => Some(bytes),
                ReadOutcome::Pending | ReadOutcome::Unavailable => None,
            })
        });
        result.unwrap_or_else(|error| {
            tracing::debug!(%error, "record content store unavailable");
            None
        })
    }

    fn keep(&self, scope: &ContentScope, hash: &str, bytes: &[u8]) -> Result<(), String> {
        let kept = super::broker_services::with_session_blobs(|client| {
            client
                .scope(&scope.room, &scope.app)?
                .put(bytes, "application/octet-stream")
        })
        .map_err(|error| error.to_string())?;
        if kept.hash == hash {
            Ok(())
        } else {
            Err("the content store keeps these bytes under another address".into())
        }
    }
}

struct NetworkPeers(Arc<ShellNetwork>);

impl ContentPeers for NetworkPeers {
    fn want(&self, route: &CrdtRoute, hash: &str) {
        self.0.want_record_content(route, hash);
    }

    fn answer(&self, route: &CrdtRoute, peer: ContentPeer, piece: ContentPiece) {
        self.0.answer_record_content(route, peer, piece);
    }

    fn lacks(&self, route: &CrdtRoute, peer: ContentPeer, hash: &str) {
        self.0.lacks_record_content(route, peer, hash);
    }

    fn sources(&self, route: &CrdtRoute) -> tilo_action_broker::record_content::ContentSources {
        self.0.record_content_sources(route)
    }
}

pub fn session_record_content(network: &Arc<ShellNetwork>) -> Arc<RouteContent> {
    RouteContent::new(
        Arc::new(SessionShelf),
        Arc::new(NetworkPeers(Arc::clone(network))),
    )
}

/// Hand the requests and answers other devices sent to `content`. Every
/// request reaching this queue came from a device the room admitted to
/// read it.
pub fn drain(inbound: &Mutex<Vec<InboundRecordContent>>, content: &RouteContent) -> bool {
    let drained: Vec<_> = {
        let Ok(mut inbound) = inbound.lock() else {
            return false;
        };
        std::mem::take(&mut *inbound)
    };
    for entry in drained {
        match entry {
            InboundRecordContent::Want { route, peer, hash } => {
                content.peer_wants(&route, peer, true, &hash);
            }
            InboundRecordContent::Piece { route, piece } => content.peer_piece(&route, &piece),
            InboundRecordContent::Lacks { route, peer, hash } => {
                content.peer_lacks(&route, peer, &hash);
            }
            InboundRecordContent::SourcesChanged => content.sources_changed(),
        }
    }
    true
}
