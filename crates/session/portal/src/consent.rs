//! Whether an app may use a capability the person decides on: record the
//! screen, take a screenshot without asking, use the camera.
//!
//! The person answers the question once per request, for this session, or
//! for good. Answers for good are kept on this device only, never in a
//! workspace: they are one person's privacy choice, not a room's policy.
//! An app that asks the same thing again while its first question waits
//! gets no second sheet; the repeat is declined.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use tilo_service::{Answer, Question, Questions};

use crate::PortalCaller;
use crate::reply::Ended;

/// The kind of the question an access decision asks.
pub const ACCESS_QUESTION: &str = "portal.access";

/// An answer allowing it for this request only.
pub const ALLOW_ONCE: usize = 0;
/// An answer allowing it until the session ends.
pub const ALLOW_SESSION: usize = 1;
/// An answer allowing it for good, as Remember does.
pub const ALLOW_ALWAYS: usize = 2;

/// What the sheet shows for an access question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessQuestion {
    /// What the app asks to use, by its stable name (`screenshot`,
    /// `screencast`, `camera`, `remote-desktop`): the sheet words it.
    pub capability: String,
    /// What the app says it wants it for, as it said it, when it said.
    pub reason: Option<String>,
    /// Whether the person may allow it for good.
    pub may_remember: bool,
    /// The app's own title, subtitle and body for a generic access
    /// dialog (`org.freedesktop.impl.portal.Access`), shown as the app's
    /// words.
    pub text: Option<AccessText>,
}

/// An app's own words for a generic access dialog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessText {
    pub title: String,
    pub subtitle: String,
    pub body: String,
}

#[derive(Default, Serialize, Deserialize)]
struct ConsentFile {
    always: BTreeSet<String>,
}

#[derive(Default)]
struct State {
    always: BTreeSet<String>,
    session: HashSet<String>,
    asking: HashSet<String>,
}

/// The person's access decisions and the questions that make them.
#[derive(Clone)]
pub struct Consents {
    questions: Questions,
    state: Arc<Mutex<State>>,
    path: Option<Arc<PathBuf>>,
}

fn key(app_id: &str, capability: &str) -> String {
    format!("{app_id}\u{1f}{capability}")
}

impl Consents {
    /// Decisions kept at `path` (none kept when `None`), asked through
    /// `questions`.
    #[must_use]
    pub fn new(questions: Questions, path: Option<PathBuf>) -> Self {
        let always = path
            .as_deref()
            .map(|path| {
                tilo_files::native::atomic::json::load_or_default::<ConsentFile>(path).always
            })
            .unwrap_or_default();
        Self {
            questions,
            state: Arc::new(Mutex::new(State {
                always,
                ..State::default()
            })),
            path: path.map(Arc::new),
        }
    }

    /// The questions it asks through.
    #[must_use]
    pub const fn questions(&self) -> &Questions {
        &self.questions
    }

    /// Whether `caller` may use `capability` without asking.
    #[must_use]
    pub fn allowed(&self, caller: &PortalCaller, capability: &str) -> bool {
        let key = key(&caller.app_id, capability);
        let state = self.state.lock_recover();
        state.always.contains(&key) || state.session.contains(&key)
    }

    /// The apps allowed `capability` for good, as `(app, capability)`.
    #[must_use]
    pub fn remembered(&self) -> Vec<(String, String)> {
        self.state
            .lock_recover()
            .always
            .iter()
            .filter_map(|key| key.split_once('\u{1f}'))
            .map(|(app, capability)| (app.to_owned(), capability.to_owned()))
            .collect()
    }

    /// Forget what `app_id` was allowed of `capability`, for good and for
    /// this session; `false` when it was allowed nothing.
    pub fn forget(&self, app_id: &str, capability: &str) -> bool {
        let key = key(app_id, capability);
        let mut state = self.state.lock_recover();
        let session = state.session.remove(&key);
        let removed = state.always.remove(&key);
        let forgotten = session || removed;
        let always = removed.then(|| state.always.clone());
        drop(state);
        if let Some(always) = always {
            self.persist(always);
        }
        forgotten
    }

    fn persist(&self, always: BTreeSet<String>) {
        if let Some(path) = &self.path
            && let Err(error) =
                tilo_files::native::atomic::json::write_private(path, &ConsentFile { always })
        {
            tracing::warn!(%error, "portal consents not kept");
        }
    }

    /// Decide whether `caller` may use what `question` asks about: allowed
    /// already, or asked as question `handle`, which ends with the
    /// person's answer. Remembers what the answer allows beyond this
    /// request.
    ///
    /// # Errors
    /// [`Ended::Cancelled`] when the person declined, or the same question
    /// of the same app is already waiting.
    pub async fn decide(
        &self,
        caller: &PortalCaller,
        handle: &str,
        question: AccessQuestion,
    ) -> Result<(), Ended> {
        let capability = question.capability.clone();
        if self.allowed(caller, &capability) {
            return Ok(());
        }
        let key = key(&caller.app_id, &capability);
        let _asking = Asking::claim(&self.state, &key).ok_or(Ended::Cancelled)?;
        let may_remember = question.may_remember;
        let asked = self.questions.ask(Question {
            handle: handle.to_owned(),
            kind: ACCESS_QUESTION.into(),
            app_id: Some(caller.app_id.clone()),
            label: caller.app_id.clone(),
            details: serde_json::to_value(question).unwrap_or_default(),
        });
        let answer = asked.await.ok_or(Ended::Cancelled)?;
        self.keep(&key, &answer, may_remember);
        Ok(())
    }

