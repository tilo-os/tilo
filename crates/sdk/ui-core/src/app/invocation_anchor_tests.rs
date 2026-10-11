use automerge::AutoCommit;

thread_local! { static APPLIED_PROBE_MODE: Cell<u8> = const { Cell::new(0) }; }
thread_local! { static PROBE_ANCHOR_ENABLED: Cell<bool> = const { Cell::new(false) }; }
struct ProbeAnchorGuard;
impl Drop for ProbeAnchorGuard {
    fn drop(&mut self) {
        PROBE_ANCHOR_ENABLED.set(false);
        APPLIED_PROBE_MODE.set(0);
    }
}
// These require capture on the actual checked dispatch path, after encoding
// and before execution. An unqualified public projector is not the evidence.
fn probe_invocation_anchor(
    document: &AutoCommit,
    address: &tilo_actions::ActionAddress,
    encoded: &tilo_actions::EncodedAction,
) -> Option<tilo_actions::InteractionAnchor> {
    assert_eq!(
        address,
        &tilo_actions::ActionAddress::new("bound:probe", "apply")
    );
    assert_eq!(encoded.type_id, "bound.apply");
    assert!(document.get(ROOT, "follow-up").unwrap().is_none());
    assert_eq!(
        document.get(ROOT, "marker").unwrap().unwrap().0.as_str(),
        Some("child")
    );
    Some(tilo_actions::InteractionAnchor::Text {
        region: "body".into(),
        ranges: vec![tilo_actions::InteractionTextRange {
            block: "exact-pre-operation-block".into(),
            start: 2,
            end: 2,
        }],
    })
}

static PROBE_INVOCATION_ANCHORS: [crate::invocation_anchor::RegisteredInvocationAnchor; 1] =
    [crate::invocation_anchor::RegisteredInvocationAnchor::new(
        "bound.apply",
        probe_invocation_anchor,
    )
    .with_applied(probe_applied_anchor)];

#[test]
fn invocation_locus_anchor_is_captured_before_actual_apply_and_keeps_original_custody() {
    PROBE_ANCHOR_ENABLED.set(true);
    let _guard = ProbeAnchorGuard;
    let mut fixture = ActionFixture::broker();
    let target = fixture.target(0, "apply");
    let (scope, request) = locus_scope(&fixture, 7101);
    assert!(scope.pre_operation_anchor().unwrap().is_none());
    invoke_locus(&mut fixture, &scope, &request);
    let observation = scope.pre_operation_anchor().unwrap().unwrap();
    assert_eq!(observation.basis(), &scope.locus_basis().unwrap().unwrap());
    assert_eq!(observation.basis().invocation(), request.invocation);
    assert_eq!(
        observation.basis().participant(),
        request.participant.as_ref()
    );
    assert_eq!(observation.basis().target(), &target.address.address);
    assert!(
        matches!(observation.anchor(), tilo_actions::InteractionAnchor::Text { ranges, .. }
        if ranges.len() == 1 && ranges[0].start == 2 && ranges[0].end == 2)
    );
    assert!(
        fixture
            .records
            .read(&target.binding.checkout.id, |doc| doc
                .get(ROOT, "follow-up")
                .unwrap()
                .is_some())
            .unwrap()
    );
    assert_eq!(
        scope.clone().pre_operation_anchor().unwrap().unwrap(),
        observation
    );
    scope.cancel();
    assert!(scope.pre_operation_anchor().is_err());
}

#[test]
fn invocation_locus_anchor_does_not_search_foreign_owners_or_choose_ambiguous_provider() {
    PROBE_ANCHOR_ENABLED.set(true);
    let _guard = ProbeAnchorGuard;
    let fixture = ActionFixture::broker();
    let address = tilo_actions::ActionAddress::new("bound:probe", "apply");
    let encoded = tilo_actions::EncodedAction::encode("bound.apply", &FollowUp).unwrap();
    // Searching the outer host cannot borrow a nested owner's registration.
    assert!(
        crate::invocation_anchor::project(
            fixture.app.root_widget.as_ref(),
            &fixture.app.state.doc,
            &address,
            &encoded
        )
        .is_none()
    );
    let ambiguous = Column::new()
        .push(Box::new(Probe::default()))
        .push(Box::new(Probe::default()));
    assert!(
        crate::invocation_anchor::project(&ambiguous, &fixture.app.state.doc, &address, &encoded)
            .is_none()
    );
    let oversized = tilo_actions::EncodedAction {
        type_id: "bound.apply".into(),
        payload: vec![0; crate::invocation_anchor::MAX_PROJECTION_BYTES + 1],
    };
    // The callback asserts the record loan. Oversize refuses before invoking it.
    assert!(
        crate::invocation_anchor::project(
            &Probe::default(),
            &fixture.app.state.doc,
            &address,
            &oversized
        )
        .is_none()
    );
}

