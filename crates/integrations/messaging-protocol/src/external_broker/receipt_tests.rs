#![allow(
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    clippy::doc_markdown,
    reason = "one end-to-end journey reads as one test, and its prose names the services"
)]
use super::*;
use crate::signal_session::{Completion, SendJournal};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use ui::tilo_resource::SharedResource as _;
use whatsapp_rust::wacore::types::{
    events::Receipt, message::MessageSource, presence::ReceiptType,
};

struct NoNetwork(Arc<AtomicUsize>);
#[whatsapp_rust::async_trait]
impl whatsapp_rust::http::HttpClient for NoNetwork {
    async fn execute(
        &self,
        _: whatsapp_rust::http::HttpRequest,
    ) -> whatsapp_rust::anyhow::Result<whatsapp_rust::http::HttpResponse> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(whatsapp_rust::anyhow::anyhow!("fixture forbids network"))
    }
}
#[whatsapp_rust::async_trait]
impl whatsapp_rust::transport::TransportFactory for NoNetwork {
    async fn create_transport(
        &self,
    ) -> whatsapp_rust::anyhow::Result<(
        Arc<dyn whatsapp_rust::transport::Transport>,
        whatsapp_rust::async_channel::Receiver<whatsapp_rust::transport::TransportEvent>,
    )> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(whatsapp_rust::anyhow::anyhow!("fixture forbids network"))
    }
}
pub(super) async fn sdk(calls: &Arc<AtomicUsize>) -> Arc<whatsapp_rust::Client> {
    let backend = Arc::new(whatsapp_rust::wacore::store::in_memory::InMemoryBackend::new());
    let manager = Arc::new(
        whatsapp_rust::store::persistence_manager::PersistenceManager::new(backend)
            .await
            .unwrap(),
    );
    whatsapp_rust::Client::builder()
        .with_runtime(whatsapp_rust::TokioRuntime)
        .with_persistence_manager(manager)
        .with_http_client(NoNetwork(calls.clone()))
        .with_transport_factory(NoNetwork(calls.clone()))
        .build()
        .await
        .unwrap()
        .into_parts()
        .0
}
pub(super) fn receipt(route: &Route, kind: ReceiptType) -> Receipt {
    Receipt::builder()
        .source(MessageSource {
            chat: route.recipient.parse().unwrap(),
            sender: route.recipient.parse().unwrap(),
            ..Default::default()
        })
        .message_ids(vec!["operation".into()])
        .timestamp("2026-10-06T00:00:00Z".parse().unwrap())
        .r#type(kind)
        .offline(false)
        .build()
}
pub(super) struct Fixture {
    pub(super) root: tempfile::TempDir,
    pub(super) owner: Owner,
    pub(super) route: Route,
    pub(super) broker: ExternalBroker,
    pub(super) source: Arc<whatsapp_rust::Client>,
    pub(super) revoked: Arc<AtomicBool>,
    pub(super) calls: Arc<AtomicUsize>,
}
pub(super) async fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let owner = tests::owner(crate::Service::Whatsapp);
    let calls = Arc::new(AtomicUsize::new(0));
    let source = sdk(&calls).await;
    assemble(root, owner, calls, source)
}
pub(super) fn assemble(
    root: tempfile::TempDir,
    owner: Owner,
    calls: Arc<AtomicUsize>,
    source: Arc<whatsapp_rust::Client>,
) -> Fixture {
    let route = tests::route(crate::Service::Whatsapp);
    let mut journal = crate::journal::EncryptedJournal::open(
        &root.path().join("journal"),
        &owner,
        secret_vault::VaultKey::from_bytes([7; 32]),
    )
    .unwrap();
    let native = crate::Route::new(
        crate::Service::Whatsapp,
        owner.account.clone(),
        route.recipient.clone(),
    )
    .unwrap();
    journal
        .begin("operation", &native, b"private uncertain native submission")
        .unwrap();
    journal
        .complete("operation", Completion::Uncertain)
        .unwrap();
    let runtime =
        crate::whatsapp_runtime::WhatsappRuntime::new(owner.clone(), source.clone(), journal)
            .unwrap();
    let mut history = History::open(
        &root.path().join("history"),
        &owner,
        secret_vault::VaultKey::from_bytes([8; 32]),
    )
    .unwrap();
    history
        .reserve(&route, "operation", "private fixture message")
        .unwrap();
    let revoked = Arc::new(AtomicBool::new(false));
    let broker = ExternalBroker::new(
        Box::new(runtime),
        history,
        route.scope.clone(),
        Arc::new(tests::Hold {
            owner: owner.clone(),
            revoked: revoked.clone(),
        }),
    )
    .unwrap();
    Fixture {
        root,
        owner,
        route,
        broker,
        source,
        revoked,
        calls,
    }
}
async fn observe(fixture: &Fixture, kind: ReceiptType) -> Result<bool, SendError> {
    fixture
        .broker
        .observe_whatsapp_receipt(
            "operation".into(),
            fixture.source.clone(),
            receipt(&fixture.route, kind),
        )
        .await
}

