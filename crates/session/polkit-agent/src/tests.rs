use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

use super::*;
use tilo_lock::MutexRecover as _;

/// A helper that accepts one password per user and records every start.
#[derive(Clone, Default)]
struct FakeHelper {
    passwords: Vec<(String, String)>,
    started: Arc<Mutex<Vec<(String, String)>>>,
    greeting: Option<String>,
}

impl HelperSpawner for FakeHelper {
    async fn spawn(&self, user: &str, cookie: &str) -> std::io::Result<HelperIo> {
        self.started
            .lock_recover()
            .push((user.to_string(), cookie.to_string()));
        let (agent_side, helper_side) = tokio::io::duplex(1024);
        let (agent_read, agent_write) = tokio::io::split(agent_side);
        let (helper_read, mut helper_write) = tokio::io::split(helper_side);
        let expected = self
            .passwords
            .iter()
            .find(|(name, _)| name == user)
            .map(|(_, password)| password.clone());
        let greeting = self.greeting.clone();
        drop(tokio::spawn(async move {
            if let Some(greeting) = greeting {
                let _ = helper_write
                    .write_all(format!("PAM_TEXT_INFO {greeting}\n").as_bytes())
                    .await;
            }
            let _ = helper_write
                .write_all(b"PAM_PROMPT_ECHO_OFF Password: \n")
                .await;
            let mut lines = BufReader::new(helper_read).lines();
            let reply = lines.next_line().await.ok().flatten();
            let verdict: &[u8] = if reply.is_some() && reply == expected {
                b"SUCCESS\n"
            } else {
                b"PAM_ERROR_MSG Wrong\\npassword\nFAILURE\n"
            };
            let _ = helper_write.write_all(verdict).await;
        }));
        Ok(HelperIo {
            output: Box::new(BufReader::new(agent_read)),
            input: Box::new(agent_write),
        })
    }
}

/// A user who answers from a script and remembers what they were shown.
#[derive(Default)]
struct ScriptedUser {
    answers: Mutex<VecDeque<Option<PromptAnswer>>>,
    shown: Mutex<Vec<PromptView>>,
}

impl ScriptedUser {
    fn new(answers: Vec<Option<PromptAnswer>>) -> Self {
        Self {
            answers: Mutex::new(answers.into()),
            shown: Mutex::default(),
        }
    }

    fn shown(&self) -> Vec<PromptView> {
        self.shown.lock_recover().clone()
    }
}

impl Prompter for ScriptedUser {
    async fn ask(&self, view: PromptView) -> Option<PromptAnswer> {
        self.shown.lock_recover().push(view);
        self.answers.lock_recover().pop_front().flatten()
    }
}

fn request(identities: &[&str]) -> AuthRequest {
    AuthRequest {
        action_id: "org.freedesktop.udisks2.filesystem-mount-system".into(),
        message: "Authentication is required to mount the filesystem".into(),
        cookie: "cookie-1".into(),
        identities: identities.iter().map(ToString::to_string).collect(),
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "a scripted user's answer is optional; `None` is a dismissal"
)]
fn answer(identity: usize, secret: &str) -> Option<PromptAnswer> {
    Some(PromptAnswer {
        identity,
        secret: secret.into(),
    })
}

#[test]
fn helper_lines_parse_and_unescape() {
    assert_eq!(
        HelperEvent::parse("PAM_PROMPT_ECHO_OFF Password: \n"),
        Some(HelperEvent::SecretPrompt("Password: ".into()))
    );
    assert_eq!(
        HelperEvent::parse("PAM_ERROR_MSG Wrong\\npassword"),
        Some(HelperEvent::Error("Wrong\npassword".into()))
    );
    assert_eq!(HelperEvent::parse("SUCCESS\n"), Some(HelperEvent::Success));
    assert_eq!(HelperEvent::parse("FAILURE"), Some(HelperEvent::Failure));
    assert_eq!(HelperEvent::parse("garbage"), None);
}

