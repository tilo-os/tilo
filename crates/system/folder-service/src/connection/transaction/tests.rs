use super::*;
// Catches a control request waiting for a held store borrow or an admitted step.
#[test]
fn disconnect_latch_closes_before_the_admitted_step_drains() {
    let gate = Arc::new(AdmissionGate::new());
    let session = TransactionSession::new(ConnectionId([1; 16]), Weak::new(), Arc::clone(&gate));
    gate.state.lock().unwrap().in_flight = 1;
    session.request_disconnect();
    assert!(gate.stopped().load(Ordering::Acquire));
    let state = gate.state.lock().unwrap();
    assert!(state.closed);
    assert_eq!(state.in_flight, 1);
    drop(state);
}

#[test]
fn existing_ancestor_requirement_is_exact_and_bounded() {
    use crate::connection::model::{EntryState, RelativePath};
    use std::collections::BTreeMap;
    let leaf = RelativePath::unix(b"a/b/file").unwrap();
    let mut old = BTreeMap::new();
    let requirement = ExistingAncestorRequirement::from_baseline(&leaf, &old).unwrap();
    assert_eq!(requirement.prefixes(), 6);
    assert_eq!(
        requirement.paths(),
        vec![
            RelativePath::unix(b"a").unwrap(),
            RelativePath::unix(b"a/b").unwrap()
        ]
    );
    old.insert(RelativePath::unix(b"a").unwrap(), EntryState::Directory);
    assert_eq!(
        ExistingAncestorRequirement::from_baseline(&leaf, &old)
            .unwrap()
            .prefixes(),
        4
    );
    old.insert(RelativePath::unix(b"a/b").unwrap(), EntryState::Directory);
    assert_eq!(
        ExistingAncestorRequirement::from_baseline(&leaf, &old)
            .unwrap()
            .prefixes(),
        0
    );
    old.insert(
        RelativePath::unix(b"a").unwrap(),
        EntryState::File {
            sha256: [1; 32],
            bytes: 1,
            executable: false,
        },
    );
    assert!(ExistingAncestorRequirement::from_baseline(&leaf, &old).is_err());
    let max = RelativePath::unix(vec!["a"; 64].join("/").as_bytes()).unwrap();
    let requirement = ExistingAncestorRequirement::from_baseline(&max, &BTreeMap::new()).unwrap();
    assert_eq!(requirement.prefixes(), u64::MAX - 1);
    assert_eq!(requirement.paths().len(), 63);
    assert!(RelativePath::unix(vec!["a"; 65].join("/").as_bytes()).is_err());
}

#[test]
fn existing_ancestor_requirement_refuses_missing_or_conflicting_endpoint_parents() {
    use crate::connection::{
        model::{
            Decision, Encoding, Endpoint, EndpointIdentity, EntryState, NamePolicy, Observation,
            ObservedTree, RelativePath, Side, VerifiedBaseline,
        },
        store::OperationIntent,
    };
    let identities = [
        EndpointIdentity {
            volume: [1; 32],
            root: [2; 32],
        },
        EndpointIdentity {
            volume: [3; 32],
            root: [4; 32],
        },
    ];
    let endpoint = |index| Endpoint {
        identity: identities[index],
        encoding: Encoding::Unix,
        names: NamePolicy::Exact,
    };
    let parent = RelativePath::unix(b"a").unwrap();
    let leaf = RelativePath::unix(b"a/file").unwrap();
    let value = EntryState::File {
        sha256: [7; 32],
        bytes: 3,
        executable: false,
    };
    let source = ObservedTree::new(
        endpoint(0),
        [
            (parent.clone(), EntryState::Directory),
            (leaf.clone(), value.clone()),
        ],
        Observation::Complete,
    )
    .unwrap();
    let baseline = VerifiedBaseline::absent(ConnectionId([1; 16]), identities[0], identities[1]);
    let intent = OperationIntent {
        connection: baseline.connection,
        expected_revision: 1,
        expected_baseline: None,
        path: leaf,
        source_before: Some(value.clone()),
        destination_before: None,
        decision: Decision::Propagate { from: Side::Source },
    };
    for state in [None, Some(value), Some(EntryState::Directory)] {
        let destination = ObservedTree::new(
            endpoint(1),
            state.clone().map(|state| (parent.clone(), state)),
            Observation::Complete,
        )
        .unwrap();
        assert_eq!(
            regular_ancestor_prerequisite(&intent, &baseline, &source, &destination).is_ok(),
            state == Some(EntryState::Directory)
        );
    }
}