pub(super) fn messages_with_client(
    fixture: &Fixture,
    client: tilo_messaging::MessagingClient,
) -> ui::framework::testing::TestApp<messages::MessagesShell> {
    use messages::{MessagesEffect, MessagesShell};
    use tilo_contact_directory::ContactDirectory;
    let mut app = ui::framework::testing::TestApp::<MessagesShell>::for_person_with_lent(
        &fixture.owner.actor,
        client,
    )
    .unwrap();
    tests::project_account_metadata(&fixture.owner, &fixture.route, &mut app);
    let directory = app
        .shared_workspace::<ContactDirectory>(ContactDirectory::OWNER)
        .unwrap();
    let route = &fixture.route;
    directory
        .invoke(&ContactDirectory::PROJECT_CONTACT.with((route.person.clone(), "Alex".into())))
        .unwrap();
    directory
        .invoke(&ContactDirectory::CONFIRM_ACCOUNT.with((
            route.person.clone(),
            route.association.clone(),
            route.service.clone(),
            route.scope.clone(),
            route.recipient.clone(),
            route.revision.clone(),
        )))
        .unwrap();
    app.settle();
    app.effect(&MessagesEffect::OpenPerson {
        person: route.person.as_str().into(),
    })
    .unwrap();
    let choice = serde_json::json!({"service":route.service,"account":fixture.owner.account,"recipient":route.recipient,"association":route.association.as_str(),"scope":route.scope,"revision":route.revision}).to_string();
    app.effect(&MessagesEffect::SelectRoute { choice }).unwrap();
    app
}

pub(super) async fn durability_fixture() -> (Fixture, Arc<WhatsappInboxHook>) {
    let root = tempfile::tempdir().unwrap();
    let owner = tests::owner(crate::Service::Whatsapp);
    let calls = Arc::new(AtomicUsize::new(0));
    let hook = Arc::new(WhatsappInboxHook::new(owner.clone()).unwrap());
    let backend = Arc::new(
        crate::whatsapp_store::EncryptedBackend::open_with_key(
            &root.path().join("sdk"),
            owner.clone(),
            secret_vault::VaultKey::from_bytes([23; 32]),
        )
        .unwrap(),
    );
    let manager = Arc::new(
        whatsapp_rust::store::persistence_manager::PersistenceManager::new(backend)
            .await
            .unwrap(),
    );
    let source = whatsapp_rust::Client::builder()
        .with_runtime(whatsapp_rust::TokioRuntime)
        .with_persistence_manager(manager)
        .with_http_client(NoNetwork(calls.clone()))
        .with_transport_factory(NoNetwork(calls.clone()))
        .with_inbound_durability_hook_arc(hook.clone())
        .build()
        .await
        .unwrap()
        .into_parts()
        .0;
    (assemble(root, owner, calls, source), hook)
}

pub(super) fn network(calls: &Arc<AtomicUsize>) -> crate::supervisor::whatsapp::Network {
    crate::supervisor::whatsapp::Network {
        http: Arc::new(NoNetwork(calls.clone())),
        transport: Arc::new(NoNetwork(calls.clone())),
    }
}

