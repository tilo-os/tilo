//! Offline backend — the bundled [`crate::sample`] catalogue served through
//! the [`YtBackend`] trait.
//!
//! This is the default build's data source and the one every test + the
//! `--shots` renderer use, so it must be deterministic and never touch the
//! network. It simply replays the sample model's pure query helpers
//! (`search`, `related`, `channel_videos`, …) and clones the results into
//! owned [`Video`](crate::model::Video) / [`Channel`](crate::model::Channel) structs.

use crate::backend::{BackendError, BackendOutcome, BackendResult, YtBackend};
use crate::model::TubeModel;

/// Serves the offline sample catalogue.
pub struct OfflineBackend {
    model: TubeModel,
}

impl OfflineBackend {
    pub fn new() -> Self {
        Self {
            model: crate::sample::model(),
        }
    }
}

impl Default for OfflineBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl YtBackend for OfflineBackend {
    fn name(&self) -> &'static str {
        "offline sample"
    }

    fn search(&self, query: &str) -> BackendOutcome {
        Ok(BackendResult::Videos(
            self.model.search(query).into_iter().cloned().collect(),
        ))
    }

    fn video(&self, id: &str) -> BackendOutcome {
        match self.model.video(id) {
            Some(v) => Ok(BackendResult::Video(Box::new(v.clone()))),
            None => Err(BackendError(format!("no sample video {id}"))),
        }
    }

    fn channel(&self, id: &str) -> BackendOutcome {
        match self.model.channel(id) {
            Some(c) => Ok(BackendResult::Channel {
                channel: Box::new(c.clone()),
                uploads: self.model.channel_videos(id).into_iter().cloned().collect(),
            }),
            None => Err(BackendError(format!("no sample channel {id}"))),
        }
    }

    fn related(&self, id: &str) -> BackendOutcome {
        Ok(BackendResult::Videos(
            self.model.related(id).into_iter().cloned().collect(),
        ))
    }

    fn trending(&self) -> BackendOutcome {
        let mut vids: Vec<_> = self.model.videos.clone();
        vids.sort_by_key(|v| std::cmp::Reverse(v.views));
        Ok(BackendResult::Videos(vids))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_backend_serves_search_video_related_and_trending() {
        // Case `offline_search_matches_titles`.
        {
            let b = OfflineBackend::new();
            let BackendResult::Videos(v) = b.search("donut").unwrap() else {
                panic!("Videos")
            };
            assert!(v.iter().any(|v| v.title.to_lowercase().contains("donut")));
        }
        // Case `offline_video_and_related_resolve`.
        {
            let b = OfflineBackend::new();
            assert!(matches!(
                b.video("vid_rust_async"),
                Ok(BackendResult::Video(_))
            ));
            let BackendResult::Videos(rel) = b.related("vid_rust_async").unwrap() else {
                panic!("Videos")
            };
            assert!(rel.iter().all(|v| v.id != "vid_rust_async"));
        }
        // Case `offline_trending_is_view_sorted`.
        {
            let b = OfflineBackend::new();
            let BackendResult::Videos(v) = b.trending().unwrap() else {
                panic!()
            };
            for w in v.windows(2) {
                assert!(w[0].views >= w[1].views, "trending sorted by views desc");
            }
        }
        // Case `offline_missing_video_errors`.
        {
            let b = OfflineBackend::new();
            assert!(b.video("nope").is_err());
        }
    }
}
