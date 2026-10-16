//! Refuse invalid action inputs and unverifiable result contracts before success.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use serde_json::json;
use tilo_actions::{
    ActionArgs, ActionOutput, ActionParam, ActionSpec, ArgError, ArgValue, EncodedAction,
    ExecutionLocus, ParamType, ResultSpec, validate_json_schema,
};

fn encoded() -> EncodedAction {
    EncodedAction {
        type_id: "test.validated".into(),
        payload: vec![],
    }
}

#[test]
fn non_finite_numbers_never_reach_the_action_builder() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let spec = ActionSpec::typed(
        "set",
        "Set",
        "value",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required("value", ParamType::Number, "value")
                .with_schema(json!({"minimum": 0, "maximum": 1})),
        ],
        move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            encoded()
        },
    );
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(
            matches!(
                spec.build_args([("value".into(), ArgValue::Number(value))]),
                Err(ArgError::Constraint { name, reason })
                    if name == "value" && reason.contains("finite")
            ),
            "non-finite input must not become JSON null or an executable action"
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(
        spec.build_args([("value".into(), ArgValue::Number(0.5))])
            .is_ok()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn closed_object_without_properties_rejects_supplied_fields() {
    let schema = json!({"type": "object", "additionalProperties": false});
    let spec = ActionSpec::typed(
        "set",
        "Set",
        "request",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required("request", ParamType::Object, "request")
                .with_schema(schema.clone()),
        ],
        |_| encoded(),
    );
    let supplied = serde_json::Map::from_iter([("request".into(), json!({"unexpected": true}))]);
    assert!(matches!(
        spec.build_json(&supplied),
        Err(ArgError::Constraint { name, reason })
            if name == "request" && reason.contains("unexpected")
    ));
    assert!(validate_json_schema(&json!({"unexpected": true}), &schema).is_err());
    assert!(validate_json_schema(&json!({}), &schema).is_ok());
    let open = json!({"type": "object"});
    assert!(validate_json_schema(&json!({"anything": true}), &open).is_ok());
}

#[test]
fn runtime_schema_validation_refuses_unverifiable_assertions() {
    for (schema, keyword) in [
        (json!({"minProperties": 1}), "minProperties"),
        (json!({"required": "id"}), "required"),
        (json!({"minimum": "zero"}), "minimum"),
        (
            json!({"properties": {"missing": {"pattern": "["}}}),
            "pattern",
        ),
        (
            json!({"oneOf": [{"const": 1}, {"unsupported": true}]}),
            "unsupported",
        ),
    ] {
        let result = validate_json_schema(&json!({}), &schema);
        assert!(
            matches!(result, Err(ref reason) if reason.contains(keyword)),
            "must report the invalid assertion rather than accept a weakened schema: {result:?}"
        );
        let output = ActionOutput {
            value: Some(json!({})),
            resources: vec![],
        };
        let result = ResultSpec::Json { schema }.validate(&output);
        assert!(
            matches!(result, Err(ref reason) if reason.contains(keyword)),
            "result validation must not certify an unverifiable contract: {result:?}"
        );
    }
    assert!(validate_json_schema(&json!({}), &json!({"title": "Annotation only"})).is_ok());
    let param = ActionParam::required("target", ParamType::Resource, "target")
        .of_resource_kind("files.entry");
    assert!(param.validate_declaration().is_empty());
    assert!(
        ActionArgs::bind(
            &[param],
            [("target".into(), ArgValue::Resource("files.entry:a".into()))]
        )
        .is_ok()
    );
}
