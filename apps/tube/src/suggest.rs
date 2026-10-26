//! `YouTube`'s answers for what the platform search is being asked, while
//! the person types: the few top videos for the query, under the row that
//! searches `YouTube` for it.
//!
//! Bounded by construction: one request in flight per window, asked only
//! once the typing has paused for [`DEBOUNCE`], and always for the latest
//! query, so a burst of keystrokes costs one request, not one per letter.

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::api::Update;
use crate::model::Video;

/// How long the typing must pause before `YouTube` is asked.
pub const DEBOUNCE: Duration = Duration::from_millis(275);
/// How many videos an answer offers.
pub const SUGGESTED_VIDEOS: usize = 5;

/// What `YouTube` answered for the platform search, as the window holds it.
#[derive(Clone, Debug, Default)]
pub struct Suggested {
    /// The query `videos` answer.
    pub query: String,
    pub videos: Vec<Video>,
    /// The latest query is asked and not yet answered.
    pub awaiting: bool,
}

impl Suggested {
    /// Whether `query`'s answer is in.
    #[must_use]
    pub fn answered(&self, query: &str) -> bool {
        !self.query.is_empty() && self.query == query
    }

    /// The videos answered for `query`; none when what is held answers
    /// another query.
    #[must_use]
    pub fn videos_for(&self, query: &str) -> &[Video] {
        if self.answered(query) {
            &self.videos
        } else {
            &[]
        }
    }
}

/// The query waiting to be asked, shared by the window and the one task
/// asking. Cheap to clone.
#[derive(Clone, Default)]
pub struct Suggesting(Arc<Mutex<Wanted>>);

#[derive(Default)]
struct Wanted {
    /// The latest query not yet asked.
    query: Option<String>,
    /// Whether a task is asking (or waiting out the pause to ask).
    asking: bool,
}

impl Suggesting {
    fn wanted(&self) -> std::sync::MutexGuard<'_, Wanted> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ask for `query`'s videos, replacing any query not yet asked. True
    /// when no task is asking, so the caller starts one ([`ask_youtube`]).
    pub fn want(&self, query: &str) -> bool {
        let mut wanted = self.wanted();
        wanted.query = Some(query.to_owned());
        !std::mem::replace(&mut wanted.asking, true)
    }

    /// Stop asking: no task could start.
    pub fn give_up(&self) {
        *self.wanted() = Wanted::default();
    }

    /// The query to ask now: the latest, once nothing newer arrived during
    /// a [`DEBOUNCE`] pause. `None` (and the task ends) when nothing is
    /// wanted.
    async fn next(&self) -> Option<String> {
        loop {
            let seen = {
                let mut wanted = self.wanted();
                if wanted.query.is_none() {
                    wanted.asking = false;
                    return None;
                }
                wanted.query.clone()
            };
            ui::rt::sleep(DEBOUNCE).await;
            let mut wanted = self.wanted();
            if wanted.query == seen {
                return wanted.query.take();
            }
        }
    }
}

/// Ask `fetch` for the videos of each query `suggesting` wants, one at a
/// time, posting each answer (empty when the source failed, so the search
/// stops waiting) through `post`.
pub async fn ask_youtube<F, A>(suggesting: Suggesting, fetch: F, post: impl Fn(Update))
where
    F: Fn(String) -> A,
    A: Future<Output = Vec<Video>>,
{
    while let Some(query) = suggesting.next().await {
        let mut videos = fetch(query.clone()).await;
        videos.truncate(SUGGESTED_VIDEOS);
        post(Update::Suggestions { query, videos });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(id: &str) -> Video {
        Video {
            id: id.into(),
            title: format!("Video {id}"),
            ..crate::sample::model().videos[0].clone()
        }
    }

    #[test]
    fn a_burst_of_typing_asks_once_for_the_latest_query_one_at_a_time() {
        let suggesting = Suggesting::default();
        assert!(suggesting.want("c"), "the first query starts the asking");
        assert!(!suggesting.want("ca"), "later ones join it");
        assert!(!suggesting.want("cat"));
        let asked = Arc::new(Mutex::new(Vec::<String>::new()));
        let posted = Arc::new(Mutex::new(Vec::<(String, usize)>::new()));
        let fetch = {
            let (asked, typing) = (Arc::clone(&asked), suggesting.clone());
            move |query: String| {
                let (asked, typing) = (Arc::clone(&asked), typing.clone());
                async move {
                    asked.lock().unwrap().push(query.clone());
                    if query == "cat" {
                        // Typed on while the first answer is on its way.
                        assert!(!typing.want("cats"), "one request in flight");
                    }
                    (0..8).map(|at| video(&format!("{query}{at}"))).collect()
                }
            }
        };
        let post = {
            let posted = Arc::clone(&posted);
            move |update: Update| {
                if let Update::Suggestions { query, videos } = update {
                    posted.lock().unwrap().push((query, videos.len()));
                }
            }
        };
        ui::rt::app()
            .block_on(ask_youtube(suggesting.clone(), fetch, post))
            .unwrap();
        assert_eq!(*asked.lock().unwrap(), ["cat", "cats"]);
        assert_eq!(
            *posted.lock().unwrap(),
            [
                ("cat".to_owned(), SUGGESTED_VIDEOS),
                ("cats".to_owned(), SUGGESTED_VIDEOS)
            ]
        );
        assert!(suggesting.want("dogs"), "the asking ended with the typing");
    }

    #[test]
    fn an_answer_is_held_only_for_its_own_query() {
        let held = Suggested {
            query: "cats".into(),
            videos: vec![video("a")],
            awaiting: false,
        };
        assert_eq!(held.videos_for("cats").len(), 1);
        assert!(held.videos_for("cat").is_empty());
        assert!(!Suggested::default().answered(""));
    }
}
