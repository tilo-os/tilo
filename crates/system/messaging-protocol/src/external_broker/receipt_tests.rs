use super::*;
use crate::signal_session::{Completion, SendJournal};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
        Arc::new(tests::Gate {
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

#[tokio::test]
async fn actual_sdk_recipient_evidence_reaches_private_history_monotonically_without_sending() {
    let fixture = fixture().await;
    let history = fixture.broker.client().history();
    assert_eq!(
        history.read(fixture.route.clone()).await.unwrap().messages[0].delivery,
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
    let delivered = history.read(fixture.route.clone()).await.unwrap();
    assert_eq!(
        delivered.messages[0].delivery,
        Some(Delivery::RecipientDelivered)
    );
    assert!(observe(&fixture, ReceiptType::Read).await.unwrap());
    assert!(!observe(&fixture, ReceiptType::Delivered).await.unwrap());
    let read = history.read(fixture.route.clone()).await.unwrap();
    assert_eq!(read.messages[0].delivery, Some(Delivery::RecipientRead));
    assert_eq!(
        read.messages[0].timestamp_ms,
        delivered.messages[0].timestamp_ms
    );
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.revoked.store(true, Ordering::SeqCst);
    assert_eq!(
        observe(&fixture, ReceiptType::Read).await,
        Err(SendError::Revoked)
    );
    assert_eq!(
        history.read(fixture.route.clone()).await,
        Err(HistoryError::Revoked)
    );
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

#[tokio::test]
async fn foreign_sdk_and_wrong_recipient_or_operation_cannot_promote_history() {
    let fixture = fixture().await;
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
            .read(fixture.route.clone())
            .await
            .unwrap()
            .messages[0]
            .delivery,
        Some(Delivery::Uncertain)
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
}

pub(super) fn messages(
    fixture: &Fixture,
) -> ui::framework::testing::TestApp<messages::MessagesShell> {
    let mut app = messages_with_client(fixture, fixture.broker.client());
    let route = &fixture.route;
    let choice = serde_json::json!({"service":route.service,"account":fixture.owner.account,"recipient":route.recipient,"association":route.association.as_str(),"scope":route.scope,"revision":route.revision}).to_string();
    app.effect(&messages::MessagesEffect::ReadHistory { choice })
        .unwrap();
    app
}
pub(super) fn messages_with_client(
    fixture: &Fixture,
    client: tilo_messaging::MessagingClient,
) -> ui::framework::testing::TestApp<messages::MessagesShell> {
    use messages::{MessagesEffect, MessagesShell};
    use tilo_contact_directory::ContactDirectory;
    let mut app = ui::framework::testing::TestApp::<MessagesShell>::for_person_with_lent(
        &fixture.owner.principal,
        client,
    )
    .unwrap();
    tests::project_account_metadata(&fixture.owner, &fixture.route, &mut app);
    let directory = app.shared_person::<ContactDirectory>().unwrap();
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

#[tokio::test]
async fn actual_receipt_wakes_messages_and_refreshes_visible_recipient_evidence_privately() {
    let fixture = fixture().await;
    let mut app = messages(&fixture);
    assert!(observe(&fixture, ReceiptType::Read).await.unwrap());
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(5), |harness| {
                harness
                    .a11y_nodes()
                    .iter()
                    .any(|(_, node)| node.label() == Some("Read by recipient"))
            }),
        "native evidence wakes and refreshes the selected private conversation"
    );
    assert!(
        !app.harness()
            .dump_json()
            .contains("private fixture message")
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
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
