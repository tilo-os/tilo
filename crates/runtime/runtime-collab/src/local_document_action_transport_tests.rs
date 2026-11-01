use super::*;
use crate::wire::{Handle, HostFrame, Operation, Reply};

const HOST: [u8; 16] = [2; 16];
const ID: [u8; 16] = [1; 16];
const UPDATE: &str = r#"{"state":"accepted","invocation":"01010101-0101-0101-0101-010101010101"}"#;

/// How the host under test answers an admission.
#[derive(Clone, Copy)]
struct Host {
    wrong_id: bool,
    refuse: bool,
}

impl Host {
    fn answer(self) -> Result<Reply, String> {
        if self.refuse {
            return Err(if self.wrong_id {
                "admission journal outcome unknown".to_owned()
            } else {
                format!("{LOCAL_DOCUMENT_REJECTED}local document policy refused")
            });
        }
        Ok(Reply::Admitted(PublishedInvocationObservation {
            host: [9; 16],
            invocation: if self.wrong_id { [8; 16] } else { ID },
            result: Some(PublishedInvocationResult {
                id: 17,
                update_json: UPDATE.into(),
                admission_json: Some(r#"{"private":"admission"}"#.into()),
                terminal_json: None,
                uncertain: false,
                cancellation_requested: false,
            }),
        }))
    }
}

/// Admit `report` on a channel whose host answers as `host` says, returning
/// the outcome and every report the host received.
async fn admit(
    host: Host,
    report: &LocalDocumentActionReport,
) -> (
    Result<PublishedInvocationObservation, LocalDocumentActionError>,
    Vec<Vec<u8>>,
) {
    let (client, mut far) = crate::client::testing::connected();
    let channel = client.channel(Handle(1));
    let served = async {
        let mut received = Vec::new();
        loop {
            let call = far.call().await;
            match call.request {
                Operation::Admit { report, .. } => {
                    received.push(report);
                    far.send(&HostFrame::Reply {
                        id: call.id,
                        result: host.answer(),
                    })
                    .await;
                }
                Operation::Close { .. } => return received,
                other => unreachable!("unexpected {other:?}"),
            }
        }
    };
    let asked = async {
        let outcome = channel.admit_local_document_action(report).await;
        drop(channel);
        outcome
    };
    let (outcome, received) = tilo_test_env::within("the admission under test", async {
        tokio::join!(asked, served)
    })
    .await;
    (outcome, received)
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

#[tokio::test(flavor = "current_thread")]
async fn exact_report_uses_existing_session_and_preserves_private_observation() {
    let captured = report();
    let (observed, received) = admit(
        Host {
            wrong_id: false,
            refuse: false,
        },
        &captured,
    )
    .await;
    let result = observed.unwrap().result.unwrap();
    assert_eq!(
        result.admission_json.as_deref(),
        Some(r#"{"private":"admission"}"#)
    );
    assert!(result.terminal_json.is_none());
    assert_eq!(received, vec![captured.encode().unwrap()]);
}

#[tokio::test(flavor = "current_thread")]
async fn refusal_or_wrong_invocation_never_retries_or_falls_back() {
    for (wrong_id, refuse) in [(true, false), (false, true), (true, true)] {
        let (error, received) = admit(Host { wrong_id, refuse }, &report()).await;
        let error = error.unwrap_err();
        if refuse && !wrong_id {
            assert!(matches!(error, LocalDocumentActionError::Rejected(_)));
        } else {
            assert!(matches!(error, LocalDocumentActionError::Uncertain(_)));
        }
        assert_eq!(received.len(), 1);
    }
}

#[test]
fn reliable_invocation_evidence_preserves_existing_outcome_budget_and_redacts_debug() {
    let mut outcome = UPDATE.to_owned();
    outcome.push_str(&" ".repeat(256 * 1024 - outcome.len()));
    let mut observation = PublishedInvocationObservation {
        host: HOST,
        invocation: ID,
        result: Some(PublishedInvocationResult {
            id: 1,
            update_json: outcome,
            admission_json: Some("n".repeat(32 * 1024)),
            terminal_json: Some("t".repeat(32 * 1024)),
            uncertain: false,
            cancellation_requested: false,
        }),
    };
    let checked = checked_observation(observation.clone(), &report()).unwrap();
    let result = checked.result.unwrap();
    assert_eq!(result.update_json.len(), 256 * 1024);
    assert_eq!(result.admission_json.as_ref().unwrap().len(), 32 * 1024);
    assert!(!format!("{result:?}").contains("nnnn"));
    assert!(!format!("{result:?}").contains("tttt"));
    if let Some(result) = &mut observation.result {
        result.admission_json = Some("n".repeat(32 * 1024 + 1));
    }
    assert!(checked_observation(observation, &report()).is_err());
}
