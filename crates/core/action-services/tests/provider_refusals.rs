//! The refusals a provider sends, as they travel: `provider_refusals.json` holds
//! each `ActionCallError` as JSON carries it, with its code and the words it
//! says, and every caller's reading of them (`tilo-session-client`'s `DeviceRefusal`) is
//! tested against the same file.

use serde_json::Value;
use tilo_action_services::server_account::{NextStep, NextStepKind, Reason, Refusal};
use tilo_actions::remote::ActionCallError;

/// One refusal of each kind, in the file's order — and one that states what
/// waits for the person, which only a page action's own run reaches.
fn refusals() -> Vec<ActionCallError> {
    let second_own = Refusal {
        reason: Reason::TooManyWorkspaces {
            limit: 1,
            existing: vec!["6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71".to_owned()],
        },
        message: "You already have a workspace of your own on this server; use that one."
            .to_owned(),
        next: vec![NextStep {
            kind: NextStepKind::UseExisting,
            url: None,
        }],
    };
    vec![
        ActionCallError::Unavailable {
            reason: "no live apps".to_owned(),
        },
        ActionCallError::UnknownAction,
        ActionCallError::NotPermitted,
        ActionCallError::confirmation_required(),
        ActionCallError::asks(tilo_action_services::browser::departure_asked(vec![
            ("kind".to_owned(), "submit".to_owned()),
            ("picture".to_owned(), "tilo.content:5f2b".to_owned()),
        ])),
        ActionCallError::InvalidRequest {
            reason: "`name` is text".to_owned(),
        },
        ActionCallError::StaleRevision,
        ActionCallError::UnknownInvocation,
        ActionCallError::InvocationConflict,
        ActionCallError::NotCancellable {
            reason: "it finished".to_owned(),
        },
        ActionCallError::Refused {
            reason: "device revoked".to_owned(),
        },
        ActionCallError::Refused {
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
            let read: ActionCallError = serde_json::from_value(wire.clone()).unwrap();
            serde_json::json!({
                "wire": wire,
                "code": read.code(),
                "display": read.to_string(),
            })
        })
        .collect();
    let file: Vec<Value> = serde_json::from_str(include_str!("provider_refusals.json")).unwrap();
    assert_eq!(
        actual,
        file,
        "provider_refusals.json no longer says how refusals travel; it should read:\n{}",
        serde_json::to_string_pretty(&actual).unwrap()
    );
}
