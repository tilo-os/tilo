//! Deterministic proposals, never execution or durable success.
use super::model::{
    Conflict, ConnectionStatus, Decision, Endpoint, EndpointIdentity, EntryState, MAX_PLAN_ENTRIES,
    NamePolicy, Observation, ObservedTree, PathProposal, PendingReason, ReconcilePlan,
    RelativePath, ScopeIssue, Side, VerifiedBaseline,
};

use std::collections::{BTreeMap, BTreeSet};

/// Compare bounded snapshots. Every proposed write still requires endpoint and
/// expected-content revalidation, recovery, a durable receipt, and baseline
/// publication by the future executor. In particular, this is not an operation
/// ordering: directory recovery must account for all descendants in this plan.
pub fn reconcile(
    baseline: &VerifiedBaseline,
    source: &ObservedTree,
    destination: &ObservedTree,
) -> ReconcilePlan {
    let source_entries = logical_entries(source);
    let destination_entries = logical_entries(destination);
    let empty = BTreeMap::new();
    let base = baseline.entries.as_ref().unwrap_or(&empty);
    let paths: BTreeSet<_> = base
        .keys()
        .chain(source_entries.keys())
        .chain(destination_entries.keys())
        .cloned()
        .collect();
    // Each constructor bounds its tree; union and output are bounded by 3N.
    debug_assert!(paths.len() <= MAX_PLAN_ENTRIES);
    let mut issues = observation_issues(Side::Source, source, baseline.source);
    issues.extend(observation_issues(
        Side::Destination,
        destination,
        baseline.destination,
    ));
    let status = if issues
        .iter()
        .any(|(side, issue)| *side == Side::Source && issue.path.is_none())
    {
        ConnectionStatus::SourceUnavailable
    } else if issues
        .iter()
        .any(|(side, issue)| *side == Side::Destination && issue.path.is_none())
    {
        ConnectionStatus::DestinationUnavailable
    } else {
        ConnectionStatus::ChangesPending
    };
    let mut blockers = namespace_blockers(&paths, source);
    blockers.extend(namespace_blockers(&paths, destination));
    // Structural conflicts protect the entire subtree, including children that
    // otherwise appear to be unrelated additions or unchanged deletions.
    for path in &paths {
        let s = source_entries.get(path);
        let d = destination_entries.get(path);
        if s.zip(d).is_some_and(|(s, d)| s.is_dir() != d.is_dir()) {
            blockers.push((path.clone(), Decision::Conflict(Conflict::FileDirectory)));
        } else if matches!(
            compare_entry(base.get(path), s, d, baseline.entries.is_none()),
            Decision::Conflict(_)
        ) {
            blockers.push((
                path.clone(),
                compare_entry(base.get(path), s, d, baseline.entries.is_none()),
            ));
        } else if base.get(path) == Some(&EntryState::Directory) && (s.is_none() != d.is_none()) {
            let remaining = if s.is_none() {
                &destination_entries
            } else {
                &source_entries
            };
            if paths
                .iter()
                .filter(|p| path.contains(p))
                .any(|p| base.get(p) != remaining.get(p))
            {
                blockers.push((path.clone(), Decision::Conflict(Conflict::EditDelete)));
            }
        }
    }
    let proposals = paths
        .into_iter()
        .map(|path| {
            let b = base.get(&path);
            let s = source_entries.get(&path);
            let d = destination_entries.get(&path);
            let decision = if let Some(reason) = issues.iter().find_map(|(side, issue)| {
                let endpoint = match side {
                    Side::Source => &source.endpoint,
                    Side::Destination => &destination.endpoint,
                };
                scope_pending_reason(&path, issue, endpoint)
            }) {
                Decision::Pending(reason)
            } else if let Some((_, block)) =
                blockers.iter().find(|(scope, _)| scope.overlaps(&path))
            {
                block.clone()
            } else {
                compare_entry(b, s, d, baseline.entries.is_none())
            };
            PathProposal {
                path,
                baseline: b.cloned(),
                source: s.cloned(),
                destination: d.cloned(),
                decision,
            }
        })
        .collect();
    ReconcilePlan {
        connection: baseline.connection,
        source: baseline.source,
        destination: baseline.destination,
        status,
        issues,
        proposals,
        requires_durable_verification: true,
        initial_comparison: baseline.entries.is_none(),
    }
}

