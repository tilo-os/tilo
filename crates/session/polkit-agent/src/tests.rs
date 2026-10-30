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

/// The agent asking through the session's questions.
mod question_sheets {
    use std::collections::HashMap;

    use tilo_service::{Answer, Question, Questions};
    use tokio::sync::mpsc;
    use zbus::zvariant::{OwnedObjectPath, OwnedValue};
    use zeroize::Zeroizing;

    use super::*;
    use crate::questions::{AuthenticateDetails, KIND, Notice, QuestionSheets};
    use crate::service::{AGENT_PATH, SheetPrompter, Sheets};

    /// Questions that report each change on the returned receiver.
    fn watched() -> (Questions, mpsc::UnboundedReceiver<()>) {
        let questions = Questions::new();
        let (notify, changes) = mpsc::unbounded_channel();
        questions.on_change(move || {
            let _ = notify.send(());
        });
        (questions, changes)
    }

    async fn next_question(
        questions: &Questions,
        changes: &mut mpsc::UnboundedReceiver<()>,
    ) -> Question {
        loop {
            if let Some(question) = questions.pending().into_iter().next() {
                return question;
            }
            changes.recv().await.expect("the questions stay alive");
        }
    }

    fn details(question: &Question) -> AuthenticateDetails {
        serde_json::from_value(question.details.clone()).expect("authenticate details")
    }

    fn typed(choice: Option<usize>, secret: &str) -> Answer {
        Answer {
            choice,
            values: vec![Zeroizing::new(secret.to_owned())],
            ..Answer::default()
        }
    }

    fn prompter(questions: &Questions) -> SheetPrompter {
        SheetPrompter::new(Arc::new(QuestionSheets::new(questions.clone())), "cookie-1")
    }

    #[tokio::test]
    async fn a_prompt_is_a_question_and_its_answer_reaches_pam() {
        let (questions, mut changes) = watched();
        let helper = FakeHelper {
            passwords: vec![("quinn".into(), "hunter2".into())],
            ..FakeHelper::default()
        };
        let prompter = prompter(&questions);
        let request = request(&["quinn"]);
        let running = authenticate(&helper, &prompter, &request, 0);
        let answering = async {
            let question = next_question(&questions, &mut changes).await;
            assert_eq!(question.handle, "polkit:cookie-1");
            assert_eq!(question.kind, KIND);
            assert_eq!(
                question.label, request.message,
                "listed as polkit's message"
            );
            let shown = details(&question);
            assert_eq!(shown.action_id, request.action_id);
            assert_eq!(shown.identities, ["quinn"]);
            assert_eq!(shown.identity, 0);
            assert_eq!(shown.prompt, "Password: ");
            assert!(!shown.echo);
            assert_eq!(shown.notice, None);
            assert!(questions.answer(&question.handle, typed(None, "hunter2")));
        };
        let (outcome, ()) = tokio::join!(running, answering);
        assert_eq!(
            outcome,
            Outcome::Authorized {
                identity: "quinn".into()
            }
        );
        assert!(questions.pending().is_empty());
    }

    #[tokio::test]
    async fn a_wrong_password_asks_again_with_pams_words() {
        let (questions, mut changes) = watched();
        let helper = FakeHelper {
            passwords: vec![("quinn".into(), "hunter2".into())],
            ..FakeHelper::default()
        };
        let prompter = prompter(&questions);
        let request = request(&["quinn"]);
        let running = authenticate(&helper, &prompter, &request, 0);
        let answering = async {
            let first = next_question(&questions, &mut changes).await;
            assert!(questions.answer(&first.handle, typed(None, "wrong")));
            let second = next_question(&questions, &mut changes).await;
            assert_eq!(
                details(&second).notice,
                Some(Notice::Verbatim("Wrong\npassword".into()))
            );
            assert!(questions.answer(&second.handle, typed(None, "hunter2")));
        };
        let (outcome, ()) = tokio::join!(running, answering);
        assert!(matches!(outcome, Outcome::Authorized { .. }), "{outcome:?}");
    }

    #[tokio::test]
    async fn the_choice_picks_who_authenticates() {
        let (questions, mut changes) = watched();
        let helper = FakeHelper {
            passwords: vec![("root".into(), "toor".into())],
            ..FakeHelper::default()
        };
        let prompter = prompter(&questions);
        let request = request(&["quinn", "root"]);
        let running = authenticate(&helper, &prompter, &request, 0);
        let answering = async {
            let question = next_question(&questions, &mut changes).await;
            assert_eq!(details(&question).identities, ["quinn", "root"]);
            assert!(questions.answer(&question.handle, typed(Some(1), "toor")));
        };
        let (outcome, ()) = tokio::join!(running, answering);
        assert_eq!(
            outcome,
            Outcome::Authorized {
                identity: "root".into()
            }
        );
    }

