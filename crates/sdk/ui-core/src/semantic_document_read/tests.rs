use super::*;
use automerge::{ChangeHash, ROOT, transaction::Transactable};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use tilo_actions::{ActionContract, ExecutionMode, OperationKind, ResultSpec};

fn schema() -> &'static serde_json::Value {
    static VALUE: OnceLock<serde_json::Value> = OnceLock::new();
    VALUE.get_or_init(|| {
        result_schema(
            serde_json::json!({"type":"object","required":["text"],"properties":{"text":{"type":"string"}},"additionalProperties":false}),
            true,
        )
    })
}
fn project(
    document: &AutoCommit,
    heads: &[ChangeHash],
    _payload: &[u8],
) -> Result<ReadPage, SemanticReadError> {
    let text = automerge::ReadDoc::get_at(document, ROOT, "text", heads)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
        .unwrap_or_default();
    Ok(ReadPage {
        content: serde_json::json!({ "text": text }),
        next_cursor: None,
    })
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
    project,
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
        RegisteredSemanticReader::new("fixture.read", schema, decode, project),
        RegisteredSemanticReader::new("fixture.read", schema, decode, project),
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
            1 => resource.actions[0].contract.kind = tilo_actions::ActionKind::Undoable,
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
        project,
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
fn the_loaned_record_reads_at_committed_heads_and_nothing_else_discloses() {
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
        selected.read(Some(&scope), &copied).unwrap_err(),
        SemanticReadError::InvalidOwner,
        "a copy of the record is not the record"
    );
    assert_eq!(
        selected.read(None, &copied).unwrap_err(),
        SemanticReadError::MissingQualifiedOwner,
        "an unscoped read needs the mounted primary document"
    );
    fixture
        .store
        .with_checked(&fixture.checkout.id, |document| {
            document.put(ROOT, "text", "local record").unwrap();
            document.commit();
            let local = crate::invocation_scope::with_loaned_record(
                &fixture.store,
                &fixture.checkout,
                "fixture.app",
                None,
                document,
                |doc| selected.read(None, doc).unwrap().value.unwrap(),
            );
            assert_eq!(local["content"]["text"], "local record");
            assert!(local["record"]["roomId"].is_null());
            assert_eq!(local["record"]["appId"], "fixture.app");
            assert_eq!(local["record"]["recordId"], fixture.checkout.id.as_str());
            let mut copy = document.clone();
            crate::invocation_scope::with_loaned_record(
                &fixture.store,
                &fixture.checkout,
                "fixture.app",
                None,
                &mut copy,
                |doc| {
                    assert_eq!(
                        selected.read(None, doc).unwrap_err(),
                        SemanticReadError::MissingQualifiedOwner,
                        "a copy is not the store's loan"
                    );
                },
            );
            document.put(ROOT, "text", "committed content").unwrap();
            document.commit();
            let heads = document.base_heads();
            let output = selected.read(Some(&scope), document).unwrap();
            resource.actions[0]
                .contract
                .result
                .validate(&output)
                .unwrap();
            let value = output.value.unwrap();
            assert_eq!(value["content"]["text"], "committed content");
            assert_eq!(value["basis"]["heads"][0], heads[0].to_string());
            let route = scope.route();
            assert_eq!(value["record"]["roomId"], route.cluster_id.to_string());
            assert_eq!(value["record"]["appId"], route.app_id.as_str());

            document.put(ROOT, "text", "pending content").unwrap();
            let pending = document.pending_ops();
            assert_eq!(
                selected.read(Some(&scope), document).unwrap_err(),
                SemanticReadError::Pending
            );
            assert_eq!(document.pending_ops(), pending, "the read commits nothing");
            assert_eq!(document.base_heads(), heads);
            document.commit();
            current.store(false, Ordering::Release);
            assert_eq!(
                selected.read(Some(&scope), document).unwrap_err(),
                SemanticReadError::InvalidOwner,
                "an expired caller is refused before disclosure"
            );
        })
        .unwrap();
}

#[test]
fn the_mounted_primary_document_reads_on_its_route_only() {
    let resource = resource();
    let encoded = encoded();
    let selected = select(&READERS, &resource, &resource.actions[0], &encoded)
        .unwrap()
        .unwrap();
    let route =
        crate::collaboration::RouteSpec::app("fixture.app".to_string(), uuid::Uuid::from_u128(7));
    let mut mounted = AutoCommit::new();
    mounted.put(ROOT, "text", "primary").unwrap();
    mounted.commit();
    let other = mounted.clone();
    crate::invocation_scope::with_mounted_document(Some(route.clone()), &mut mounted, |doc| {
        let value = selected.read(None, doc).unwrap().value.unwrap();
        assert_eq!(value["content"]["text"], "primary");
        assert_eq!(value["record"]["appId"], "fixture.app");
        assert_eq!(value["record"]["roomId"], route.cluster_id.to_string());
        assert_eq!(
            selected.read(None, &other).unwrap_err(),
            SemanticReadError::MissingQualifiedOwner,
            "only the exact mounted document is identified"
        );
    });
    crate::invocation_scope::with_mounted_document(None, &mut mounted, |doc| {
        assert_eq!(
            selected.read(None, doc).unwrap_err(),
            SemanticReadError::MissingQualifiedOwner,
            "without a primary route the document is not a record"
        );
    });
    assert_eq!(
        selected.read(None, &mounted).unwrap_err(),
        SemanticReadError::MissingQualifiedOwner
    );
}
