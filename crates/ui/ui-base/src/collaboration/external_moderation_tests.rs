//! The external-moderation endpoint as a dispatched operation target uses it:
//! the exact record route, the exact invocation and operation, one bounded
//! endpoint per route.

use crate::collaboration::external_moderation::{admit_for_test, enqueue};
use crate::collaboration::{CollabRequest, RouteSpec};

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
    let workspace = uuid::Uuid::new_v4();
    let first_route = RouteSpec::document("so.tilo.store", workspace, "first");
    let second_route = RouteSpec::document("so.tilo.store", workspace, "second");
    let (first, mut first_receive) = admit_for_test(first_route.channel_id()).unwrap();
    let (_second, mut second_receive) = admit_for_test(second_route.channel_id()).unwrap();
    let operation = operation();
    let address = tilo_action_types::ActionAddress::new("registry", "submit-report");
    let target = OperationTarget::new(first_route, address.clone());
    let other = OperationTarget::new(second_route, address);
    let invocation = tilo_action_types::InvocationId::new();
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
    let second_id = tilo_action_types::InvocationId::new();
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

#[test]
fn external_moderation_bridge_refuses_wrong_action_foreign_full_invalid_and_retired_routes() {
    let route = RouteSpec::document("so.tilo.store", uuid::Uuid::new_v4(), "bounded");
    let (endpoint, mut receive) = admit_for_test(route.channel_id()).unwrap();
    assert!(admit_for_test(route.channel_id()).is_err());
    let invocation = tilo_action_types::InvocationId::new();
    let operation = operation();
    for action in [
        "registry.publisher_reply",
        operation.action(),
        "appeal",
        "report",
    ] {
        let wrong = OperationTarget::new(
            route.clone(),
            tilo_action_types::ActionAddress::new("registry", action),
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
            tilo_action_types::InvocationId(uuid::Uuid::nil()),
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