/// A failed observation is expressed in the failing endpoint's namespace,
/// including issue-only spellings absent from the entry/baseline candidate set.
/// Without both canonical keys we cannot prove a candidate is outside its scope.
fn scope_pending_reason(
    path: &RelativePath,
    issue: &ScopeIssue,
    endpoint: &Endpoint,
) -> Option<PendingReason> {
    let Some(scope) = &issue.path else {
        return Some(issue.reason);
    };
    if scope.overlaps(path) {
        return Some(issue.reason);
    }
    match &endpoint.names {
        NamePolicy::Exact => None,
        NamePolicy::Canonical(keys) => {
            let key_for = |path: &RelativePath| {
                path.to_encoding(endpoint.encoding)
                    .ok()
                    .and_then(|native| keys.get(&native))
            };
            match (key_for(path), key_for(scope)) {
                (Some(path_key), Some(scope_key)) => (path_key.starts_with(scope_key)
                    || scope_key.starts_with(path_key))
                .then_some(issue.reason),
                _ => Some(PendingReason::MissingNameEvidence),
            }
        }
    }
}

fn logical_entries(tree: &ObservedTree) -> BTreeMap<RelativePath, EntryState> {
    tree.entries
        .iter()
        .map(|(p, e)| (p.logical(), e.clone()))
        .collect()
}
fn observation_issues(
    side: Side,
    tree: &ObservedTree,
    expected: EndpointIdentity,
) -> Vec<(Side, ScopeIssue)> {
    if tree.endpoint.identity != expected {
        return vec![(
            side,
            ScopeIssue {
                path: None,
                reason: PendingReason::IdentityChanged,
            },
        )];
    }
    match &tree.observation {
        Observation::Complete => vec![],
        Observation::Unavailable(reason) => vec![(
            side,
            ScopeIssue {
                path: None,
                reason: *reason,
            },
        )],
        Observation::Incomplete(issues) => issues
            .iter()
            .map(|i| {
                (
                    side,
                    ScopeIssue {
                        path: i.path.as_ref().map(RelativePath::logical),
                        reason: i.reason,
                    },
                )
            })
            .collect(),
    }
}
fn namespace_blockers(
    paths: &BTreeSet<RelativePath>,
    tree: &ObservedTree,
) -> Vec<(RelativePath, Decision)> {
    let mut blockers = Vec::new();
    let mut names: BTreeMap<Vec<Vec<u8>>, RelativePath> = BTreeMap::new();
    for path in paths {
        let Ok(native) = path.to_encoding(tree.endpoint.encoding) else {
            blockers.push((
                path.clone(),
                Decision::Conflict(Conflict::UnrepresentableName),
            ));
            continue;
        };
        if let NamePolicy::Canonical(keys) = &tree.endpoint.names {
            let Some(key) = keys.get(&native) else {
                blockers.push((
                    path.clone(),
                    Decision::Pending(PendingReason::MissingNameEvidence),
                ));
                continue;
            };
            if let Some(prior) = names.insert(key.clone(), path.clone()) {
                blockers.push((prior, Decision::Conflict(Conflict::NameCollision)));
                blockers.push((path.clone(), Decision::Conflict(Conflict::NameCollision)));
            }
        }
    }
    blockers
}
fn compare_entry(
    base: Option<&EntryState>,
    source: Option<&EntryState>,
    destination: Option<&EntryState>,
    initial: bool,
) -> Decision {
    if initial {
        return if source.zip(destination).is_some_and(|(s, d)| s != d) {
            Decision::Conflict(Conflict::DivergentEdits)
        } else {
            Decision::InitialPreview
        };
    }
    if source == destination {
        return Decision::VerifyEqual;
    }
    if source == base {
        return if destination.is_none() {
            Decision::RecoverDeletion { at: Side::Source }
        } else {
            Decision::Propagate {
                from: Side::Destination,
            }
        };
    }
    if destination == base {
        return if source.is_none() {
            Decision::RecoverDeletion {
                at: Side::Destination,
            }
        } else {
            Decision::Propagate { from: Side::Source }
        };
    }
    Decision::Conflict(if source.is_none() || destination.is_none() {
        Conflict::EditDelete
    } else {
        Conflict::DivergentEdits
    })
}

