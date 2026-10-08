// These are native SDK delivery/owner tests. The signed broker stream test
// separately proves that the declaration originates at real admission.
fn native_effect_delivery(
    fixture: &ActionFixture,
    live: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> (
    crate::invocation_scope::NativeInvocationDelivery,
    crate::invocation_scope::PublishedInvocationRequest,
) {
    use crate::invocation_scope::{
        PublishedInvocationAdmission, PublishedInvocationRequest, PublishedInvocationSpec,
    };
    let target = fixture.target(0, "follow-up");
    let resource = fixture
        .app
        .actions
        .bound_publication
        .alias(&target.binding, "bound:probe")
        .unwrap();
    let snapshots = crate::bound_dispatch::discover_bound_resources(
        fixture.app.root_widget.as_ref(),
        &fixture.app.state.doc,
        Some(&target.address.address),
    )
    .unwrap();
    let resource_spec = snapshots
        .iter()
        .find(|snapshot| snapshot.binding.same_component(&target.binding))
        .unwrap()
        .resources
        .iter()
        .find(|resource| resource.id.as_str() == "bound:probe")
        .unwrap();
    let spec = resource_spec
        .actions
        .iter()
        .find(|spec| spec.id == "follow-up")
        .unwrap();
    let request = PublishedInvocationRequest {
        wire_id: 27001,
        invocation: tilo_actions::InvocationId::new(),
        address: tilo_actions::ActionAddress::new(resource, "follow-up"),
        argument: None,
        actor: "fixture-user".into(),
        participant: None,
    };
    let declaration = PublishedInvocationAdmission {
        version: PublishedInvocationAdmission::VERSION,
        wire_id: request.wire_id,
        invocation: request.invocation,
        spec: PublishedInvocationSpec {
            kind: spec.kind,
            contract: spec.contract.clone(),
            params: spec.params().to_vec(),
            input: tilo_actions::InputSpec::None,
            directory_revision: None,
        },
    };
    let delivery = crate::invocation_scope::NativeInvocationDelivery::from_stream(
        request.clone(),
        target.binding.broker_route().unwrap(),
        &serde_json::to_string(&declaration).unwrap(),
        std::sync::Arc::new(move || {
            live.load(std::sync::atomic::Ordering::Acquire)
                .then_some(())
                .ok_or_else(|| "native stream retired".into())
        }),
    )
    .unwrap();
    (delivery, request)
}

fn native_effect_scope(
    fixture: &ActionFixture,
    delivery: &crate::invocation_scope::NativeInvocationDelivery,
    request: &crate::invocation_scope::PublishedInvocationRequest,
) -> crate::invocation_scope::ResourceInvocationScope {
    fixture
        .app
        .published_invocation_scope(&request.address, delivery.lease(request).unwrap())
        .unwrap()
        .unwrap()
}

#[test]
fn native_effect_exact_delivery_lends_read_scope_without_document_authority() {
    COMPOUND_MODE.set(2);
    let mut fixture = ActionFixture::broker();
    let live = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (delivery, request) = native_effect_delivery(&fixture, live);
    let scope = native_effect_scope(&fixture, &delivery, &request);
    let target = fixture.target(0, "follow-up");
    let before = fixture
        .records
        .read(&target.binding.checkout.id, |doc| doc.base_heads())
        .unwrap();
    let result = fixture
        .app
        .invoke_bound_target_with_broker(
            &target,
            &ResourceInvocation::new(&request.address, &request.actor)
                .confirmed(true)
                .broker_admitted(true)
                .invocation(Some(request.invocation))
                .scope(Some(&scope)),
            None,
        )
        .unwrap();
    assert!(matches!(
        result,
        crate::widget::ResourceOperationStart::Accepted
    ));
    assert_eq!(
        fixture
            .records
            .read(&target.binding.checkout.id, |doc| doc.base_heads())
            .unwrap(),
        before
    );
    target
        .binding
        .with_document(&mut fixture.app.state.doc, |doc, _| {
            let mut entered = false;
            let refused = scope
                .with_document_mutation(doc, |_| {
                    entered = true;
                    Ok(())
                })
                .unwrap_err();
            assert!(refused.contains("not final-owner"));
            assert!(!entered);
        })
        .unwrap();
    assert!(scope.effect_scope().unwrap().with_effect(|| Ok(())).is_ok());
    COMPOUND_MODE.set(0);
}

#[test]
fn native_effect_delivery_rejects_foreign_inputs_retired_stream_and_required_host_fallback() {
    let mut fixture = ActionFixture::broker();
    let live = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (delivery, request) = native_effect_delivery(&fixture, live.clone());
    let mut foreign = request.clone();
    foreign.actor = "foreign-caller".into();
    assert!(delivery.lease(&foreign).is_err());
    foreign = request.clone();
    foreign.argument = Some("foreign input".into());
    assert!(delivery.lease(&foreign).is_err());
    let scope = native_effect_scope(&fixture, &delivery, &request);
    let worker = scope.effect_scope().unwrap();
    live.store(false, std::sync::atomic::Ordering::Release);
    assert!(delivery.lease(&request).is_err());
    assert!(
        worker
            .with_effect(|| -> Result<(), String> {
                panic!("retired native delivery entered effect")
            })
            .is_err()
    );
    fixture.app.actions.published_invocations.native_delivery = Some(Ok(delivery));
    fixture.app.require_published_invocation_authorizer();
    assert!(
        fixture
            .app
            .authorize_published_invocation(&request)
            .is_err()
    );
}

#[test]
fn native_effect_undeclared_writes_restore_exact_loan_before_publication_and_export() {
    for mode in [3, 4, 5, 6] {
        COMPOUND_MODE.set(mode);
        let mut fixture = ActionFixture::broker();
        let live = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (delivery, request) = native_effect_delivery(&fixture, live);
        let scope = native_effect_scope(&fixture, &delivery, &request);
        let target = fixture.target(0, "follow-up");
        target
            .binding
            .with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "prior-typing", "preserved").unwrap();
            })
            .unwrap();
        let before = fixture
            .records
            .replication_snapshot(&target.binding.checkout.id)
            .unwrap();
        let generation = fixture
            .records
            .action_generation(&target.binding.checkout.id)
            .unwrap();
        let history = fixture
            .records
            .read_history(&target.binding.checkout.id, |_, history| history.revision())
            .unwrap();
        PUBLICATION_NORMAL_POLL.set(true);
        let result = fixture.app.invoke_bound_target_with_broker(
            &target,
            &ResourceInvocation::new(&request.address, &request.actor)
                .confirmed(true)
                .broker_admitted(true)
                .invocation(Some(request.invocation))
                .scope(Some(&scope)),
            None,
        );
        PUBLICATION_NORMAL_POLL.set(false);
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("changes were refused")
        );
        assert!(scope.effect_scope().is_err());
        assert_eq!(
            fixture
                .records
                .replication_snapshot(&target.binding.checkout.id)
                .unwrap(),
            before
        );
        assert_eq!(
            fixture
                .records
                .action_generation(&target.binding.checkout.id)
                .unwrap(),
            generation
        );
        assert_eq!(
            fixture
                .records
                .read_history(&target.binding.checkout.id, |_, history| history.revision())
                .unwrap(),
            history
        );
        let reopened = automerge::AutoCommit::load(&before.0).unwrap();
        assert!(reopened.get(ROOT, "raw-before").unwrap().is_none());
        assert!(reopened.get(ROOT, "raw-after").unwrap().is_none());
        assert!(reopened.get(ROOT, "follow-up").unwrap().is_none());
        assert_eq!(
            reopened
                .get(ROOT, "prior-typing")
                .unwrap()
                .unwrap()
                .0
                .as_str(),
            Some("preserved")
        );
        let owner = crate::history_mode::HistoryOwner::component(target.binding);
        assert!(matches!(
            owner
                .prepare_publication(&fixture.app.state.doc, &before.1, false, false)
                .unwrap(),
            crate::history_mode::owner::OwnedDocumentPublication::Unchanged
        ));
        let snapshot = owner
            .prepare_publication(&fixture.app.state.doc, &before.1, true, false)
            .unwrap();
        let crate::history_mode::owner::OwnedDocumentPublication::Changed {
            mutation,
            authorization,
            ..
        } = snapshot
        else {
            panic!("original native snapshot must remain publishable")
        };
        assert!(
            authorization.is_none(),
            "native lease must not mint Host mutation authority"
        );
        let collab_types::DocumentMutation::Snapshot(bytes) = mutation else {
            panic!("actual complete snapshot")
        };
        assert!(
            automerge::AutoCommit::load(&bytes)
                .unwrap()
                .get(ROOT, "raw-before")
                .unwrap()
                .is_none()
        );
    }
    COMPOUND_MODE.set(0);
}