    #[tokio::test]
    async fn declining_the_question_cancels_the_request() {
        let (questions, mut changes) = watched();
        let helper = FakeHelper::default();
        let prompter = prompter(&questions);
        let request = request(&["quinn"]);
        let running = authenticate(&helper, &prompter, &request, 0);
        let declining = async {
            let question = next_question(&questions, &mut changes).await;
            assert!(questions.decline(&question.handle));
        };
        let (outcome, ()) = tokio::join!(running, declining);
        assert_eq!(outcome, Outcome::Cancelled);
    }

    fn view() -> PromptView {
        PromptView {
            message: "Authentication is required to mount the filesystem".into(),
            action_id: "org.freedesktop.udisks2.filesystem-mount-system".into(),
            identities: vec!["quinn".into()],
            identity: 0,
            prompt: "Password: ".into(),
            echo: false,
            notice: Some(StockNotice::Rejected.message_key().into()),
        }
    }

    #[tokio::test]
    async fn cancelling_the_sheet_withdraws_its_question() {
        let questions = Questions::new();
        let sheets = QuestionSheets::new(questions.clone());
        let answer = sheets.ask("polkit:c", view());
        let pending = questions.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            details(&pending[0]).notice,
            Some(Notice::Message {
                key: "portal-auth-rejected".into()
            }),
            "the agent's own notice is a key the sheet resolves"
        );
        sheets.cancel("polkit:c");
        assert!(questions.pending().is_empty());
        assert_eq!(answer.await, None);
    }

    #[test]
    fn dropping_the_sheet_withdraws_its_question() {
        let questions = Questions::new();
        let sheets = QuestionSheets::new(questions.clone());
        let answer = sheets.ask("polkit:c", view());
        assert_eq!(questions.pending().len(), 1);
        drop(answer);
        assert!(questions.pending().is_empty());
    }

    #[test]
    fn stock_notices_are_handed_on_as_keys() {
        let sheets = QuestionSheets::new(Questions::new());
        for notice in StockNotice::ALL {
            assert_eq!(sheets.stock_notice(notice), notice.message_key());
        }
    }

    /// polkit's authority and logind, recording each registration.
    #[derive(Clone, Default)]
    struct FakeAuthority {
        registered: Arc<Mutex<Vec<(String, String)>>>,
    }

    #[zbus::interface(name = "org.freedesktop.PolicyKit1.Authority")]
    impl FakeAuthority {
        fn register_authentication_agent(
            &self,
            subject: (String, HashMap<String, OwnedValue>),
            locale: &str,
            object_path: String,
        ) {
            let _ = locale;
            self.registered
                .lock_recover()
                .push((subject.0, object_path));
        }
    }

    const SESSION_PATH: &str = "/org/freedesktop/login1/session/_31";

    struct FakeLogin {
        session: OwnedObjectPath,
    }

    #[zbus::interface(name = "org.freedesktop.login1.Manager")]
    impl FakeLogin {
        #[zbus(name = "GetSessionByPID")]
        fn get_session_by_pid(&self, pid: u32) -> OwnedObjectPath {
            let _ = pid;
            self.session.clone()
        }
    }

    struct FakeSession {
        id: String,
    }

    #[zbus::interface(name = "org.freedesktop.login1.Session")]
    impl FakeSession {
        #[zbus(property)]
        fn id(&self) -> String {
            self.id.clone()
        }
    }

    #[tokio::test]
    async fn the_agent_is_served_and_registered_on_a_given_connection() {
        let (agent_socket, polkit_socket) = tokio::net::UnixStream::pair().expect("socket pair");
        let authority = FakeAuthority::default();
        let registered = Arc::clone(&authority.registered);
        let agent_side = zbus::connection::Builder::unix_stream(agent_socket)
            .server("0123456789abcdef0123456789abcdef")
            .expect("guid")
            .p2p();
        let polkit_side = zbus::connection::Builder::unix_stream(polkit_socket)
            .p2p()
            .serve_at("/org/freedesktop/PolicyKit1/Authority", authority)
            .expect("authority")
            .serve_at(
                "/org/freedesktop/login1",
                FakeLogin {
                    session: OwnedObjectPath::try_from(SESSION_PATH).expect("path"),
                },
            )
            .expect("login")
            .serve_at(SESSION_PATH, FakeSession { id: "1".into() })
            .expect("session");
        let (agent_conn, polkit) =
            futures_util::try_join!(agent_side.build(), polkit_side.build()).expect("p2p pair");
        let questions = Questions::new();
        let _agent = crate::questions::serve_on(agent_conn, questions)
            .await
            .expect("served and registered");
        let registered = registered.lock_recover().clone();
        assert_eq!(
            registered,
            [("unix-session".to_owned(), AGENT_PATH.to_owned())]
        );
        let agent = zbus::Proxy::new(
            &polkit,
            "org.freedesktop.PolicyKit1.AuthenticationAgent",
            AGENT_PATH,
            "org.freedesktop.PolicyKit1.AuthenticationAgent",
        )
        .await
        .expect("agent proxy");
        agent
            .call::<_, _, ()>("CancelAuthentication", &("no-such-cookie",))
            .await
            .expect("the agent answers on the connection");
    }
}
