use super::{GaveUp, Needs, sealed_for};
use std::collections::{BTreeSet, HashSet};
use tilo_peer_state::document_frame::DocumentRoute;

const WORKSPACE: &str = "6f1c1b62-4f5e-4b8a-9b52-1f8a3f0f3a11";
const OTHER_WORKSPACE: &str = "0b7f1d2e-5a6c-4d3b-8e9f-a0b1c2d3e4f5";

fn route(workspace: &str, app: &str, record: &str) -> DocumentRoute {
    DocumentRoute {
        app_id: app.into(),
        cluster_id: workspace.into(),
        record: record.into(),
    }
}

/// A failure the caller may ask again is not the invocation's answer; one
/// that is, is, and the same id keeps saying it.
#[test]
fn a_retryable_failure_leaves_the_id_free_to_run_again() {
    let failure = |retryable| super::ActionFailure {
        code: "commit_failed".into(),
        message: "the host did not save the change: the workspace refused the change: \
                  workspace membership changed"
            .into(),
        details: None,
        retryable,
    };
    assert!(
        !super::settles(&failure(true)),
        "asking again runs it again, over the session the workspace holds now"
    );
    assert!(
        super::settles(&failure(false)),
        "a failure of its own is its answer"
    );
}

#[test]
fn a_request_waits_only_for_the_keys_of_what_it_reads() {
    let workspace = tilo_relay_protocol::room_workspace_id(WORKSPACE).unwrap();
    let b = route(WORKSPACE, "so.tilo.sheets", "b");
    let elsewhere = route(OTHER_WORKSPACE, "so.tilo.sheets", "a");
    // `a` opened; `b` here and the record elsewhere wait for their keys.
    let routes = BTreeSet::from([b.clone(), elsewhere]);
    let act = |record| Needs::Act {
        app_id: "so.tilo.sheets",
        record,
    };
    let read = |record| Needs::Read {
        app_id: "so.tilo.sheets",
        record,
    };
    let fresh = GaveUp::default();

    assert!(
        sealed_for(&routes, workspace, act("a"), &fresh).is_empty(),
        "another record's missing keys never hold back this one"
    );
    assert!(sealed_for(&routes, workspace, read("a"), &fresh).is_empty());
    assert_eq!(
        sealed_for(&routes, workspace, act("b"), &fresh),
        std::slice::from_ref(&b)
    );
    assert_eq!(
        sealed_for(&routes, workspace, read("b"), &fresh),
        std::slice::from_ref(&b)
    );
    assert!(
        sealed_for(
            &routes,
            workspace,
            Needs::Act {
                app_id: "so.tilo.slides",
                record: "b"
            },
            &fresh
        )
        .is_empty(),
        "a record is named by its app and id together"
    );
    assert_eq!(
        sealed_for(&routes, workspace, Needs::Workspace, &fresh),
        std::slice::from_ref(&b),
        "the first listing waits for this workspace's sealed records only"
    );
    assert!(sealed_for(&routes, workspace, Needs::Joined, &fresh).is_empty());

    let waited = GaveUp {
        listed: true,
        records: HashSet::from([b.clone()]),
    };
    assert!(
        sealed_for(&routes, workspace, Needs::Workspace, &waited).is_empty(),
        "later listings wait for nothing"
    );
    assert!(
        sealed_for(&routes, workspace, read("b"), &waited).is_empty(),
        "a listing waits for the same missing keys once"
    );
    assert_eq!(
        sealed_for(&routes, workspace, act("b"), &waited),
        [b],
        "acting on a record always waits for its keys"
    );
}
