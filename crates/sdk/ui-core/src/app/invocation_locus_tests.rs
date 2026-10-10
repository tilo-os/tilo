// These tests use the real bound action door and record loan. Removing capture
// from dispatch, substituting the outer document, or reusing another target's
// basis must fail even if the action itself still succeeds.
fn locus_scope(
    fixture: &ActionFixture,
    id: u128,
) -> (
    crate::invocation_scope::ResourceInvocationScope,
    crate::invocation_scope::PublishedInvocationRequest,
) {
    locus_scope_with(fixture, id, "fixture-user", std::sync::Arc::new(|| Ok(())))
}

fn locus_scope_with(
    fixture: &ActionFixture,
    id: u128,
    actor: &str,
    check: std::sync::Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
) -> (
    crate::invocation_scope::ResourceInvocationScope,
    crate::invocation_scope::PublishedInvocationRequest,
) {
    use crate::invocation_scope::{
        PublishedInvocationLease, PublishedInvocationRequest, PublishedInvocationSpec,
    };
    let target = fixture.target(0, "apply");
    fixture
        .records
        .require_invocation_provenance(&target.binding.checkout)
        .unwrap();
    let publication = fixture
        .app
        .actions
        .bound_publication
        .alias(&target.binding, "bound:probe")
        .unwrap();
    let request = PublishedInvocationRequest {
        wire_id: u64::try_from(id).unwrap(),
        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(id)),
        address: tilo_actions::ActionAddress::new(publication, "apply"),
        argument: None,
        actor: actor.into(),
        participant: Some(
            tilo_actions::ParticipantId::new(format!("host-participant-{id}")).unwrap(),
        ),
    };
    let snapshots = crate::bound_dispatch::discover_bound_resources(
        fixture.app.root_widget.as_ref(),
        &fixture.app.state.doc,
        Some(&target.address.address),
    )
    .unwrap();
    let resource = snapshots
        .iter()
        .find(|s| s.binding.same_component(&target.binding))
        .unwrap()
        .resources()
        .iter()
        .find(|r| r.id.as_str() == "bound:probe")
        .unwrap();
    let spec = resource.actions.iter().find(|s| s.id == "apply").unwrap();
    let lease = PublishedInvocationLease::new(
        request.clone(),
        target.binding.broker_route().unwrap(),
        check,
    )
    .with_dispatch_spec(PublishedInvocationSpec {
        kind: spec.kind,
        contract: spec.contract.clone(),
        params: spec.params().to_vec(),
        input: tilo_actions::InputSpec::None,
        directory_revision: None,
        fresh_binding: None,
    })
    .unwrap()
    .with_owner_token(std::sync::Arc::new(request.clone()))
    .unwrap();
    let scope = fixture
        .app
        .published_invocation_scope(&request.address, Some(lease))
        .unwrap()
        .unwrap();
    (scope, request)
}

fn invoke_locus(
    fixture: &mut ActionFixture,
    scope: &crate::invocation_scope::ResourceInvocationScope,
    request: &crate::invocation_scope::PublishedInvocationRequest,
) {
    let target = fixture.target(0, "apply");
    let _invoker = crate::invocation_scope::InvokerScope::enter(request.invoker());
    let result = fixture
        .app
        .invoke_bound_target_with_broker(
            &target,
            &ResourceInvocation::new(&request.address, &request.actor)
                .confirmed(true)
                .broker_admitted(true)
                .invocation(Some(request.invocation))
                .scope(Some(scope)),
            None,
        )
        .unwrap();
    assert!(matches!(
        result,
        crate::widget::ResourceOperationStart::Completed(_)
    ));
}

