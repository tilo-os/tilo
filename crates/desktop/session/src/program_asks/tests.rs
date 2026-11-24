//! A program's question as the person meets it: raised by the session through the device's
//! dispatch, kept there as its asked record in a real [`AskedStore`], answered by that record's
//! `resolve` as the asking view calls it, and handed back to the program that waits. The device's
//! dispatch here does what the device's does with a call its provider refuses for want of the
//! person's yes: it keeps the call in its asked record, and on approve sends the same call back
//! with the person's answer.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion"
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use tilo_actions::remote::asked::{
    Asked, AskedDispatch, AskedKeeping, AskedRecords, AskedStore, KeptAsk, Offer, RecordRef,
    Resolve,
};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, Completer,
    InvocationReceipt, InvocationState, InvokeRequest, Pending, ReceiptStatus, pending,
};
use tilo_actions::{InvocationId, InvocationUpdate};

use super::{Dispatch, ProgramAsks};

/// The person the questions ask, and the device they sit at.
const ADA: [u8; 32] = [1; 32];
const DEVICE: [u8; 32] = [3; 32];
const WORKSPACE: &str = "home";
const WAIT: Duration = Duration::from_secs(10);
/// What the person types, which reaches the program and nothing else.
const SECRET: &str = "correct horse battery staple";

/// Ada at this device: who answers, and whose call the session's question is.
fn ada() -> Caller {
    Caller::authenticated(ADA, DEVICE).in_workspace(WORKSPACE)
}

/// What the device keeps of the records that wait, across restarts.
#[derive(Default)]
struct Kept(Mutex<Vec<KeptAsk>>);

impl AskedKeeping for Kept {
    fn keep(&self, ask: &KeptAsk) {
        let mut kept = self.0.lock().unwrap();
        kept.retain(|kept| kept.at != ask.at);
        kept.push(ask.clone());
    }

    fn forget(&self, at: &RecordRef) {
        self.0.lock().unwrap().retain(|kept| kept.at != *at);
    }
}

/// The device's dispatch of the session's own calls: each sent to the session's provider, kept
/// in its asked record when the provider asks for the person's yes, and followed to its end.
struct Device {
    me: Weak<Self>,
    asks: ProgramAsks,
    store: Arc<AskedStore>,
    kept: Arc<Kept>,
    records: Mutex<Option<Arc<dyn AskedRecords>>>,
    states: Mutex<HashMap<InvocationId, InvocationUpdate>>,
    changes: Mutex<Vec<Completer<()>>>,
}

impl Device {
    fn start(asks: &ProgramAsks) -> Arc<Self> {
        let kept = Arc::new(Kept::default());
        let store = AskedStore::opened(
            None,
            Some(Arc::clone(&kept) as Arc<dyn AskedKeeping>),
            Vec::new(),
        )
        .expect("the store keeps what waits");
        store.asks_on(&DEVICE);
        let device = Arc::new_cyclic(|me| Self {
            me: me.clone(),
            asks: asks.clone(),
            store,
            kept,
            records: Mutex::default(),
            states: Mutex::default(),
            changes: Mutex::default(),
        });
        let dispatch: Weak<dyn AskedDispatch> = Arc::downgrade(&device) as Weak<dyn AskedDispatch>;
        *device.records.lock().unwrap() = Some(device.store.for_dispatch("device", dispatch));
        asks.reach(Dispatch {
            actions: Arc::clone(&device) as Arc<dyn ActionProvider + Send + Sync>,
            caller: Caller::local_system(),
        });
        device
    }

    /// The call `invocation` stands as `update` now.
    fn stands(&self, invocation: InvocationId, update: InvocationUpdate) {
        self.states.lock().unwrap().insert(invocation, update);
        for change in std::mem::take(&mut *self.changes.lock().unwrap()) {
            change.complete(());
        }
    }

    /// What waits for Ada now, once something does.
    async fn asked(&self) -> Asked {
        tokio::time::timeout(WAIT, async {
            loop {
                let changed = self.store.changed();
                if let Some(asked) = self.waiting().into_iter().next() {
                    return asked;
                }
                changed.await;
            }
        })
        .await
        .expect("the question is asked within the wait")
    }

    fn waiting(&self) -> Vec<Asked> {
        self.records
            .lock()
            .unwrap()
            .as_ref()
            .expect("the records are written")
            .asked_of(&ADA)
    }

