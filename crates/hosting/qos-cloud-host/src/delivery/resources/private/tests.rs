use super::*;
use crate::delivery::resources::CodecBudget;

fn actual_v1() -> PrivateAgentState {
    let (_dir, mut store, device) = crate::agent_owner_tests::enrolled();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_ledger(&caller).unwrap();
    let (caller, request) = crate::agent_owner_tests::prepare(&store, device, "retained");
    store.commit_agent_request(&request, &caller).unwrap();
    store.state.private_agent_ledger.take().unwrap()
}
#[test]
fn cloud_aggregate_decode_private_actual_admission_bytes_and_nested_required_fields() {
    let expected = actual_v1();
    let bytes = serde_json::to_string(&expected).unwrap();
    let value = serde_json::from_str::<PrivateWire>(&bytes).unwrap().0;
    assert_eq!(value, expected);
    assert_eq!(serde_json::to_string(&value).unwrap(), bytes);
    for changed in [
        bytes.replace("\"error\":null,", ""),
        bytes.replace(",\"error\":null", ""),
        bytes.replace("\"expires_at_ns\":null,", ""),
        bytes.replacen("\"records\":", "\"records\":{},\"records\":", 1),
        bytes.replacen("\"mutations\":", "\"unknown\":null,\"mutations\":", 1),
    ] {
        if changed == bytes {
            continue;
        }
        assert!(
            serde_json::from_str::<PrivateAgentState>(&changed).is_err(),
            "baseline exact private reader rejects this shape"
        );
        assert!(serde_json::from_str::<PrivateWire>(&changed).is_err());
    }
}
#[test]
fn cloud_aggregate_decode_private_literal_versions_and_absent_vs_null() {
    let v1 = r#"{"version":1,"records":{},"mutations":{}}"#;
    let v2 = r#"{"version":2,"writer_floor":2,"legacy_v1":{"version":1,"records":{},"mutations":{}},"facts":{}}"#;
    for bytes in [v1, v2] {
        let expected: PrivateAgentState = serde_json::from_str(bytes).unwrap();
        assert_eq!(
            serde_json::from_str::<PrivateWire>(bytes).unwrap().0,
            expected
        );
    }
    for bytes in [
        v1.replace("\"version\":1", "\"version\":1,\"facts\":null"),
        v2.replace("\"writer_floor\":2,", ""),
        v2.replace("\"facts\":{}", "\"facts\":{},\"facts\":{}"),
        v2.replace("\"records\":{}", "\"records\":{},\"extra\":null"),
    ] {
        assert!(serde_json::from_str::<PrivateAgentState>(&bytes).is_err());
        assert!(serde_json::from_str::<PrivateWire>(&bytes).is_err());
    }
}

