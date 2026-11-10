//! The refusals a host sends, as they travel: `host_refusals.json` holds
//! each `ActionHostError` as JSON carries it, with its code and the words it
//! says, and every caller's reading of them (`tilo-session-host`'s `HostRefusal`) is
//! tested against the same file.

use serde_json::Value;
use tilo_actions::remote::ActionHostError;
use tilo_actions::remote::host_account::{NextStep, NextStepKind, Reason, Refusal};

/// One refusal of each kind, in the file's order.
fn refusals() -> Vec<ActionHostError> {
    let second_own = Refusal {
        reason: Reason::TooManyWorkspaces {
            limit: 1,
            existing: vec!["6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71".to_owned()],
        },
        message: "You already have a workspace of your own on this host; use that one.".to_owned(),
        next: vec![NextStep {
            kind: NextStepKind::UseExisting,
            url: None,
        }],
    };
    vec![
        ActionHostError::Unavailable {
            reason: "no live apps".to_owned(),
        },
        ActionHostError::UnknownAction,
        ActionHostError::NotPermitted,
        ActionHostError::ConfirmationRequired,
        ActionHostError::InvalidRequest {
            reason: "`name` is text".to_owned(),
        },
        ActionHostError::StaleRevision,
        ActionHostError::UnknownInvocation,
        ActionHostError::InvocationConflict,
        ActionHostError::NotCancellable {
            reason: "it finished".to_owned(),
        },
        ActionHostError::Refused {
            reason: "device revoked".to_owned(),
        },
        ActionHostError::Refused {
            reason: second_own.to_reason(),
        },
    ]
}

#[test]
fn every_refusal_travels_as_the_file_says() {
    let actual: Vec<Value> = refusals()
        .iter()
        .map(|error| {
            // What the receiving side decodes it as: a stale revision reads
            // back as the invalid request it travels as.
            let wire = serde_json::to_value(error).unwrap();
            let read: ActionHostError = serde_json::from_value(wire.clone()).unwrap();
            serde_json::json!({
                "wire": wire,
                "code": read.code(),
                "display": read.to_string(),
            })
        })
        .collect();
    let file: Vec<Value> = serde_json::from_str(include_str!("host_refusals.json")).unwrap();
    assert_eq!(
        actual,
        file,
        "host_refusals.json no longer says how refusals travel; it should read:\n{}",
        serde_json::to_string_pretty(&actual).unwrap()
    );
}
