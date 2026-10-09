//! SDK correlation proof using an RPC-delivered source and actual executor.
//! This protocol fixture does not establish native owner/model acceptance.
use super::*;
use automerge::{ROOT, transaction::Transactable};
use runtime_collab::collab_capnp::{channel_handle, collab_service, invocation_stream};

const ROOM: &str = "00000000-0000-4000-8000-000000000071";
const TEXT: &str = "Explicit input";
const AGENT: &str = "scribe";

fn source_spec() -> PublishedInvocationSpec {
    let resource = format!("chat.person:{AGENT}");
    let params = vec![tilo_actions::ActionParam::required(
        tilo_actions::PROMPTED_PARAM,
        tilo_actions::ParamType::Text,
        "Message text",
    )];
    let descriptor = tilo_actions::ActionDescriptor {
        address: tilo_actions::ActionAddress::new(&resource, "message"),
        semantic_identity: Some(tilo_actions::ResourceSemanticIdentity {
            kind: "chat.agent".into(),
            key: resource,
        }),
        resource_kind: "chat.agent".into(),
        resource_label: "Agent".into(),
        action_label: "Message".into(),
        description: None,
        locus: tilo_actions::ExecutionLocus::ClientLocal,
        kind: tilo_actions::OperationKind::Document,
        contract: tilo_actions::ActionContract::default(),
        input: tilo_actions::InputSpec::None,
        params: params.clone(),
        view: None,
    };
    PublishedInvocationSpec {
        kind: tilo_actions::OperationKind::Document,
        contract: tilo_actions::ActionContract::default(),
        params,
        input: tilo_actions::InputSpec::None,
        directory_revision: None,
        fresh_binding: Some(tilo_actions::remote::FreshActionBinding {
            descriptor,
            operation_type: Some("chat.action".into()),
        }),
    }
}

struct SourceStream;
impl invocation_stream::Server for SourceStream {
    fn read(
        self: Rc<Self>,
        params: invocation_stream::ReadParams,
        mut results: invocation_stream::ReadResults,
    ) -> impl Future<Output = Result<(), capnp::Error>> + 'static {
        assert!(params.get().unwrap().get_native_document_source());
        let spec = source_spec();
        let mut out = results.get();
        out.set_id(7);
        out.set_invocation_uuid(&[7; 16]);
        out.set_resource(format!("chat.person:{AGENT}"));
        out.set_action("message");
        out.set_actor("person");
        out.set_argument(format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::json!({tilo_actions::PROMPTED_PARAM: TEXT})
        ));
        out.set_has_argument(true);
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

type TestMessage = tilo_actions::agent_message::AgentMessagePayload<
    serde_json::Value,
    serde_json::Value,
    serde_json::Value,
    serde_json::Value,
>;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
enum TestMessageMutation {
    PostAgentMessage {
        workspace: String,
        agent: String,
        message: TestMessage,
    },
}

impl tilo_actions::Action for TestMessageMutation {
    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
        doc.put(ROOT, "explicit", "input")?;
        Ok(())
    }

    fn locus(&self) -> tilo_actions::ExecutionLocus {
        tilo_actions::ExecutionLocus::ClientLocal
    }
}

fn message_action(workspace: &str) -> TestMessageMutation {
    TestMessageMutation::PostAgentMessage {
        workspace: workspace.to_owned(),
        agent: AGENT.to_owned(),
        message: TestMessage {
            id: "inert".into(),
            ts: 7,
            text: TEXT.into(),
            intent: tilo_actions::agent_message::AgentIntent::Message,
            attachment: None,
            mentions: Vec::new(),
            reference: None,
            body: Vec::new(),
        },
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
        self.apply_to_workspace(ROOM, extra_writer)
    }

    fn apply_to_workspace(
        &self,
        workspace: &str,
        extra_writer: bool,
    ) -> Result<tilo_actions::ActionEnvelope, String> {
        let _invoker = super::super::InvokerScope::enter(self.capture.source.request.invoker());
        let _active = self.capture.enter();
        self.store
            .with_action_state(&self.checkout.id, |doc, _, _| {
                let before = doc.get_actor().clone();
                let context = tilo_actions::ActionContext {
                    source_invocation: Some(self.capture.source.request.invocation),
                    ..Default::default()
                };
                let encoded =
                    tilo_actions::EncodedAction::encode("chat.action", &message_action(workspace))
                        .unwrap();
                NativeMessageCapture::validate_encoded(doc, &encoded)?;
                let envelope = tilo_actions::executor::apply_local_with_context(
                    doc,
                    &TypedWrite,
                    encoded,
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

    pub(crate) fn dispatch(&self, workspace: &str) -> Result<(), tilo_actions::DoorError> {
        let _invoker = super::super::InvokerScope::enter(self.capture.source.request.invoker());
        let _active = self.capture.enter();
        self.store
            .with_action_state(&self.checkout.id, |doc, _, _| {
                let action = message_action(workspace);
                let encoded = tilo_actions::EncodedAction::encode("chat.action", &action).unwrap();
                let mut registry = tilo_actions::ActionRegistry::new();
                registry.register::<TestMessageMutation>("chat.action");
                let resource = tilo_actions::Resource::new(
                    format!("chat.person:{AGENT}"),
                    "chat.agent",
                    "Agent",
                );
                let spec = tilo_actions::ActionSpec::immediate(
                    "message",
                    "Message",
                    "chat.action",
                    &action,
                )
                .unwrap();
                let mut widget = crate::widget::Container::new();
                let mut transport = tilo_actions::LocalTransport::default();
                crate::bound_dispatch::execute_resource_operation(
                    &mut widget,
                    doc,
                    &ROOT,
                    &registry,
                    &mut transport,
                    crate::bound_dispatch::ResourceOperation {
                        target_route: Some(self.capture.source.route().clone()),
                        address: &tilo_actions::ActionAddress::new(
                            format!("chat.person:{AGENT}"),
                            "message",
                        ),
                        resource: &resource,
                        spec: &spec,
                        encoded,
                        actor: "person",
                        local_author: "person",
                        broker_admitted: true,
                        invocation: self.capture.source.request.invocation,
                        source_invocation: Some(self.capture.source.request.invocation),
                        ambient: None,
                        scope: None,
                    },
                )?;
                Ok(())
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

#[tokio::test(flavor = "current_thread")]
async fn fresh_message_refuses_changed_workspace_before_document_mutation() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let lifetime = NativeMessageStreamLifetime::new();
            let fixture = RecordFixture::new(delivered_source(&lifetime).await);
            let before = fixture
                .store
                .read(&fixture.checkout.id, |doc| doc.clone().get_heads())
                .unwrap();
            fixture.dispatch(ROOM).unwrap();
            let changes = fixture
                .store
                .read(&fixture.checkout.id, |doc| {
                    let mut doc = doc.clone();
                    doc.get_changes(&before).len()
                })
                .unwrap();
            assert_eq!(
                changes, 1,
                "matching destination applies exactly one change"
            );
            assert!(fixture.capture.has_changes());

            let fixture = RecordFixture::new(delivered_source(&lifetime).await);
            let before = fixture
                .store
                .read(&fixture.checkout.id, |doc| doc.clone().get_heads())
                .unwrap();
            assert!(
                fixture
                    .dispatch("00000000-0000-4000-8000-000000000072")
                    .is_err()
            );
            let after = fixture
                .store
                .read(&fixture.checkout.id, |doc| doc.clone().get_heads())
                .unwrap();
            assert_eq!(after, before, "refused destination made no document delta");
            assert!(!fixture.capture.has_changes());
        })
        .await;
}