#[test]
fn invocation_locus_actual_dispatch_retains_original_target_and_pre_execution_frontier() {
    let mut fixture = ActionFixture::broker();
    let target = fixture.target(0, "apply");
    let (scope, request) = locus_scope(&fixture, 7001);
    let before = fixture
        .records
        .read(&target.binding.checkout.id, |doc| doc.base_heads())
        .unwrap();
    assert!(scope.locus_basis().unwrap().is_none());
    invoke_locus(&mut fixture, &scope, &request);
    let basis = scope
        .locus_basis()
        .unwrap()
        .expect("actual dispatch captured a basis");
    assert_eq!(basis.invocation(), request.invocation);
    assert_eq!(basis.actor(), "fixture-user");
    assert_eq!(basis.participant(), request.participant.as_ref());
    assert_eq!(basis.publication(), &request.address);
    assert_eq!(basis.target(), &target.address.address);
    assert_eq!(basis.instance(), target.binding.instance_id);
    assert_eq!(basis.owner_lifetime(), target.binding.lifetime_id);
    assert_eq!(basis.checkout(), &target.binding.checkout.id);
    assert_eq!(basis.route(), &target.binding.broker_route().unwrap());
    let mut before = before;
    before.sort_unstable();
    assert_eq!(basis.heads(), before);
    assert!(
        fixture
            .records
            .read(&target.binding.checkout.id, |doc| doc
                .get(ROOT, "follow-up")
                .unwrap()
                .is_some())
            .unwrap(),
        "the original document action still ran"
    );
    assert_ne!(
        fixture
            .records
            .read(&target.binding.checkout.id, |doc| doc.base_heads())
            .unwrap(),
        before
    );
    assert!(
        fixture.app.state.doc.base_heads().is_empty(),
        "outer app is not the owned record"
    );
}

#[test]
fn invocation_locus_two_runs_and_scope_clones_keep_their_original_bases() {
    let mut fixture = ActionFixture::broker();
    let (first, a) = locus_scope(&fixture, 7002);
    let (second, b) = locus_scope(&fixture, 7003);
    invoke_locus(&mut fixture, &first, &a);
    let captured = first.locus_basis().unwrap().unwrap();
    invoke_locus(&mut fixture, &second, &b);
    let other = second.locus_basis().unwrap().unwrap();
    assert_eq!(captured.actor(), other.actor());
    assert_ne!(captured.invocation(), other.invocation());
    assert_ne!(captured.participant(), other.participant());
    assert_ne!(captured.heads(), other.heads());
    assert_eq!(first.clone().locus_basis().unwrap().unwrap(), captured);
    first.cancel();
    assert!(first.locus_basis().is_err());
    assert!(
        second.locus_basis().unwrap().is_some(),
        "retiring one run cannot retire another"
    );
    fixture.bindings[0].closing.set(true);
    assert!(
        second.locus_basis().is_err(),
        "closed owner cannot disclose a retained basis"
    );
}

#[test]
fn invocation_locus_mismatched_target_or_invoker_never_acquires_a_basis() {
    for case in 0..8 {
        let fixture = ActionFixture::broker();
        let (scope, request) = locus_scope(&fixture, 7010 + case);
        let mut target = fixture.target(0, "apply");
        let mut actor = request.actor.clone();
        let mut invocation = request.invocation;
        match case {
            0 => target = fixture.target(1, "apply"), // same record, different owner
            1 => target.address.address.resource = "another-resource".into(),
            2 => target.address.address.action = "another-action".into(),
            3 => actor = "another-actor".into(),
            4 => invocation = tilo_actions::InvocationId::new(),
            5 | 6 => {}
            7 => target.binding.lifetime_id = uuid::Uuid::new_v4(),
            _ => unreachable!(),
        }
        let mut invoker = request.invoker();
        if case == 5 {
            invoker.participant = Some(tilo_actions::ParticipantId::new("another-run").unwrap());
        } else if case == 6 {
            invoker.actor = "another-actor".into();
        }
        let _invoker = crate::invocation_scope::InvokerScope::enter(invoker);
        fixture
            .records
            .with_checked(&fixture.bindings[0].checkout.id, |doc| {
                scope.capture_locus_basis(&target, invocation, &actor, doc);
            })
            .unwrap();
        assert!(scope.locus_basis().unwrap().is_none(), "case {case}");
    }
}

#[test]
fn invocation_locus_oversized_metadata_and_expired_custody_remain_unavailable() {
    let fixture = ActionFixture::broker();
    let actor = "a".repeat(4097);
    let (scope, request) = locus_scope_with(&fixture, 7030, &actor, std::sync::Arc::new(|| Ok(())));
    let target = fixture.target(0, "apply");
    let _invoker = crate::invocation_scope::InvokerScope::enter(request.invoker());
    fixture
        .records
        .with_checked(&target.binding.checkout.id, |doc| {
            scope.capture_locus_basis(&target, request.invocation, &request.actor, doc);
        })
        .unwrap();
    assert!(scope.locus_basis().unwrap().is_none());

    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let allowed = Arc::new(AtomicBool::new(true));
    let check = allowed.clone();
    let mut fixture = ActionFixture::broker();
    let (scope, request) = locus_scope_with(
        &fixture,
        7031,
        "fixture-user",
        Arc::new(move || {
            check
                .load(Ordering::Acquire)
                .then_some(())
                .ok_or_else(|| "expired".into())
        }),
    );
    invoke_locus(&mut fixture, &scope, &request);
    assert!(scope.locus_basis().unwrap().is_some());
    allowed.store(false, Ordering::Release);
    assert!(scope.locus_basis().is_err());
    allowed.store(true, Ordering::Release);
    assert!(
        scope.locus_basis().is_err(),
        "original custody cannot revive after expiry"
    );
}

