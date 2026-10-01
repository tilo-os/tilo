#![allow(
    clippy::too_many_lines,
    reason = "each test spells out one end-to-end scenario"
)]
use super::*;
fn request(message: ActionMessage) -> DirectedAction {
    DirectedAction {
        room: Uuid::from_u128(1),
        request_id: Uuid::from_u128(2),
        source: [3; 32],
        target: [4; 32],
        message,
    }
}
fn scope() -> PrivateScope {
    PrivateScope {
        room: Uuid::from_u128(1),
        source: [3; 32],
        target: [4; 32],
    }
}
#[test]
fn private_wire_preserves_leaf_and_rejects_scope_direction_and_tail() {
    let id = tilo_actions::InvocationId(Uuid::from_u128(5));
    for (op, message) in [
        (
            PrivateOperation::Catalogue,
            ActionMessage::CatalogueRefresh { scope: None },
        ),
        (
            PrivateOperation::Status,
            ActionMessage::Status { invocation: id },
        ),
        (
            PrivateOperation::Cancel,
            ActionMessage::Cancel { invocation: id },
        ),
    ] {
        let request = request(message);
        let encoded = encode_request(op, scope(), &request).unwrap();
        let (kind, payload, tail) = framing::decode_frame_prefix(&encoded).unwrap();
        assert_eq!(kind, op.request_kind());
        assert!(tail.is_empty());
        assert_eq!(payload, postcard::to_stdvec(&request).unwrap());
        assert_eq!(decode_request(op, scope(), &encoded).unwrap(), request);
        let mut bad = encoded.clone();
        bad.push(0);
        assert!(decode_request(op, scope(), &bad).is_err());
        let mut wrong = scope();
        wrong.source[0] ^= 1;
        assert!(decode_request(op, wrong, &encoded).is_err());
        wrong = scope();
        wrong.target[0] ^= 1;
        assert!(decode_request(op, wrong, &encoded).is_err());
        wrong = scope();
        wrong.room = Uuid::from_u128(9);
        assert!(decode_request(op, wrong, &encoded).is_err());
        let other = if op == PrivateOperation::Status {
            PrivateOperation::Cancel
        } else {
            PrivateOperation::Status
        };
        assert!(decode_request(other, scope(), &encoded).is_err());
        let response = DirectedAction {
            room: request.room,
            request_id: request.request_id,
            source: request.target,
            target: request.source,
            message: ActionMessage::Rejected {
                code: "denied".into(),
            },
        };
        let reply = encode_response(op, &request, &response).unwrap();
        assert_eq!(decode_response(op, &request, &reply).unwrap(), response);
        assert!(decode_request(op, scope(), &reply).is_err());
        let mut wrong_request = request.clone();
        wrong_request.request_id = Uuid::from_u128(8);
        assert!(decode_response(op, &wrong_request, &reply).is_err());
    }
}
#[test]
fn private_wire_closed_routes_registry_and_preflight() {
    assert_eq!(
        [
            MessageType::CatalogueRequest as u16,
            MessageType::CatalogueResponse as u16,
            MessageType::AgentRead as u16,
            MessageType::AgentReadResult as u16
        ],
        [26, 27, 28, 29]
    );
    assert_eq!(
        [
            MessageType::Invoke as u16,
            MessageType::Status as u16,
            MessageType::Cancel as u16,
            MessageType::PrivateUpdate as u16,
            MessageType::PrivateHistoryQuery as u16,
            MessageType::PrivateHistoryPage as u16
        ],
        [20, 21, 22, 23, 24, 25]
    );
    for op in [
        PrivateOperation::Catalogue,
        PrivateOperation::Invoke,
        PrivateOperation::Status,
        PrivateOperation::Cancel,
    ] {
        assert_eq!(
            PrivateOperation::from_target("POST", op.path()).unwrap(),
            op
        );
        assert!(PrivateOperation::from_target("GET", op.path()).is_err());
        assert!(PrivateOperation::from_target("POST", &format!("{}?x=1", op.path())).is_err());
    }
    assert!(MessageType::try_from(37).is_err());
    assert!(
        decode_request(
            PrivateOperation::Status,
            scope(),
            &vec![0; framing::MAX_PAYLOAD_BYTES + framing::HEADER_BYTES + 1]
        )
        .is_err()
    );
    // Declared leaf length is hostile and truncated. A borrowed preflight must reject before owned JSON allocation.
    let request = request(ActionMessage::Status {
        invocation: tilo_actions::InvocationId(Uuid::from_u128(5)),
    });
    let mut payload = postcard::to_stdvec(&request).unwrap();
    payload.truncate(98);
    payload.extend_from_slice(&[255; 10]);
    let mut bytes = Vec::new();
    framing::append_frame(&mut bytes, MessageType::Status, &payload, 1024).unwrap();
    assert!(decode_request(PrivateOperation::Status, scope(), &bytes).is_err());
}

