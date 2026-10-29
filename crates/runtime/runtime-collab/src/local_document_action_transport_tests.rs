use super::*;
use std::cell::RefCell;

const HOST: [u8; 16] = [2; 16];
const ID: [u8; 16] = [1; 16];
const UPDATE: &str = r#"{"state":"accepted","invocation":"01010101-0101-0101-0101-010101010101"}"#;

/// Run `action` against `server` through a real Cap'n Proto connection.
async fn over_wire<S, F, Fut>(server: S, action: F)
where
    S: collab_capnp::channel_handle::Server + 'static,
    F: FnOnce(ChannelHandle) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    tokio::task::LocalSet::new()
        .run_until(async {
            let (server_io, client_io) = tokio::io::duplex(512);
            let raw: collab_capnp::channel_handle::Client = capnp_rpc::new_client(server);
            let task =
                tokio::task::spawn_local(runtime_portal::serve_connected_io(server_io, raw.client));
            let connection = runtime_portal::CapnpClient::from_connected_io(
                client_io,
                capnp_rpc::rpc_twoparty_capnp::Side::Client,
            );
            let channel = ChannelHandle {
                inner: Rc::new(connection.bootstrap()),
                descriptors: None,
            };
            tilo_test_env::within("the RPC under test", action(channel)).await;
            task.abort();
            let _ = task.await;
        })
        .await;
}

fn report() -> LocalDocumentActionReport {
    LocalDocumentActionReport {
        custody: None,
        version: 1,
        invocation: uuid::Uuid::from_bytes([1; 16]),
        capture_id: uuid::Uuid::from_bytes([4; 16]),
        revision: "captured-revision".into(),
        resource: "document".into(),
        action: "edit".into(),
        argument: Some("private text".into()),
        encoded: tilo_actions::EncodedAction {
            type_id: "private-action-type".into(),
            payload: br#"{"text":"private encoded payload"}"#.to_vec(),
        },
        context: Some(tilo_actions::ActionContext {
            purpose: Some("private purpose".into()),
            source_invocation: Some(tilo_actions::InvocationId(uuid::Uuid::from_bytes([7; 16]))),
            ..Default::default()
        }),
        original_heads: vec![[2; 32]],
        mutation_hashes: vec![[3; 32]],
        group: None,
    }
}

#[test]
fn closed_report_is_bounded_and_debug_does_not_disclose_payload() {
    let mut value = report();
    let json: serde_json::Value = serde_json::from_slice(&value.encode().unwrap()).unwrap();
    let mut fields = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    fields.compact_sort();
    assert_eq!(
        fields,
        vec![
            "action",
            "argument",
            "capture_id",
            "context",
            "custody",
            "encoded",
            "group",
            "invocation",
            "mutation_hashes",
            "original_heads",
            "resource",
            "revision",
            "version",
        ]
    );
    for forbidden in ["actor", "app", "record", "peer", "target"] {
        assert!(json.get(forbidden).is_none());
    }
    assert_eq!(json["invocation"], value.invocation.to_string());
    assert_eq!(json["capture_id"], value.capture_id.to_string());
    let mut missing_custody = json.clone();
    missing_custody.as_object_mut().unwrap().remove("custody");
    let legacy: LocalDocumentActionReport = serde_json::from_value(missing_custody).unwrap();
    assert!(legacy.custody.is_none());
    let mut missing_capture = json.clone();
    missing_capture
        .as_object_mut()
        .unwrap()
        .remove("capture_id");
    assert!(serde_json::from_value::<LocalDocumentActionReport>(missing_capture).is_err());
    let mut nil_capture = value.clone();
    nil_capture.capture_id = uuid::Uuid::nil();
    assert!(nil_capture.encode().is_err());
    let debug = format!("{value:?}");
    assert!(!debug.contains("private text"));
    assert!(!debug.contains("captured-revision"));
    assert!(!debug.contains("private-action-type"));
    assert!(!debug.contains("private encoded payload"));
    assert!(!debug.contains("private purpose"));
    assert!(!debug.contains("encoded"));
    assert!(!debug.contains("context"));
    let decoded: LocalDocumentActionReport = serde_json::from_value(json).unwrap();
    assert_eq!(decoded.encoded, value.encoded);
    assert_eq!(decoded.context, value.context);
    assert_eq!(decoded.argument, value.argument);
    assert_eq!(decoded.encode().unwrap(), value.encode().unwrap());
    value.mutation_hashes.push([3; 32]);
    assert!(value.encode().is_err());
    value.mutation_hashes.clear();
    value.original_heads.clear();
    assert!(value.encode().is_ok());
    value.mutation_hashes.push([3; 32]);
    value.argument = Some("x".repeat(64 * 1024 + 1));
    assert!(value.encode().is_err());
}