    fn keep(&self, key: &str, answer: &Answer, may_remember: bool) {
        let always = may_remember && (answer.remember || answer.choice == Some(ALLOW_ALWAYS));
        let mut state = self.state.lock_recover();
        if always {
            state.always.insert(key.to_owned());
            let kept = state.always.clone();
            drop(state);
            self.persist(kept);
        } else if answer.choice == Some(ALLOW_SESSION) {
            state.session.insert(key.to_owned());
        }
    }
}

/// One question's place among those waiting, until it is answered or its
/// request goes away.
struct Asking {
    state: Arc<Mutex<State>>,
    key: String,
}

impl Asking {
    fn claim(state: &Arc<Mutex<State>>, key: &str) -> Option<Self> {
        state
            .lock_recover()
            .asking
            .insert(key.to_owned())
            .then(|| Self {
                state: Arc::clone(state),
                key: key.to_owned(),
            })
    }
}

impl Drop for Asking {
    fn drop(&mut self) {
        self.state.lock_recover().asking.remove(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller() -> PortalCaller {
        PortalCaller::new("org.example.Recorder")
    }

    fn screencast() -> AccessQuestion {
        AccessQuestion {
            capability: "screencast".into(),
            reason: None,
            may_remember: true,
            text: None,
        }
    }

    async fn answered(consents: &Consents, handle: &str, answer: Option<Answer>) -> bool {
        let caller = caller();
        let deciding = consents.decide(&caller, handle, screencast());
        let answering = async {
            tokio::task::yield_now().await;
            match answer {
                Some(answer) => assert!(consents.questions().answer(handle, answer)),
                None => assert!(consents.questions().decline(handle)),
            }
        };
        let (decided, ()) = tokio::join!(deciding, answering);
        decided.is_ok()
    }

    #[tokio::test]
    async fn once_asks_again_session_and_always_do_not() {
        let consents = Consents::new(Questions::new(), None);
        let once = Answer {
            choice: Some(ALLOW_ONCE),
            ..Answer::default()
        };
        assert!(answered(&consents, "a", Some(once)).await);
        assert!(!consents.allowed(&caller(), "screencast"));

        let session = Answer {
            choice: Some(ALLOW_SESSION),
            ..Answer::default()
        };
        assert!(answered(&consents, "b", Some(session)).await);
        assert!(consents.allowed(&caller(), "screencast"));
        assert!(
            consents.remembered().is_empty(),
            "a session answer is not kept"
        );
        assert!(consents.forget("org.example.Recorder", "screencast"));
        assert!(!consents.allowed(&caller(), "screencast"));
    }

    #[tokio::test]
    async fn a_declined_question_denies() {
        let consents = Consents::new(Questions::new(), None);
        assert!(!answered(&consents, "a", None).await);
        assert!(!consents.allowed(&caller(), "screencast"));
    }

    #[tokio::test]
    async fn remembered_answers_survive_a_restart_on_this_device_only() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("consents.json");
        let consents = Consents::new(Questions::new(), Some(path.clone()));
        let always = Answer {
            remember: true,
            ..Answer::default()
        };
        assert!(answered(&consents, "a", Some(always)).await);
        let again = Consents::new(Questions::new(), Some(path.clone()));
        assert!(again.allowed(&caller(), "screencast"));
        assert_eq!(
            again.remembered(),
            [("org.example.Recorder".to_owned(), "screencast".to_owned())]
        );
        assert!(again.forget("org.example.Recorder", "screencast"));
        assert!(!Consents::new(Questions::new(), Some(path)).allowed(&caller(), "screencast"));
    }

    #[tokio::test]
    async fn an_app_repeating_a_waiting_question_gets_no_second_sheet() {
        let questions = Questions::new();
        let consents = Consents::new(questions.clone(), None);
        let caller = caller();
        let first = consents.decide(&caller, "a", screencast());
        let second = consents.decide(&caller, "b", screencast());
        let answering = async {
            tokio::task::yield_now().await;
            assert_eq!(questions.pending().len(), 1, "one sheet");
            assert!(questions.answer("a", Answer::default()));
        };
        let (first, second, ()) = tokio::join!(first, second, answering);
        assert!(first.is_ok());
        assert_eq!(second, Err(Ended::Cancelled));
    }

    #[test]
    fn remember_is_ignored_where_it_is_not_offered() {
        let consents = Consents::new(Questions::new(), None);
        consents.keep(
            &key("org.example.Recorder", "screenshot"),
            &Answer {
                remember: true,
                ..Answer::default()
            },
            false,
        );
        assert!(!consents.allowed(&caller(), "screenshot"));
    }
}
