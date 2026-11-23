use super::{COMPACTION_RETRY, Resting, WorkspaceId};

fn workspace() -> WorkspaceId {
    WorkspaceId::parse("23fb5a50-60db-42bd-bb3b-f597bfab1377").unwrap()
}

/// One history that cannot fold yet rests a while, and every other history of the
/// workspace — one that is full and refusing writes, say — is folded on the very next pass.
#[test]
fn a_history_that_cannot_fold_rests_alone() {
    let resting = Resting::default();
    let (stuck, ready) = (
        ("so.tilo.files".to_owned(), "tree".to_owned()),
        ("so.tilo.files".to_owned(), "chunk.1@tree".to_owned()),
    );
    resting.failed(workspace(), stuck.clone());
    let still = resting.pass(workspace());
    assert_eq!(still, std::collections::BTreeSet::from([stuck]));
    assert!(!still.contains(&ready), "another record is not resting");

    // Its rest is spent after its passes, and it is tried again.
    for _ in 0..COMPACTION_RETRY {
        resting.pass(workspace());
    }
    assert!(resting.pass(workspace()).is_empty());

    // One that folds leaves no rest behind.
    resting.failed(workspace(), ready.clone());
    resting.folded(workspace(), ready.clone());
    assert!(!resting.pass(workspace()).contains(&ready));
}