#[test]
fn native_effect_unwind_restores_live_typing_and_retires_only_original_scope() {
    COMPOUND_MODE.set(11);
    let mut fixture = ActionFixture::broker();
    let live = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (delivery, request) = native_effect_delivery(&fixture, live);
    let scope = native_effect_scope(&fixture, &delivery, &request);
    let worker = scope.effect_scope().unwrap();
    let target = fixture.target(0, "follow-up");
    target
        .binding
        .with_document(&mut fixture.app.state.doc, |doc, _| {
            doc.put(ROOT, "prior-typing", "still pending").unwrap();
        })
        .unwrap();
    assert!(
        fixture
            .records
            .read(&target.binding.checkout.id, |doc| doc.pending_ops() > 0)
            .unwrap()
    );
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fixture.app.invoke_bound_target_with_broker(
            &target,
            &ResourceInvocation::new(&request.address, &request.actor)
                .confirmed(true)
                .broker_admitted(true)
                .invocation(Some(request.invocation))
                .scope(Some(&scope)),
            None,
        )
    }));
    assert!(panicked.is_err());
    assert!(worker.with_effect(|| Ok(())).is_err());
    let (bytes, _) = fixture
        .records
        .replication_snapshot(&target.binding.checkout.id)
        .unwrap();
    let restored = automerge::AutoCommit::load(&bytes).unwrap();
    assert_eq!(
        restored
            .get(ROOT, "prior-typing")
            .unwrap()
            .unwrap()
            .0
            .as_str(),
        Some("still pending")
    );
    assert!(restored.get(ROOT, "raw-unwind").unwrap().is_none());
    let fresh = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (delivery, request) = native_effect_delivery(&fixture, fresh);
    let fresh_scope = native_effect_scope(&fixture, &delivery, &request);
    assert!(
        fresh_scope
            .effect_scope()
            .unwrap()
            .with_effect(|| Ok(()))
            .is_ok()
    );
    COMPOUND_MODE.set(0);
}