#[test]
fn cloud_aggregate_decode_private_all_fact_variants_and_exact_nested_shape() {
    use serde_json::json;
    let native = actual_v1();
    let caller = native
        .legacy()
        .records
        .values()
        .next()
        .unwrap()
        .caller
        .clone();
    let binding = json!({"room":"room","app":"app","agent":"agent","task":"task","run":"run","launch":"launch","generation":3});
    let runtime = json!({"runtime_epoch":"epoch","package_id":"package","package_hash":"hash","executable_hash":"hash","runtime_hash":"hash"});
    let process = json!({"pid":1,"start":2});
    let acceptance = json!({"version":1,"message_id":"message","task_id":"task","run_id":"run","launch_id":"launch","runtime_epoch":"epoch","recipient_agent":"agent","channel_lease":"lease","acceptance_sequence":1});
    let error = json!({"code":"outcome-uncertain","message":"retained private failure"});
    let admitted = json!({"kind":"admitted","ids":{"agent_id":"agent","task_id":"task","run_id":"run","launch_id":"launch","message_id":"message"},"intent":{"scope":{"machine":"machine","room":"room","host":"host","caller":caller},"route":{"app_id":"app","app_peer":"1","cluster_id":"room","record_id":"record","resource":"resource","resource_kind":"kind","action":"spawn"},"descriptor_revision":"revision","request":"retained private request","grants":[]},"parent_invocation":null,"reservation":{"control_bytes":12,"task_bytes":13,"process_bytes":14,"fact_slots":11},"timestamp_ms":1});
    let variants = vec![
        admitted.clone(),
        json!({"kind":"launch_intended","binding":binding,"runtime":runtime}),
        json!({"kind":"process_bound","launch_fact":"launch","process":{"binding":binding,"runtime":runtime,"process":process,"supervisor":process,"channel_id":"channel"}}),
        json!({"kind":"dispatch_intended","process_fact":"process","envelope":"opaque exact envelope"}),
        json!({"kind":"completed","dispatch_fact":"dispatch","acceptance":acceptance,"timestamp_ms":1}),
        json!({"kind":"failed","error":error,"timestamp_ms":1}),
        json!({"kind":"unknown","error":error,"timestamp_ms":1}),
        json!({"kind":"task_unknown","error":error,"timestamp_ms":1}),
        json!({"kind":"task_finished","completed_fact":"completed","acceptance_sequence":1,"update_sequence":2,"outcome":"succeeded","text":"retained result","timestamp_ms":1}),
        json!({"kind":"process_exited","launch_fact":"launch","exit":{"runtime":runtime,"binding":binding,"supervisor":process,"exit_code":null,"signal":null},"timestamp_ms":1}),
    ];
    let wrap = |fact| json!({"version":2,"writer_floor":2,"legacy_v1":{"version":1,"records":{},"mutations":{}},"facts":{"fact":{"binding":{"room":"room","target":{"AgentRuntime":{"mutation":"mutation"}},"request_digest":vec![0;32],"basis":vec![0;32]},"generation":2,"change":{"mutation":"mutation","invocation":"invocation","expected_generation":1,"expected_governance":"governance","payload_digest":vec![0;32],"fact":fact}}}});
    for fact in variants {
        let bytes = serde_json::to_vec(&wrap(fact)).unwrap();
        let expected: PrivateAgentState = serde_json::from_slice(&bytes).unwrap();
        let actual = serde_json::from_slice::<PrivateWire>(&bytes).unwrap().0;
        assert_eq!(actual, expected);
        // This is storage-shape/allocation evidence, not acceptance of these
        // inert fact labels by a native authority or private business owner.
        let mut state: crate::State =
            serde_json::from_slice(include_bytes!("../../fixtures/v5.json")).unwrap();
        state.private_agent_ledger = Some(actual.clone());
        let aggregate = serde_json::to_vec(&state).unwrap();
        let (allowance, turn) = super::super::tests::work(64 * 1024 * 1024);
        let input = super::super::AggregateBytes::copy_from(
            &aggregate,
            aggregate.len(),
            &CodecBudget::native(&turn),
        )
        .unwrap();
        let limits = super::super::decode::DecodeLimits {
            maximum_bytes: 64 * 1024 * 1024,
            allocation_bytes: 8 * 1024 * 1024,
            collection_entries: 1024 * 1024,
        };
        let held = super::super::tests::precharged(&allowance, || {
            super::super::decode::HeldCloudState::decode(
                &input,
                limits,
                &CodecBudget::native(&turn),
            )
        })
        .unwrap();
        assert_eq!(serde_json::to_vec(held.state()).unwrap(), aggregate);
        assert_eq!(
            serde_json::to_vec(&actual).unwrap(),
            serde_json::to_vec(&expected).unwrap()
        );
    }
    let mut no_parent = admitted.clone();
    no_parent
        .as_object_mut()
        .unwrap()
        .remove("parent_invocation");
    let mut no_expiry = admitted.clone();
    no_expiry["intent"]["scope"]["caller"]
        .as_object_mut()
        .unwrap()
        .remove("expires_at_ns");
    let mut foreign_null = admitted;
    foreign_null["text"] = serde_json::Value::Null;
    for invalid in [no_parent, no_expiry, foreign_null] {
        let bytes = serde_json::to_vec(&wrap(invalid)).unwrap();
        assert!(serde_json::from_slice::<PrivateAgentState>(&bytes).is_err());
        assert!(serde_json::from_slice::<PrivateWire>(&bytes).is_err());
    }
}

#[test]
fn cloud_aggregate_decode_private_duplicate_entries_and_uuid_spelling_are_not_normalized() {
    let native = actual_v1();
    let mut value = serde_json::to_value(&native).unwrap();
    let records = value["records"].as_object().unwrap();
    let (key, record) = records.iter().next().unwrap();
    let entry = format!(
        "{}:{}",
        serde_json::to_string(key).unwrap(),
        serde_json::to_string(record).unwrap()
    );
    let original = serde_json::to_string(&value).unwrap();
    let duplicate = original.replacen(
        &format!("\"records\":{{{entry}}}"),
        &format!("\"records\":{{{entry},{entry}}}"),
        1,
    );
    assert_ne!(duplicate, original);
    assert!(serde_json::from_str::<PrivateAgentState>(&duplicate).is_err());
    assert!(serde_json::from_str::<PrivateWire>(&duplicate).is_err());
    let receipt = value["mutations"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .next()
        .unwrap();
    receipt["binding"]["target"] =
        serde_json::json!({"EffectReceipt":{"invocation":"00000000-0000-0000-0000-abcdefabcdef"}});
    let canonical = serde_json::to_string(&value).unwrap();
    assert!(serde_json::from_str::<PrivateAgentState>(&canonical).is_ok());
    assert!(serde_json::from_str::<PrivateWire>(&canonical).is_ok());
    let upper = canonical.replace("abcdefabcdef", "ABCDEFABCDEF");
    assert!(serde_json::from_str::<PrivateAgentState>(&upper).is_err());
    assert!(serde_json::from_str::<PrivateWire>(&upper).is_err());
}