fn probe_applied_anchor(
    document: &AutoCommit,
    address: &tilo_actions::ActionAddress,
    encoded: &tilo_actions::EncodedAction,
) -> Option<tilo_actions::InteractionAnchor> {
    assert_eq!(
        address,
        &tilo_actions::ActionAddress::new("bound:probe", "apply")
    );
    assert_eq!(encoded.type_id, "bound.apply");
    assert!(document.get(ROOT, "follow-up").unwrap().is_some());
    Some(tilo_actions::InteractionAnchor::Text {
        region: "body".into(),
        ranges: vec![tilo_actions::InteractionTextRange {
            block: "exact-applied-block".into(),
            start: 3,
            end: 3,
        }],
    })
}

#[test]
fn invocation_locus_applied_anchor_requires_actual_completed_local_mutation_and_original_custody() {
    PROBE_ANCHOR_ENABLED.set(true);
    let _guard = ProbeAnchorGuard;
    let mut fixture = ActionFixture::broker();
    let (scope, request) = locus_scope(&fixture, 7201);
    assert!(scope.applied_anchor().unwrap().is_none());
    invoke_locus(&mut fixture, &scope, &request);
    let receipt = scope
        .applied_anchor()
        .unwrap()
        .expect("actual applied path finalized");
    assert_eq!(receipt.basis(), &scope.locus_basis().unwrap().unwrap());
    assert_ne!(receipt.heads(), receipt.basis().heads());
    assert!(
        !receipt.terminal_handed_off(),
        "Completed is not a terminal delivery receipt"
    );
    let target = fixture.target(0, "apply");
    let mut actual = fixture
        .records
        .read(&target.binding.checkout.id, |doc| doc.base_heads())
        .unwrap();
    actual.sort_unstable();
    assert_eq!(receipt.heads(), actual);
    assert!(
        matches!(receipt.anchor(), tilo_actions::InteractionAnchor::Text { ranges, .. }
        if ranges.len() == 1 && ranges[0].start == 3 && ranges[0].end == 3)
    );
    // Explicitly test handoff as distinct metadata, never broker confirmation.
    scope.note_terminal_handoff(true);
    assert!(
        scope
            .applied_anchor()
            .unwrap()
            .unwrap()
            .terminal_handed_off()
    );
    scope.cancel();
    assert!(scope.applied_anchor().is_err());
}

#[test]
fn invocation_locus_applied_anchor_refuses_real_noop_and_failed_action() {
    PROBE_ANCHOR_ENABLED.set(true);
    let _guard = ProbeAnchorGuard;
    for mode in [1, 2] {
        APPLIED_PROBE_MODE.set(mode);
        let mut fixture = ActionFixture::broker();
        let (scope, request) = locus_scope(&fixture, 7202 + u128::from(mode));
        let target = fixture.target(0, "apply");
        let _invoker = crate::invocation_scope::InvokerScope::enter(request.invoker());
        let result = fixture.app.invoke_bound_target_with_broker(
            &target,
            &ResourceInvocation::new(&request.address, &request.actor)
                .confirmed(true)
                .broker_admitted(true)
                .invocation(Some(request.invocation))
                .scope(Some(&scope)),
            None,
        );
        assert_eq!(result.is_ok(), mode == 1);
        assert!(scope.applied_anchor().unwrap().is_none());
        scope.note_terminal_handoff(true);
        assert!(
            scope.applied_anchor().unwrap().is_none(),
            "terminal metadata cannot invent an applied receipt"
        );
    }
}

#[test]
fn invocation_locus_applied_anchor_disposes_failed_handoff_and_closed_owner() {
    PROBE_ANCHOR_ENABLED.set(true);
    let _guard = ProbeAnchorGuard;
    for closed in [false, true] {
        let mut fixture = ActionFixture::broker();
        let (scope, request) = locus_scope(&fixture, 7210 + u128::from(closed));
        invoke_locus(&mut fixture, &scope, &request);
        assert!(scope.applied_anchor().unwrap().is_some());
        if closed {
            fixture.bindings[0].closing.set(true);
            assert!(scope.applied_anchor().is_err());
        } else {
            scope.note_terminal_handoff(false);
            assert!(scope.applied_anchor().unwrap().is_none());
        }
    }
}

#[test]
fn invocation_locus_applied_anchor_expiry_and_repeated_dispatch_cannot_revive_receipt() {
    PROBE_ANCHOR_ENABLED.set(true);
    let _guard = ProbeAnchorGuard;
    for expired in [false, true] {
        let mut fixture = ActionFixture::broker();
        let (scope, request) = locus_scope(&fixture, 7220 + u128::from(expired));
        invoke_locus(&mut fixture, &scope, &request);
        assert!(scope.applied_anchor().unwrap().is_some());
        if expired {
            let later = crate::time::now()
                .checked_add(crate::time::Duration::from_secs(10))
                .unwrap();
            assert!(scope.applied_anchor_at(later).unwrap().is_none());
        } else {
            invoke_locus(&mut fixture, &scope, &request);
        }
        scope.note_terminal_handoff(true);
        assert!(scope.applied_anchor().unwrap().is_none());
    }
}