#[test]
fn native_effect_worker_lease_retires_with_actual_component_owner() {
    let mut fixture = ActionFixture::broker();
    let live = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (delivery, request) = native_effect_delivery(&fixture, live);
    let scope = native_effect_scope(&fixture, &delivery, &request);
    let worker = scope.effect_scope().unwrap();
    fixture.app.close_components();
    assert!(scope.check().is_err());
    assert!(worker.with_effect(|| Ok(())).is_err());
}

thread_local! {
    static NATIVE_MISSING_DEPENDENCY: RefCell<Option<automerge::Change>> = const { RefCell::new(None) };
}

#[test]
fn native_effect_orphan_changes_are_refused_before_save_and_later_dependency() {
    COMPOUND_MODE.set(12);
    let mut fixture = ActionFixture::broker();
    let live = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (delivery, request) = native_effect_delivery(&fixture, live);
    let scope = native_effect_scope(&fixture, &delivery, &request);
    let target = fixture.target(0, "follow-up");
    let before = fixture
        .records
        .replication_snapshot(&target.binding.checkout.id)
        .unwrap();
    PUBLICATION_NORMAL_POLL.set(true);
    let result = fixture.app.invoke_bound_target_with_broker(
        &target,
        &ResourceInvocation::new(&request.address, &request.actor)
            .confirmed(true)
            .broker_admitted(true)
            .invocation(Some(request.invocation))
            .scope(Some(&scope)),
        None,
    );
    PUBLICATION_NORMAL_POLL.set(false);
    COMPOUND_MODE.set(0);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("changes were refused")
    );
    let after = fixture
        .records
        .replication_snapshot(&target.binding.checkout.id)
        .unwrap();
    assert_eq!(after, before);
    let mut reopened = automerge::AutoCommit::load(&after.0).unwrap();
    let dependency = NATIVE_MISSING_DEPENDENCY.with_borrow_mut(|slot| slot.take().unwrap());
    reopened.apply_changes([dependency]).unwrap();
    assert!(reopened.get(ROOT, "orphan-dependency").unwrap().is_some());
    assert!(reopened.get(ROOT, "undeclared-orphan").unwrap().is_none());
}

