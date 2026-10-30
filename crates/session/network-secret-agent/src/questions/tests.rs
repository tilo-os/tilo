use std::collections::VecDeque;

use tokio::sync::mpsc;
use zbus::zvariant::ObjectPath;

use super::*;
use crate::store::SecretStore;
use crate::testing::{
    Link, MemoryStore, WIFI_PATH, gsm_connection, link, request_for, string_of, vpn_connection,
    wifi_psk_connection,
};
use crate::{FLAG_ALLOW_INTERACTION, FLAG_REQUEST_NEW};

const SIM_PATH: &str = "/org/freedesktop/ModemManager1/SIM/0";
const HOME_UUID: &str = "5f2c7c9e-0000-4000-8000-000000000001";
const CARRIER_UUID: &str = "5f2c7c9e-0000-4000-8000-000000000004";

/// The SIM as a test sets it, the PUK answers it gives, and what it was
/// handed.
#[derive(Default)]
struct FixedModem {
    sim: Mutex<Option<SimUnlock>>,
    puk_answers: Mutex<VecDeque<Result<(), PukRefusal>>>,
    sent: Mutex<Vec<(String, String, String)>>,
    joined: Mutex<Vec<String>>,
}

impl Modem for FixedModem {
    fn read_sim(&self) -> ModemFuture<Option<SimUnlock>> {
        let sim = self.sim.lock_recover().clone();
        Box::pin(async move { sim })
    }

    fn send_puk(
        &self,
        sim_path: &str,
        puk: Zeroizing<String>,
        new_pin: Zeroizing<String>,
    ) -> ModemFuture<Result<(), PukRefusal>> {
        self.sent.lock_recover().push((
            sim_path.to_owned(),
            puk.as_str().to_owned(),
            new_pin.as_str().to_owned(),
        ));
        let answer = self
            .puk_answers
            .lock_recover()
            .pop_front()
            .unwrap_or(Ok(()));
        Box::pin(async move { answer })
    }

    fn join(&self, connection_path: &str) {
        self.joined.lock_recover().push(connection_path.to_owned());
    }
}

/// An agent asking in fresh questions, linked to a stand-in
/// `NetworkManager`.
struct Setup {
    questions: Questions,
    changes: mpsc::UnboundedReceiver<()>,
    modem: Arc<FixedModem>,
    store: Arc<MemoryStore>,
    link: Link,
}

async fn setup(sim: Option<SimUnlock>) -> Setup {
    let questions = Questions::new();
    let (notify, changes) = mpsc::unbounded_channel();
    questions.on_change(move || {
        let _ = notify.send(());
    });
    let modem = Arc::new(FixedModem::default());
    *modem.sim.lock_recover() = sim;
    let store = Arc::new(MemoryStore::default());
    let sheets =
        QuestionSheets::with_modem(questions.clone(), Arc::clone(&modem) as Arc<dyn Modem>);
    let link = link(Arc::new(sheets), Arc::clone(&store) as Arc<dyn SecretStore>).await;
    Setup {
        questions,
        changes,
        modem,
        store,
        link,
    }
}

impl Setup {
    /// The next question pending, once one is.
    async fn question(&mut self) -> Question {
        let deadline = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
        loop {
            if let Some(question) = self.questions.pending().into_iter().next() {
                return question;
            }
            tokio::time::timeout_at(deadline, self.changes.recv())
                .await
                .expect("a question in time")
                .expect("the questions stay alive");
        }
    }

    /// Answer the question pending now with `values`, after it is asked.
    async fn answer(&mut self, values: &[&str], remember: bool) -> SecretsDetails {
        let question = self.question().await;
        assert_eq!(question.kind, KIND);
        let details = secrets(&question);
        assert!(
            self.questions.answer(
                &question.handle,
                Answer {
                    choice: Some(0),
                    remember,
                    values: values
                        .iter()
                        .map(|value| Zeroizing::new((*value).to_owned()))
                        .collect(),
                    ..Answer::default()
                },
            )
        );
        details
    }
}