#[test]
fn invocation_locus_copied_pending_or_unscoped_documents_are_not_observation_evidence() {
    for pending in [false, true] {
        let fixture = ActionFixture::broker();
        let (scope, request) = locus_scope(&fixture, if pending { 7021 } else { 7020 });
        let target = fixture.target(0, "apply");
        let _invoker = crate::invocation_scope::InvokerScope::enter(request.invoker());
        fixture
            .records
            .with_checked(&target.binding.checkout.id, |doc| {
                if pending {
                    doc.put(ROOT, "pending-observation", true).unwrap();
                    let heads = doc.base_heads();
                    let count = doc.pending_ops();
                    scope.capture_locus_basis(&target, request.invocation, &request.actor, doc);
                    assert_eq!(doc.base_heads(), heads);
                    assert_eq!(doc.pending_ops(), count, "observation must not commit");
                } else {
                    scope.capture_locus_basis(
                        &target,
                        request.invocation,
                        &request.actor,
                        &doc.clone(),
                    );
                }
            })
            .unwrap();
        assert!(scope.locus_basis().unwrap().is_none());
    }
    let fixture = ActionFixture::broker();
    let (scope, request) = locus_scope(&fixture, 7022);
    let target = fixture.target(0, "apply");
    fixture
        .records
        .with_checked(&target.binding.checkout.id, |doc| {
            scope.capture_locus_basis(&target, request.invocation, &request.actor, doc);
        })
        .unwrap();
    assert!(
        scope.locus_basis().unwrap().is_none(),
        "no actual invoker scope"
    );
}

#[test]
fn invocation_locus_cannot_refresh_or_repair_an_original_observation() {
    let mut fixture = ActionFixture::broker();
    let (scope, request) = locus_scope(&fixture, 7040);
    invoke_locus(&mut fixture, &scope, &request);
    let first = scope.locus_basis().unwrap().unwrap();
    let target = fixture.target(0, "apply");
    let _invoker = crate::invocation_scope::InvokerScope::enter(request.invoker());
    fixture
        .records
        .with_checked(&target.binding.checkout.id, |doc| {
            scope.capture_locus_basis(&target, request.invocation, &request.actor, doc);
        })
        .unwrap();
    assert_eq!(
        scope.locus_basis().unwrap().unwrap(),
        first,
        "a later observation cannot silently move the original frontier"
    );
    let mut changed = target.clone();
    changed.address.address.resource = "other-resource".into();
    fixture
        .records
        .with_checked(&target.binding.checkout.id, |doc| {
            scope.capture_locus_basis(&changed, request.invocation, &request.actor, doc);
            scope.capture_locus_basis(&target, request.invocation, &request.actor, doc);
        })
        .unwrap();
    assert!(
        scope.locus_basis().unwrap().is_none(),
        "identity conflict stays unavailable"
    );
}

#[test]
fn invocation_locus_large_committed_frontier_does_not_create_a_partial_basis() {
    let fixture = ActionFixture::broker();
    let target = fixture.target(0, "apply");
    fixture
        .records
        .with_checked(&target.binding.checkout.id, |doc| {
            let mut base = doc.fork();
            for value in 0..129 {
                let mut branch = base.fork();
                branch.put(ROOT, "concurrent-head", value).unwrap();
                branch.commit();
                doc.merge(&mut branch).unwrap();
            }
            assert_eq!(doc.base_heads().len(), 129);
        })
        .unwrap();
    let (scope, request) = locus_scope(&fixture, 7041);
    let _invoker = crate::invocation_scope::InvokerScope::enter(request.invoker());
    fixture
        .records
        .with_checked(&target.binding.checkout.id, |doc| {
            let before = doc.base_heads();
            scope.capture_locus_basis(&target, request.invocation, &request.actor, doc);
            assert_eq!(doc.base_heads(), before);
            assert_eq!(doc.pending_ops(), 0);
        })
        .unwrap();
    assert!(scope.locus_basis().unwrap().is_none());
}
