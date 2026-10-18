//! Bounded action-only moderation requests on acknowledged record channels.
use super::CollabRequest;
use crate::invocation_scope::ExternalModerationReply;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tokio::sync::{mpsc, oneshot};

type Reply = Result<registry_types::ModerationCase, String>;
const MAX_ROUTES: usize = 16;
const MAX_PENDING: usize = 2;
struct Endpoint(mpsc::Sender<CollabRequest>);
pub(super) struct Registration {
    _endpoint: Arc<Endpoint>,
    worker: tokio::task::JoinHandle<()>,
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.worker.abort();
    }
}
fn registry() -> &'static Mutex<BTreeMap<String, Weak<Endpoint>>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<String, Weak<Endpoint>>>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

pub(crate) fn enqueue(
    route: &str,
    invocation: tilo_actions::InvocationId,
    operation: registry_types::ExternalModerationOperation,
) -> Result<oneshot::Receiver<Reply>, String> {
    let command = runtime_collab::external_moderation::Request {
        protocol: runtime_collab::external_moderation::PROTOCOL.into(),
        invocation: invocation.0.to_string(),
        operation,
    };
    command.validate()?;
    if serde_json::to_vec(&command)
        .map_err(|error| error.to_string())?
        .len()
        > runtime_collab::external_moderation::MAX_WIRE_BYTES
    {
        return Err("external moderation exceeds the protocol bound".into());
    }
    let endpoint = registry()
        .lock()
        .map_err(|_| "external moderation bridge unavailable")?
        .get(route)
        .and_then(Weak::upgrade)
        .ok_or("no acknowledged external moderation route")?;
    let (reply, receive) = ExternalModerationReply::new();
    endpoint
        .0
        .try_send(CollabRequest::ExternalModeration {
            command,
            on_behalf_of: invocation,
            reply,
        })
        .map_err(|_| "external moderation bounded lane unavailable or full")?;
    Ok(receive)
}

fn admit(route: String, endpoint: &Arc<Endpoint>) -> Result<(), String> {
    let mut entries = registry()
        .lock()
        .map_err(|_| "external moderation bridge unavailable")?;
    entries.retain(|_, endpoint| endpoint.strong_count() != 0);
    if route.is_empty()
        || route.len() > 1024
        || entries.contains_key(&route)
        || entries.len() >= MAX_ROUTES
    {
        return Err("external moderation route is ambiguous or exceeds admission bounds".into());
    }
    entries.insert(route, Arc::downgrade(endpoint));
    Ok(())
}

pub(super) fn register(
    route: String,
    channel: runtime_collab::ChannelHandle,
) -> Result<Registration, String> {
    let (send, mut receive) = mpsc::channel(MAX_PENDING);
    let endpoint = Arc::new(Endpoint(send));
    admit(route, &endpoint)?;
    let worker = tokio::task::spawn_local(async move {
        while let Some(request) = receive.recv().await {
            if let CollabRequest::ExternalModeration {
                command,
                on_behalf_of,
                reply,
            } = request
            {
                execute(&channel, &command, on_behalf_of, &reply).await;
            }
        }
    });
    Ok(Registration {
        _endpoint: endpoint,
        worker,
    })
}