fn secrets(question: &Question) -> SecretsDetails {
    serde_json::from_value(question.details.clone()).expect("secrets details")
}

fn key(key: &str) -> Text {
    Text::key(key)
}

#[tokio::test]
async fn a_wifi_password_is_a_question_and_its_answer_goes_to_networkmanager() {
    let mut setup = setup(None).await;
    let reply = setup
        .link
        .get_secrets(
            wifi_psk_connection(),
            "802-11-wireless-security",
            vec!["psk"],
            FLAG_ALLOW_INTERACTION,
        )
        .await;
    let question = setup.question().await;
    assert!(
        question.handle.starts_with("nm-secrets:"),
        "{}",
        question.handle
    );
    assert_eq!(question.label, "Home Net", "listed under the SSID");
    let details = setup.answer(&["short horse"], true).await;
    assert_eq!(details.network.as_deref(), Some("Home Net"));
    assert_eq!(details.title, key("portal-network-wifi-title"));
    assert_eq!(
        details.detail,
        [key("portal-network-enter-password").with("network", Text::Verbatim("Home Net".into()))]
    );
    let field = details.fields.first().expect("the passphrase field");
    assert_eq!(field.key, "psk");
    assert!(field.masked && field.required);
    assert_eq!(field.rule, FieldRule::WpaPassphrase);
    assert!(details.remember.is_some());
    assert_eq!(details.sim, None);
    assert_eq!(details.refusal, None);

    let reply = reply.await.expect("call task").expect("secrets returned");
    assert_eq!(
        string_of(&reply, "802-11-wireless-security", "psk"),
        "short horse"
    );
    assert_eq!(
        setup
            .store
            .get(HOME_UUID, "802-11-wireless-security")
            .and_then(|kept| kept.get("psk").map(str::to_owned))
            .as_deref(),
        Some("short horse"),
        "remembered as asked"
    );
    assert!(setup.questions.pending().is_empty());
}

#[tokio::test]
async fn an_answer_networkmanager_would_refuse_is_asked_again_saying_why() {
    let mut setup = setup(None).await;
    let reply = setup
        .link
        .get_secrets(
            wifi_psk_connection(),
            "802-11-wireless-security",
            vec!["psk"],
            FLAG_ALLOW_INTERACTION,
        )
        .await;
    let first = setup.answer(&["short"], false).await;
    assert_eq!(first.refusal, None);
    let second = setup.answer(&[], false).await;
    assert_eq!(second.refusal, Some(Refusal::at(0, REFUSAL_WIFI_LENGTH)));
    let third = setup.answer(&["long enough"], false).await;
    assert_eq!(third.refusal, Some(Refusal::at(0, REFUSAL_REQUIRED)));
    let reply = reply.await.expect("call task").expect("secrets returned");
    assert_eq!(
        string_of(&reply, "802-11-wireless-security", "psk"),
        "long enough"
    );
    assert!(
        setup
            .store
            .get(HOME_UUID, "802-11-wireless-security")
            .is_none(),
        "not remembered unless asked"
    );
}

#[tokio::test]
async fn declining_the_question_is_a_user_cancel() {
    let mut setup = setup(None).await;
    let reply = setup
        .link
        .get_secrets(
            wifi_psk_connection(),
            "802-11-wireless-security",
            vec!["psk"],
            FLAG_ALLOW_INTERACTION,
        )
        .await;
    let question = setup.question().await;
    assert!(setup.questions.decline(&question.handle));
    let error = reply.await.expect("call task").expect_err("declined");
    assert!(
        error
            .to_string()
            .contains("org.freedesktop.NetworkManager.SecretAgent.UserCanceled"),
        "{error}"
    );
}