struct NativeAcceptedWorker {
    invocation: tilo_actions::InvocationId,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    exited: std::sync::mpsc::Receiver<bool>,
    thread: Option<std::thread::JoinHandle<()>>,
    delayed: bool,
    remaining_cancel_polls: usize,
    cleanup_observed: bool,
    foreign_terminal_pending: bool,
}
impl Drop for NativeAcceptedWorker {
    fn drop(&mut self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
thread_local! {
    static NATIVE_ACCEPTED_WORKERS: RefCell<Vec<NativeAcceptedWorker>> = const { RefCell::new(vec![]) };
}
fn start_native_accepted_worker(invocation: tilo_actions::InvocationId) {
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let signal = cancelled.clone();
    let (tx, exited) = std::sync::mpsc::channel();
    let thread = tilo_rt::spawn_thread("native-effect-contract-worker", move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !signal.load(std::sync::atomic::Ordering::Acquire)
            && std::time::Instant::now() < deadline
        {
            std::thread::park_timeout(std::time::Duration::from_millis(5));
        }
        let _ = tx.send(signal.load(std::sync::atomic::Ordering::Acquire));
    })
    .unwrap();
    NATIVE_ACCEPTED_WORKERS.with_borrow_mut(|workers| {
        workers.push(NativeAcceptedWorker {
            invocation,
            cancelled,
            exited,
            thread: Some(thread),
            delayed: matches!(COMPOUND_MODE.get(), 15 | 16),
            remaining_cancel_polls: usize::from(matches!(COMPOUND_MODE.get(), 15 | 16)),
            cleanup_observed: false,
            foreign_terminal_pending: COMPOUND_MODE.get() == 16,
        })
    });
}
enum NativeWorkerPoll {
    Absent,
    Pending,
    Update(tilo_actions::InvocationUpdate),
}
fn poll_native_accepted_worker(invocation: tilo_actions::InvocationId) -> NativeWorkerPoll {
    NATIVE_ACCEPTED_WORKERS.with_borrow_mut(|workers| {
        let Some(worker) = workers
            .iter_mut()
            .find(|worker| worker.invocation == invocation)
        else {
            return NativeWorkerPoll::Absent;
        };
        if worker.remaining_cancel_polls > 0 {
            worker.remaining_cancel_polls -= 1;
            return NativeWorkerPoll::Pending;
        }
        if worker.foreign_terminal_pending {
            worker.foreign_terminal_pending = false;
            return NativeWorkerPoll::Update(tilo_actions::InvocationUpdate::Succeeded {
                invocation: tilo_actions::InvocationId::new(),
                output: tilo_actions::ActionOutput::default(),
            });
        }
        worker
            .cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        if worker.delayed {
            match worker.exited.try_recv() {
                Ok(true) => {
                    worker.thread.take().unwrap().join().unwrap();
                    worker.cleanup_observed = true;
                    // A late successful handler answer must never replace the
                    // original contract refusal; it only releases cleanup custody.
                    NativeWorkerPoll::Update(tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput::default(),
                    })
                }
                Ok(false) => panic!("worker exited without observing cancellation"),
                Err(std::sync::mpsc::TryRecvError::Empty) => NativeWorkerPoll::Pending,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    panic!("worker lost cleanup receipt")
                }
            }
        } else {
            NativeWorkerPoll::Update(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: None,
            })
        }
    })
}