#[test]
fn the_session_user_is_asked_first_when_allowed() {
    let identities = vec!["root".to_string(), "quinn".to_string()];
    assert_eq!(initial_identity(&identities, Some("quinn")), 1);
    assert_eq!(initial_identity(&identities, Some("guest")), 0);
}

#[tokio::test]
async fn a_right_password_authorizes_and_the_prompt_says_what_for() {
    let helper = FakeHelper {
        passwords: vec![("quinn".into(), "hunter2".into())],
        greeting: Some("Touch the key or type the password".into()),
        ..FakeHelper::default()
    };
    let user = ScriptedUser::new(vec![answer(0, "hunter2")]);
    let outcome = authenticate(&helper, &user, &request(&["quinn"]), 0).await;
    assert_eq!(
        outcome,
        Outcome::Authorized {
            identity: "quinn".into()
        }
    );
    let shown = user.shown();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].prompt, "Password: ");
    assert!(!shown[0].echo);
    assert_eq!(
        shown[0].notice.as_deref(),
        Some("Touch the key or type the password")
    );
    assert!(shown[0].message.contains("mount the filesystem"));
    assert_eq!(
        helper.started.lock_recover().as_slice(),
        &[("quinn".to_string(), "cookie-1".to_string())],
        "the helper was started for the identity with polkit's cookie"
    );
}

#[tokio::test]
async fn a_wrong_password_retries_with_the_reason_then_gives_up() {
    let helper = FakeHelper {
        passwords: vec![("quinn".into(), "hunter2".into())],
        ..FakeHelper::default()
    };
    let user = ScriptedUser::new(vec![answer(0, "a"), answer(0, "hunter2")]);
    let outcome = authenticate(&helper, &user, &request(&["quinn"]), 0).await;
    assert!(matches!(outcome, Outcome::Authorized { .. }));
    let shown = user.shown();
    assert_eq!(shown.len(), 2);
    assert_eq!(shown[1].notice.as_deref(), Some("Wrong\npassword"));

    let user = ScriptedUser::new(vec![answer(0, "a"), answer(0, "b"), answer(0, "c")]);
    let outcome = authenticate(&helper, &user, &request(&["quinn"]), 0).await;
    assert!(matches!(outcome, Outcome::Failed(_)));
    assert_eq!(user.shown().len(), MAX_ATTEMPTS);
}

#[tokio::test]
async fn dismissing_the_sheet_cancels() {
    let helper = FakeHelper::default();
    let user = ScriptedUser::new(vec![None]);
    assert_eq!(
        authenticate(&helper, &user, &request(&["quinn"]), 0).await,
        Outcome::Cancelled
    );
}

#[tokio::test]
async fn picking_another_identity_restarts_the_helper_for_it() {
    let helper = FakeHelper {
        passwords: vec![("root".into(), "toor".into())],
        ..FakeHelper::default()
    };
    // Shown for "quinn" first; the user picks "root" and types its password.
    let user = ScriptedUser::new(vec![answer(1, "toor")]);
    let outcome = authenticate(&helper, &user, &request(&["quinn", "root"]), 0).await;
    assert_eq!(
        outcome,
        Outcome::Authorized {
            identity: "root".into()
        }
    );
    let shown = user.shown();
    assert_eq!(shown.len(), 1, "the typed password went to the new helper");
    assert_eq!(shown[0].identities, vec!["quinn", "root"]);
    let started: Vec<String> = helper
        .started
        .lock_recover()
        .iter()
        .map(|(user, _)| user.clone())
        .collect();
    assert_eq!(started, vec!["quinn", "root"]);
}

#[tokio::test]
async fn nobody_to_ask_fails_without_starting_a_helper() {
    let helper = FakeHelper::default();
    let user = ScriptedUser::default();
    assert!(matches!(
        authenticate(&helper, &user, &request(&[]), 0).await,
        Outcome::Failed(_)
    ));
    assert!(helper.started.lock_recover().is_empty());
}
