//! The non-blocking streaming worker.
//!
//! All network + extraction work — real `YouTube` Music search and audio-stream
//! resolution — runs on a background thread so the UI thread never blocks. The
//! app talks to it with two plain channels: it *sends* [`StreamRequest`]s
//! (fire-and-forget) and *drains* [`StreamUpdate`]s once per frame via
//! [`StreamWorker::poll`] (a non-blocking `try_iter`). The worker owns a
//! [`crate::backend::MusicBackend`] chosen at runtime — the live rustypipe
//! client when `--features live` is on and it initialises, otherwise the
//! offline sample (so this seam compiles and runs identically either way).
//!
//! The worker is spawned lazily on the first real interaction (a Play or a
//! search) — never during `--shots` or layout — so the screenshot path stays
//! hermetic and no thread or socket is created for a headless render.

use std::sync::mpsc::{Receiver, Sender};

use crate::backend::{
    AlbumPage, ArtistPage, HomeContent, MusicBackend, PlaylistPage, ResolvedStream, SearchBundle,
    SearchKind, default_backend, preferred_backend,
};

/// A unit of work for the background thread.
#[derive(Clone, Debug)]
pub enum StreamRequest {
    /// Run a real multi-type YouTube-Music search (songs, albums, artists,
    /// playlists + a top result) in one call.
    Search { query: String, request_id: u64 },
    /// A deep per-type "see all" search for the given filter tab.
    SearchMore {
        query: String,
        kind: SearchKind,
        request_id: u64,
    },
    /// Resolve a directly-playable audio URL for `video_id`. `track_id` is the
    /// app's library id, echoed back so the result can be correlated with the
    /// track that is (still) current when it lands.
    Resolve {
        track_id: String,
        video_id: String,
        /// Caller-owned monotonic identity. The app accepts a response only
        /// when it still matches its newest request, preventing A→B→A from
        /// adopting the first (stale) A response.
        request_id: u64,
    },
    /// Fetch the real Home page (charts / new releases / top artists).
    Home,
    /// Fetch a real artist page by `YouTube` channel id.
    Artist(String),
    /// Fetch a real album's track list by `YouTube` album id.
    Album(String),
    /// Fetch a real playlist's track list by `YouTube` playlist id.
    Playlist(String),
}

/// A result folded back into the app on the UI thread.
#[derive(Clone, Debug)]
pub enum StreamUpdate {
    /// One-time announcement of which backend the worker bound to.
    Backend { name: String, live: bool },
    /// Real multi-type search results for a query (the mixed "All" bundle).
    SearchResults {
        query: String,
        request_id: u64,
        bundle: Box<SearchBundle>,
    },
    /// A search could not be completed. This is a terminal response with the
    /// original request identity, not an unstructured log note, so the UI can
    /// clear its busy state and offer a retry for the exact query.
    SearchUnavailable {
        query: String,
        request_id: u64,
        error: String,
    },
    /// A deep per-type "see all" result for a filter tab.
    SearchMore {
        query: String,
        kind: SearchKind,
        request_id: u64,
        bundle: Box<SearchBundle>,
    },
    SearchMoreUnavailable {
        query: String,
        kind: SearchKind,
        request_id: u64,
        error: String,
    },
    /// A track's audio stream resolved to a playable URL.
    Resolved {
        track_id: String,
        request_id: u64,
        stream: Box<ResolvedStream>,
    },
    /// A resolve attempt failed (offline, geo-blocked, PoToken-gated, …).
    ResolveFailed {
        track_id: String,
        request_id: u64,
        error: String,
    },
    /// The real Home page landed.
    Home { content: Box<HomeContent> },
    /// The Home fetch could not complete (offline build, geo-block, network
    /// error). The UI stops the loading skeleton and shows its honest empty
    /// Home rather than shimmering forever.
    HomeUnavailable,
    /// A real artist page landed.
    Artist { page: Box<ArtistPage> },
    /// A real album page landed.
    Album { page: Box<AlbumPage> },
    /// A real playlist page landed.
    Playlist { page: Box<PlaylistPage> },
    /// A requested detail page (artist / album / playlist) could not be
    /// fetched. Carries the id so the UI stops that page's skeleton and shows
    /// an honest "couldn't load" state instead of an endless shimmer.
    PageUnavailable { id: String },
    /// A non-fatal note for the status log / a toast.
    Note(String),
}