#[test]
fn native_effect_contract_violation_cancels_exact_accepted_worker_before_failure() {
    let mut fixture = ActionFixture::broker();
    let live = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let target = fixture.target(0, "follow-up");
    COMPOUND_MODE.set(14);
    let (first_delivery, first_request) = native_effect_delivery(&fixture, live.clone());
    let first_scope = native_effect_scope(&fixture, &first_delivery, &first_request);
    fixture
        .app
        .invoke_bound_target_with_broker(
            &target,
            &ResourceInvocation::new(&first_request.address, &first_request.actor)
                .confirmed(true)
                .broker_admitted(true)
                .invocation(Some(first_request.invocation))
                .scope(Some(&first_scope)),
            None,
        )
        .unwrap();
    COMPOUND_MODE.set(13);
    let (delivery, request) = native_effect_delivery(&fixture, live);
    let scope = native_effect_scope(&fixture, &delivery, &request);
    let result = fixture.app.invoke_bound_target_with_broker(
        &target,
        &ResourceInvocation::new(&request.address, &request.actor)
            .confirmed(true)
            .broker_admitted(true)
            .invocation(Some(request.invocation))
            .scope(Some(&scope)),
        None,
    );
    COMPOUND_MODE.set(0);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("changes were refused")
    );
    NATIVE_ACCEPTED_WORKERS.with_borrow_mut(|workers| {
        assert_eq!(workers.len(), 2);
        assert_eq!(workers[0].invocation, first_request.invocation);
        assert!(
            !workers[0]
                .cancelled
                .load(std::sync::atomic::Ordering::Acquire)
        );
        assert_eq!(workers[1].invocation, request.invocation);
        assert_eq!(
            workers[1]
                .exited
                .recv_timeout(std::time::Duration::from_secs(1)),
            Ok(true)
        );
        workers[1].thread.take().unwrap().join().unwrap();
    });
    assert!(
        first_scope
            .effect_scope()
            .unwrap()
            .with_effect(|| Ok(()))
            .is_ok()
    );
    assert!(scope.effect_scope().is_err());
    assert!(
        fixture
            .records
            .read(&target.binding.checkout.id, |doc| doc
                .get(ROOT, "accepted-unauthorized")
                .unwrap()
                .is_none())
            .unwrap()
    );
    crate::bound_dispatch::poll_resource_operation(
        fixture.app.root_widget.as_mut(),
        &fixture.app.state.doc,
        &target,
        first_request.invocation,
        true,
    )
    .unwrap();
    NATIVE_ACCEPTED_WORKERS.with_borrow_mut(|workers| workers.clear());
}