#[tokio::test]
async fn networkmanager_cancelling_withdraws_the_question() {
    let mut setup = setup(None).await;
    let reply = setup
        .link
        .get_secrets(
            wifi_psk_connection(),
            "802-11-wireless-security",
            vec!["psk"],
            FLAG_ALLOW_INTERACTION,
        )
        .await;
    let _question = setup.question().await;
    setup
        .link
        .agent()
        .await
        .call::<_, _, ()>(
            "CancelGetSecrets",
            &(
                ObjectPath::from_static_str_unchecked(WIFI_PATH),
                "802-11-wireless-security",
            ),
        )
        .await
        .expect("cancelled");
    let error = reply.await.expect("call task").expect_err("withdrawn");
    assert!(error.to_string().contains("AgentCanceled"), "{error}");
    assert!(setup.questions.pending().is_empty());
}

#[tokio::test]
async fn a_sim_pin_is_asked_with_the_attempts_left_and_never_remembered() {
    let mut setup = setup(Some(SimUnlock {
        step: SimUnlockStep::Pin,
        attempts_left: Some(3),
        sim_path: Some(SIM_PATH.into()),
    }))
    .await;
    let reply = setup
        .link
        .get_secrets(gsm_connection(), "gsm", vec!["pin"], FLAG_ALLOW_INTERACTION)
        .await;
    let details = setup.answer(&["1234"], true).await;
    assert_eq!(
        details.sim,
        Some(SimDetails {
            step: SimUnlockStep::Pin,
            attempts_left: Some(3),
        })
    );
    assert_eq!(details.remember, None, "a SIM PIN is never remembered");
    assert_eq!(details.title, key("portal-network-sim-pin-title"));
    let reply = reply.await.expect("call task").expect("the PIN returned");
    assert_eq!(string_of(&reply, "gsm", "pin"), "1234");
    assert!(setup.store.get(CARRIER_UUID, "gsm").is_none());
    assert!(setup.modem.joined.lock_recover().is_empty());
}

#[tokio::test]
async fn a_sim_locked_to_its_puk_is_unlocked_through_the_modem_and_joined_again() {
    let mut setup = setup(Some(SimUnlock {
        step: SimUnlockStep::Puk,
        attempts_left: Some(10),
        sim_path: Some(SIM_PATH.into()),
    }))
    .await;
    setup
        .modem
        .puk_answers
        .lock_recover()
        .extend([Err(PukRefusal::Wrong {
            attempts_left: Some(9),
        })]);
    // NetworkManager asks for the PIN again after the last wrong one.
    let reply = setup
        .link
        .get_secrets(
            gsm_connection(),
            "gsm",
            vec!["pin"],
            FLAG_ALLOW_INTERACTION | FLAG_REQUEST_NEW,
        )
        .await;
    let first = setup.answer(&["11111111", "2468"], false).await;
    assert_eq!(first.title, key("portal-sim-puk-title"));
    assert_eq!(
        first.detail,
        [key("portal-sim-puk-lead").with("network", Text::Verbatim("Carrier".into()))]
    );
    assert_eq!(
        first.sim,
        Some(SimDetails {
            step: SimUnlockStep::Puk,
            attempts_left: Some(10),
        })
    );
    let second = setup.answer(&["12345678"], false).await;
    assert_eq!(second.refusal, Some(Refusal::at(0, REFUSAL_PUK_WRONG)));
    assert_eq!(
        second.sim.and_then(|sim| sim.attempts_left),
        Some(9),
        "the attempts the modem says are left"
    );
    let third = setup.answer(&["12345678", "2468"], false).await;
    assert_eq!(third.refusal, Some(Refusal::at(1, REFUSAL_REQUIRED)));

    let error = reply
        .await
        .expect("call task")
        .expect_err("NetworkManager gets no PIN for an unlocked SIM");
    assert!(error.to_string().contains("UserCanceled"), "{error}");
    assert_eq!(
        setup.modem.sent.lock_recover().as_slice(),
        [
            (
                SIM_PATH.to_owned(),
                "11111111".to_owned(),
                "2468".to_owned()
            ),
            (
                SIM_PATH.to_owned(),
                "12345678".to_owned(),
                "2468".to_owned()
            ),
        ]
    );
    assert_eq!(
        setup.modem.joined.lock_recover().as_slice(),
        [WIFI_PATH.to_owned()],
        "the connection starts again on the unlocked SIM"
    );
}