    /// Ada answers `asked` as the asking view does: its record's own `resolve`.
    async fn resolve(&self, asked: &Asked, answer: Resolve) -> Result<(), ActionCallError> {
        self.store.resolve(&asked.record, &ada(), &answer).await
    }

    /// Everything the device keeps or shows of the records, as text: where a secret must never be.
    fn everything_kept(&self) -> String {
        let kept: Vec<serde_json::Value> = self
            .kept
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|ask| serde_json::to_value(ask).unwrap())
            .collect();
        let rows: Vec<serde_json::Value> = self
            .store
            .waiting_in(WORKSPACE)
            .iter()
            .map(|row| serde_json::to_value(&row.asked).unwrap())
            .collect();
        format!("{kept:?} {rows:?}")
    }
}

impl ActionProvider for Device {
    fn query(
        &self,
        _: &Caller,
        _: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::default()))
    }

    fn invoke(
        &self,
        _: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        let records = self.records.lock().unwrap().clone().expect("records");
        let (done, answer) = pending();
        let device = self.states_handle();
        self.asks.invoke(&ada(), request.clone()).then(move |sent| {
            let receipt = match sent {
                Some(Err(ActionCallError::ConfirmationRequired(facts))) => {
                    match records.write_asked(WORKSPACE, &ada(), &request, &facts) {
                        Ok(_) => {
                            device(invocation, InvocationUpdate::Accepted { invocation });
                            Ok(InvocationReceipt {
                                invocation,
                                status: ReceiptStatus::Accepted,
                            })
                        }
                        Err(reason) => Err(ActionCallError::Refused { reason }),
                    }
                }
                Some(other) => other,
                None => Err(ActionCallError::Refused {
                    reason: "the provider went away".to_owned(),
                }),
            };
            done.complete(receipt);
        });
        answer
    }

    fn observe(&self, _: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.states
            .lock()
            .unwrap()
            .get(id)
            .map(|update| InvocationState::new(*id, update.clone()))
    }

    fn next_change(&self, _: &Caller, _: &InvocationId) -> Option<Pending<()>> {
        let (done, change) = pending();
        self.changes.lock().unwrap().push(done);
        Some(change)
    }

    /// The session takes its question back: its record is withdrawn, and the call ends.
    fn cancel(&self, _: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        if let Some(at) = self.store.record(&id.0.to_string()) {
            self.store
                .withdraw(&at, "Whoever asked took the question back");
        }
        self.stands(
            *id,
            InvocationUpdate::Cancelled {
                invocation: *id,
                message: None,
            },
        );
        Pending::ready(Ok(()))
    }
}

impl Device {
    /// Where the outcome of a call is told, from a callback that outlives the borrow.
    fn states_handle(&self) -> impl Fn(InvocationId, InvocationUpdate) + Send + 'static {
        let device = self.me.clone();
        move |invocation, update| {
            if let Some(device) = device.upgrade() {
                device.stands(invocation, update);
            }
        }
    }
}

impl AskedDispatch for Device {
    fn context(&self, _: InvocationId) -> serde_json::Value {
        serde_json::Value::Null
    }

    /// The person approved: the same call goes back to the session, carrying their answer.
    fn readmit(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        _: &serde_json::Value,
    ) -> Result<(), ActionCallError> {
        let invocation = request.invocation;
        let device = self.states_handle();
        self.asks.invoke(caller, request).then(move |ran| {
            let update = match ran {
                Some(Ok(_)) => InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                Some(Err(error)) => InvocationUpdate::Failed {
                    invocation,
                    error: error.to_string(),
                },
                None => InvocationUpdate::Cancelled {
                    invocation,
                    message: None,
                },
            };
            device(invocation, update);
        });
        Ok(())
    }

    fn declined(&self, _: &Caller, invocation: InvocationId) -> Result<(), ActionCallError> {
        self.stands(
            invocation,
            InvocationUpdate::Cancelled {
                invocation,
                message: None,
            },
        );
        Ok(())
    }
}

/// The session's side of the programs' questions, and the device it raises them through.
fn session() -> (tilo_service::AskSender, Arc<Device>) {
    let (sender, receiver) = tilo_service::asks::channel();
    let asks = ProgramAsks::over(receiver, tokio::runtime::Handle::current(), || {});
    (sender, Device::start(&asks))
}

