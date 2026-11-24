//! Whether an app may use a device of this machine the person decides on:
//! capture the screen, take a screenshot without asking, use the camera.
//!
//! The person answers with one of the durations every prompt offers
//! ([`Lifetime::offered`]). An answer that outlasts the session is kept in the
//! person's own workspace, naming this device ([`DeviceGrants`]): it follows them
//! to their other devices, where they can take it back, and answers on this
//! device alone. A grant some member of a shared workspace recorded for an app
//! never answers for them.
//! An app that asks the same thing again while its first question waits
//! gets no second sheet; the repeat is declined.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use collab_types::caps::DeviceCap;
use serde::{Deserialize, Serialize};
use tilo_action_policy::Lifetime;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::{DeviceGrants, Kept};
use tilo_service::{Answer, AskSender, Question};

use crate::PortalCaller;
use crate::reply::Ended;

/// The kind of the question an access decision asks.
pub const ACCESS_QUESTION: &str = "portal.access";

/// How long `answer` allows it: the offered duration it chose, until taken back when the person
/// ticked Remember, and this once when it chose none.
fn lifetime_of(answer: &Answer) -> Lifetime {
    if answer.remember {
        return Lifetime::UntilRevoked;
    }
    answer
        .choice
        .and_then(|choice| Lifetime::chosen(choice, tilo_clock::unix_now_ms()))
        .unwrap_or(Lifetime::Once)
}

/// What the sheet shows for an access question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessQuestion {
    /// What the app asks to use, by its stable name (`screenshot`,
    /// `screencast`, `camera`, `remote-desktop`): the sheet words it.
    pub capability: String,
    /// What the app says it wants it for, as it said it, when it said.
    pub reason: Option<String>,
    /// Whether the person may allow it beyond this session.
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

/// The field of an access question's details that names the device it is about, by
/// [`DeviceCap::name`]: what the answer it leaves is kept and taken back under.
pub const DEVICE_DETAIL: &str = "device";

/// What the sheet is handed for `question` about `device`.
fn details(question: &AccessQuestion, device: DeviceCap) -> serde_json::Value {
    let mut details = serde_json::to_value(question).unwrap_or_default();
    if let serde_json::Value::Object(fields) = &mut details {
        fields.insert(DEVICE_DETAIL.into(), device.name().into());
    }
    details
}

/// The person's device answers and the questions that make them.
#[derive(Clone)]
pub struct Consents {
    questions: AskSender,
    grants: DeviceGrants,
    asking: Arc<Mutex<HashSet<(String, DeviceCap)>>>,
}

impl Consents {
    /// Answers kept in `grants`, asked through `questions`.
    #[must_use]
    pub fn new(questions: AskSender, grants: DeviceGrants) -> Self {
        Self {
            questions,
            grants,
            asking: Arc::default(),
        }
    }

    /// Where the answers are kept: hand it the person's store once it is open.
    #[must_use]
    pub const fn grants(&self) -> &DeviceGrants {
        &self.grants
    }

    /// The questions it asks through.
    #[must_use]
    pub const fn questions(&self) -> &AskSender {
        &self.questions
    }

    /// What `caller` may do with `device`: allowed or refused without asking, or asked.
    #[must_use]
    pub fn answer(&self, caller: &PortalCaller, device: DeviceCap) -> tilo_action_policy::Answer {
        self.grants
            .decide(&caller.app_id, caller.workspace.as_deref(), device)
    }

    /// Whether `caller` may use `device` without asking.
    #[must_use]
    pub fn allowed(&self, caller: &PortalCaller, device: DeviceCap) -> bool {
        self.answer(caller, device) == tilo_action_policy::Answer::Allow
    }

    /// Whether `caller` is refused `device` without asking: its workspace refuses it.
    #[must_use]
    pub fn refused(&self, caller: &PortalCaller, device: DeviceCap) -> bool {
        self.answer(caller, device) == tilo_action_policy::Answer::Deny
    }

    /// The answers the person kept that still stand, on any of their devices.
    #[must_use]
    pub fn kept(&self) -> Vec<Kept> {
        self.grants.kept()
    }

