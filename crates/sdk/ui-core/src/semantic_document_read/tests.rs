use super::*;
use automerge::{ROOT, transaction::Transactable};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use tilo_actions::{
    ActionContract, ExecutionMode, OperationKind, PermissionRequirement, ResultSpec,
};

fn schema() -> &'static serde_json::Value {
    static VALUE: OnceLock<serde_json::Value> = OnceLock::new();
    VALUE.get_or_init(|| serde_json::json!({"type":"object","required":["text"],"properties":{"text":{"type":"string"}},"additionalProperties":false}))
}
fn decode(resource: &Resource, payload: &[u8]) -> Result<(), SemanticReadError> {
    if resource.id.as_str() != "fixture.document" || payload != b"{\"pageSize\":2}" {
        return Err(SemanticReadError::InvalidRequest);
    }
    Ok(())
}
static READERS: [RegisteredSemanticReader; 1] = [RegisteredSemanticReader::new(
    "fixture.read",
    schema,
    decode,
)];
fn resource() -> Resource {
    Resource::new("fixture.document", "fixture.document", "Document").with_action(
        ActionSpec::typed_operation(
            "read",
            "Read text",
            "",
            OperationKind::Effect,
            "fixture.read",
            vec![],
            |_| encoded(),
        )
        .with_contract(ActionContract {
            result: ResultSpec::Json {
                schema: schema().clone(),
            },
            ..ActionContract::view()
        }),
    )
}
fn encoded() -> EncodedAction {
    EncodedAction {
        type_id: "fixture.read".into(),
        payload: b"{\"pageSize\":2}".to_vec(),
    }
}

#[test]
fn exact_registered_spec_selects_but_copied_or_client_only_claims_do_not() {
    let resource = resource();
    let encoded = encoded();
    assert!(
        select(&READERS, &resource, &resource.actions[0], &encoded)
            .unwrap()
            .is_some()
    );
    assert!(
        select(&[], &resource, &resource.actions[0], &encoded)
            .unwrap()
            .is_none()
    );
    let copied = resource.actions[0].clone();
    assert!(matches!(
        select(&READERS, &resource, &copied, &encoded),
        Err(SemanticReadError::InvalidContract)
    ));
    let duplicate = [
        RegisteredSemanticReader::new("fixture.read", schema, decode),
        RegisteredSemanticReader::new("fixture.read", schema, decode),
    ];
    assert!(matches!(
        select(&duplicate, &resource, &resource.actions[0], &encoded),
        Err(SemanticReadError::InvalidRegistration)
    ));
}

#[test]
fn changed_contract_resource_and_closed_payload_are_refused() {
    for change in 0..5 {
        let mut resource = resource();
        match change {
            0 => resource.actions[0].kind = OperationKind::View,
            1 => resource.actions[0].contract.permission = PermissionRequirement::Write,
            2 => {
                resource.actions[0].contract.execution =
                    ExecutionMode::Background { cancellable: false }
            }
            3 => {
                resource.actions[0].contract.result = ResultSpec::Json {
                    schema: serde_json::json!({"type":"object"}),
                }
            }
            _ => resource.actions[0].operation_type = Some("client.assertion".into()),
        }
        assert!(matches!(
            select(&READERS, &resource, &resource.actions[0], &encoded()),
            Err(SemanticReadError::InvalidContract)
        ));
    }
    let mut wrong_resource = resource();
    wrong_resource.id = tilo_actions::ResourceId::new("another.document");
    assert!(matches!(
        select(
            &READERS,
            &wrong_resource,
            &wrong_resource.actions[0],
            &encoded()
        ),
        Err(SemanticReadError::InvalidRequest)
    ));
    let resource = resource();
    let mut bad = encoded();
    bad.payload = b"{\"pageSize\":2,\"heads\":[]}".to_vec();
    assert!(matches!(
        select(&READERS, &resource, &resource.actions[0], &bad),
        Err(SemanticReadError::InvalidRequest)
    ));
}

#[test]
fn oversized_request_is_refused_before_app_decoder() {
    fn must_not_decode(_: &Resource, _: &[u8]) -> Result<(), SemanticReadError> {
        panic!("oversized request reached app decoder")
    }
    let readers = [RegisteredSemanticReader::new(
        "fixture.read",
        schema,
        must_not_decode,
    )];
    let resource = resource();
    let encoded = EncodedAction {
        type_id: "fixture.read".into(),
        payload: vec![b' '; MAX_REQUEST_BYTES + 1],
    };
    assert!(matches!(
        select(&readers, &resource, &resource.actions[0], &encoded),
        Err(SemanticReadError::Limit)
    ));
}

#[test]
fn actual_record_loan_stays_unqualified_and_never_commits_pending_text() {
    let fixture = crate::bound_dispatch::legacy_document_tests::Fixture::in_room(
        983,
        "77777777-7777-4777-8777-777777777777",
    );
    fixture
        .store
        .require_invocation_provenance(&fixture.checkout)
        .unwrap();
    let current = Arc::new(AtomicBool::new(true));
    let checked = current.clone();
    let request = crate::invocation_scope::PublishedInvocationRequest {
        wire_id: 983,
        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(983)),
        address: tilo_actions::ActionAddress::new("fixture.document", "read"),
        argument: None,
        actor: "original-reader".into(),
        participant: None,
    };
    // A concrete local test owner token supplies no bounded read qualification.
    let lease = crate::invocation_scope::PublishedInvocationLease::new(
        request,
        fixture.target.owner.broker_route().unwrap(),
        Arc::new(move || {
            checked
                .load(Ordering::Acquire)
                .then_some(())
                .ok_or_else(|| "original caller expired".into())
        }),
    )
    .with_owner_token(Arc::new(983_u64))
    .unwrap();
    let scope = crate::invocation_scope::ResourceInvocationScope::for_owner(
        fixture.target.owner.clone(),
        lease,
    )
    .unwrap();
    let resource = resource();
    let encoded = encoded();
    let selected = select(&READERS, &resource, &resource.actions[0], &encoded)
        .unwrap()
        .unwrap();
    let copied = fixture
        .store
        .read(&fixture.checkout.id, Clone::clone)
        .unwrap();
    assert_eq!(
        selected.admit_frame(&scope, &copied).unwrap_err(),
        SemanticReadError::InvalidOwner
    );
    fixture
        .store
        .with_checked(&fixture.checkout.id, |document| {
            let before = document.base_heads();
            assert_eq!(
                selected.admit_frame(&scope, document).unwrap_err(),
                SemanticReadError::MissingQualifiedOwner
            );
            document.put(ROOT, "text", "pending content").unwrap();
            let pending = document.pending_ops();
            assert!(pending > 0);
            assert_eq!(
                selected.admit_frame(&scope, document).unwrap_err(),
                SemanticReadError::MissingQualifiedOwner
            );
            assert_eq!(document.pending_ops(), pending);
            assert_eq!(document.base_heads(), before);
            current.store(false, Ordering::Release);
            assert_eq!(
                selected.admit_frame(&scope, document).unwrap_err(),
                SemanticReadError::InvalidOwner
            );
        })
        .unwrap();
}
