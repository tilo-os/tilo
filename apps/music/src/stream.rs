//! The non-blocking streaming worker.
//!
//! All network + extraction work — real `YouTube` Music search and audio-stream
//! resolution — runs on a background thread so the UI thread never blocks (in
//! a browser page, which has no threads, as app tasks one request at a time). The
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

use std::sync::mpsc::{Receiver, Sender, SyncSender, TrySendError};

use crate::backend::{
    AlbumPage, ArtistPage, HomeContent, MusicBackend, PlaylistPage, ResolvedStream, SearchBundle,
    default_backend, preferred_backend,
};

/// Network work is deliberately back-pressured. A fast key-repeat or an agent
/// cannot enqueue an unbounded number of expensive searches/resolves while the
/// serial source is waiting on the network.
const REQUEST_QUEUE_LIMIT: usize = 16;
const MAX_QUERY_BYTES: usize = 512;
const MAX_EXTERNAL_ID_BYTES: usize = 512;

/// A unit of work for the background thread.
#[derive(Clone, Debug)]
pub enum StreamRequest {
    Spotify {
        request_id: u64,
        operation: crate::backend::SpotifyOperation,
    },
    ConnectSpotifyPlayback,
    ConnectSpotify,
    NativeSpotify {
        track_id: String,
        source_id: String,
        request_id: u64,
    },
    DisconnectSpotify,
    Source {
        spotify: bool,
        sign_in: bool,
    },
    /// Run a real multi-type YouTube-Music search (songs, albums, artists,
    /// playlists + a top result) in one call.
    Search {
        query: String,
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
    Spotify {
        request_id: u64,
        result: Result<crate::backend::SpotifyReply, String>,
    },
    NativeSpotify {
        track_id: String,
        source_id: String,
        account: String,
        request_id: u64,
    },
    SourceSelected {
        result: Result<(String, bool), String>,
    },
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
    HomeUnavailable { error: String },
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
    tx: SyncSender<StreamRequest>,
    rx: Receiver<StreamUpdate>,
    startup_error: Option<String>,
    /// Where there are no threads (a browser page): the backend and the
    /// requests, answered one at a time as app tasks.
    tasks: Option<Tasks>,
}

struct Tasks {
    backend: std::sync::Arc<dyn MusicBackend>,
    requests: std::sync::Arc<std::sync::Mutex<Receiver<StreamRequest>>>,
    updates: Sender<StreamUpdate>,
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
        let (req_tx, req_rx) = std::sync::mpsc::sync_channel::<StreamRequest>(REQUEST_QUEUE_LIMIT);
        let (up_tx, up_rx) = std::sync::mpsc::channel::<StreamUpdate>();

        if !ui::rt::available(ui::rt::Feature::Threads) {
            let backend: std::sync::Arc<dyn MusicBackend> =
                std::sync::Arc::from(open_backend(&up_tx, force_offline));
            return Self {
                tx: req_tx,
                rx: up_rx,
                startup_error: None,
                tasks: Some(Tasks {
                    backend,
                    requests: std::sync::Arc::new(std::sync::Mutex::new(req_rx)),
                    updates: up_tx,
                }),
            };
        }

        let startup_error = ui::rt::spawn_thread("music-stream", move || {
            run_worker(&req_rx, &up_tx, force_offline);
        })
        .err()
        .map(|error| {
            tracing::error!(target: "music::stream", %error, "could not spawn streaming worker");
            format!("could not start music worker: {error}")
        });

        Self {
            tx: req_tx,
            rx: up_rx,
            startup_error,
            tasks: None,
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
        validate_request(&req)?;
        self.tx.try_send(req).map_err(|error| match error {
            TrySendError::Full(_) => {
                "music worker is busy; wait for the current request or try again".to_string()
            }
            TrySendError::Disconnected(_) => "music worker is unavailable".to_string(),
        })?;
        if let Some(tasks) = &self.tasks {
            let (backend, requests, updates) = (
                std::sync::Arc::clone(&tasks.backend),
                std::sync::Arc::clone(&tasks.requests),
                tasks.updates.clone(),
            );
            // One queued request per task, answered in order.
            drop(ui::rt::app().spawn_blocking(move || {
                let next = requests.lock().ok().and_then(|queue| queue.try_recv().ok());
                if let Some(request) = next {
                    handle_request(backend.as_ref(), &updates, request);
                }
            }));
        }
        Ok(())
    }