fn approve(typed: &[&str]) -> Resolve {
    Resolve {
        resolution: Offer::Approve,
        typed: typed.iter().map(|typed| (*typed).to_owned()).collect(),
        ..Resolve::default()
    }
}

fn polkit_prompt() -> tilo_polkit_agent::PromptView {
    tilo_polkit_agent::PromptView {
        message: "Authentication is required to install software".into(),
        action_id: "org.freedesktop.packagekit.package-install".into(),
        identities: vec!["ada".into()],
        identity: 0,
        prompt: "Password:".into(),
        echo: false,
        notice: None,
    }
}

/// polkit asks for a password: the person is asked through an asked record that takes it in
/// secret, and what they type reaches polkit's agent alone, never the record or what the device
/// keeps of it.
#[tokio::test(flavor = "multi_thread")]
async fn a_polkit_password_reaches_the_agent_and_nowhere_else() {
    use tilo_polkit_agent::service::Sheets as _;
    let (sender, device) = session();
    let sheets = tilo_polkit_agent::questions::QuestionSheets::new(sender);
    let answer = sheets.ask("polkit-1", polkit_prompt());
    let asked = device.asked().await;
    assert_eq!(asked.facts.takes.len(), 1, "{asked:?}");
    assert!(asked.facts.takes[0].secret, "the password is a secret");
    assert_eq!(
        asked.record.device.as_deref(),
        Some(hex::encode(DEVICE).as_str()),
        "a secret is answered only where it was asked"
    );
    device.resolve(&asked, approve(&[SECRET])).await.unwrap();
    let answered = tokio::time::timeout(WAIT, answer)
        .await
        .expect("polkit is answered")
        .expect("the person allowed it");
    assert_eq!(answered.secret, SECRET);
    assert_eq!(answered.identity, 0);
    assert!(
        !device.everything_kept().contains(SECRET),
        "the password is in no record: {}",
        device.everything_kept()
    );
    assert!(device.waiting().is_empty(), "nothing waits once answered");
}

/// A network asks for its Wi-Fi password: typed into the record's secret take, it reaches
/// `NetworkManager`'s agent, and no record keeps it.
#[tokio::test(flavor = "multi_thread")]
async fn a_wifi_password_reaches_the_network_and_nowhere_else() {
    use tilo_network_secret_agent::questions::{
        FieldDetails, FieldRule, KIND, SecretsDetails, Text,
    };
    let (sender, device) = session();
    let details = SecretsDetails {
        network: Some("Cafe".into()),
        title: Text::Verbatim("Join Cafe".into()),
        detail: Vec::new(),
        fields: vec![FieldDetails {
            key: "psk".into(),
            label: Text::Verbatim("Password".into()),
            masked: true,
            required: true,
            initial: String::new(),
            rule: FieldRule::WpaPassphrase,
        }],
        remember: None,
        sim: None,
        refusal: None,
    };
    let answer = sender.ask(tilo_service::Question {
        handle: "wifi-1".into(),
        kind: KIND.into(),
        app_id: None,
        label: "Cafe".into(),
        details: serde_json::to_value(details).unwrap(),
    });
    let asked = device.asked().await;
    assert_eq!(asked.facts.on.as_deref(), Some("Cafe"));
    assert!(asked.facts.takes.iter().all(|take| take.secret));
    device.resolve(&asked, approve(&[SECRET])).await.unwrap();
    let answered = tokio::time::timeout(WAIT, answer)
        .await
        .expect("the network is answered")
        .expect("the person answered");
    assert_eq!(
        answered
            .values
            .iter()
            .map(|value| value.as_str())
            .collect::<Vec<_>>(),
        [SECRET]
    );
    assert!(!device.everything_kept().contains(SECRET));
}

/// Declining refuses: polkit is told no, and nothing waits.
#[tokio::test(flavor = "multi_thread")]
async fn declining_refuses() {
    use tilo_polkit_agent::service::Sheets as _;
    let (sender, device) = session();
    let sheets = tilo_polkit_agent::questions::QuestionSheets::new(sender);
    let answer = sheets.ask("polkit-2", polkit_prompt());
    let asked = device.asked().await;
    device
        .resolve(
            &asked,
            Resolve {
                resolution: Offer::Decline,
                ..Resolve::default()
            },
        )
        .await
        .unwrap();
    let answered = tokio::time::timeout(WAIT, answer)
        .await
        .expect("polkit is told");
    assert!(answered.is_none(), "a declined question is no answer");
    assert!(device.waiting().is_empty());
}