struct Server {
    received: Rc<RefCell<Vec<Vec<u8>>>>,
    wrong_id: bool,
    refuse: bool,
}
impl collab_capnp::channel_handle::Server for Server {
    fn admit_local_document_action(
        self: Rc<Self>,
        params: collab_capnp::channel_handle::AdmitLocalDocumentActionParams,
        mut results: collab_capnp::channel_handle::AdmitLocalDocumentActionResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        capnp::capability::Promise::from((|| {
            self.received
                .borrow_mut()
                .push(params.get()?.get_request()?.to_vec());
            if self.refuse {
                let reason = if self.wrong_id {
                    "admission journal outcome unknown".to_owned()
                } else {
                    format!("{LOCAL_DOCUMENT_REJECTED}local document policy refused")
                };
                return Err(capnp::Error::failed(reason));
            }
            let mut out = results.get().init_observation();
            out.set_host(&[9; 16]);
            out.set_invocation(if self.wrong_id { &[8; 16] } else { &[1; 16] });
            out.set_found(true);
            out.set_id(17);
            out.set_update_json(
                r#"{"state":"accepted","invocation":"01010101-0101-0101-0101-010101010101"}"#,
            );
            out.set_admission_json(r#"{"private":"admission"}"#);
            Ok(())
        })())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn exact_report_uses_existing_session_and_preserves_private_observation() {
    let received = Rc::new(RefCell::new(Vec::new()));
    let captured = report();
    let expected = captured.encode().unwrap();
    over_wire(
        Server {
            received: received.clone(),
            wrong_id: false,
            refuse: false,
        },
        |channel| async move {
            let observed = channel
                .admit_local_document_action(&captured)
                .await
                .unwrap();
            let result = observed.result.unwrap();
            assert_eq!(
                result.admission_json.as_deref(),
                Some(r#"{"private":"admission"}"#)
            );
            assert!(result.terminal_json.is_none());
        },
    )
    .await;
    assert_eq!(*received.borrow(), vec![expected]);
}

#[tokio::test(flavor = "current_thread")]
async fn refusal_or_wrong_invocation_never_retries_or_falls_back() {
    for (wrong_id, refuse) in [(true, false), (false, true), (true, true)] {
        let received = Rc::new(RefCell::new(Vec::new()));
        over_wire(
            Server {
                received: received.clone(),
                wrong_id,
                refuse,
            },
            |channel| async move {
                let error = channel
                    .admit_local_document_action(&report())
                    .await
                    .unwrap_err();
                if refuse && !wrong_id {
                    assert!(matches!(error, LocalDocumentActionError::Rejected(_)));
                } else {
                    assert!(matches!(error, LocalDocumentActionError::Uncertain(_)));
                }
            },
        )
        .await;
        assert_eq!(received.borrow().len(), 1);
    }
}

#[test]
fn reliable_invocation_evidence_preserves_existing_outcome_budget_and_redacts_debug() {
    let mut message = capnp::message::Builder::new_default();
    let mut outcome = UPDATE.to_owned();
    outcome.push_str(&" ".repeat(256 * 1024 - outcome.len()));
    {
        let mut out =
            message.init_root::<collab_capnp::published_invocation_observation::Builder<'_>>();
        out.set_host(&HOST);
        out.set_invocation(&ID);
        out.set_found(true);
        out.set_id(1);
        out.set_update_json(&outcome);
        out.set_admission_json("n".repeat(32 * 1024));
        out.set_terminal_json("t".repeat(32 * 1024));
    }
    let observation =
        reliable_decode_observation(message.get_root_as_reader().unwrap(), &HOST, &ID).unwrap();
    let result = observation.result.unwrap();
    assert_eq!(result.update_json.len(), 256 * 1024);
    assert_eq!(result.admission_json.as_ref().unwrap().len(), 32 * 1024);
    assert!(!format!("{result:?}").contains("nnnn"));
    assert!(!format!("{result:?}").contains("tttt"));
    message
        .get_root::<collab_capnp::published_invocation_observation::Builder<'_>>()
        .unwrap()
        .set_admission_json("n".repeat(32 * 1024 + 1));
    assert!(
        reliable_decode_observation(message.get_root_as_reader().unwrap(), &HOST, &ID).is_err()
    );
}
