//! Offline backend — the default. Answers nothing from the network.
//!
//! The app's bundled [`crate::seed`] sample library is what populates every
//! screen offline, so this backend deliberately returns an empty search result
//! (the UI falls back to [`crate::model::Library::search`] over the sample) and
//! refuses stream resolution rather than fabricating a URL. Zero dependencies —
//! this is what the hermetic `--shots` / CI build links.

use super::{BackendError, MusicBackend, ResolveOutcome, SearchOutcome};

/// The metadata-only offline data source.
#[derive(Clone, Copy, Debug, Default)]
pub struct OfflineBackend;

impl OfflineBackend {
    pub fn new() -> Self {
        Self
    }
}

impl MusicBackend for OfflineBackend {
    fn name(&self) -> &'static str {
        "offline (bundled library)"
    }

    fn search_tracks(&self, _query: &str) -> SearchOutcome {
        // The app renders its bundled library for search offline; the backend
        // adds nothing. Empty (not an error) so callers treat it as "no live
        // hits" and keep showing local results.
        Ok(Vec::new())
    }

    fn resolve_audio(&self, video_id: &str) -> ResolveOutcome {
        Err(BackendError(format!(
            "offline: no live backend compiled — cannot resolve a stream for {video_id} \
             (build with `--features live` to stream from YouTube Music)"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_search_is_empty_and_resolve_errors() {
        let b = OfflineBackend::new();
        assert_eq!(b.name(), "offline (bundled library)");
        assert!(b.search_tracks("anything").unwrap().is_empty());
        assert!(b.resolve_audio("abc").is_err());
    }
}