/// An app asks through the portal to record the screen: the person is asked with the
/// durations they may allow it for, and the one they pick is what the portal keeps.
#[tokio::test(flavor = "multi_thread")]
async fn a_portal_request_asks_and_is_answered() {
    use collab_types::caps::DeviceCap;
    use tilo_portal::consent::{AccessQuestion, Consents};
    let (sender, device) = session();
    let consents = Consents::new(sender, tilo_peer_state::DeviceGrants::unkept());
    let caller = tilo_portal::PortalCaller::new("org.example.Recorder");
    let question = AccessQuestion {
        capability: "screencast".into(),
        reason: None,
        may_remember: true,
        text: None,
    };
    let deciding = consents.decide(&caller, "portal-1", DeviceCap::Screen, question);
    let answering = async {
        let asked = device.asked().await;
        assert!(asked.facts.choices.len() >= 2, "{:?}", asked.facts.choices);
        // The second offered duration: for as long as this session lasts.
        device
            .resolve(
                &asked,
                Resolve {
                    resolution: Offer::Approve,
                    choice: Some(1),
                    ..Resolve::default()
                },
            )
            .await
            .unwrap();
    };
    let (decided, ()) = tokio::time::timeout(WAIT, async { tokio::join!(deciding, answering) })
        .await
        .expect("the portal is answered");
    assert!(decided.is_ok(), "the person allowed it");
    assert!(
        consents.allowed(&caller, DeviceCap::Screen),
        "allowed for the session, it is not asked again"
    );
}

/// The program that asked goes away (polkit cancels, the process exits): its question is taken
/// back, and its record stops waiting.
#[tokio::test(flavor = "multi_thread")]
async fn the_asker_going_away_withdraws_its_record() {
    let (sender, device) = session();
    let answer = sender.ask(tilo_service::Question {
        handle: "polkit-3".into(),
        kind: tilo_polkit_agent::questions::KIND.into(),
        app_id: None,
        label: "Install".into(),
        details: serde_json::to_value(tilo_polkit_agent::questions::AuthenticateDetails::from(
            polkit_prompt(),
        ))
        .unwrap(),
    });
    let asked = device.asked().await;
    drop(answer);
    tokio::time::timeout(WAIT, async {
        loop {
            let changed = device.store.changed();
            if device.waiting().is_empty() {
                return;
            }
            changed.await;
        }
    })
    .await
    .expect("the record stops waiting once its asker is gone");
    let refused = device.resolve(&asked, approve(&[SECRET])).await;
    assert!(refused.is_err(), "a withdrawn question takes no answer");
}

/// Verifying a receiver of another person shows the file's name and the receiver's endpoint
/// exactly as it showed them, on lines that fit the card: a name or an address with no space to
/// wrap at must not run past it.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn a_long_name_and_an_ipv6_receiver_fit_the_verification_card() {
    use tilo_workspace_service::apps::nearby_send::{VERIFY_QUESTION, VerifyQuestion};
    let (sender, device) = session();
    let name = format!("{}.txt", "contract".repeat(30));
    let endpoint = format!("AirDrop · [2001:db8:{}]:443", "ab".repeat(24));
    let verifying = VerifyQuestion {
        name: name.clone(),
        bytes: 40_000,
        endpoint: endpoint.clone(),
        certificate: "cd".repeat(32),
    };
    let answer = sender.ask(tilo_service::Question {
        handle: "airdrop:1".into(),
        kind: VERIFY_QUESTION.into(),
        app_id: None,
        label: endpoint.clone(),
        details: serde_json::to_value(verifying).unwrap(),
    });
    let asked = device.asked().await;
    let detail = asked
        .facts
        .facts
        .iter()
        .map(|fact| fact.value.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        detail.replace('\n', "").contains(&name),
        "the name is whole: {detail}"
    );
    assert!(
        detail.replace('\n', "").contains(&endpoint),
        "the endpoint is whole: {detail}"
    );
    let longest = detail
        .split_whitespace()
        .map(|word| word.chars().count())
        .max()
        .unwrap_or(0);
    assert!(
        longest <= 32,
        "no unbroken run outruns the card, and {longest} does: {detail}"
    );
    let fingerprint = asked.facts.takes.first().expect("a fingerprint to type");
    assert!(
        !fingerprint.secret,
        "the fingerprint is read as it is typed"
    );
    drop(answer);
}
