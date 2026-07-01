//! The non-blocking streaming worker.
//!
//! All network + extraction work — real YouTube Music search and audio-stream
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

use crate::backend::{preferred_backend, ResolvedStream, TrackHit};

/// A unit of work for the background thread.
#[derive(Clone, Debug)]
pub enum StreamRequest {
    /// Run a real YouTube-Music track search.
    Search(String),
    /// Resolve a directly-playable audio URL for `video_id`. `track_id` is the
    /// app's library id, echoed back so the result can be correlated with the
    /// track that is (still) current when it lands.
    Resolve { track_id: String, video_id: String },
}

/// A result folded back into the app on the UI thread.
#[derive(Clone, Debug)]
pub enum StreamUpdate {
    /// One-time announcement of which backend the worker bound to.
    Backend { name: String, live: bool },
    /// Real search hits for a query.
    SearchResults { query: String, hits: Vec<TrackHit> },
    /// A track's audio stream resolved to a playable URL.
    Resolved { track_id: String, stream: Box<ResolvedStream> },
    /// A resolve attempt failed (offline, geo-blocked, PoToken-gated, …).
    ResolveFailed { track_id: String, error: String },
    /// A non-fatal note for the status log / a toast.
    Note(String),
}

/// Handle to the background worker: a request sender + an update receiver.
pub struct StreamWorker {
    tx: Sender<StreamRequest>,
    rx: Receiver<StreamUpdate>,
}

impl StreamWorker {
    /// Spawn the worker thread. Constructing the backend (live network init)
    /// happens *inside* the thread, so this call never blocks the caller.
    pub fn spawn() -> Self {
        let (req_tx, req_rx) = std::sync::mpsc::channel::<StreamRequest>();
        let (up_tx, up_rx) = std::sync::mpsc::channel::<StreamUpdate>();

        std::thread::Builder::new()
            .name("music-stream".into())
            .spawn(move || {
                // Build the data source once. `preferred_backend` picks live
                // rustypipe if compiled + reachable, else the offline sample —
                // never panicking, so the worker always has a backend.
                let backend = preferred_backend(|note| {
                    let _ = up_tx.send(StreamUpdate::Note(note));
                });
                let _ = up_tx.send(StreamUpdate::Backend {
                    name: backend.name().to_string(),
                    live: backend.is_live(),
                });

                while let Ok(req) = req_rx.recv() {
                    match req {
                        StreamRequest::Search(query) => match backend.search_tracks(&query) {
                            Ok(hits) => {
                                let _ = up_tx
                                    .send(StreamUpdate::SearchResults { query, hits });
                            }
                            Err(e) => {
                                let _ = up_tx.send(StreamUpdate::Note(format!("search: {e}")));
                            }
                        },
                        StreamRequest::Resolve { track_id, video_id } => {
                            match backend.resolve_audio(&video_id) {
                                Ok(stream) => {
                                    let _ = up_tx.send(StreamUpdate::Resolved {
                                        track_id,
                                        stream: Box::new(stream),
                                    });
                                }
                                Err(e) => {
                                    let _ = up_tx.send(StreamUpdate::ResolveFailed {
                                        track_id,
                                        error: e.to_string(),
                                    });
                                }
                            }
                        }
                    }
                }
            })
            .expect("spawn music-stream worker");

        Self { tx: req_tx, rx: up_rx }
    }

    /// Queue a request. Fire-and-forget; a dead worker is silently ignored.
    pub fn request(&self, req: StreamRequest) {
        let _ = self.tx.send(req);
    }

    /// Non-blocking drain of any updates ready this frame.
    pub fn poll(&self) -> Vec<StreamUpdate> {
        self.rx.try_iter().collect()
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
        });

        // Drain with a bounded wait — the worker replies on its own thread.
        let mut announced = false;
        let mut failed_or_resolved = false;
        for _ in 0..200 {
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
        assert!(failed_or_resolved, "resolve produced a terminal update, no hang");
    }
}
