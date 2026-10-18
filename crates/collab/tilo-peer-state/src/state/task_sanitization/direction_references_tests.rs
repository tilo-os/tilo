use super::*;
use std::collections::BTreeMap;

fn install_direction(source: &mut crate::StatePeer, reference: &str) {
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
    doc.put(
        &tasks,
        "task",
        serde_json::to_vec(&serde_json::json!({"route": "model", "title": "Review"})).unwrap(),
    )
    .unwrap();
    let mut control = tilo_desktop_model::WorkspaceTaskControl::default();
    control.directions.push(
        tilo_desktop_model::task::WorkspaceTaskDirection::owner_admitted(
            1,
            uuid::Uuid::from_u128(1),
            "person".into(),
            reference.into(),
            1,
        ),
    );
    doc.put(&controls, "task", serde_json::to_vec(&control).unwrap())
        .unwrap();
    doc.commit();
    source.core.room_doc = Some(doc.into());
}

#[test]
fn post_checkpoint_direction_custody_is_exact_signed_and_chain_bound() {
    let (mut source, key, _, _, now) = crate::admission_tests::seeded();
    source
        .extract_native_task_private_baseline(&key, now, &BTreeMap::new())
        .unwrap();
    let reference = crate::workspace_task::private_instruction_reference("private direction");
    install_direction(&mut source, &reference);
    let unsigned = source.clone();
    let known = BTreeMap::from([("task".into(), BTreeMap::from([(1, reference.clone())]))]);
    let foreign = SigningKey::from_bytes(&[77; 32]);
    assert!(
        source
            .attest_native_task_private_directions(&foreign, &known)
            .is_err()
    );
    source
        .attest_native_task_private_directions(&key, &known)
        .unwrap();
    let signed = source.clone();
    let before = source.durable().unwrap();
    source
        .attest_native_task_private_directions(&key, &known)
        .unwrap();
    assert_eq!(source.durable().unwrap(), before);
    let mut recovered = BTreeMap::new();
    source
        .merge_native_task_private_direction_references(&mut recovered)
        .unwrap();
    assert_eq!(recovered, known);
    let (text, _) = source
        .extract_native_task_private_baseline(&key, now, &BTreeMap::new())
        .unwrap();
    assert!(
        text.get("task")
            .is_none_or(|text| text.directions.is_empty())
    );

    // Identical spelling without custody evidence is still private plaintext.
    let mut unsigned = unsigned;
    let (text, _) = unsigned
        .extract_native_task_private_baseline(&key, now, &BTreeMap::new())
        .unwrap();
    assert_eq!(text["task"].directions[&1], reference);

    for changed in ["signature", "task", "revision", "reference", "chain"] {
        let mut doc = signed.core.room_doc.as_deref().unwrap().clone();
        let map = doc.get(ROOT, FIELD).unwrap().unwrap().1;
        let id = serde_json::to_string(&("task", 1_u64)).unwrap();
        let (Value::Scalar(value), _) = doc.get(&map, &id).unwrap().unwrap() else {
            panic!("custody bytes absent")
        };
        let ScalarValue::Bytes(bytes) = value.as_ref() else {
            panic!("custody is not bytes")
        };
        let mut proof: DirectionCustody = postcard::from_bytes(bytes).unwrap();
        match changed {
            "signature" => proof.signature[0] ^= 1,
            "task" => proof.task.push('x'),
            "revision" => proof.revision += 1,
            "reference" => proof.reference.push('x'),
            _ => {
                let mut binding = sanitized_baseline::binding(&doc).unwrap().unwrap();
                binding.generation += 1;
                doc.put(
                    ROOT,
                    sanitized_baseline::FIELD,
                    postcard::to_allocvec(&binding).unwrap(),
                )
                .unwrap();
            }
        }
        doc.put(&map, &id, postcard::to_allocvec(&proof).unwrap())
            .unwrap();
        doc.commit();
        let mut forged = signed.clone();
        forged.core.room_doc = Some(doc.into());
        let (text, _) = forged
            .extract_native_task_private_baseline(&key, now, &BTreeMap::new())
            .unwrap();
        assert_eq!(text["task"].directions[&1], reference, "{changed}");
    }
}