    /// Non-blocking drain of any updates ready this frame.
    pub fn poll(&self) -> Vec<StreamUpdate> {
        self.rx.try_iter().collect()
    }

    #[cfg(test)]
    fn unavailable_for_test() -> Self {
        let (tx, _request_rx) = std::sync::mpsc::sync_channel::<StreamRequest>(REQUEST_QUEUE_LIMIT);
        let (_update_tx, rx) = std::sync::mpsc::channel::<StreamUpdate>();
        Self {
            tx,
            rx,
            startup_error: Some("test worker startup failure".to_string()),
            tasks: None,
        }
    }
}

pub(crate) fn valid_native_track(id: &str) -> bool {
    id.strip_prefix("spotify:track:")
        .is_some_and(|id| id.len() == 22 && id.bytes().all(|byte| byte.is_ascii_alphanumeric()))
}

fn validate_request(request: &StreamRequest) -> Result<(), String> {
    let valid_id = |value: &str| !value.is_empty() && value.len() <= MAX_EXTERNAL_ID_BYTES;
    match request {
        StreamRequest::Spotify { operation, .. } => {
            if format!("{operation:?}").len() > 2048 {
                return Err("Spotify operation is too large".into());
            }
        }
        StreamRequest::NativeSpotify {
            track_id,
            source_id,
            ..
        } => {
            if !valid_id(track_id) || !valid_native_track(source_id) {
                return Err("Invalid Spotify track identity".into());
            }
        }
        StreamRequest::Search { query, .. } => {
            if query.trim().is_empty() {
                return Err("music search needs a non-empty query".to_string());
            }
            if query.len() > MAX_QUERY_BYTES {
                return Err(format!(
                    "music search is limited to {MAX_QUERY_BYTES} UTF-8 bytes"
                ));
            }
        }
        StreamRequest::Resolve {
            track_id, video_id, ..
        } => {
            if !valid_id(track_id) || !valid_id(video_id) {
                return Err("music track identifiers are empty or too long".to_string());
            }
        }
        StreamRequest::Artist(id) | StreamRequest::Album(id) | StreamRequest::Playlist(id) => {
            if !valid_id(id) {
                return Err("music resource identifier is empty or too long".to_string());
            }
        }
        StreamRequest::Home
        | StreamRequest::Source { .. }
        | StreamRequest::DisconnectSpotify
        | StreamRequest::ConnectSpotifyPlayback
        | StreamRequest::ConnectSpotify => {}
    }
    Ok(())
}

fn run_worker(req_rx: &Receiver<StreamRequest>, up_tx: &Sender<StreamUpdate>, force_offline: bool) {
    let backend = open_backend(up_tx, force_offline);
    while let Ok(request) = req_rx.recv() {
        handle_request(backend.as_ref(), up_tx, request);
    }
}

fn open_backend(up_tx: &Sender<StreamUpdate>, force_offline: bool) -> Box<dyn MusicBackend> {
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
    backend
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
        StreamRequest::Spotify {
            request_id,
            operation,
        } => {
            let result = backend
                .spotify_operation(&operation)
                .map_err(|error| error.to_string());
            let _ = up_tx.send(StreamUpdate::Spotify { request_id, result });
        }
        StreamRequest::ConnectSpotifyPlayback => {
            let result = backend
                .spotify_playback(true)
                .map(|_| (crate::i18n::t("music-spotify-playback-authorized"), true))
                .map_err(|error| error.to_string());
            let _ = up_tx.send(StreamUpdate::SourceSelected { result });
        }
        StreamRequest::NativeSpotify {
            track_id,
            source_id,
            request_id,
        } => {
            let result = if valid_native_track(&source_id) {
                backend.spotify_playback(false)
            } else {
                Err(crate::backend::BackendError("Invalid Spotify track".into()))
            };
            let update = match result {
                Ok(account) => StreamUpdate::NativeSpotify {
                    track_id,
                    source_id,
                    account,
                    request_id,
                },
                Err(error) => StreamUpdate::ResolveFailed {
                    track_id,
                    request_id,
                    error: error.to_string(),
                },
            };
            let _ = up_tx.send(update);
        }
        StreamRequest::DisconnectSpotify => {
            let result = backend
                .disconnect_spotify()
                .map(|()| (backend.name().to_string(), backend.is_live()))
                .map_err(|error| error.to_string());
            let _ = up_tx.send(StreamUpdate::SourceSelected { result });
        }
        StreamRequest::ConnectSpotify => {
            let result = backend
                .connect_spotify_read_only()
                .map(|()| (backend.name().to_string(), backend.is_live()))
                .map_err(|error| error.to_string());
            let _ = up_tx.send(StreamUpdate::SourceSelected { result });
        }
        StreamRequest::Source { spotify, sign_in } => {
            let result = backend
                .select_source(spotify, sign_in)
                .map(|()| (backend.name().to_string(), backend.is_live()))
                .map_err(|error| error.to_string());
            let _ = up_tx.send(StreamUpdate::SourceSelected { result });
        }
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
                let _ = up_tx.send(StreamUpdate::HomeUnavailable {
                    error: e.to_string(),
                });
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
    fn stream_worker_is_bounded_honest_and_non_blocking() {
        // Phase `worker_announces_backend_and_offline_resolve_fails_gracefully`.
        {
            let w = StreamWorker::spawn_offline();
            // Pin the hermetic backend: a unit test must never depend on the host's
            // network or spend the live request deadline probing a synthetic id.
            w.request(StreamRequest::Resolve {
                track_id: "tk_x".into(),
                video_id: "synthetic".into(),
                request_id: 1,
            })
            .expect("worker request is accepted");

            // Drain with a bounded wait — the worker replies on its own thread. A
            // It breaks out the instant a terminal update lands.
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

        // Phase `unavailable_worker_rejects_enqueue_instead_of_silently_losing_work`.
        {
            let worker = StreamWorker::unavailable_for_test();
            let error = worker
                .request(StreamRequest::Search {
                    query: "anything".into(),
                    request_id: 1,
                })
                .expect_err("startup failure must be surfaced to the caller");
            assert!(error.contains("startup failure"));
        }

        // Phase `request_boundary_rejects_oversized_values`.
        {
            let worker = StreamWorker::spawn_offline();
            let error = worker
                .request(StreamRequest::Search {
                    query: "x".repeat(MAX_QUERY_BYTES + 1),
                    request_id: 1,
                })
                .expect_err("oversized query must be refused before enqueue");
            assert!(error.contains("limited"));

            let error = worker
                .request(StreamRequest::Resolve {
                    track_id: "track".into(),
                    video_id: "v".repeat(MAX_EXTERNAL_ID_BYTES + 1),
                    request_id: 2,
                })
                .expect_err("oversized external id must be refused before enqueue");
            assert!(error.contains("too long"));
        }

        // Phase `request_queue_is_bounded_and_never_blocks_the_caller`.
        {
            let (tx, request_rx) = std::sync::mpsc::sync_channel(REQUEST_QUEUE_LIMIT);
            let (_update_tx, rx) = std::sync::mpsc::channel();
            let worker = StreamWorker {
                tx,
                rx,
                startup_error: None,
                tasks: None,
            };
            for _ in 0..REQUEST_QUEUE_LIMIT {
                worker
                    .request(StreamRequest::Home)
                    .expect("capacity remains available");
            }
            let error = worker
                .request(StreamRequest::Home)
                .expect_err("one more request is back-pressured");
            assert!(error.contains("busy"));
            drop(request_rx);
        }
    }
}
