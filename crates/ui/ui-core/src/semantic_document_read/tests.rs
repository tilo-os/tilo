use super::*;
use automerge::{ChangeHash, ROOT, transaction::Transactable};
use std::sync::OnceLock;
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
fn decode(resource: &ResourceSpec, payload: &[u8]) -> Result<(), SemanticReadError> {
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
fn resource() -> ResourceSpec {
    ResourceSpec::new("fixture.document", "fixture.document", "Document").with_action(
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

tilo_test_env::scenario! {
    fn only_the_exact_registered_reader_and_request_are_admitted;

    phase exact_registered_spec_selects_but_copied_or_client_only_claims_do_not;
    {
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

    phase changed_contract_resource_and_closed_payload_are_refused;
    {
        for change in 0..5 {
            let mut resource = resource();
            match change {
                0 => resource.actions[0].type_mut().kind = OperationKind::View,
                1 => resource.actions[0].type_mut().contract.kind = tilo_actions::ActionKind::Undoable,
                2 => {
                    resource.actions[0].type_mut().contract.execution =
                        ExecutionMode::Background { cancellable: false }
                }
                3 => {
                    resource.actions[0].type_mut().contract.result = ResultSpec::Json {
                        schema: serde_json::json!({"type":"object"}),
                    }
                }
                _ => resource.actions[0].type_mut().operation_type = Some("client.assertion".into()),
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

    phase oversized_request_is_refused_before_app_decoder;
    {
        fn must_not_decode(_: &ResourceSpec, _: &[u8]) -> Result<(), SemanticReadError> {
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
}

#[test]
fn the_loaned_record_reads_at_committed_heads_and_nothing_else_discloses() {
    let fixture = crate::history_mode::owner::ResourceOwnerFixture::in_workspace(
        983,
        "77777777-7777-4777-8777-777777777777",
    );
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
        selected.read(&copied).unwrap_err(),
        SemanticReadError::MissingQualifiedOwner,
        "an unscoped read needs the mounted primary document"
    );
    fixture
        .store
        .with_checked(&fixture.checkout.id, |document| {
            document.put(ROOT, "text", "local record").unwrap();
            document.commit();
            let local = crate::invocation_scope::with_loaned_resource(
                &fixture.store,
                &fixture.checkout,
                "fixture.app",
                None,
                document,
                |doc| selected.read(doc).unwrap().value.unwrap(),
            );
            assert_eq!(local["content"]["text"], "local record");
            assert!(local["record"]["workspaceId"].is_null());
            assert_eq!(local["record"]["appId"], "fixture.app");
            assert_eq!(local["record"]["recordId"], fixture.checkout.id.as_str());
            let mut copy = document.clone();
            crate::invocation_scope::with_loaned_resource(
                &fixture.store,
                &fixture.checkout,
                "fixture.app",
                None,
                &mut copy,
                |doc| {
                    assert_eq!(
                        selected.read(doc).unwrap_err(),
                        SemanticReadError::MissingQualifiedOwner,
                        "a copy is not the store's loan"
                    );
                },
            );
            document.put(ROOT, "text", "committed content").unwrap();
            document.commit();
            let heads = document.base_heads();
            let route = tilo_ui_base::collaboration::RouteSpec::document(
                "fixture.app",
                uuid::Uuid::new_v4(),
                fixture.checkout.id.as_str(),
            );
            crate::invocation_scope::with_loaned_resource(
                &fixture.store,
                &fixture.checkout,
                "fixture.app",
                Some(route.clone()),
                document,
                |document| {
                    let output = selected.read(document).unwrap();
                    resource.actions[0]
                        .contract
                        .result
                        .validate(&output)
                        .unwrap();
                    let value = output.value.unwrap();
                    assert_eq!(value["content"]["text"], "committed content");
                    assert_eq!(value["basis"]["heads"][0], heads[0].to_string());
                    assert_eq!(value["record"]["workspaceId"], route.cluster_id.to_string());
                    assert_eq!(value["record"]["appId"], route.app_id.as_str());

                    document.put(ROOT, "text", "pending content").unwrap();
                    let pending = document.pending_ops();
                    assert_eq!(
                        selected.read(document).unwrap_err(),
                        SemanticReadError::Pending
                    );
                    assert_eq!(document.pending_ops(), pending, "the read commits nothing");
                    assert_eq!(document.base_heads(), heads);
                    document.commit();
                },
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
        tilo_ui_base::collaboration::RouteSpec::app("fixture.app".to_string(), uuid::Uuid::from_u128(7));
    let mut mounted = AutoCommit::new();
    mounted.put(ROOT, "text", "primary").unwrap();
    mounted.commit();
    let other = mounted.clone();
    crate::invocation_scope::with_mounted_document(Some(route.clone()), &mut mounted, |doc| {
        let value = selected.read(doc).unwrap().value.unwrap();
        assert_eq!(value["content"]["text"], "primary");
        assert_eq!(value["record"]["appId"], "fixture.app");
        assert_eq!(value["record"]["workspaceId"], route.cluster_id.to_string());
        assert_eq!(
            selected.read(&other).unwrap_err(),
            SemanticReadError::MissingQualifiedOwner,
            "only the exact mounted document is identified"
        );
    });
    crate::invocation_scope::with_mounted_document(None, &mut mounted, |doc| {
        assert_eq!(
            selected.read(doc).unwrap_err(),
            SemanticReadError::MissingQualifiedOwner,
            "without a primary route the document is not a record"
        );
    });
    assert_eq!(
        selected.read(&mounted).unwrap_err(),
        SemanticReadError::MissingQualifiedOwner
    );
}