#[cfg(test)]
mod tests {
    use super::super::model::{ConnectionId, Encoding, MAX_ENTRIES};
    use super::*;
    use std::collections::BTreeMap;
    fn p(s: &str) -> RelativePath {
        RelativePath::unix(s.as_bytes()).unwrap()
    }
    fn file(n: u8) -> EntryState {
        EntryState::File {
            sha256: [n; 32],
            bytes: 42,
            executable: false,
        }
    }
    fn id(n: u8) -> EndpointIdentity {
        EndpointIdentity {
            volume: [n; 32],
            root: [n; 32],
        }
    }
    fn endpoint(n: u8) -> Endpoint {
        Endpoint {
            identity: id(n),
            encoding: Encoding::Unix,
            names: NamePolicy::Exact,
        }
    }
    fn tree(n: u8, entries: &[(&str, EntryState)]) -> ObservedTree {
        ObservedTree::new(
            endpoint(n),
            entries.iter().map(|(s, e)| (p(s), e.clone())),
            Observation::Complete,
        )
        .unwrap()
    }
    fn base(entries: &[(&str, EntryState)]) -> VerifiedBaseline {
        VerifiedBaseline::from_verified(
            ConnectionId([1; 16]),
            id(1),
            id(2),
            entries.iter().map(|(s, e)| (p(s), e.clone())),
        )
        .unwrap()
    }
    fn compare(
        b: &[(&str, EntryState)],
        s: &[(&str, EntryState)],
        d: &[(&str, EntryState)],
    ) -> ReconcilePlan {
        reconcile(&base(b), &tree(1, s), &tree(2, d))
    }
    fn decision(plan: &ReconcilePlan, path: &str) -> Decision {
        plan.proposals
            .iter()
            .find(|item| item.path == p(path))
            .expect("path proposal")
            .decision
            .clone()
    }
    fn no_writes(plan: &ReconcilePlan) {
        assert!(plan.proposals.iter().all(|p| !matches!(
            p.decision,
            Decision::Propagate { .. } | Decision::RecoverDeletion { .. }
        )));
        assert_ne!(plan.status, ConnectionStatus::UpToDate);
    }
    #[test]
    fn canonical_unavailable_file_aliases_never_propose_deletion_on_either_side() {
        for (name, alias) in [("A", "a"), ("é", "e\u{301}")] {
            let b = [(name, file(1))];
            for unavailable_side in [Side::Source, Side::Destination] {
                let n = if unavailable_side == Side::Source {
                    1
                } else {
                    2
                };
                let names = NamePolicy::Canonical(BTreeMap::from([
                    (p(name), vec![b"same".to_vec()]),
                    (p(alias), vec![b"same".to_vec()]),
                ]));
                let incomplete = ObservedTree::new(
                    Endpoint {
                        names,
                        ..endpoint(n)
                    },
                    [],
                    Observation::Incomplete(vec![ScopeIssue {
                        path: Some(p(alias)),
                        reason: PendingReason::PermissionDenied,
                    }]),
                )
                .unwrap();
                let plan = match unavailable_side {
                    Side::Source => reconcile(&base(&b), &incomplete, &tree(2, &b)),
                    Side::Destination => reconcile(&base(&b), &tree(1, &b), &incomplete),
                };
                assert_eq!(
                    decision(&plan, name),
                    Decision::Pending(PendingReason::PermissionDenied)
                );
                no_writes(&plan);
            }
        }
    }
    #[test]
    fn canonical_unavailable_directory_scope_protects_descendants() {
        let b = [("A", EntryState::Directory), ("A/child", file(1))];
        let names = NamePolicy::Canonical(BTreeMap::from([
            (p("A"), vec![b"a".to_vec()]),
            (p("a"), vec![b"a".to_vec()]),
            (p("A/child"), vec![b"a".to_vec(), b"child".to_vec()]),
        ]));
        let source = ObservedTree::new(
            Endpoint {
                names,
                ..endpoint(1)
            },
            [],
            Observation::Incomplete(vec![ScopeIssue {
                path: Some(p("a")),
                reason: PendingReason::PermissionDenied,
            }]),
        )
        .unwrap();
        let plan = reconcile(&base(&b), &source, &tree(2, &b));
        assert_eq!(
            decision(&plan, "A"),
            Decision::Pending(PendingReason::PermissionDenied)
        );
        assert_eq!(
            decision(&plan, "A/child"),
            Decision::Pending(PendingReason::PermissionDenied)
        );
        no_writes(&plan);
    }
    #[test]
    fn canonical_unavailable_descendant_scope_protects_ancestor_recovery() {
        let b = [("A", EntryState::Directory), ("A/child", file(1))];
        let names = NamePolicy::Canonical(BTreeMap::from([
            (p("A"), vec![b"a".to_vec()]),
            (p("a"), vec![b"a".to_vec()]),
            (p("A/child"), vec![b"a".to_vec(), b"child".to_vec()]),
            (p("a/child"), vec![b"a".to_vec(), b"child".to_vec()]),
        ]));
        let source = ObservedTree::new(
            Endpoint {
                names,
                ..endpoint(1)
            },
            [],
            Observation::Incomplete(vec![ScopeIssue {
                path: Some(p("a/child")),
                reason: PendingReason::Unstable,
            }]),
        )
        .unwrap();
        let plan = reconcile(&base(&b), &source, &tree(2, &b));
        assert_eq!(
            decision(&plan, "A"),
            Decision::Pending(PendingReason::Unstable)
        );
        assert_eq!(
            decision(&plan, "A/child"),
            Decision::Pending(PendingReason::Unstable)
        );
        no_writes(&plan);
    }
    #[test]
    fn canonical_unavailable_scope_without_key_evidence_fails_closed() {
        let b = [("A", file(1))];
        let names = NamePolicy::Canonical(BTreeMap::from([(p("A"), vec![b"a".to_vec()])]));
        let source = ObservedTree::new(
            Endpoint {
                names,
                ..endpoint(1)
            },
            [],
            Observation::Incomplete(vec![ScopeIssue {
                path: Some(p("a")),
                reason: PendingReason::PermissionDenied,
            }]),
        )
        .unwrap();
        let plan = reconcile(&base(&b), &source, &tree(2, &b));
        assert_eq!(
            decision(&plan, "A"),
            Decision::Pending(PendingReason::MissingNameEvidence)
        );
        no_writes(&plan);
    }
    #[test]
    fn canonical_unavailable_disjoint_scope_allows_verified_other_scope() {
        let b = [("A", file(1))];
        let names = NamePolicy::Canonical(BTreeMap::from([
            (p("A"), vec![b"a".to_vec()]),
            (p("b"), vec![b"b".to_vec()]),
        ]));
        let source = ObservedTree::new(
            Endpoint {
                names,
                ..endpoint(1)
            },
            [],
            Observation::Incomplete(vec![ScopeIssue {
                path: Some(p("b")),
                reason: PendingReason::PermissionDenied,
            }]),
        )
        .unwrap();
        let plan = reconcile(&base(&b), &source, &tree(2, &b));
        assert_eq!(
            decision(&plan, "A"),
            Decision::RecoverDeletion {
                at: Side::Destination
            }
        );
    }
    #[test]
    fn windows_non_unicode_child_keeps_ancestor_conflict_and_scope_relationships() {
        let parent = RelativePath::windows(&[97]).unwrap();
        let child = RelativePath::windows(&[97, 92, 0xd800]).unwrap();
        let b = VerifiedBaseline::from_verified(
            ConnectionId([1; 16]),
            id(1),
            id(2),
            [
                (parent.clone(), EntryState::Directory),
                (child.clone(), file(1)),
            ],
        )
        .unwrap();
        let source = ObservedTree::new(
            Endpoint {
                encoding: Encoding::Windows,
                ..endpoint(1)
            },
            [],
            Observation::Complete,
        )
        .unwrap();
        let mut destination = ObservedTree::new(
            Endpoint {
                encoding: Encoding::Windows,
                ..endpoint(2)
            },
            [(parent, EntryState::Directory), (child.clone(), file(2))],
            Observation::Complete,
        )
        .unwrap();
        let plan = reconcile(&b, &source, &destination);
        assert_eq!(
            decision(&plan, "a"),
            Decision::Conflict(Conflict::EditDelete)
        );
        no_writes(&plan);
        destination.observation = Observation::Incomplete(vec![ScopeIssue {
            path: Some(child),
            reason: PendingReason::Unstable,
        }]);
        let plan = reconcile(&b, &source, &destination);
        assert_eq!(
            decision(&plan, "a"),
            Decision::Pending(PendingReason::Unstable)
        );
        no_writes(&plan);
    }
    #[test]
    fn same_directory_replaced_by_divergent_files_keeps_child_baselines_unresolved() {
        let plan = compare(
            &[("a", EntryState::Directory), ("a/child", file(1))],
            &[("a", file(2))],
            &[("a", file(3))],
        );
        assert_eq!(
            decision(&plan, "a/child"),
            Decision::Conflict(Conflict::DivergentEdits)
        );
        no_writes(&plan);
    }
    #[test]
    fn descendant_failure_blocks_a_directory_recovery() {
        let b = [("a", EntryState::Directory), ("a/c", file(1))];
        let mut destination = tree(2, &b);
        destination.observation = Observation::Incomplete(vec![ScopeIssue {
            path: Some(p("a/c")),
            reason: PendingReason::Unstable,
        }]);
        let plan = reconcile(&base(&b), &tree(1, &[]), &destination);
        assert_eq!(
            decision(&plan, "a"),
            Decision::Pending(PendingReason::Unstable)
        );
        no_writes(&plan);
    }
    #[test]
    fn directory_delete_with_new_descendant_preserves_it() {
        let plan = compare(
            &[("a", EntryState::Directory)],
            &[],
            &[("a", EntryState::Directory), ("a/new", file(2))],
        );
        assert_eq!(
            decision(&plan, "a"),
            Decision::Conflict(Conflict::EditDelete)
        );
        assert_eq!(
            decision(&plan, "a/new"),
            Decision::Conflict(Conflict::EditDelete)
        );
        no_writes(&plan);
    }
    #[test]
    fn executable_metadata_changes_are_not_mistaken_for_equal_content() {
        let edited = EntryState::File {
            sha256: [1; 32],
            bytes: 42,
            executable: true,
        };
        let plan = compare(&[("a", file(1))], &[("a", edited)], &[("a", file(1))]);
        assert_eq!(
            decision(&plan, "a"),
            Decision::Propagate { from: Side::Source }
        );
    }
    #[test]
    fn full_candidate_union_is_bounded_and_deterministic() {
        let entries = |prefix: &str| {
            (0..MAX_ENTRIES)
                .map(|i| (p(&format!("{prefix}{i}")), file(1)))
                .collect::<Vec<_>>()
        };
        let b = VerifiedBaseline::from_verified(ConnectionId([1; 16]), id(1), id(2), entries("b"))
            .unwrap();
        let source = ObservedTree::new(endpoint(1), entries("s"), Observation::Complete).unwrap();
        let destination =
            ObservedTree::new(endpoint(2), entries("d"), Observation::Complete).unwrap();
        let plan = reconcile(&b, &source, &destination);
        assert_eq!(plan.proposals.len(), MAX_PLAN_ENTRIES);
        assert!(plan.proposals.windows(2).all(|w| w[0].path < w[1].path));
        assert_eq!(plan, reconcile(&b, &source, &destination));
    }
    #[test]
    fn missing_baseline_is_preview_even_for_matching_copies() {
        let b = VerifiedBaseline::absent(ConnectionId([1; 16]), id(1), id(2));
        let plan = reconcile(
            &b,
            &tree(1, &[("a", file(1)), ("b", file(2))]),
            &tree(2, &[("a", file(1))]),
        );
        assert_eq!(decision(&plan, "a"), Decision::InitialPreview);
        assert_eq!(decision(&plan, "b"), Decision::InitialPreview);
        assert!(plan.initial_comparison && plan.requires_durable_verification);
        no_writes(&plan);
    }
    #[test]
    fn initial_divergence_preserves_both_versions() {
        let plan = reconcile(
            &VerifiedBaseline::absent(ConnectionId([1; 16]), id(1), id(2)),
            &tree(1, &[("a", file(1))]),
            &tree(2, &[("a", file(2))]),
        );
        assert_eq!(
            decision(&plan, "a"),
            Decision::Conflict(Conflict::DivergentEdits)
        );
        assert_eq!(plan.proposals[0].source, Some(file(1)));
        assert_eq!(plan.proposals[0].destination, Some(file(2)));
        no_writes(&plan);
    }
    #[test]
    fn content_edits_flow_in_both_directions_despite_equal_sizes() {
        let b = [("a", file(1))];
        assert_eq!(
            decision(&compare(&b, &[("a", file(2))], &b), "a"),
            Decision::Propagate { from: Side::Source }
        );
        assert_eq!(
            decision(&compare(&b, &b, &[("a", file(2))]), "a"),
            Decision::Propagate {
                from: Side::Destination
            }
        );
    }
    #[test]
    fn equal_independent_edits_still_require_durable_verification() {
        let plan = compare(&[("a", file(1))], &[("a", file(2))], &[("a", file(2))]);
        assert_eq!(decision(&plan, "a"), Decision::VerifyEqual);
        assert!(plan.requires_durable_verification);
        no_writes(&plan);
    }
    #[test]
    fn divergent_edits_and_edit_delete_preserve_both_sides() {
        let b = [("a", file(1))];
        assert_eq!(
            decision(&compare(&b, &[("a", file(2))], &[("a", file(3))]), "a"),
            Decision::Conflict(Conflict::DivergentEdits)
        );
        for plan in [
            compare(&b, &[], &[("a", file(2))]),
            compare(&b, &[("a", file(2))], &[]),
        ] {
            assert_eq!(
                decision(&plan, "a"),
                Decision::Conflict(Conflict::EditDelete)
            );
            no_writes(&plan);
        }
    }
    #[test]
    fn deletion_of_unchanged_content_requires_recovery_in_either_direction() {
        let b = [("a", file(1))];
        assert_eq!(
            decision(&compare(&b, &[], &b), "a"),
            Decision::RecoverDeletion {
                at: Side::Destination
            }
        );
        assert_eq!(
            decision(&compare(&b, &b, &[]), "a"),
            Decision::RecoverDeletion { at: Side::Source }
        );
    }
    #[test]
    fn file_directory_replacement_blocks_every_descendant() {
        let plan = compare(
            &[("a", file(1))],
            &[("a", EntryState::Directory), ("a/child", file(2))],
            &[("a", file(1))],
        );
        assert_eq!(
            decision(&plan, "a"),
            Decision::Conflict(Conflict::FileDirectory)
        );
        assert_eq!(
            decision(&plan, "a/child"),
            Decision::Conflict(Conflict::FileDirectory)
        );
        no_writes(&plan);
    }
    #[test]
    fn deleting_directory_with_edited_child_is_not_safe() {
        let plan = compare(
            &[("a", EntryState::Directory), ("a/c", file(1))],
            &[],
            &[("a", EntryState::Directory), ("a/c", file(2))],
        );
        assert_eq!(
            decision(&plan, "a"),
            Decision::Conflict(Conflict::EditDelete)
        );
        no_writes(&plan);
    }
    #[test]
    fn unknown_rename_is_independent_addition_and_recoverable_deletion() {
        let plan = compare(
            &[("old", file(1))],
            &[("new", file(1))],
            &[("old", file(1))],
        );
        assert_eq!(
            decision(&plan, "new"),
            Decision::Propagate { from: Side::Source }
        );
        assert_eq!(
            decision(&plan, "old"),
            Decision::RecoverDeletion {
                at: Side::Destination
            }
        );
    }
    #[test]
    fn incomplete_subtree_blocks_ancestor_deletion_and_descendant_changes() {
        let b = [
            ("a", EntryState::Directory),
            ("a/c", file(1)),
            ("other", file(1)),
        ];
        let source = ObservedTree::new(
            endpoint(1),
            [(p("other"), file(2))],
            Observation::Incomplete(vec![ScopeIssue {
                path: Some(p("a")),
                reason: PendingReason::PermissionDenied,
            }]),
        )
        .unwrap();
        let plan = reconcile(&base(&b), &source, &tree(2, &b));
        assert_eq!(
            decision(&plan, "a"),
            Decision::Pending(PendingReason::PermissionDenied)
        );
        assert_eq!(
            decision(&plan, "a/c"),
            Decision::Pending(PendingReason::PermissionDenied)
        );
        assert_eq!(
            decision(&plan, "other"),
            Decision::Propagate { from: Side::Source }
        );
        assert_eq!(plan.issues.len(), 1);
    }
    #[test]
    fn offline_and_identity_changes_are_never_deletions_even_for_empty_trees() {
        let mut source = tree(1, &[]);
        source.observation = Observation::Unavailable(PendingReason::Offline);
        let plan = reconcile(
            &base(&[("a", file(1))]),
            &source,
            &tree(2, &[("a", file(1))]),
        );
        assert_eq!(plan.status, ConnectionStatus::SourceUnavailable);
        assert_eq!(
            decision(&plan, "a"),
            Decision::Pending(PendingReason::Offline)
        );
        no_writes(&plan);
        let plan = reconcile(&base(&[]), &source, &tree(2, &[]));
        assert!(!plan.issues.is_empty());
        no_writes(&plan);
        let plan = reconcile(
            &base(&[("a", file(1))]),
            &tree(3, &[]),
            &tree(2, &[("a", file(1))]),
        );
        assert_eq!(
            decision(&plan, "a"),
            Decision::Pending(PendingReason::IdentityChanged)
        );
        no_writes(&plan);
    }
    #[test]
    fn canonical_collision_blocks_both_names_and_children() {
        let s = [
            ("A", EntryState::Directory),
            ("A/c", file(1)),
            ("a", EntryState::Directory),
        ];
        let mut destination = tree(2, &[]);
        destination.endpoint.names = NamePolicy::Canonical(BTreeMap::from([
            (p("A"), vec![b"a".to_vec()]),
            (p("a"), vec![b"a".to_vec()]),
            (p("A/c"), vec![b"a".to_vec(), b"c".to_vec()]),
        ]));
        let plan = reconcile(&base(&[]), &tree(1, &s), &destination);
        for path in ["A", "a", "A/c"] {
            assert_eq!(
                decision(&plan, path),
                Decision::Conflict(Conflict::NameCollision)
            );
        }
        no_writes(&plan);
    }
    #[test]
    fn canonical_key_is_required_for_incoming_names() {
        let mut destination = tree(2, &[]);
        destination.endpoint.names = NamePolicy::Canonical(BTreeMap::new());
        let plan = reconcile(&base(&[]), &tree(1, &[("a", file(1))]), &destination);
        assert_eq!(
            decision(&plan, "a"),
            Decision::Pending(PendingReason::MissingNameEvidence)
        );
        no_writes(&plan);
    }
    #[test]
    fn unrepresentable_names_are_conflicts_without_lossy_replacement() {
        let bad = RelativePath::unix(b"\xff").unwrap();
        let source =
            ObservedTree::new(endpoint(1), [(bad.clone(), file(1))], Observation::Complete)
                .unwrap();
        let mut destination = tree(2, &[]);
        destination.endpoint.encoding = Encoding::Windows;
        let plan = reconcile(&base(&[]), &source, &destination);
        assert_eq!(plan.proposals[0].path, bad);
        assert_eq!(
            plan.proposals[0].decision,
            Decision::Conflict(Conflict::UnrepresentableName)
        );
        no_writes(&plan);
    }
    #[test]
    fn lossless_cross_encoding_paths_compare_as_same_file() {
        let destination = ObservedTree::new(
            Endpoint {
                encoding: Encoding::Windows,
                ..endpoint(2)
            },
            [(
                RelativePath::windows(&"é".encode_utf16().collect::<Vec<_>>()).unwrap(),
                file(1),
            )],
            Observation::Complete,
        )
        .unwrap();
        let plan = reconcile(
            &base(&[("é", file(1))]),
            &tree(1, &[("é", file(2))]),
            &destination,
        );
        assert_eq!(plan.proposals.len(), 1);
        assert_eq!(
            decision(&plan, "é"),
            Decision::Propagate { from: Side::Source }
        );
    }
}