#[test]
fn native_effect_refusal_keeps_delayed_worker_in_existing_cleanup_polling() {
    COMPOUND_MODE.set(14);
    let mut fixture = ActionFixture::broker();
    let live = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let target = fixture.target(0, "follow-up");
    let (unrelated_delivery, unrelated_request) = native_effect_delivery(&fixture, live.clone());
    let unrelated_scope = native_effect_scope(&fixture, &unrelated_delivery, &unrelated_request);
    fixture
        .app
        .invoke_bound_target_with_broker(
            &target,
            &ResourceInvocation::new(&unrelated_request.address, &unrelated_request.actor)
                .confirmed(true)
                .broker_admitted(true)
                .invocation(Some(unrelated_request.invocation))
                .scope(Some(&unrelated_scope)),
            None,
        )
        .unwrap();
    COMPOUND_MODE.set(15);
    let (delivery, request) = native_effect_delivery(&fixture, live);
    let scope = native_effect_scope(&fixture, &delivery, &request);
    let refused = fixture.app.invoke_bound_target_with_broker(
        &target,
        &ResourceInvocation::new(&request.address, &request.actor)
            .confirmed(true)
            .broker_admitted(true)
            .invocation(Some(request.invocation))
            .scope(Some(&scope)),
        None,
    );
    COMPOUND_MODE.set(0);
    assert!(
        refused
            .unwrap_err()
            .to_string()
            .contains("changes were refused")
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while std::time::Instant::now() < deadline {
        fixture.app.poll_directed_invocations();
        if NATIVE_ACCEPTED_WORKERS.with_borrow(|workers| workers[1].cleanup_observed) {
            break;
        }
        std::thread::park_timeout(std::time::Duration::from_millis(5));
    }
    NATIVE_ACCEPTED_WORKERS.with_borrow_mut(|workers| {
        assert_eq!(workers[1].invocation, request.invocation);
        assert!(
            workers[1].cleanup_observed,
            "existing lifecycle lost the cancellation-pending accepted worker"
        );
        assert!(workers[1].thread.is_none());
        assert_eq!(workers[0].invocation, unrelated_request.invocation);
        assert!(
            !workers[0]
                .cancelled
                .load(std::sync::atomic::Ordering::Acquire)
        );
        workers.clear();
    });
    assert!(
        !fixture
            .app
            .actions
            .bound_invocations
            .contains_key(&request.invocation)
    );
    assert!(
        !fixture
            .app
            .actions
            .directed_invocations
            .contains_key(&request.invocation)
    );
    assert!(scope.effect_scope().is_err());
    assert!(
        unrelated_scope
            .effect_scope()
            .unwrap()
            .with_effect(|| Ok(()))
            .is_ok()
    );
    assert!(
        fixture
            .app
            .actions
            .bound_invocations
            .contains_key(&unrelated_request.invocation)
    );
    assert!(
        fixture
            .records
            .read(&target.binding.checkout.id, |doc| doc
                .get(ROOT, "accepted-unauthorized")
                .unwrap()
                .is_none())
            .unwrap()
    );
}

#[test]
fn native_effect_pending_cleanup_survives_a_foreign_record_loan() {
    COMPOUND_MODE.set(15);
    let mut fixture = ActionFixture::with_same_record(false);
    fixture.app.collaboration.manager = super::semantic_tree_app_tests::admitted_delivery_manager();
    fixture.app.actions.bound_broker_route_for_test =
        Some(crate::collaboration::PrimaryRouteWitness {
            route: fixture.bindings[0].broker_route().unwrap(),
            epoch: uuid::Uuid::new_v4(),
        });
    let live = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (delivery, request) = native_effect_delivery(&fixture, live);
    let scope = native_effect_scope(&fixture, &delivery, &request);
    let target = fixture.target(0, "follow-up");
    assert!(
        fixture
            .app
            .invoke_bound_target_with_broker(
                &target,
                &ResourceInvocation::new(&request.address, &request.actor)
                    .confirmed(true)
                    .broker_admitted(true)
                    .invocation(Some(request.invocation))
                    .scope(Some(&scope)),
                None
            )
            .is_err()
    );
    COMPOUND_MODE.set(0);
    assert!(
        fixture
            .app
            .actions
            .refused_bound_invocations
            .contains(&request.invocation)
    );
    let foreign = fixture.bindings[1].clone();
    let mut outer = automerge::AutoCommit::new();
    foreign
        .with_document(&mut outer, |_, _| {
            fixture.app.poll_refused_bound_invocations();
        })
        .unwrap();
    assert!(
        fixture
            .app
            .actions
            .refused_bound_invocations
            .contains(&request.invocation),
        "temporary cross-record admission is not target retirement"
    );
    assert!(
        fixture
            .app
            .actions
            .bound_invocations
            .contains_key(&request.invocation)
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while std::time::Instant::now() < deadline {
        fixture.app.poll_refused_bound_invocations();
        if NATIVE_ACCEPTED_WORKERS.with_borrow(|workers| workers[0].cleanup_observed) {
            break;
        }
        std::thread::park_timeout(std::time::Duration::from_millis(5));
    }
    NATIVE_ACCEPTED_WORKERS.with_borrow_mut(|workers| {
        assert!(workers[0].cleanup_observed);
        workers.clear();
    });
}

#[test]
fn native_effect_foreign_terminal_preserves_exact_component_closed_cleanup() {
    COMPOUND_MODE.set(16);
    let mut fixture = ActionFixture::broker();
    let live = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (delivery, request) = native_effect_delivery(&fixture, live);
    let scope = native_effect_scope(&fixture, &delivery, &request);
    let target = fixture.target(0, "follow-up");
    assert!(
        fixture
            .app
            .invoke_bound_target_with_broker(
                &target,
                &ResourceInvocation::new(&request.address, &request.actor)
                    .confirmed(true)
                    .broker_admitted(true)
                    .invocation(Some(request.invocation))
                    .scope(Some(&scope)),
                None
            )
            .is_err()
    );
    COMPOUND_MODE.set(0);
    fixture.app.poll_refused_bound_invocations();
    assert!(
        fixture
            .app
            .actions
            .refused_bound_invocations
            .contains(&request.invocation)
    );
    fixture.app.root_widget = Box::new(Column::new());
    NATIVE_ACCEPTED_WORKERS.with_borrow_mut(|workers| {
        assert_eq!(workers[0].invocation, request.invocation);
        assert_eq!(
            workers[0]
                .exited
                .recv_timeout(std::time::Duration::from_secs(1)),
            Ok(true),
            "foreign terminal discarded original CLOSED cancellation custody"
        );
        workers[0].thread.take().unwrap().join().unwrap();
        workers.clear();
    });
    assert!(scope.effect_scope().is_err());
    fixture.app.poll_refused_bound_invocations();
    assert!(
        !fixture
            .app
            .actions
            .refused_bound_invocations
            .contains(&request.invocation)
    );
}