#[tokio::test]
async fn a_network_that_could_not_be_joined_is_told_once_and_joins_as_the_person() {
    let setup = setup(None).await;
    for _ in 0..2 {
        let reply = setup
            .link
            .get_secrets(
                wifi_psk_connection(),
                "802-11-wireless-security",
                vec!["psk"],
                0,
            )
            .await;
        let error = reply
            .await
            .expect("call task")
            .expect_err("nothing to answer");
        assert!(error.to_string().contains("NoSecrets"), "{error}");
    }
    let pending = setup.questions.pending();
    assert_eq!(pending.len(), 1, "told once per network");
    let question = &pending[0];
    assert_eq!(question.kind, CANNOT_JOIN_KIND);
    assert_eq!(question.label, "Home Net");
    let details: CannotJoinDetails =
        serde_json::from_value(question.details.clone()).expect("cannot-join details");
    assert_eq!(
        details.title,
        key("network-cannot-join-title").with("network", Text::Verbatim("Home Net".into()))
    );
    assert_eq!(details.join, key("network-cannot-join-action"));
    assert!(setup.questions.answer(
        &question.handle,
        Answer {
            choice: Some(0),
            ..Answer::default()
        }
    ));
    let deadline = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
    while setup.modem.joined.lock_recover().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "joined in time");
        tokio::task::yield_now().await;
    }
    assert_eq!(
        setup.modem.joined.lock_recover().as_slice(),
        [WIFI_PATH.to_owned()]
    );
}

#[tokio::test]
async fn dropping_the_sheet_withdraws_its_question() {
    let questions = Questions::new();
    let sheets = QuestionSheets::with_modem(questions.clone(), Arc::new(FixedModem::default()));
    let request = request_for(wifi_psk_connection(), "802-11-wireless-security", &["psk"]);
    let mut answer = sheets.ask("nm-secrets:0:test", &request);
    // Polled once, the question is asked.
    assert!(futures_util::poll!(&mut answer).is_pending());
    assert_eq!(questions.pending().len(), 1);
    drop(answer);
    assert!(questions.pending().is_empty());

    let mut answer = sheets.ask("nm-secrets:1:test", &request);
    assert!(futures_util::poll!(&mut answer).is_pending());
    sheets.cancel("nm-secrets:1:test");
    assert!(questions.pending().is_empty());
    assert!(answer.await.is_none());
}

#[test]
fn answers_are_checked_against_each_fields_rule() {
    let request = request_for(wifi_psk_connection(), "802-11-wireless-security", &["psk"]);
    let value = |text: &str| vec![Zeroizing::new(text.to_owned())];
    assert_eq!(
        refusal(&request.fields, &[]),
        Some(Refusal::at(0, REFUSAL_REQUIRED))
    );
    assert_eq!(
        refusal(&request.fields, &value("1234567")),
        Some(Refusal::at(0, REFUSAL_WIFI_LENGTH))
    );
    assert_eq!(refusal(&request.fields, &value("12345678")), None);
    assert_eq!(refusal(&request.fields, &value(&"a".repeat(64))), None);
    assert_eq!(
        refusal(&request.fields, &value(&"g".repeat(64))),
        Some(Refusal::at(0, REFUSAL_WIFI_LENGTH)),
        "64 characters only as hexadecimal digits"
    );
}

#[test]
fn a_sim_pin_without_a_modem_answer_is_still_asked_as_a_sim_pin() {
    let request = request_for(gsm_connection(), "gsm", &["pin"]);
    let shown = details(&request, None, None);
    assert_eq!(shown.remember, None);
    assert_eq!(
        shown.sim,
        Some(SimDetails {
            step: SimUnlockStep::Pin,
            attempts_left: None,
        })
    );
    assert_eq!(
        shown.fields.first().map(|field| field.label.clone()),
        Some(key("portal-network-sim-pin"))
    );
}

