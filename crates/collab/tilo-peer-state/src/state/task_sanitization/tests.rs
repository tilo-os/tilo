use super::*;
use automerge::{ObjType, ROOT, ReadDoc};
use serde_json::json;
use std::collections::BTreeMap;

#[test]
fn signed_extraction_preserves_public_fields_and_retained_direction_bindings() {
    let (mut source, key, _, _, now) = crate::admission_tests::seeded();
    let instruction = "private original goal\n  exact whitespace";
    let direction = "private next direction";
    let mut doc = source.core.room_doc.as_deref().unwrap().clone();
    let tasks = doc
        .put_object(
            ROOT,
            tilo_desktop_model::projection::ROOM_TASKS_KEY,
            ObjType::Map,
        )
        .unwrap();
    let controls = doc
        .put_object(ROOT, crate::workspace_task::TASK_CONTROL_KEY, ObjType::Map)
        .unwrap();
    let task = json!({"route": "model", "title": "Public label", "audit": [], "future": {"retained": true}});
    let mut control =
        serde_json::to_value(tilo_desktop_model::WorkspaceTaskControl::default()).unwrap();
    control["instruction"] = instruction.into();
    control["directions"] = json!([{"revision": 3, "text": direction}]);
    doc.put(&tasks, "task", serde_json::to_vec(&task).unwrap())
        .unwrap();
    doc.put(&controls, "task", serde_json::to_vec(&control).unwrap())
        .unwrap();
    doc.commit();
    source.core.room_doc = Some(doc.into());
    let mut receiver = source.clone();
    let before = source.durable().unwrap();
    let foreign = ed25519_dalek::SigningKey::from_bytes(&[77; 32]);
    assert!(
        source
            .extract_native_task_private_baseline(&foreign, now, &BTreeMap::new())
            .is_err()
    );
    assert_eq!(source.durable().unwrap(), before);
    let (private, baseline) = source
        .extract_native_task_private_baseline(&key, now, &BTreeMap::new())
        .unwrap();
    assert_eq!(private["task"].instruction.as_deref(), Some(instruction));
    assert_eq!(private["task"].directions[&3], direction);
    receiver
        .admit_sanitized_room_baseline(&baseline, now)
        .unwrap();
    let public = baseline
        .verify_anchored_document(&baseline.room, baseline.governor, &baseline.anchor())
        .unwrap();
    let public_task = public
        .get(ROOT, tilo_desktop_model::projection::ROOM_TASKS_KEY)
        .unwrap()
        .unwrap()
        .1;
    let public_control = public
        .get(ROOT, crate::workspace_task::TASK_CONTROL_KEY)
        .unwrap()
        .unwrap()
        .1;
    let bytes = |map| {
        let (automerge::Value::Scalar(value), _) = public.get(map, "task").unwrap().unwrap() else {
            panic!("task bytes")
        };
        let automerge::ScalarValue::Bytes(bytes) = value.as_ref() else {
            panic!("task bytes")
        };
        serde_json::from_slice::<serde_json::Value>(bytes).unwrap()
    };
    assert_eq!(bytes(public_task), task);
    let scrubbed = bytes(public_control);
    assert!(scrubbed.get("instruction").is_none());
    let reference = crate::workspace_task::private_instruction_reference(direction);
    assert_eq!(scrubbed["directions"][0]["text"], reference);
    let known = BTreeMap::from([("task".into(), BTreeMap::from([(3, reference)]))]);
    let (retried_private, second) = source
        .extract_native_task_private_baseline(&key, now, &known)
        .unwrap();
    assert!(
        retried_private
            .get("task")
            .is_none_or(|text| text.directions.is_empty())
    );
    assert_eq!(second.generation, baseline.generation + 1);
    receiver
        .admit_sanitized_room_baseline(&second, now)
        .unwrap();
    assert!(
        receiver
            .admit_sanitized_room_baseline(&baseline, now)
            .is_err()
    );
}