#[test]
fn private_wire_invoke_and_disposable_read_keep_original_identity() {
    let host = Uuid::from_u128(6);
    let id = tilo_actions::InvocationId(Uuid::from_u128(5));
    let invoke = request(ActionMessage::InvokeBound {
        host,
        invocation: id,
        app_peer: "1".into(),
        app_id: "so.tilo.chat".into(),
        record_id: String::new(),
        resource: "tilo-ui-bound:v1:exact".into(),
        action: "collapse".into(),
        argument: None,
        confirmation: None,
        revision: Some("1".into()),
    });
    let encoded = encode_request(PrivateOperation::Invoke, scope(), &invoke).unwrap();
    assert_eq!(
        decode_request(PrivateOperation::Invoke, scope(), &encoded).unwrap(),
        invoke
    );
    let observed = DirectedAction {
        room: invoke.room,
        request_id: invoke.request_id,
        source: invoke.target,
        target: invoke.source,
        message: ActionMessage::Observation {
            host,
            invocation: id,
            update: None,
            uncertain: true,
            cancellation_requested: false,
        },
    };
    let bytes = encode_response(PrivateOperation::Invoke, &invoke, &observed).unwrap();
    assert_eq!(
        decode_response(PrivateOperation::Invoke, &invoke, &bytes).unwrap(),
        observed
    );
    let mut wrong = invoke;
    if let ActionMessage::InvokeBound { invocation, .. } = &mut wrong.message {
        *invocation = tilo_actions::InvocationId(Uuid::from_u128(7));
    }
    assert!(decode_response(PrivateOperation::Invoke, &wrong, &bytes).is_err());
}
#[test]
fn private_wire_preflight_rejects_oversized_json_tree_before_owned_action() {
    #[derive(serde::Serialize)]
    struct Leaf<'a> {
        room: Uuid,
        request_id: Uuid,
        source: [u8; 32],
        target: [u8; 32],
        #[serde(with = "serde_bytes")]
        message: &'a [u8],
    }
    for json in [
        format!("{}0{}", "[".repeat(34), "]".repeat(34)),
        format!("[{}]", vec!["0"; 65537].join(",")),
    ] {
        let leaf = Leaf {
            room: Uuid::from_u128(1),
            request_id: Uuid::from_u128(2),
            source: [3; 32],
            target: [4; 32],
            message: json.as_bytes(),
        };
        let payload = postcard::to_stdvec(&leaf).unwrap();
        let mut bytes = Vec::new();
        framing::append_frame(
            &mut bytes,
            MessageType::Invoke,
            &payload,
            framing::MAX_PAYLOAD_BYTES + framing::HEADER_BYTES,
        )
        .unwrap();
        assert!(decode_request(PrivateOperation::Invoke, scope(), &bytes).is_err());
    }
}