#[test]
fn a_mobile_password_is_no_sim_pin() {
    let request = request_for(gsm_connection(), "gsm", &["password"]);
    let shown = details(&request, None, None);
    assert_eq!(shown.sim, None);
    assert!(shown.remember.is_some());
}

#[test]
fn a_vpn_question_carries_the_plugin_message_as_it_came() {
    let request = request_for(
        vpn_connection(),
        "vpn",
        &["cert-pass", "x-vpn-message:Use your one-time code"],
    );
    let shown = details(&request, None, None);
    assert_eq!(shown.title, key("portal-network-vpn-title"));
    assert!(
        shown
            .detail
            .contains(&Text::Verbatim("Use your one-time code".into())),
        "{:?}",
        shown.detail
    );
    assert_eq!(shown.remember, Some(false));
}

#[test]
fn an_unnamed_sim_its_puk_locks_is_this_network() {
    let mut connection = gsm_connection();
    connection
        .get_mut("connection")
        .expect("connection")
        .remove("id");
    let request = request_for(connection, "gsm", &["pin"]);
    let sim = SimUnlock::of(
        os_actions::SimLock::Puk,
        os_actions::UnlockRetries {
            pin: Some(0),
            puk: Some(10),
        },
        Some(SIM_PATH.into()),
    );
    let shown = details(&request, Some(&sim), None);
    assert_eq!(
        shown.detail,
        [key("portal-sim-puk-lead").with("network", key("portal-network-this-network"))]
    );
    assert_eq!(
        shown.sim.and_then(|sim| sim.attempts_left),
        Some(10),
        "the PUK's attempts, not the PIN's"
    );
}

#[test]
fn text_reads_as_keys_and_arguments() {
    let text = key("portal-network-enter-password").with("network", Text::Verbatim("Home".into()));
    assert_eq!(
        serde_json::to_value(&text).expect("json"),
        serde_json::json!({
            "message": {
                "key": "portal-network-enter-password",
                "args": { "network": { "verbatim": "Home" } }
            }
        })
    );
    assert_eq!(
        serde_json::to_value(key("portal-network-retry")).expect("json"),
        serde_json::json!({ "message": { "key": "portal-network-retry" } })
    );
}

fn modem(path: &str, lock: os_actions::SimLock, sim: &str) -> os_actions::ModemInfo {
    os_actions::ModemInfo {
        path: path.into(),
        name: "Quectel EM05".into(),
        state: os_actions::ModemState::Locked,
        failure: None,
        lock,
        retries: os_actions::UnlockRetries {
            pin: Some(2),
            puk: Some(10),
        },
        pin_enabled: Some(true),
        signal: None,
        technology: None,
        operator: None,
        registration: os_actions::Registration::Idle,
        modes: None,
        supported_modes: Vec::new(),
        sims: vec![os_actions::SimCard {
            path: sim.into(),
            slot: None,
            active: true,
            operator: None,
            operator_code: None,
            iccid: None,
            kind: os_actions::SimKind::Physical,
        }],
        numbers: Vec::new(),
        traffic: None,
    }
}

#[test]
fn the_sim_asked_for_is_the_one_a_lock_holds() {
    let reading = os_actions::MobileReading {
        modems: vec![
            modem(
                "/org/freedesktop/ModemManager1/Modem/0",
                os_actions::SimLock::None,
                "/org/freedesktop/ModemManager1/SIM/0",
            ),
            modem(
                "/org/freedesktop/ModemManager1/Modem/1",
                os_actions::SimLock::Puk,
                "/org/freedesktop/ModemManager1/SIM/1",
            ),
        ],
    };
    assert_eq!(
        sim_unlock(&reading),
        Some(SimUnlock {
            step: SimUnlockStep::Puk,
            attempts_left: Some(10),
            sim_path: Some("/org/freedesktop/ModemManager1/SIM/1".into()),
        })
    );
    assert_eq!(sim_unlock(&os_actions::MobileReading::default()), None);
}