/// Handle to the background worker: a request sender + an update receiver.
pub struct StreamWorker {
    tx: Sender<StreamRequest>,
    rx: Receiver<StreamUpdate>,
    startup_error: Option<String>,
}

impl StreamWorker {
    /// Spawn the worker thread. Constructing the backend (live network init)
    /// happens *inside* the thread, so this call never blocks the caller.
    pub fn spawn() -> Self {
        Self::spawn_inner(false)
    }

    /// Spawn a worker pinned to the **bundled offline backend**, whatever this
    /// build compiled in.
    ///
    /// [`spawn`](Self::spawn) asks [`preferred_backend`] for the best source
    /// available, and since `live` is a *default* feature that is the real
    /// YouTube-Music client on any ordinary build. A test about how the app
    /// behaves when a page cannot be served therefore depends on the host's
    /// network being down — it passes on a disconnected machine and fails on a
    /// connected one, having proved nothing either way. This pins the source so
    /// the outcome is the app's, not the network's.
    pub fn spawn_offline() -> Self {
        Self::spawn_inner(true)
    }

    fn spawn_inner(force_offline: bool) -> Self {
        let (req_tx, req_rx) = std::sync::mpsc::channel::<StreamRequest>();
        let (up_tx, up_rx) = std::sync::mpsc::channel::<StreamUpdate>();

        let startup_error = std::thread::Builder::new()
            .name("music-stream".into())
            .spawn(move || run_worker(&req_rx, &up_tx, force_offline))
            .err()
            .map(|error| {
                tracing::error!(target: "music::stream", %error, "could not spawn streaming worker");
                format!("could not start music worker: {error}")
            });

        Self {
            tx: req_tx,
            rx: up_rx,
            startup_error,
        }
    }

    /// Queue a request. A failed worker is explicit so callers can settle
    /// loading UI rather than spinning forever behind an undeliverable job.
    ///
    /// # Errors
    ///
    /// Returns an error when worker startup failed or its receiving thread has
    /// exited before the request could be delivered.
    pub fn request(&self, req: StreamRequest) -> Result<(), String> {
        if let Some(error) = &self.startup_error {
            return Err(error.clone());
        }
        self.tx
            .send(req)
            .map_err(|error| format!("music worker is unavailable: {error}"))
    }

    /// Non-blocking drain of any updates ready this frame.
    pub fn poll(&self) -> Vec<StreamUpdate> {
        self.rx.try_iter().collect()
    }

    #[cfg(test)]
    fn unavailable_for_test() -> Self {
        let (tx, _request_rx) = std::sync::mpsc::channel::<StreamRequest>();
        let (_update_tx, rx) = std::sync::mpsc::channel::<StreamUpdate>();
        Self {
            tx,
            rx,
            startup_error: Some("test worker startup failure".to_string()),
        }
    }
}