#[allow(
    clippy::future_not_send,
    reason = "the broker worker owns Rc-based Cap'n Proto handles on its LocalSet"
)]
pub(super) async fn execute(
    channel: &runtime_collab::ChannelHandle,
    command: &runtime_collab::external_moderation::Request,
    on_behalf_of: tilo_actions::InvocationId,
    reply: &ExternalModerationReply,
) {
    if reply.is_closed() {
        return;
    }
    let result = channel
        .external_moderation(command, *on_behalf_of.0.as_bytes())
        .await
        .map_err(|error| error.to_string());
    reply.send(result);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collaboration::RouteSpec;
    use crate::host_operation::OperationTarget;
    use registry_types::{ExternalModerationOperation, ModerationDestination, ModerationMutation};

    fn operation() -> ExternalModerationOperation {
        ExternalModerationOperation {
            destination: ModerationDestination {
                base_url: "https://registry.example".into(),
                owner_pubkey_hex: "ab".repeat(32),
            },
            request_id: "report-one".into(),
            issued_at_unix: 100,
            expires_at_unix: 200,
            mutation: ModerationMutation::FileReport {
                package_id: "org.example.app".into(),
                category: "misleading".into(),
                summary: "The package description is misleading".into(),
                evidence_digest_hex: "cd".repeat(32),
            },
        }
    }

    #[tokio::test]
    async fn external_moderation_bridge_preserves_exact_record_invocation_operation_and_reply() {
        let room = uuid::Uuid::new_v4();
        let first_route = RouteSpec::record("so.tilo.store", room, "first");
        let second_route = RouteSpec::record("so.tilo.store", room, "second");
        let (first_send, mut first_receive) = mpsc::channel(MAX_PENDING);
        let (second_send, mut second_receive) = mpsc::channel(MAX_PENDING);
        let first = Arc::new(Endpoint(first_send));
        let second = Arc::new(Endpoint(second_send));
        admit(first_route.channel_id(), &first).unwrap();
        admit(second_route.channel_id(), &second).unwrap();
        let operation = operation();
        let address = tilo_actions::ActionAddress::new("registry", "submit-report");
        let target = OperationTarget::new(first_route, address.clone());
        let other = OperationTarget::new(second_route, address);
        let invocation = tilo_actions::InvocationId::new();
        let future = target
            .external_moderation(invocation, operation.clone())
            .unwrap();
        fn assert_send(_: &impl Send) {}
        assert_send(&future);
        assert!(second_receive.try_recv().is_err());
        let CollabRequest::ExternalModeration {
            command,
            on_behalf_of,
            reply,
        } = first_receive.try_recv().unwrap()
        else {
            panic!("wrong collaboration request");
        };
        assert_eq!(command.operation, operation);
        assert_eq!(command.invocation, invocation.0.to_string());
        assert_eq!(on_behalf_of, invocation);
        let case = registry_types::ModerationCase {
            case_id: registry_types::moderation_case_id("org.example.app", "report-one"),
            package_id: "org.example.app".into(),
            publisher_pubkey_hex: "ef".repeat(32),
            status: registry_types::ModerationCaseStatus::Open,
            opened_at_unix: 101,
            updated_at_unix: 101,
            latest_sequence: 1,
        };
        reply.send(Ok(case.clone()));
        assert_eq!(future.await.unwrap(), case);
        let second_id = tilo_actions::InvocationId::new();
        let pending = other
            .external_moderation(second_id, operation.clone())
            .unwrap();
        assert!(
            matches!(second_receive.try_recv().unwrap(), CollabRequest::ExternalModeration { on_behalf_of, .. } if on_behalf_of == second_id)
        );
        assert!(first_receive.try_recv().is_err());
        drop(pending);
        drop(first);
        assert!(target.external_moderation(invocation, operation).is_err());
    }

    #[tokio::test]
    async fn external_moderation_bridge_scoped_custody_rejects_changed_parameters_and_cancelled_receipt()
     {
        use crate::invocation_scope::{
            PublishedInvocationLease, PublishedInvocationRequest, ResourceInvocationScope,
        };
        let fixture = crate::bound_dispatch::legacy_document_tests::Fixture::in_room(
            1980,
            "77777777-7777-4777-8777-777777777777",
        );
        fixture
            .store
            .require_invocation_provenance(&fixture.checkout)
            .unwrap();
        let route = fixture.target.owner.broker_route().unwrap();
        let (send, mut receive) = mpsc::channel(MAX_PENDING);
        let endpoint = Arc::new(Endpoint(send));
        admit(route.channel_id(), &endpoint).unwrap();
        let operation = operation();
        let invocation = tilo_actions::InvocationId::new();
        let lease = PublishedInvocationLease::new(
            PublishedInvocationRequest {
                wire_id: 1980,
                invocation,
                address: tilo_actions::ActionAddress::new("fixture", "submit-report"),
                argument: Some(format!(
                    "{}{}",
                    tilo_actions::ARGUMENTS_JSON_PREFIX,
                    serde_json::json!({
                        "app": "org.example.app",
                        "operation": serde_json::to_string(&operation).unwrap(),
                    })
                )),
                actor: "original-caller".into(),
                participant: None,
            },
            route,
            Arc::new(|| Ok(())),
        )
        .with_owner_token(Arc::new("original moderation caller"))
        .unwrap();
        let scope =
            ResourceInvocationScope::for_owner(fixture.target.owner.clone(), lease).unwrap();
        let mut changed = operation.clone();
        changed.destination.base_url = "https://other-registry.example".into();
        assert!(scope.external_moderation(changed).is_err());
        assert!(
            receive.try_recv().is_err(),
            "changed destination must never reach the host"
        );
        let mut changed = operation.clone();
        if let ModerationMutation::FileReport { summary, .. } = &mut changed.mutation {
            summary.push('!');
        }
        assert!(scope.external_moderation(changed).is_err());
        assert!(receive.try_recv().is_err());
        let pending = scope.external_moderation(operation.clone()).unwrap();
        let CollabRequest::ExternalModeration {
            command,
            on_behalf_of,
            reply,
        } = receive.try_recv().unwrap()
        else {
            panic!("wrong request")
        };
        assert_eq!(command.operation, operation);
        assert_eq!(on_behalf_of, invocation);
        scope.cancel();
        reply.send(Ok(registry_types::ModerationCase {
            case_id: "case".into(),
            package_id: "org.example.app".into(),
            publisher_pubkey_hex: "ef".repeat(32),
            status: registry_types::ModerationCaseStatus::Open,
            opened_at_unix: 101,
            updated_at_unix: 101,
            latest_sequence: 1,
        }));
        assert!(pending.await.unwrap_err().contains("retired"));
        assert!(scope.external_moderation(operation).is_err());
        assert!(receive.try_recv().is_err());
    }

    #[test]
    fn external_moderation_bridge_refuses_wrong_action_foreign_full_invalid_and_retired_routes() {
        let route = RouteSpec::record("so.tilo.store", uuid::Uuid::new_v4(), "bounded");
        let (send, mut receive) = mpsc::channel(MAX_PENDING);
        let endpoint = Arc::new(Endpoint(send));
        admit(route.channel_id(), &endpoint).unwrap();
        let (duplicate, _) = mpsc::channel(MAX_PENDING);
        assert!(admit(route.channel_id(), &Arc::new(Endpoint(duplicate))).is_err());
        let invocation = tilo_actions::InvocationId::new();
        let operation = operation();
        for action in [
            "registry.publisher_reply",
            operation.action(),
            "appeal",
            "report",
        ] {
            let wrong = OperationTarget::new(
                route.clone(),
                tilo_actions::ActionAddress::new("registry", action),
            );
            assert!(
                wrong
                    .external_moderation(invocation, operation.clone())
                    .is_err(),
                "{action}"
            );
            assert!(receive.try_recv().is_err());
        }
        assert!(enqueue("foreign-record", invocation, operation.clone()).is_err());
        let mut invalid = operation.clone();
        invalid.destination.base_url = "file:///untrusted".into();
        assert!(enqueue(&route.channel_id(), invocation, invalid).is_err());
        assert!(
            enqueue(
                &route.channel_id(),
                tilo_actions::InvocationId(uuid::Uuid::nil()),
                operation.clone()
            )
            .is_err()
        );
        let first = enqueue(&route.channel_id(), invocation, operation.clone()).unwrap();
        let second = enqueue(&route.channel_id(), invocation, operation.clone()).unwrap();
        assert!(enqueue(&route.channel_id(), invocation, operation.clone()).is_err());
        let CollabRequest::ExternalModeration { reply, .. } = receive.try_recv().unwrap() else {
            panic!("wrong request")
        };
        drop(first);
        assert!(
            reply.is_closed(),
            "retired action observers must not start an effect"
        );
        drop(second);
        drop(receive);
        assert!(enqueue(&route.channel_id(), invocation, operation.clone()).is_err());
        drop(endpoint);
        assert!(enqueue(&route.channel_id(), invocation, operation).is_err());
    }
}