/// A WhatsApp account's host lifecycle and its recipient evidence: the
/// supervisor reports each state on the person's Messages view without ever
/// admitting a route, freezes its link generation, and native receipts promote
/// the exact operation monotonically, privately and durably.
#[tokio::test]
async fn whatsapp_lifecycle_and_receipt_journey_never_admits_a_route() {
    use crate::supervisor::{AccountSupervisor, whatsapp::WhatsappBootstrap};
    use tilo_messaging::{AccountState, ServiceAccount};
    let root = tempfile::tempdir().unwrap();
    let owner = tests::owner(crate::Service::Whatsapp);
    let calls = Arc::new(AtomicUsize::new(0));
    let backend = crate::whatsapp_store::EncryptedBackend::open_with_key(
        &root.path().join("sdk"),
        owner.clone(),
        secret_vault::VaultKey::from_bytes([23; 32]),
    )
    .unwrap();
    let bootstrap = WhatsappBootstrap::build(backend, network(&calls))
        .await
        .unwrap();
    let source = bootstrap.client();
    let fixture = assemble(root, owner, calls, source);
    let supervisor = AccountSupervisor::new(&fixture.owner).unwrap();
    let account = ServiceAccount::new("whatsapp", &fixture.owner.account);
    let client = supervisor.client();
    assert_eq!(client.account_state(&account), AccountState::Unavailable);
    // A lifecycle label cannot claim readiness.
    assert_eq!(
        supervisor.unavailable(AccountState::Ready),
        Err(SendError::InvalidRequest)
    );
    let mut app = messages_with_client(&fixture, client.clone());
    supervisor.unavailable(AccountState::Linking).unwrap();
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(5), |h| h
                .a11y_nodes()
                .iter()
                .any(|(_, node)| node.label()
                    == Some(
                        "Linking this account… Confirm the request on your phone."
                    )))
    );
    supervisor.unavailable(AccountState::Locked).unwrap();
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(5), |h| h
                .a11y_nodes()
                .iter()
                .any(
                    |(_, node)| node.label() == Some("Unlock this account to use messaging.")
                ))
    );
    assert!(!client.supports(&fixture.route));
    supervisor
        .install_whatsapp(Arc::new(fixture.broker.clone()), bootstrap)
        .await
        .unwrap();
    // An SDK that has not logged in/connected must never advertise readiness.
    assert_eq!(client.account_state(&account), AccountState::NeedsRelink);
    let states = app
        .effect(&messages::MessagesEffect::ListAccountStates)
        .unwrap()
        .value
        .unwrap();
    assert!(
        states
            .as_array()
            .unwrap()
            .iter()
            .any(|state| state["service"] == "whatsapp"
                && state["account"] == fixture.owner.account
                && state["state"] == "needs-relink")
    );
    assert!(!client.supports(&fixture.route));
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(5), |h| h
                .a11y_nodes()
                .iter()
                .any(|(_, node)| node.label()
                    == Some("Link this account again to resume messaging.")))
    );
    supervisor.unavailable(AccountState::Failed).unwrap();
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(5), |h| h
                .a11y_nodes()
                .iter()
                .any(|(_, node)| node.label()
                    == Some(
                        "This account's connection stopped. Check its connection and account setup."
                    )))
    );
    assert_eq!(
        client.account_state(&ServiceAccount::new("signal", &fixture.owner.account)),
        AccountState::Unavailable
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    assert!(
        !app.harness()
            .dump_json()
            .contains("private fixture message")
    );

    // A supervisor frozen at another link generation installs neither an
    // external broker nor another owner's SDK, and never becomes ready.
    let fixture = super::receipt_tests::fixture().await;
    let mut stale = fixture.owner.clone();
    stale.linking_generation = stale.linking_generation.saturating_add(1);
    let supervisor = AccountSupervisor::new(&stale).unwrap();
    assert_eq!(
        supervisor.install_external(&fixture.broker),
        Err(SendError::Unavailable)
    );
    assert_eq!(
        supervisor.unavailable(AccountState::Ready),
        Err(SendError::InvalidRequest)
    );
    let backend = crate::whatsapp_store::EncryptedBackend::open_with_key(
        &fixture.root.path().join("stale-sdk"),
        fixture.owner.clone(),
        secret_vault::VaultKey::from_bytes([24; 32]),
    )
    .unwrap();
    let bootstrap = WhatsappBootstrap::build(backend, network(&fixture.calls))
        .await
        .unwrap();
    assert_eq!(
        supervisor
            .install_whatsapp(Arc::new(fixture.broker.clone()), bootstrap)
            .await,
        Err(SendError::Revoked)
    );
    assert_eq!(
        supervisor
            .client()
            .account_state(&ServiceAccount::new("whatsapp", &stale.account)),
        AccountState::Unavailable
    );
    assert!(!supervisor.client().supports(&fixture.route));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);

    // Native recipient evidence reaches private history monotonically, wakes
    // its readers, and never sends or replays anything.
    let fixture = super::receipt_tests::fixture().await;
    let history = fixture.broker.client().history();
    assert_eq!(
        history
            .read(fixture.route.clone(), None)
            .await
            .unwrap()
            .messages[0]
            .delivery,
        Some(Delivery::Uncertain)
    );
    let wakes = Arc::new(AtomicUsize::new(0));
    let changed = wakes.clone();
    let _guard = fixture
        .broker
        .observe(Arc::new(move || {
            changed.fetch_add(1, Ordering::SeqCst);
        }))
        .unwrap();
    assert!(!observe(&fixture, ReceiptType::Sent).await.unwrap());
    assert_eq!(wakes.load(Ordering::SeqCst), 0);
    assert!(observe(&fixture, ReceiptType::Delivered).await.unwrap());
    let delivered = history.read(fixture.route.clone(), None).await.unwrap();
    assert_eq!(
        delivered.messages[0].delivery,
        Some(Delivery::RecipientDelivered)
    );
    assert!(observe(&fixture, ReceiptType::Read).await.unwrap());
    assert!(!observe(&fixture, ReceiptType::Delivered).await.unwrap());
    let read = history.read(fixture.route.clone(), None).await.unwrap();
    assert_eq!(read.messages[0].delivery, Some(Delivery::RecipientRead));
    assert_eq!(
        read.messages[0].timestamp_ms,
        delivered.messages[0].timestamp_ms
    );
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);

    // A foreign SDK client, a self or group receipt, an unsupported kind, a
    // wrong sender or a wrong operation cannot promote anything.
    let foreign = sdk(&fixture.calls).await;
    assert_eq!(
        fixture
            .broker
            .observe_whatsapp_receipt(
                "operation".into(),
                foreign,
                receipt(&fixture.route, ReceiptType::Read)
            )
            .await,
        Err(SendError::Revoked)
    );
    let mut event = receipt(&fixture.route, ReceiptType::Read);
    event.source.is_group = true;
    assert!(
        !fixture
            .broker
            .observe_whatsapp_receipt("operation".into(), fixture.source.clone(), event)
            .await
            .unwrap()
    );
    let mut event = receipt(&fixture.route, ReceiptType::Read);
    event.source.is_from_me = true;
    assert!(
        !fixture
            .broker
            .observe_whatsapp_receipt("operation".into(), fixture.source.clone(), event)
            .await
            .unwrap()
    );
    let mut event = receipt(&fixture.route, ReceiptType::Read);
    event.r#type = ReceiptType::Other("delivery".into());
    assert_eq!(
        fixture
            .broker
            .observe_whatsapp_receipt("operation".into(), fixture.source.clone(), event)
            .await,
        Err(SendError::Revoked)
    );
    let mut event = receipt(&fixture.route, ReceiptType::Read);
    event.source.sender = "different@lid".parse().unwrap();
    assert_eq!(
        fixture
            .broker
            .observe_whatsapp_receipt("operation".into(), fixture.source.clone(), event)
            .await,
        Err(SendError::Revoked)
    );
    let mut event = receipt(&fixture.route, ReceiptType::Read);
    event.message_ids = vec!["other-operation".into()];
    assert_eq!(
        fixture
            .broker
            .observe_whatsapp_receipt("operation".into(), fixture.source.clone(), event)
            .await,
        Err(SendError::Revoked)
    );
    assert_eq!(
        fixture
            .broker
            .observe_whatsapp_receipt(
                "other-operation".into(),
                fixture.source.clone(),
                receipt(&fixture.route, ReceiptType::Read)
            )
            .await,
        Err(SendError::InvalidRequest)
    );
    assert_eq!(
        fixture
            .broker
            .client()
            .history()
            .read(fixture.route.clone(), None)
            .await
            .unwrap()
            .messages[0]
            .delivery,
        Some(Delivery::RecipientRead)
    );
    fixture.revoked.store(true, Ordering::SeqCst);
    assert_eq!(
        observe(&fixture, ReceiptType::Read).await,
        Err(SendError::Revoked)
    );
    assert_eq!(
        history.read(fixture.route.clone(), None).await,
        Err(HistoryError::Revoked)
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    drop(history);
    let Fixture {
        root,
        owner,
        route,
        broker,
        ..
    } = fixture;
    drop(broker);
    let history = History::open(
        &root.path().join("history"),
        &owner,
        secret_vault::VaultKey::from_bytes([8; 32]),
    )
    .unwrap();
    assert_eq!(
        history.page(route, 128).unwrap().messages[0].delivery,
        Some(Delivery::RecipientRead)
    );
    assert_eq!(
        history.check_operation("operation"),
        Err(SendError::AlreadyReserved)
    );
    let encrypted = std::fs::read(root.path().join("history/history.enc")).unwrap();
    assert!(
        !encrypted
            .windows(b"private fixture message".len())
            .any(|bytes| bytes == b"private fixture message")
    );
}