#[test]
fn private_reply_rejects_wrong_host_and_nested_invocation_for_each_command() {
    let host = Uuid::from_u128(6);
    let id = tilo_actions::InvocationId(Uuid::from_u128(5));
    for (op, message) in [
        (
            PrivateOperation::Invoke,
            ActionMessage::InvokeBound {
                host,
                invocation: id,
                app_peer: "1".into(),
                app_id: "so.tilo.chat".into(),
                record_id: String::new(),
                resource: "r".into(),
                action: "collapse".into(),
                argument: None,
                confirmation: None,
                revision: None,
            },
        ),
        (
            PrivateOperation::Status,
            ActionMessage::Status { invocation: id },
        ),
        (
            PrivateOperation::Cancel,
            ActionMessage::Cancel { invocation: id },
        ),
    ] {
        let request = request(message);
        let response = DirectedAction {
            room: request.room,
            request_id: request.request_id,
            source: request.target,
            target: request.source,
            message: ActionMessage::Observation {
                host,
                invocation: id,
                update: Some(tilo_actions::InvocationUpdate::Accepted { invocation: id }),
                uncertain: false,
                cancellation_requested: false,
            },
        };
        let correct = encode_response(op, &request, &response).unwrap();
        assert_eq!(decode_response(op, &request, &correct).unwrap(), response);
        for wrong_host in [false, true] {
            if wrong_host && op != PrivateOperation::Invoke {
                continue;
            }
            let mut wrong = response.clone();
            if let ActionMessage::Observation { host, update, .. } = &mut wrong.message {
                if wrong_host {
                    *host = Uuid::from_u128(7);
                } else {
                    *update = Some(tilo_actions::InvocationUpdate::Accepted {
                        invocation: tilo_actions::InvocationId(Uuid::from_u128(8)),
                    });
                }
            }
            assert!(
                encode_response(op, &request, &wrong).is_err(),
                "{op:?} wrong_host={wrong_host}"
            );
            let payload = postcard::to_stdvec(&wrong).unwrap();
            let mut bytes = Vec::new();
            framing::append_frame(
                &mut bytes,
                op.response_kind(),
                &payload,
                framing::MAX_PAYLOAD_BYTES + framing::HEADER_BYTES,
            )
            .unwrap();
            assert!(
                decode_response(op, &request, &bytes).is_err(),
                "raw {op:?} wrong_host={wrong_host}"
            );
        }
    }
}

#[test]
fn private_cancel_ack_is_only_cancel_and_keeps_exact_invocation() {
    let id = tilo_actions::InvocationId(Uuid::from_u128(17));
    let cancel = request(ActionMessage::Cancel { invocation: id });
    let reply = DirectedAction {
        room: cancel.room,
        request_id: cancel.request_id,
        source: cancel.target,
        target: cancel.source,
        message: ActionMessage::CancellationRequested { invocation: id },
    };
    let bytes = encode_response(PrivateOperation::Cancel, &cancel, &reply).unwrap();
    assert_eq!(
        decode_response(PrivateOperation::Cancel, &cancel, &bytes).unwrap(),
        reply
    );
    for (op, message) in [
        (
            PrivateOperation::Status,
            ActionMessage::Status { invocation: id },
        ),
        (
            PrivateOperation::Invoke,
            ActionMessage::InvokeBound {
                host: Uuid::from_u128(19),
                invocation: id,
                app_peer: "1".into(),
                app_id: "so.tilo.chat".into(),
                record_id: String::new(),
                resource: "r".into(),
                action: "collapse".into(),
                argument: None,
                confirmation: None,
                revision: None,
            },
        ),
    ] {
        let original = request(message);
        assert!(encode_response(op, &original, &reply).is_err());
        assert!(decode_response(op, &original, &bytes).is_err());
    }
    let mut wrong = reply;
    wrong.message = ActionMessage::CancellationRequested {
        invocation: tilo_actions::InvocationId(Uuid::from_u128(18)),
    };
    assert!(encode_response(PrivateOperation::Cancel, &cancel, &wrong).is_err());
    let payload = postcard::to_stdvec(&wrong).unwrap();
    let mut raw = Vec::new();
    framing::append_frame(
        &mut raw,
        MessageType::PrivateUpdate,
        &payload,
        framing::MAX_PAYLOAD_BYTES + framing::HEADER_BYTES,
    )
    .unwrap();
    assert!(decode_response(PrivateOperation::Cancel, &cancel, &raw).is_err());
}