fn run_worker(req_rx: &Receiver<StreamRequest>, up_tx: &Sender<StreamUpdate>, force_offline: bool) {
    // Build the data source once. `preferred_backend` picks live rustypipe if
    // compiled + reachable, else the offline sample.
    let backend = if force_offline {
        let _ = up_tx.send(StreamUpdate::Note(
            "offline backend pinned; using bundled library".to_string(),
        ));
        default_backend()
    } else {
        preferred_backend(|note| {
            let _ = up_tx.send(StreamUpdate::Note(note));
        })
    };
    let _ = up_tx.send(StreamUpdate::Backend {
        name: backend.name().to_string(),
        live: backend.is_live(),
    });

    while let Ok(request) = req_rx.recv() {
        handle_request(backend.as_ref(), up_tx, request);
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive worker protocol match keeps each request and its terminal update adjacent"
)]
fn handle_request(
    backend: &dyn MusicBackend,
    up_tx: &Sender<StreamUpdate>,
    request: StreamRequest,
) {
    match request {
        StreamRequest::Search { query, request_id } => match backend.search_all(&query) {
            Ok(bundle) => {
                let _ = up_tx.send(StreamUpdate::SearchResults {
                    query,
                    request_id,
                    bundle: Box::new(bundle),
                });
            }
            Err(e) => {
                let _ = up_tx.send(StreamUpdate::SearchUnavailable {
                    query,
                    request_id,
                    error: e.to_string(),
                });
            }
        },
        StreamRequest::SearchMore {
            query,
            kind,
            request_id,
        } => match backend.search_more(&query, kind) {
            Ok(bundle) => {
                let _ = up_tx.send(StreamUpdate::SearchMore {
                    query,
                    kind,
                    request_id,
                    bundle: Box::new(bundle),
                });
            }
            Err(e) => {
                let _ = up_tx.send(StreamUpdate::SearchMoreUnavailable {
                    query,
                    kind,
                    request_id,
                    error: e.to_string(),
                });
            }
        },
        StreamRequest::Resolve {
            track_id,
            video_id,
            request_id,
        } => match backend.resolve_audio(&video_id) {
            Ok(stream) => {
                let _ = up_tx.send(StreamUpdate::Resolved {
                    track_id,
                    request_id,
                    stream: Box::new(stream),
                });
            }
            Err(e) => {
                let _ = up_tx.send(StreamUpdate::ResolveFailed {
                    track_id,
                    request_id,
                    error: e.to_string(),
                });
            }
        },
        StreamRequest::Home => match backend.home() {
            Ok(content) => {
                let _ = up_tx.send(StreamUpdate::Home {
                    content: Box::new(content),
                });
            }
            Err(e) => {
                let _ = up_tx.send(StreamUpdate::Note(format!("home: {e}")));
                let _ = up_tx.send(StreamUpdate::HomeUnavailable);
            }
        },
        StreamRequest::Artist(id) => match backend.artist(&id) {
            Ok(page) => {
                let _ = up_tx.send(StreamUpdate::Artist {
                    page: Box::new(page),
                });
            }
            Err(e) => {
                let _ = up_tx.send(StreamUpdate::Note(format!("artist: {e}")));
                let _ = up_tx.send(StreamUpdate::PageUnavailable { id });
            }
        },
        StreamRequest::Album(id) => match backend.album(&id) {
            Ok(page) => {
                let _ = up_tx.send(StreamUpdate::Album {
                    page: Box::new(page),
                });
            }
            Err(e) => {
                let _ = up_tx.send(StreamUpdate::Note(format!("album: {e}")));
                let _ = up_tx.send(StreamUpdate::PageUnavailable { id });
            }
        },
        StreamRequest::Playlist(id) => match backend.playlist(&id) {
            Ok(page) => {
                let _ = up_tx.send(StreamUpdate::Playlist {
                    page: Box::new(page),
                });
            }
            Err(e) => {
                let _ = up_tx.send(StreamUpdate::Note(format!("playlist: {e}")));
                let _ = up_tx.send(StreamUpdate::PageUnavailable { id });
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_announces_backend_and_offline_resolve_fails_gracefully() {
        let w = StreamWorker::spawn();
        // Ask it to resolve a synthetic id. In the default (offline) build this
        // must come back as a ResolveFailed, never a hang or panic.
        w.request(StreamRequest::Resolve {
            track_id: "tk_x".into(),
            video_id: "synthetic".into(),
            request_id: 1,
        })
        .expect("worker request is accepted");

        // Drain with a bounded wait — the worker replies on its own thread. A
        // `live` build first probes the rangeable-source instances (a bounded
        // few-second budget) before falling back, so give it comfortable slack;
        // it breaks out the instant a terminal update lands.
        let mut announced = false;
        let mut failed_or_resolved = false;
        for _ in 0..2000 {
            for up in w.poll() {
                match up {
                    StreamUpdate::Backend { .. } => announced = true,
                    StreamUpdate::ResolveFailed { track_id, .. } => {
                        assert_eq!(track_id, "tk_x");
                        failed_or_resolved = true;
                    }
                    // A `live` build with network could actually resolve it;
                    // either terminal outcome satisfies "never hangs".
                    StreamUpdate::Resolved { .. } => failed_or_resolved = true,
                    _ => {}
                }
            }
            if announced && failed_or_resolved {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(announced, "worker announced its backend");
        assert!(
            failed_or_resolved,
            "resolve produced a terminal update, no hang"
        );
    }

    #[test]
    fn unavailable_worker_rejects_enqueue_instead_of_silently_losing_work() {
        let worker = StreamWorker::unavailable_for_test();
        let error = worker
            .request(StreamRequest::Search {
                query: "anything".into(),
                request_id: 1,
            })
            .expect_err("startup failure must be surfaced to the caller");
        assert!(error.contains("startup failure"));
    }
}
