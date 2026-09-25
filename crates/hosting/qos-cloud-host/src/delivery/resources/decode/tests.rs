use super::super::tests::{precharged, work};
use super::*;
use crate::delivery::resources::CodecBudget;
fn limits() -> DecodeLimits {
    DecodeLimits {
        maximum_bytes: 64 * 1024 * 1024,
        allocation_bytes: 8 * 1024 * 1024,
        collection_entries: 1024 * 1024,
    }
}
#[test]
fn cloud_aggregate_decode_top_level_null_matches_store_refusal() {
    for original in [
        include_bytes!("../../fixtures/v4.json").as_slice(),
        include_bytes!("../../fixtures/v5.json").as_slice(),
    ] {
        for field in ["delivery", "private_agent_ledger"] {
            let mut value: serde_json::Value = serde_json::from_slice(original).unwrap();
            value[field] = serde_json::Value::Null;
            let bytes = serde_json::to_vec(&value).unwrap();
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("state.json");
            std::fs::write(&path, &bytes).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            assert!(crate::Store::open(dir.path(), "delivery-literal").is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            let (allowance, turn) = work(64 * 1024 * 1024);
            let input = AggregateBytes::copy_from(&bytes, bytes.len(), &CodecBudget::native(&turn))
                .unwrap();
            let before = allowance.available_bytes();
            assert!(
                precharged(&allowance, || HeldCloudState::decode(
                    &input,
                    limits(),
                    &CodecBudget::native(&turn)
                ))
                .is_err(),
                "held reader must preserve Store's explicit-null refusal for {field}"
            );
            assert_eq!(allowance.available_bytes(), before);
            assert_eq!(input.as_bytes(), bytes);
        }
    }
}
#[test]
fn cloud_aggregate_decode_literal_shapes_actual_heap_and_retained_lifetime() {
    for bytes in [
        include_bytes!("../../fixtures/v4.json").as_slice(),
        include_bytes!("../../fixtures/v5.json").as_slice(),
    ] {
        let expected: State = serde_json::from_slice(bytes).unwrap();
        let expected = serde_json::to_vec(&expected).unwrap();
        let (allowance, turn) = work(64 * 1024 * 1024);
        let input =
            AggregateBytes::copy_from(bytes, bytes.len(), &CodecBudget::native(&turn)).unwrap();
        let before = allowance.available_bytes();
        let held = precharged(&allowance, || {
            HeldCloudState::decode(&input, limits(), &CodecBudget::native(&turn))
        })
        .unwrap();
        assert_eq!(serde_json::to_vec(held.state()).unwrap(), expected);
        assert!(allowance.available_bytes() < before);
        drop(held);
        assert_eq!(allowance.available_bytes(), before);
    }
}
#[test]
fn cloud_aggregate_decode_finite_values_and_paid_fork_preserve_source() {
    let mut state: State =
        serde_json::from_slice(include_bytes!("../../fixtures/v4.json")).unwrap();
    state.consumed.insert("unicode".into(),("retained".into(),serde_json::json!({"escaped":"é\\\"\n".repeat(300),"numbers":[1e100,f64::MAX,f64::MIN_POSITIVE,f64::from_bits(1),-0.0],"nested":[{"a":[1,2,3]}]})));
    let bytes = serde_json::to_vec(&state).unwrap();
    let (allowance, turn) = work(64 * 1024 * 1024);
    let input =
        AggregateBytes::copy_from(&bytes, bytes.len(), &CodecBudget::native(&turn)).unwrap();
    let held = precharged(&allowance, || {
        HeldCloudState::decode(&input, limits(), &CodecBudget::native(&turn))
    })
    .unwrap();
    let candidate = precharged(&allowance, || {
        held.fork(limits(), &CodecBudget::native(&turn))
    })
    .unwrap();
    assert_eq!(serde_json::to_vec(held.state()).unwrap(), bytes);
    assert_eq!(serde_json::to_vec(candidate.state()).unwrap(), bytes);
}
#[test]
fn cloud_aggregate_decode_real_private_delivery_and_nested_custody_fork() {
    let (_dir, mut store, device) = crate::agent_owner_tests::enrolled();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_ledger(&caller).unwrap();
    let (caller, request) = crate::agent_owner_tests::prepare(&store, device, "retained-decode");
    store.commit_agent_request(&request, &caller).unwrap();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    let bytes = serde_json::to_vec(&store.state).unwrap();
    let (allowance, turn) = work(64 * 1024 * 1024);
    let input =
        AggregateBytes::copy_from(&bytes, bytes.len(), &CodecBudget::native(&turn)).unwrap();
    let held = precharged(&allowance, || {
        HeldCloudState::decode(&input, limits(), &CodecBudget::native(&turn))
    })
    .unwrap();
    let candidate = precharged(&allowance, || {
        held.fork(limits(), &CodecBudget::native(&turn))
    })
    .unwrap();
    assert_eq!(serde_json::to_vec(candidate.state()).unwrap(), bytes);
    drop(held);
    assert_eq!(serde_json::to_vec(candidate.state()).unwrap(), bytes);
}

#[test]
fn cloud_aggregate_decode_long_decimal_and_underflow_keep_legacy_finite_semantics() {
    let template = std::str::from_utf8(include_bytes!("../../fixtures/v4.json")).unwrap();
    for number in [
        format!("1.{}1","0".repeat(1200)),
        format!("0.{}1e5000","0".repeat(5000)),
        "1e-999999999999999999999999999".into(),
        "2.4703282292062327208828439643411068618252990130716238221279284125033775363510437593264991818081796158768127951940541693547781484267986830054354712040243973096301363028726030307908276730956415918674556085523301311563843729762958355479016887311834499496540113249712307005434782566407750716029436292702710846751139784184149183081504367804514161345565668683499749278288016541115389618815988122331304972909278408005244817624195855164455561116882018093598582522538164006572907627015625e-324".into(),
    ] {
        let bytes=template.replace("\"consumed\":{}",&format!("\"consumed\":{{\"number\":[\"retained\",{number}]}}"));
        assert_ne!(bytes,template);
        let expected:State=serde_json::from_str(&bytes).unwrap();
        let expected=serde_json::to_vec(&expected).unwrap();
        let (allowance,turn)=work(64*1024*1024);
        let input=AggregateBytes::copy_from(bytes.as_bytes(),bytes.len(),&CodecBudget::native(&turn)).unwrap();
        let held=precharged(&allowance,||HeldCloudState::decode(&input,limits(),&CodecBudget::native(&turn))).unwrap();
        assert_eq!(serde_json::to_vec(held.state()).unwrap(),expected);
    }
}
#[test]
fn cloud_aggregate_decode_pressure_and_errors_release_all_temporary_credit() {
    let bytes = include_bytes!("../../fixtures/v4.json");
    let (allowance, turn) = work(64 * 1024 * 1024);
    let input = AggregateBytes::copy_from(bytes, bytes.len(), &CodecBudget::native(&turn)).unwrap();
    let before = allowance.available_bytes();
    for policy in [
        DecodeLimits {
            allocation_bytes: 1,
            ..limits()
        },
        DecodeLimits {
            collection_entries: 4,
            ..limits()
        },
        DecodeLimits {
            maximum_bytes: bytes.len() - 1,
            ..limits()
        },
    ] {
        assert!(matches!(
            precharged(&allowance, || HeldCloudState::decode(
                &input,
                policy,
                &CodecBudget::native(&turn)
            )),
            Err(ResourceError::Capacity)
        ));
        assert_eq!(allowance.available_bytes(), before);
    }
    let occupied = turn.reserve(before).unwrap();
    assert!(matches!(
        precharged(&allowance, || HeldCloudState::decode(
            &input,
            limits(),
            &CodecBudget::native(&turn)
        )),
        Err(ResourceError::Capacity)
    ));
    drop(occupied);
    assert_eq!(allowance.available_bytes(), before);
    for bad in [
        b"{\"machine_id\":\"escape\\u2603\",\"version\":4,\"unknown\":[]}".as_slice(),
        b"{\"version\":4,\"version\":4}",
        b"{\"version\":4,\"machine_id\":\"unterminated",
    ] {
        let input = AggregateBytes::copy_from(bad, bad.len(), &CodecBudget::native(&turn)).unwrap();
        let before = allowance.available_bytes();
        assert!(
            precharged(&allowance, || HeldCloudState::decode(
                &input,
                limits(),
                &CodecBudget::native(&turn)
            ))
            .is_err()
        );
        assert_eq!(allowance.available_bytes(), before);
    }
}
#[test]
fn cloud_aggregate_decode_inner_two_requires_exact_baseline_cut_and_refuses_one() {
    let (_dir, mut store, _) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    let original = serde_json::to_value(&store.state).unwrap();
    let mut cases = Vec::new();
    let mut one = original.clone();
    one["delivery"]["version"] = 1.into();
    cases.push(one);
    let mut missing = original.clone();
    missing["delivery"]
        .as_object_mut()
        .unwrap()
        .remove("baseline_generation");
    cases.push(missing);
    for generation in [0, u64::MAX] {
        let mut invalid = original.clone();
        invalid["delivery"]["baseline_generation"] = generation.into();
        cases.push(invalid);
    }
    for bad in cases {
        let bytes = serde_json::to_vec(&bad).unwrap();
        let (allowance, turn) = work(64 * 1024 * 1024);
        let input =
            AggregateBytes::copy_from(&bytes, bytes.len(), &CodecBudget::native(&turn)).unwrap();
        let before = allowance.available_bytes();
        assert!(
            precharged(&allowance, || HeldCloudState::decode(
                &input,
                limits(),
                &CodecBudget::native(&turn)
            ))
            .is_err()
        );
        assert_eq!(input.as_bytes(), bytes);
        assert_eq!(allowance.available_bytes(), before);
    }
}

#[test]
fn cloud_aggregate_decode_numeric_parser_credit_is_independent_of_field_arena() {
    for number in [
        format!("1.{}1", "0".repeat(1200)),
        format!("0.{}1e5000", "0".repeat(5000)),
        format!("1.{}", "23456789".repeat(160)),
        "2.2250738585072011e-308".into(),
        "4.9406564584124654e-324".into(),
        "0e9999999999999999999999999999".into(),
    ] {
        let expected: f64 = serde_json::from_str(&number).unwrap();
        let (allowance, turn) = work(4 * 1024 * 1024);
        let before = allowance.available_bytes();
        let actual = precharged(&allowance, || {
            let _parser = reserve(
                &CodecBudget::native(&turn),
                parser::bytes(number.as_bytes())?,
            )?;
            serde_json::from_str::<f64>(&number).map_err(|_| ResourceError::Malformed)
        })
        .unwrap();
        assert_eq!(actual.to_bits(), expected.to_bits());
        assert_eq!(allowance.available_bytes(), before);
    }
}

#[test]
fn cloud_aggregate_decode_ordinary_maps_boxed_operations_and_publication_bytes_are_paid() {
    use crate::NativeOperation;
    use qos_action_broker::collaboration_admin::*;
    let mut state: State =
        serde_json::from_slice(include_bytes!("../../fixtures/v4.json")).unwrap();
    let mut remote = NativeOperation::ChatSend {
        channel: "chat".into(),
        text: "retained chat text".repeat(128),
    };
    for _ in 0..12 {
        remote = NativeOperation::Remote {
            link_id: "link".into(),
            target_machine_id: "target".into(),
            operation: Box::new(remote),
        };
    }
    let allowed = MediaDirectionsInput {
        publish: true,
        subscribe: false,
    };
    let call = MediaCallInput {
        call_id: "call".into(),
        expected_authorization_revision: 3,
        expected_epoch: 2,
    };
    let operations = [
        remote,
        NativeOperation::MediaCreate(MediaCreateInput {
            app_id: "app".into(),
            record_id: "record".into(),
            channel_id: "channel".into(),
            expected_authorization_revision: None,
            allowed,
        }),
        NativeOperation::MediaEnd(call.clone()),
        NativeOperation::MediaRestart(call),
        NativeOperation::MediaSetDirections(MediaDirectionsChangeInput {
            call_id: "call".into(),
            expected_authorization_revision: 3,
            expected_epoch: 2,
            allowed,
        }),
        NativeOperation::MediaSetParticipantDenial(MediaDenialInput {
            call_id: "call".into(),
            expected_authorization_revision: 3,
            expected_epoch: 2,
            person_root: "person".into(),
            denied: allowed,
        }),
    ];
    for (i, operation) in operations.into_iter().enumerate() {
        state.canonical_intents.insert(
            i.to_string(),
            serde_json::from_value(serde_json::json!({"digest":vec![0;32],"operation":operation}))
                .unwrap(),
        );
    }
    state.mesh=serde_json::from_value(serde_json::json!({"challenge":null,"links":{},"consumed":{"key":["digest",{"payload":[1,2,{"string":"mesh"}]}]},"pending_scopes":{"invocation":"scope"}})).unwrap();
    state.pending_models.insert(
        "pending".into(),
        crate::model::PendingModel {
            digest: "digest".into(),
            gateway_id: "gateway".into(),
        },
    );
    state.receipts.insert(
        "receipt".into(),
        (
            "digest".into(),
            ActionReceipt {
                invocation_id: "invocation".into(),
                status: "accepted".into(),
                error: None,
                output: Some(serde_json::json!({"text":"preserved","values":[1e100,3.0]})),
            },
        ),
    );
    state.invitations.insert(
        "invitation".into(),
        crate::collaboration::StoredInvitation {
            signed_invitation: vec![42; 256],
            role: qos_peer_state::join::MemberRole::Editor,
            invitee_name: Some("person".into()),
            expires_at_ns: 19,
            status: crate::collaboration::InvitationStatus::Pending,
        },
    );
    // Opaque storage reconstruction does not certify this publication or dispatch
    // any retained operation; it must nevertheless pay actual body allocations.
    state.outbox.push(StoredPublication {
        message: qos_peer_state::NeighbourMsg::SurfaceFrame {
            surface: 1,
            source_peer: 2,
            encoded_h264: vec![42; 128 * 1024],
            ts: 3,
        },
        kind: crate::PublicationKind::Presence,
    });
    let bytes = serde_json::to_vec(&state).unwrap();
    let (allowance, turn) = work(64 * 1024 * 1024);
    let input =
        AggregateBytes::copy_from(&bytes, bytes.len(), &CodecBudget::native(&turn)).unwrap();
    let held = precharged(&allowance, || {
        HeldCloudState::decode(&input, limits(), &CodecBudget::native(&turn))
    })
    .unwrap();
    let candidate = precharged(&allowance, || {
        held.fork(limits(), &CodecBudget::native(&turn))
    })
    .unwrap();
    assert_eq!(serde_json::to_vec(held.state()).unwrap(), bytes);
    assert_eq!(serde_json::to_vec(candidate.state()).unwrap(), bytes);
}