    /// Take back what `app_id` was answered of `device` on the device whose key
    /// is `on` (in hex); `false` when it was answered nothing.
    pub fn forget(&self, on: &str, app_id: &str, device: DeviceCap) -> bool {
        self.grants.revoke(on, app_id, device)
    }

    /// Decide whether `caller` may use `device`, which `question` asks
    /// about: allowed already, or asked as question `handle`, which ends with
    /// the person's answer. Keeps what the answer allows beyond this request.
    ///
    /// # Errors
    /// [`Ended::Cancelled`] when the person declined, the app's workspace
    /// refuses it the device, or the same question of the same app is
    /// already waiting.
    pub async fn decide(
        &self,
        caller: &PortalCaller,
        handle: &str,
        device: DeviceCap,
        question: AccessQuestion,
    ) -> Result<(), Ended> {
        match self.answer(caller, device) {
            tilo_action_policy::Answer::Allow => return Ok(()),
            tilo_action_policy::Answer::Deny => return Err(Ended::Cancelled),
            tilo_action_policy::Answer::Ask => {}
        }
        let key = (caller.app_id.clone(), device);
        let _asking = Asking::claim(&self.asking, key).ok_or(Ended::Cancelled)?;
        let may_remember = question.may_remember;
        let asked = self.questions.ask(Question {
            handle: handle.to_owned(),
            kind: ACCESS_QUESTION.into(),
            app_id: Some(caller.app_id.clone()),
            label: caller.app_id.clone(),
            details: details(&question, device),
        });
        let answer = asked.await.ok_or(Ended::Cancelled)?;
        self.keep(&caller.app_id, device, &answer, may_remember);
        Ok(())
    }

    fn keep(&self, app_id: &str, device: DeviceCap, answer: &Answer, may_remember: bool) {
        let lifetime = match lifetime_of(answer) {
            Lifetime::Until(_) | Lifetime::UntilRevoked if !may_remember => Lifetime::Once,
            lifetime => lifetime,
        };
        self.grants
            .answer(app_id, device, tilo_action_policy::Answer::Allow, lifetime);
    }
}

/// One question's place among those waiting, until it is answered or its
/// request goes away.
struct Asking {
    asking: Arc<Mutex<HashSet<(String, DeviceCap)>>>,
    key: (String, DeviceCap),
}

impl Asking {
    fn claim(
        asking: &Arc<Mutex<HashSet<(String, DeviceCap)>>>,
        key: (String, DeviceCap),
    ) -> Option<Self> {
        asking.lock_recover().insert(key.clone()).then(|| Self {
            asking: Arc::clone(asking),
            key,
        })
    }
}

