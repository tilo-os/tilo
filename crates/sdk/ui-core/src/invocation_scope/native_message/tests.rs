//! SDK correlation proof using an RPC-delivered source and actual executor.
//! This protocol fixture does not establish native owner/model acceptance.
use super::*;
use automerge::{ROOT, transaction::Transactable};
use runtime_collab::collab_capnp::{channel_handle, collab_service, invocation_stream};

const ROOM: &str = "00000000-0000-4000-8000-000000000071";

struct SourceStream;
impl invocation_stream::Server for SourceStream {
    fn read(
        self: Rc<Self>,
        params: invocation_stream::ReadParams,
        mut results: invocation_stream::ReadResults,
    ) -> impl Future<Output = Result<(), capnp::Error>> + 'static {
        assert!(params.get().unwrap().get_native_document_source());
        let spec = PublishedInvocationSpec {
            kind: tilo_actions::OperationKind::Document,
            contract: tilo_actions::ActionContract::default(),
            params: vec![],
            input: tilo_actions::InputSpec::None,
            directory_revision: None,
        };
        let mut out = results.get();
        out.set_id(7);
        out.set_invocation_uuid(&[7; 16]);
        out.set_resource("opaque-agent");
        out.set_action("message");
        out.set_actor("person");
        out.set_source_token(&[8; 32]);
        out.set_source_spec_json(serde_json::to_string(&spec).unwrap());
        std::future::ready(Ok(()))
    }
}
struct SourceChannel;
impl channel_handle::Server for SourceChannel {
    fn subscribe_invocations(
        self: Rc<Self>,
        _: channel_handle::SubscribeInvocationsParams,
        mut results: channel_handle::SubscribeInvocationsResults,
    ) -> impl Future<Output = Result<(), capnp::Error>> + 'static {
        results
            .get()
            .set_stream(capnp_rpc::new_client(SourceStream));
        std::future::ready(Ok(()))
    }
}
struct SourceBroker;
impl collab_service::Server for SourceBroker {
    fn join_channel(
        self: Rc<Self>,
        _: collab_service::JoinChannelParams,
        mut results: collab_service::JoinChannelResults,
    ) -> impl Future<Output = Result<(), capnp::Error>> + 'static {
        results
            .get()
            .set_handle(capnp_rpc::new_client(SourceChannel));
        std::future::ready(Ok(()))
    }
}

#[expect(
    clippy::future_not_send,
    reason = "test RPC capabilities run solely on the LocalSet"
)]
pub(crate) async fn delivered_source(
    lifetime: &NativeMessageStreamLifetime,
) -> NativeInvocationSource {
    let channel = source_channel().await;
    let stream = channel.subscribe_stable_invocations().await.unwrap();
    let delivery = stream.read_with_document_source().await.unwrap();
    NativeInvocationSource::from_delivery(
        crate::collaboration::RouteSpec::app("so.tilo.chat", uuid::Uuid::parse_str(ROOM).unwrap()),
        &delivery,
        None,
        lifetime,
    )
    .unwrap()
    .unwrap()
}

#[expect(
    clippy::future_not_send,
    reason = "test RPC capabilities run solely on the LocalSet"
)]
pub(crate) async fn source_channel() -> runtime_collab::ChannelHandle {
    let client = runtime_collab::CollabClient::from_raw(capnp_rpc::new_client(SourceBroker));
    client.join_channel("fixture", &[]).await.unwrap()
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct TypedWrite;
impl tilo_actions::Action for TypedWrite {
    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
        doc.put(ROOT, "explicit", "input")?;
        Ok(())
    }
    fn locus(&self) -> tilo_actions::ExecutionLocus {
        tilo_actions::ExecutionLocus::ClientLocal
    }
}

pub(crate) struct RecordFixture {
    pub(crate) store: crate::RecordStore,
    pub(crate) checkout: crate::record::RecordCheckout,
    pub(crate) capture: NativeMessageCapture,
    _lifetime: Rc<crate::history_mode::owner::LegacyHistoryLifetime>,
    _temp: tempfile::TempDir,
}
impl RecordFixture {
    pub(crate) fn new(source: NativeInvocationSource) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let mut owners = crate::record::RecordOwners::new(temp.path().into());
        let store = owners.open("so.tilo.chat", ROOM).unwrap();
        let checkout = crate::record::RecordCheckout::package_primary(
            crate::record::RecordKind::new("chat.workspace"),
        );
        store.create_record(&checkout).unwrap();
        let lifetime = crate::history_mode::owner::LegacyHistoryLifetime::new();
        let owner = HistoryOwner::legacy(store.clone(), checkout.clone(), &lifetime);
        let capture = NativeMessageCapture::new(owner, source).unwrap();
        Self {
            store,
            checkout,
            capture,
            _lifetime: lifetime,
            _temp: temp,
        }
    }
    pub(crate) fn apply(&self, extra_writer: bool) -> Result<tilo_actions::ActionEnvelope, String> {
        let _invoker = super::super::InvokerScope::enter(self.capture.source.request.invoker());
        let _active = self.capture.enter();
        self.store
            .with_action_state(&self.checkout.id, |doc, _, _| {
                let before = doc.get_actor().clone();
                let context = tilo_actions::ActionContext {
                    source_invocation: Some(self.capture.source.request.invocation),
                    ..Default::default()
                };
                let envelope = tilo_actions::executor::apply_local_with_context(
                    doc,
                    &TypedWrite,
                    tilo_actions::EncodedAction::encode("chat.action", &TypedWrite).unwrap(),
                    None,
                    context,
                )
                .unwrap();
                assert_eq!(doc.get_actor(), &before, "executor restores document actor");
                if extra_writer {
                    doc.put(ROOT, "foreign", "not this action").unwrap();
                    doc.commit();
                }
                NativeMessageCapture::retain_applied(doc, &envelope)?;
                Ok(envelope)
            })
            .unwrap()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn actual_executor_capture_retains_exact_writer_action_uuid_and_range() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let lifetime = NativeMessageStreamLifetime::new();
            let fixture = RecordFixture::new(delivered_source(&lifetime).await);
            let envelope = fixture.apply(false).unwrap();
            let contribution = fixture.capture.finish().unwrap();
            let encoded: tilo_actions::ActionEnvelope =
                serde_json::from_slice(contribution.envelope()).unwrap();
            assert_eq!(encoded.id, envelope.id);
            assert_eq!(
                encoded.context.unwrap().source_invocation,
                Some(contribution.source().request().invocation)
            );
            fixture
                .store
                .read(&fixture.checkout.id, |doc| {
                    let mut doc = doc.clone();
                    let changes = doc.get_changes(&envelope.base_change_hashes());
                    assert!(!changes.is_empty());
                    for change in changes {
                        assert_eq!(change.actor_id(), &envelope.actor());
                        assert!(contribution.changes().contains(&change.hash().0));
                    }
                })
                .unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn capture_refuses_foreign_committed_writer_and_retired_delivery() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let lifetime = NativeMessageStreamLifetime::new();
            let fixture = RecordFixture::new(delivered_source(&lifetime).await);
            assert!(fixture.apply(true).is_err());
            assert!(fixture.capture.has_changes());
            assert!(fixture.capture.finish().is_err());
            let fixture = RecordFixture::new(delivered_source(&lifetime).await);
            drop(lifetime);
            assert!(fixture.apply(false).is_err());
            assert!(fixture.capture.finish().is_err());
        })
        .await;
}