#[test]
fn private_event_decoder_is_one_bounded_inert_correlated_response_family() {
    let id = tilo_actions::InvocationId(Uuid::from_u128(51));
    let host = Uuid::from_u128(52);
    let original = request(ActionMessage::Status { invocation: id });
    let scope = response_scope(&original);
    let reply = DirectedAction {
        room: original.room,
        request_id: original.request_id,
        source: original.target,
        target: original.source,
        message: ActionMessage::Observation {
            host,
            invocation: id,
            update: Some(tilo_actions::InvocationUpdate::Accepted { invocation: id }),
            uncertain: false,
            cancellation_requested: false,
        },
    };
    let bytes = encode_response(PrivateOperation::Status, &original, &reply).unwrap();
    assert_eq!(decode_event(scope, host, &bytes).unwrap(), reply);
    assert!(decode_event(scope, Uuid::from_u128(53), &bytes).is_err());
    assert!(decode_event(scope, Uuid::nil(), &bytes).is_err());
    let mut wrong_scope = scope;
    wrong_scope.target = [7; 32];
    assert!(decode_event(wrong_scope, host, &bytes).is_err());
    for message in [
        ActionMessage::Observation {
            host: Uuid::from_u128(53),
            invocation: id,
            update: None,
            uncertain: false,
            cancellation_requested: false,
        },
        ActionMessage::Observation {
            host,
            invocation: id,
            update: Some(tilo_actions::InvocationUpdate::Accepted {
                invocation: tilo_actions::InvocationId(Uuid::from_u128(54)),
            }),
            uncertain: false,
            cancellation_requested: false,
        },
        ActionMessage::CancellationRequested { invocation: id },
        ActionMessage::Status { invocation: id },
    ] {
        let mut wrong = reply.clone();
        wrong.message = message;
        let raw = encode(MessageType::PrivateUpdate, &wrong).unwrap();
        assert!(decode_event(scope, host, &raw).is_err());
    }
    let mut rejected = reply.clone();
    rejected.message = ActionMessage::Rejected {
        code: "unavailable".into(),
    };
    let raw = encode(MessageType::PrivateUpdate, &rejected).unwrap();
    assert_eq!(decode_event(scope, host, &raw).unwrap(), rejected);
    for malformed in [
        bytes[..bytes.len() - 1].to_vec(),
        [bytes.as_slice(), bytes.as_slice()].concat(),
        vec![255; 64],
    ] {
        assert!(decode_event(scope, host, &malformed).is_err());
    }
    // No original request is supplied: outer correlation remains mandatory in
    // PendingActions and is not authority inferred by this inert decoder.
    let mut other = reply;
    other.request_id = Uuid::from_u128(60);
    let raw = encode(MessageType::PrivateUpdate, &other).unwrap();
    assert_eq!(
        decode_event(scope, host, &raw).unwrap().request_id,
        other.request_id
    );
}
/// A native host builds its catalogue with keys in insertion order
/// (`version` before `surfaces`); every reader must re-encode it unchanged.
#[test]
fn catalogue_response_keeps_insertion_ordered_keys_through_decode() {
    let request = request(ActionMessage::CatalogueRefresh { scope: None });
    let catalogue: serde_json::Value = serde_json::from_str(
        r#"{"version":1,"surfaces":[{"peer_id":1,"app_id":"so.tilo.desktop","cluster_id":"c","record_id":"","live":true,"actions":[]}]}"#,
    )
    .unwrap();
    let response = DirectedAction {
        room: request.room,
        request_id: request.request_id,
        source: request.target,
        target: request.source,
        message: ActionMessage::BrokerCatalogue {
            host: Uuid::from_u128(7),
            catalogue,
        },
    };
    let reply = encode_response(PrivateOperation::Catalogue, &request, &response).unwrap();
    let decoded = decode_response(PrivateOperation::Catalogue, &request, &reply).unwrap();
    let ActionMessage::BrokerCatalogue { catalogue, .. } = &decoded.message else {
        panic!("catalogue expected");
    };
    assert_eq!(
        catalogue.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["version", "surfaces"]
    );
}