impl Drop for Asking {
    fn drop(&mut self) {
        self.asking.lock_recover().remove(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller() -> PortalCaller {
        PortalCaller::new("org.example.Recorder")
    }

    /// Consents that ask on a fresh channel, and the person's side of it.
    fn consents() -> (Consents, tilo_service::AskReceiver) {
        let (asks, questions) = tilo_service::asks::channel();
        (Consents::new(asks, DeviceGrants::unkept()), questions)
    }

    fn screencast() -> AccessQuestion {
        AccessQuestion {
            capability: "screencast".into(),
            reason: None,
            may_remember: true,
            text: None,
        }
    }

    async fn answered(
        (consents, questions): &(Consents, tilo_service::AskReceiver),
        handle: &str,
        answer: Option<Answer>,
    ) -> bool {
        let caller = caller();
        let deciding = consents.decide(&caller, handle, DeviceCap::Screen, screencast());
        let answering = async {
            tokio::task::yield_now().await;
            match answer {
                Some(answer) => assert!(questions.answer(handle, answer)),
                None => assert!(questions.decline(handle)),
            }
        };
        let (decided, ()) = tokio::join!(deciding, answering);
        decided.is_ok()
    }

    #[tokio::test]
    async fn once_asks_again_session_does_not_and_neither_is_kept() {
        let asking = consents();
        let consents = &asking.0;
        let once = Answer {
            choice: Some(0),
            ..Answer::default()
        };
        assert!(answered(&asking, "a", Some(once)).await);
        assert!(!consents.allowed(&caller(), DeviceCap::Screen));

        let session = Answer {
            choice: Some(1),
            ..Answer::default()
        };
        assert!(answered(&asking, "b", Some(session)).await);
        assert!(consents.allowed(&caller(), DeviceCap::Screen));
        assert!(consents.kept().is_empty(), "a session answer is not kept");
        assert!(consents.forget("", "org.example.Recorder", DeviceCap::Screen));
        assert!(!consents.allowed(&caller(), DeviceCap::Screen));
    }

    #[tokio::test]
    async fn a_declined_question_denies() {
        let asking = consents();
        let consents = &asking.0;
        assert!(!answered(&asking, "a", None).await);
        assert!(!consents.allowed(&caller(), DeviceCap::Screen));
    }

    #[tokio::test]
    async fn an_app_repeating_a_waiting_question_gets_no_second_sheet() {
        let (consents, questions) = consents();
        let caller = caller();
        let first = consents.decide(&caller, "a", DeviceCap::Screen, screencast());
        let second = consents.decide(&caller, "b", DeviceCap::Screen, screencast());
        let answering = async {
            tokio::task::yield_now().await;
            assert_eq!(questions.pending().len(), 1, "one sheet");
            assert!(questions.answer("a", Answer::default()));
        };
        let (first, second, ()) = tokio::join!(first, second, answering);
        assert!(first.is_ok());
        assert_eq!(second, Err(Ended::Cancelled));
    }

    #[tokio::test]
    async fn an_app_in_a_shared_workspace_is_asked_on_this_device() {
        let (consents, questions) = consents();
        let in_studio = PortalCaller {
            workspace: Some("studio".into()),
            ..caller()
        };
        assert!(!consents.allowed(&in_studio, DeviceCap::Screen));
        let deciding = consents.decide(&in_studio, "a", DeviceCap::Screen, screencast());
        let answering = async {
            tokio::task::yield_now().await;
            assert_eq!(questions.pending().len(), 1, "the person is asked");
            assert!(questions.answer("a", Answer::default()));
        };
        let (decided, ()) = tokio::join!(deciding, answering);
        assert_eq!(decided, Ok(()));
    }

    /// A workspace that refuses its apps the screen or the input refuses them without asking,
    /// whatever the person answered; the same app elsewhere is answered as the person said.
    #[tokio::test]
    async fn a_workspace_refusal_refuses_without_asking() {
        let (consents, questions) = consents();
        consents.grants().answer(
            "org.example.Recorder",
            DeviceCap::Screen,
            tilo_action_policy::Answer::Allow,
            Lifetime::Session,
        );
        consents.grants().refuse_in(
            [
                (
                    "studio".to_owned(),
                    "org.example.Recorder".to_owned(),
                    DeviceCap::Screen,
                ),
                (
                    "studio".to_owned(),
                    "org.example.Recorder".to_owned(),
                    DeviceCap::Input,
                ),
            ]
            .into(),
        );
        let in_studio = PortalCaller {
            workspace: Some("studio".into()),
            ..caller()
        };
        for device in [DeviceCap::Screen, DeviceCap::Input] {
            assert_eq!(
                consents.decide(&in_studio, "a", device, screencast()).await,
                Err(Ended::Cancelled)
            );
            assert!(consents.refused(&in_studio, device));
        }
        assert!(questions.pending().is_empty(), "nobody is asked");
        assert!(consents.allowed(&caller(), DeviceCap::Screen));
    }

    #[test]
    fn remember_is_ignored_where_it_is_not_offered() {
        let (consents, _questions) = consents();
        consents.keep(
            "org.example.Recorder",
            DeviceCap::Screen,
            &Answer {
                remember: true,
                ..Answer::default()
            },
            false,
        );
        assert!(!consents.allowed(&caller(), DeviceCap::Screen));
    }
}
