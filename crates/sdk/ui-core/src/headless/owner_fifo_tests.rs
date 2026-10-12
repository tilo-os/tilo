//! Real mounted owners through the host's retained exact-owner service route.
use super::*;
use crate::application::{AppComponent, HostContext, LaunchTarget, RestoreInput};
use crate::application_test_support::{self as probe, ProbeHost};
use crate::bound_dispatch::{BoundDispatchError, ComponentRequest};
use crate::owner_wake::OwnerWakeQueue;
use crate::widget::{AppKind, AppWidget, BoundAppWidget, IconDescriptor, Widget};
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Seen {
    syncs: Rc<Cell<usize>>,
    updates: Rc<Cell<usize>>,
    drain_close_gate: Rc<RefCell<Option<Rc<Cell<bool>>>>>,
    drained: Rc<Cell<bool>>,
    saves: Rc<Cell<usize>>,
    write_next: Rc<Cell<bool>>,
    body: Rc<RefCell<String>>,
    wake: Rc<RefCell<Option<crate::WakeHandle>>>,
    late: Rc<RefCell<Option<crate::WakeHandle>>>,
    panic: Rc<Cell<bool>>,
    order: Rc<RefCell<Vec<u64>>>,
}
struct OwnerProbe {
    seen: Seen,
    id: u64,
    actions: crate::action_host::ActionHost,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct BackgroundWrite;
impl tilo_actions::Action for BackgroundWrite {
    fn apply(&self, doc: &mut AutoCommit) -> Result<(), tilo_actions::ActionError> {
        doc.put(ROOT, "body", "background model write")?;
        Ok(())
    }
}
impl Widget for OwnerProbe {
    crate::provides!(
        dyn crate::undo::UndoPoints,
        dyn crate::undo::CommittedActions
    );
    fn update(&mut self, _: &crate::widget::AppModel, _: &crate::widget::ModelPath) -> bool {
        self.seen.updates.set(self.seen.updates.get() + 1);
        false
    }
    crate::testing::inert_widget_methods!(leaf_layout, name = "shared-owner-probe");
    fn sync(&mut self, model: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        let (doc, _) = crate::doc::document_ref(model, path);
        assert!(crate::record::identity(doc).is_some());
        self.seen.syncs.set(self.seen.syncs.get() + 1);
        self.seen.order.borrow_mut().push(self.id);
        assert!(
            !self.seen.panic.replace(false),
            "real owner callback failed"
        );
        *self.seen.body.borrow_mut() = doc
            .get(ROOT, "body")
            .unwrap()
            .unwrap()
            .0
            .as_str()
            .unwrap()
            .into();
        if let Some(wake) = self.seen.late.borrow_mut().take() {
            wake.wake();
        }
    }
    fn settle(&mut self, edits: &mut crate::widget::Edits<'_>, _: &automerge::ObjId) {
        if self.seen.write_next.replace(false) {
            edits
                .apply(
                    &mut self.actions,
                    "probe.background",
                    &BackgroundWrite,
                    tilo_actions::ActionContext::default(),
                )
                .unwrap();
        }
    }
}
impl crate::undo::CommittedActions for OwnerProbe {
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions
            .take_committed()
            .or_else(|| self.seen.drain_close_gate.borrow().is_some().then(Vec::new))
    }
}
impl crate::undo::UndoPoints for OwnerProbe {
    fn get_state(&self) -> Option<String> {
        if let Some(gate) = self.seen.drain_close_gate.borrow_mut().take() {
            self.seen.drained.set(true);
            gate.set(true);
        }
        None
    }
}
impl AppWidget for OwnerProbe {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &str {
        "Owner FIFO probe"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("Q")
    }
    fn permission_hint(&self) -> Option<AppKind> {
        None
    }
    fn record_save_status(&mut self, _: crate::record::RecordSaveStatus) {
        self.seen.saves.set(self.seen.saves.get() + 1);
    }
}
thread_local! { static ORDER: RefCell<Rc<RefCell<Vec<u64>>>> = RefCell::new(Rc::new(RefCell::new(Vec::new()))); }
thread_local! { static NEXT: RefCell<Option<(Seen, u64, bool)>> = const { RefCell::new(None) }; }
fn factory(
    mut context: HostContext,
    _: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, crate::launch::LaunchError> {
    context
        .initialize_new_record(|doc| {
            doc.put(ROOT, "body", "canonical owner body")
                .map_err(|e| e.to_string())?;
            doc.commit();
            Ok(())
        })
        .map_err(crate::launch::LaunchError::Record)?;
    let (seen, id, _declared) = NEXT.with_borrow_mut(|next| next.take().unwrap());
    *seen.wake.borrow_mut() = Some(context.wake_handle());
    let mut registry = tilo_actions::ActionRegistry::new();
    registry.register::<BackgroundWrite>("probe.background");
    Ok(AppComponent::new(
        context,
        Box::new(OwnerProbe {
            seen,
            id,
            actions: crate::action_host::ActionHost::new(),
        }),
        registry,
    ))
}
fn prepared(
    host: &ProbeHost,
    queue: &OwnerWakeQueue,
    id: u64,
    declared: bool,
) -> (BoundAppWidget, Seen, Arc<Mutex<Vec<bool>>>) {
    let seen = Seen {
        order: ORDER.with_borrow(Clone::clone),
        ..Seen::default()
    };
    NEXT.with_borrow_mut(|next| *next = Some((seen.clone(), id, declared)));
    let definition = probe::definition(factory);
    let context = host.prepare(&definition, id, &LaunchTarget::New).unwrap();
    let mut component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let received = events.clone();
    component
        .mount_with_owner_queue(queue, move |event| {
            received.lock().unwrap().push(event.requires_sync())
        })
        .unwrap();
    let mut bound = BoundAppWidget::new(component);
    bound.sync(&AutoCommit::new(), &ROOT);
    assert_eq!(seen.syncs.get(), 1);
    (bound, seen, events)
}
// All owners are genuinely retained and closed. This host deliberately does
// not present or synchronize them in its ordinary visible sync/layout pass.
struct HiddenOwners {
    owners: Vec<BoundAppWidget>,
    visits: Rc<Cell<usize>>,
    refuse: Rc<Cell<bool>>,
}
impl Widget for HiddenOwners {
    crate::testing::inert_widget_methods!(leaf_layout, name = "hidden-owner-host");
    fn sync(&mut self, _: &crate::widget::AppModel, _: &crate::widget::ModelPath) {}
    fn children(&self) -> Vec<&dyn Widget> {
        self.owners.iter().map(|o| o as &dyn Widget).collect()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.owners
            .iter_mut()
            .map(|o| o as &mut dyn Widget)
            .collect()
    }
    // No retained override: preserve the original custom visitor fallback.
    fn visit_components(
        &mut self,
        request: &mut ComponentRequest<'_>,
    ) -> Result<(), BoundDispatchError> {
        self.visits.set(self.visits.get() + 1);
        if self.refuse.get() {
            return Err(BoundDispatchError::CrossRecordReentry);
        }
        for owner in &mut self.owners {
            owner.visit_components(request)?;
            if request.target.is_some() && request.found {
                break;
            }
        }
        Ok(())
    }
}
fn frame(
    queue: &OwnerWakeQueue,
    owners: Vec<BoundAppWidget>,
) -> (FrameHarness, Rc<Cell<usize>>, Rc<Cell<bool>>) {
    let visits = Rc::new(Cell::new(0));
    let refuse = Rc::new(Cell::new(false));
    let inputs = probe::runner_inputs();
    let mut frame = FrameHarness::new_with_controlled_inputs(
        State::new(),
        Box::new(HiddenOwners {
            owners,
            visits: visits.clone(),
            refuse: refuse.clone(),
        }),
        (320, 180),
        inputs.theme,
        inputs.text_renderer,
        probe::author(),
    );
    frame.attach_owner_wake_queue(queue.clone()).unwrap();
    // Original initialization notifications are already reconciled by each
    // owner's actual canonical sync above; finish their host intake explicitly.
    frame.service_pending_owner_models().unwrap();
    (frame, visits, refuse)
}
fn close(frame: &mut FrameHarness, expected: usize) {
    frame.close_components();
    let reports = frame.take_component_close_reports();
    assert_eq!(reports.len(), expected);
    assert!(
        reports.iter().all(|r| r.record_error.is_none()
            && r.view_error.is_none()
            && r.teardown_error.is_none())
    );
}
#[test]
fn one_dirty_hidden_owner_of_110_enters_only_its_actual_model_and_save_service_without_paint() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let owners = (0..110)
        .map(|i| prepared(&host, &queue, 5000 + i, true))
        .collect::<Vec<_>>();
    let seen = owners
        .iter()
        .map(|(_, seen, _)| seen.clone())
        .collect::<Vec<_>>();
    let (mut frame, _, _) = frame(
        &queue,
        owners.into_iter().map(|(owner, _, _)| owner).collect(),
    );
    let counts = seen
        .iter()
        .map(|s| (s.syncs.get(), s.saves.get()))
        .collect::<Vec<_>>();
    let layouts = frame.relayout_frames;
    seen[73].wake.borrow().as_ref().unwrap().wake();
    let serviced = frame.service_pending_owner_models().unwrap();
    let after = seen
        .iter()
        .map(|s| (s.syncs.get(), s.saves.get()))
        .collect::<Vec<_>>();
    close(&mut frame, 110);
    assert_eq!(
        serviced, 1,
        "RED: genuine queued hidden owner has no mounted host consumer"
    );
    assert_eq!(
        frame.relayout_frames, layouts,
        "model service must not depend on a paint/layout pass"
    );
    for i in 0..110 {
        assert_eq!(
            after[i],
            if i == 73 {
                (counts[i].0 + 1, counts[i].1 + 1)
            } else {
                counts[i]
            }
        );
    }
}
#[test]
fn first_dirty_fifo_and_reason_or_preserve_every_original_sink_event() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (a, sa, ea) = prepared(&host, &queue, 900, true);
    let (b, sb, eb) = prepared(&host, &queue, 901, true);
    let order = Rc::new(RefCell::new(Vec::new()));
    // Same actual callback order is observed independently per real owner.
    let (mut frame, _, _) = frame(&queue, vec![a, b]);
    ea.lock().unwrap().clear();
    eb.lock().unwrap().clear();
    sb.order.borrow_mut().clear();
    sa.order.borrow_mut().clear();
    sb.wake.borrow().as_ref().unwrap().notify_events();
    sa.wake.borrow().as_ref().unwrap().wake();
    sb.wake.borrow().as_ref().unwrap().wake();
    let serviced = frame.service_pending_owner_models().unwrap();
    // Fresh queue request order is checked by an independent second batch.
    sb.wake.borrow().as_ref().unwrap().wake();
    sa.wake.borrow().as_ref().unwrap().wake();
    let requests = queue.take_pending();
    for request in &requests {
        order.borrow_mut().push(request.binding().instance_id);
    }
    for request in requests {
        assert!(queue.requeue(request));
    }
    assert_eq!(&*order.borrow(), &[901, 900]);
    assert_eq!(&*ea.lock().unwrap(), &[true, true]);
    assert_eq!(&*eb.lock().unwrap(), &[false, true, true]);
    assert_eq!(serviced, 2);
    assert_eq!(
        &*sa.order.borrow(),
        &[901, 900],
        "actual model callbacks follow first-dirty order"
    );
    assert!(Rc::ptr_eq(&sa.order, &sb.order));
    frame.service_pending_owner_models().unwrap();
    close(&mut frame, 2);
}
#[test]
fn wake_raised_by_actual_service_remains_in_next_fifo_snapshot() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (a, sa, _) = prepared(&host, &queue, 910, true);
    let (b, sb, _) = prepared(&host, &queue, 911, true);
    let (mut frame, _, _) = frame(&queue, vec![a, b]);
    let b_before = sb.syncs.get();
    *sa.late.borrow_mut() = Some(sb.wake.borrow().as_ref().unwrap().clone());
    sa.wake.borrow().as_ref().unwrap().wake();
    assert_eq!(frame.service_pending_owner_models().unwrap(), 1);
    assert!(queue.has_pending());
    assert_eq!(sb.syncs.get(), b_before);
    assert_eq!(frame.service_pending_owner_models().unwrap(), 1);
    assert_eq!(sb.syncs.get(), b_before + 1);
    assert!(!queue.has_pending());
    close(&mut frame, 2);
}
#[test]
fn busy_canonical_admission_requeues_taken_batch_without_touching_later_owners() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (a, sa, _) = prepared(&host, &queue, 920, true);
    let id = a.component.record().id.clone();
    let (b, sb, _) = prepared(&host, &queue, 921, true);
    let (mut frame, _, _) = frame(&queue, vec![a, b]);
    let before = (sa.syncs.get(), sb.syncs.get());
    sa.wake.borrow().as_ref().unwrap().wake();
    sb.wake.borrow().as_ref().unwrap().wake();
    host.records
        .with_action_state(&id, |_, _, _| {
            assert!(matches!(
                frame.service_pending_owner_models(),
                Err(BoundDispatchError::Record(
                    crate::record::RecordOpenError::Busy
                ))
            ))
        })
        .unwrap();
    assert_eq!((sa.syncs.get(), sb.syncs.get()), before);
    assert!(queue.has_pending());
    assert_eq!(frame.service_pending_owner_models().unwrap(), 2);
    close(&mut frame, 2);
}
#[test]
fn unknown_original_custom_visitor_refusal_keeps_full_batch_for_explicit_retry() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (a, seen, _) = prepared(&host, &queue, 930, true);
    let (mut frame, visits, refuse) = frame(&queue, vec![a]);
    let before = seen.syncs.get();
    let entered = visits.get();
    refuse.set(true);
    seen.wake.borrow().as_ref().unwrap().wake();
    assert!(matches!(
        frame.service_pending_owner_models(),
        Err(BoundDispatchError::CrossRecordReentry)
    ));
    assert_eq!(visits.get(), entered + 1);
    assert_eq!(seen.syncs.get(), before);
    assert!(queue.has_pending());
    refuse.set(false);
    assert_eq!(frame.service_pending_owner_models().unwrap(), 1);
    close(&mut frame, 1);
}
#[test]
fn service_unwind_requeues_current_and_remaining_work_after_original_loans_unwind() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (a, sa, _) = prepared(&host, &queue, 940, true);
    let id = a.component.record().id.clone();
    let (b, sb, _) = prepared(&host, &queue, 941, true);
    let (mut frame, _, _) = frame(&queue, vec![a, b]);
    let before = sb.syncs.get();
    sa.panic.set(true);
    sa.wake.borrow().as_ref().unwrap().wake();
    sb.wake.borrow().as_ref().unwrap().wake();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || frame.service_pending_owner_models()
        ))
        .is_err()
    );
    assert!(queue.has_pending());
    assert_eq!(sb.syncs.get(), before);
    host.records
        .read(&id, |doc| assert!(crate::record::identity(doc).is_some()))
        .unwrap();
    assert_eq!(frame.service_pending_owner_models().unwrap(), 2);
    close(&mut frame, 2);
}

#[test]
fn same_owner_callback_late_wake_is_not_consumed_by_snapshot_completion() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (owner, seen, _) = prepared(&host, &queue, 9601, false);
    let (mut frame, _, _) = frame(&queue, vec![owner]);
    let before = seen.syncs.get();
    let wake = seen.wake.borrow().as_ref().unwrap().clone();
    *seen.late.borrow_mut() = Some(wake.clone());
    wake.wake();
    assert_eq!(frame.service_pending_owner_models().unwrap(), 1);
    assert_eq!(seen.syncs.get(), before + 1);
    assert!(queue.has_pending());
    assert_eq!(frame.service_pending_owner_models().unwrap(), 1);
    assert_eq!(seen.syncs.get(), before + 2);
    assert!(!queue.has_pending());
    close(&mut frame, 1);
}
#[test]
fn event_only_opaque_owner_retains_original_model_and_save_service() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (owner, seen, events) = prepared(&host, &queue, 9602, false);
    let (mut frame, _, _) = frame(&queue, vec![owner]);
    let before = (seen.syncs.get(), seen.saves.get());
    events.lock().unwrap().clear();
    seen.wake.borrow().as_ref().unwrap().notify_events();
    assert_eq!(frame.service_pending_owner_models().unwrap(), 1);
    assert_eq!(
        (seen.syncs.get(), seen.saves.get()),
        (before.0 + 1, before.1 + 1)
    );
    assert_eq!(&*events.lock().unwrap(), &[false]);
    close(&mut frame, 1);
}

#[test]
fn update_drain_refusal_still_attempts_fresh_sync_and_retains_first_error() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (owner, seen, _) = prepared(&host, &queue, 5210, false);
    let binding = owner.component.callback_binding();
    let (mut frame, _, _) = frame(&queue, vec![owner]);
    let updates = seen.updates.get();
    let syncs = seen.syncs.get();
    let actor = frame.state.doc.get_actor().clone();
    *seen.drain_close_gate.borrow_mut() = Some(binding.closing.clone());
    seen.wake.borrow().as_ref().unwrap().wake();
    let probe = crate::bound_dispatch::document_admission_probe::Probe::new(&binding);
    let result = frame.service_pending_owner_models();
    let attempts = probe.attempts();
    drop(probe);
    let pending = queue.has_pending();
    let restored = frame.state.doc.get_actor() == &actor;
    let update_entered = seen.updates.get() == updates + 1;
    let sync_refused = seen.syncs.get() == syncs;
    // The metadata callback tested the actual closing gate; no Source close
    // occurred. Restore only this test condition before original strict close.
    binding.closing.set(false);
    close(&mut frame, 1);
    assert!(matches!(result, Err(BoundDispatchError::ClosedLifetime)));
    assert!(update_entered && seen.drained.get());
    assert_eq!(
        attempts,
        vec![false, true],
        "fresh sync admission follows nonpanic update/drain refusal"
    );
    assert!(
        sync_refused,
        "closed lifetime must not receive a sync callback"
    );
    assert!(pending && restored);
}

#[test]
fn failed_and_unvisited_snapshot_owners_precede_callback_late_owner() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (a, sa, _) = prepared(&host, &queue, 5220, false);
    let (b, sb, _) = prepared(&host, &queue, 5221, false);
    let (c, sc, _) = prepared(&host, &queue, 5222, false);
    let (d, sd, _) = prepared(&host, &queue, 5223, false);
    let (mut frame, _, _) = frame(&queue, vec![a, b, c, d]);
    *sa.late.borrow_mut() = Some(sd.wake.borrow().as_ref().unwrap().clone());
    sb.panic.set(true);
    for seen in [&sa, &sb, &sc] {
        seen.wake.borrow().as_ref().unwrap().wake();
    }
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        frame.service_pending_owner_models()
    }));
    let requests = queue.take_pending();
    let order = requests
        .iter()
        .map(|work| work.binding().instance_id)
        .collect::<Vec<_>>();
    for work in requests {
        assert!(queue.requeue(work));
    }
    let retry = frame.service_pending_owner_models();
    close(&mut frame, 4);
    assert!(failed.is_err());
    assert_eq!(
        order,
        vec![5221, 5222, 5223],
        "older failed/unvisited work retains first-dirty FIFO ahead of callback-late work"
    );
    assert_eq!(retry.unwrap(), 3);
}

fn background_gate(presentation: super::owner_background_work::Presentation) {
    use durable_store::DurableStore;
    let backend = Rc::new(durable_store::MemoryStore::new());
    let host = memory_host(backend.clone());
    let queue = OwnerWakeQueue::default();
    let (owner, seen, _) = prepared(&host, &queue, 5230, false);
    let checkout = owner.component.record().clone();
    let (mut frame, _, _) = frame(&queue, vec![owner]);
    let before = (seen.updates.get(), seen.syncs.get(), seen.saves.get());
    let layouts = frame.relayout_frames();
    seen.write_next.set(true);
    seen.wake.borrow().as_ref().unwrap().wake();
    let outcome =
        super::owner_background_work::run(presentation, || frame.service_pending_owner_models())
            .unwrap();
    let after = (seen.updates.get(), seen.syncs.get(), seen.saves.get());
    let retained = queue.has_pending();
    // The original writer offer and replication snapshot are requested only
    // after the actual model/record loans returned, before any frame/paint.
    host.records.try_flush().unwrap();
    let (bytes, _) = host.records.replication_snapshot(&checkout.id).unwrap();
    let forwarded = AutoCommit::load(&bytes)
        .unwrap()
        .get(ROOT, "body")
        .unwrap()
        .unwrap()
        .0
        .as_str()
        .unwrap()
        .to_owned();
    close(&mut frame, 1);
    let key = durable_store::StorageKey::record(probe::APP, probe::ROOM, checkout.id.storage_key());
    let mirror = Rc::new(durable_store::MemoryStore::new());
    mirror
        .hydrate(
            key,
            backend
                .read(&durable_store::StorageKey::record(
                    probe::APP,
                    probe::ROOM,
                    checkout.id.storage_key(),
                ))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
    let mut reopened_owners = crate::record::RecordOwners::with_store(mirror);
    let reopened = reopened_owners.open(probe::APP, probe::ROOM).unwrap();
    reopened.open_existing_record_checked(&checkout).unwrap();
    let persisted = reopened
        .read(&checkout.id, |doc| {
            doc.get(ROOT, "body")
                .unwrap()
                .unwrap()
                .0
                .as_str()
                .unwrap()
                .to_owned()
        })
        .unwrap();
    assert_eq!(forwarded, "background model write");
    assert_eq!(persisted, "background model write");
    assert_eq!(outcome.serviced, 1);
    assert!(!outcome.paint_available);
    assert_eq!(after, (before.0 + 1, before.1 + 1, before.2 + 1));
    assert_eq!(frame.relayout_frames(), layouts);
    assert!(!retained);
}
#[test]
fn background_hidden_owner_services_original_model_and_save_without_paint() {
    background_gate(super::owner_background_work::Presentation {
        hidden: true,
        suspended: false,
        gpu_ready: true,
    });
}
#[test]
fn background_suspended_owner_services_original_model_and_save_without_paint() {
    background_gate(super::owner_background_work::Presentation {
        hidden: false,
        suspended: true,
        gpu_ready: true,
    });
}
#[test]
fn background_gpu_none_owner_services_original_model_and_save_without_paint() {
    background_gate(super::owner_background_work::Presentation {
        hidden: false,
        suspended: false,
        gpu_ready: false,
    });
}
#[test]
fn background_refusal_retains_original_owner_for_retry_while_paint_unavailable() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (owner, seen, _) = prepared(&host, &queue, 5231, false);
    let id = owner.component.record().id.clone();
    let (mut frame, _, _) = frame(&queue, vec![owner]);
    seen.wake.borrow().as_ref().unwrap().wake();
    let p = super::owner_background_work::Presentation {
        hidden: true,
        suspended: true,
        gpu_ready: false,
    };
    let refused = host
        .records
        .with_action_state(&id, |_, _, _| {
            super::owner_background_work::run(p, || frame.service_pending_owner_models())
        })
        .unwrap();
    let retained = queue.has_pending();
    let retried = super::owner_background_work::run(p, || frame.service_pending_owner_models());
    close(&mut frame, 1);
    assert!(matches!(
        refused,
        Err(BoundDispatchError::Record(
            crate::record::RecordOpenError::Busy
        ))
    ));
    assert!(retained);
    assert_eq!(retried.unwrap().serviced, 1);
}

fn memory_host(backend: Rc<durable_store::MemoryStore>) -> ProbeHost {
    let mut host = ProbeHost::new();
    let mut owners = crate::record::RecordOwners::with_store(backend.clone());
    host.views = Rc::new(
        crate::application_view::InstanceViewStore::with_store(backend, probe::DEVICE, probe::ROOM)
            .unwrap(),
    );
    host.records = owners.open(probe::APP, probe::ROOM).unwrap();
    host.owners = owners;
    host
}
fn commit_accepted(
    backend: &durable_store::MemoryStore,
    mirror: &durable_store::MemoryStore,
) -> Vec<durable_store::PendingWrite> {
    use durable_store::DurableStore;
    let writes = backend.drain();
    for write in &writes {
        mirror
            .write(&write.key, write.op.clone(), write.writer.as_deref())
            .unwrap();
        backend.acknowledge(write.revision, Ok(())).unwrap();
    }
    writes
}
// The subject has already captured its write/status/reopen evidence before
// close. Drive only the original close report's queued durability recovery.
fn close_memory(
    frame: &mut FrameHarness,
    expected: usize,
    backend: &durable_store::MemoryStore,
    mirror: &durable_store::MemoryStore,
) {
    frame.close_components();
    let mut reports = frame.take_component_close_reports();
    assert_eq!(reports.len(), expected);
    commit_accepted(backend, mirror);
    for report in &mut reports {
        report.retry_record().unwrap();
        report.retry_view().unwrap();
    }
    commit_accepted(backend, mirror);
    assert!(
        reports.iter().all(|r| r.record_error.is_none()
            && r.view_error.is_none()
            && r.teardown_error.is_none())
    );
}
fn reopened_body(
    backend: Rc<durable_store::MemoryStore>,
    checkout: &crate::record::RecordCheckout,
) -> String {
    let mut owners = crate::record::RecordOwners::with_store(backend);
    let records = owners.open(probe::APP, probe::ROOM).unwrap();
    records.open_existing_record_checked(checkout).unwrap();
    records
        .read(&checkout.id, |doc| {
            doc.get(ROOT, "body")
                .unwrap()
                .unwrap()
                .0
                .as_str()
                .unwrap()
                .to_owned()
        })
        .unwrap()
}
#[test]
fn background_queued_writer_accepts_acknowledges_and_reopens_before_paint_or_close() {
    let backend = Rc::new(durable_store::MemoryStore::queued());
    let mirror = Rc::new(durable_store::MemoryStore::new());
    let host = memory_host(backend.clone());
    let queue = OwnerWakeQueue::default();
    let (owner, seen, _) = prepared(&host, &queue, 5233, false);
    let checkout = owner.component.record().clone();
    let (mut frame, _, _) = frame(&queue, vec![owner]);
    commit_accepted(&backend, &mirror);
    let layouts = frame.relayout_frames();
    seen.write_next.set(true);
    seen.wake.borrow().as_ref().unwrap().wake();
    let completed = frame.service_pending_owner_models();
    let pending_model = queue.has_pending();
    let before_ack = host.records.save_status(&checkout.id).unwrap();
    // Commit/ACK only bytes the production model service already offered.
    // This never calls flush, tick, snapshot or a document mutation.
    let writes = commit_accepted(&backend, &mirror);
    let after_ack = host.records.save_status(&checkout.id).unwrap();
    let reopened = reopened_body(mirror.clone(), &checkout);
    let no_paint = frame.relayout_frames() == layouts;
    close_memory(&mut frame, 1, &backend, &mirror);
    assert_eq!(completed.unwrap(), 1);
    assert!(
        !pending_model,
        "accepted pending writes never repeat model callbacks merely awaiting ACK"
    );
    assert_eq!(
        writes.len(),
        1,
        "original exact record was offered before paint/close"
    );
    assert_eq!(before_ack, crate::record::RecordSaveStatus::Saving);
    assert_eq!(after_ack, crate::record::RecordSaveStatus::Saved);
    assert_eq!(reopened, "background model write");
    assert!(no_paint);
}
#[test]
fn background_failed_ack_retries_checkpoint_on_real_new_wake_without_pending_callback_replay() {
    use durable_store::WriteOp;
    let backend = Rc::new(durable_store::MemoryStore::queued());
    let mirror = Rc::new(durable_store::MemoryStore::new());
    let host = memory_host(backend.clone());
    let queue = OwnerWakeQueue::default();
    let (owner, seen, _) = prepared(&host, &queue, 5234, false);
    let checkout = owner.component.record().clone();
    let (mut frame, _, _) = frame(&queue, vec![owner]);
    commit_accepted(&backend, &mirror);
    seen.write_next.set(true);
    seen.wake.borrow().as_ref().unwrap().wake();
    let first = frame.service_pending_owner_models();
    let writes = backend.drain();
    let offered_count = writes.len();
    for write in writes {
        backend
            .acknowledge(write.revision, Err("quota".into()))
            .unwrap();
    }
    let failed = host.records.save_status(&checkout.id).unwrap();
    let callbacks = seen.syncs.get();
    let empty = frame.service_pending_owner_models();
    let no_replay = callbacks == seen.syncs.get();
    seen.wake.borrow().as_ref().unwrap().wake();
    let retry = frame.service_pending_owner_models();
    let retry_pending = queue.has_pending();
    let replacement_before_ack = host.records.save_status(&checkout.id).unwrap();
    let replacement_callbacks = seen.syncs.get();
    let pending_empty = frame.service_pending_owner_models();
    let replacement_no_replay = seen.syncs.get() == replacement_callbacks;
    let retry_writes = commit_accepted(&backend, &mirror);
    let saved = host.records.save_status(&checkout.id).unwrap();
    let reopened = reopened_body(mirror.clone(), &checkout);
    close_memory(&mut frame, 1, &backend, &mirror);
    assert_eq!(first.unwrap(), 1);
    assert_eq!(
        offered_count, 1,
        "only already-offered bytes reach the host ACK subject"
    );
    assert!(matches!(failed, crate::record::RecordSaveStatus::Failed(_)));
    assert_eq!(empty.unwrap(), 0);
    assert!(no_replay);
    assert_eq!(retry.unwrap(), 1);
    assert!(!retry_pending);
    assert!(matches!(
        replacement_before_ack,
        crate::record::RecordSaveStatus::Failed(_)
    ));
    assert_eq!(pending_empty.unwrap(), 0);
    assert!(replacement_no_replay);
    assert_eq!(retry_writes.len(), 1);
    assert!(matches!(retry_writes[0].op, WriteOp::Replace(_)));
    assert_eq!(saved, crate::record::RecordSaveStatus::Saved);
    assert_eq!(reopened, "background model write");
}

struct RefusingStore {
    inner: Rc<durable_store::MemoryStore>,
    refuse: Cell<bool>,
}
impl durable_store::DurableStore for RefusingStore {
    fn namespace(&self) -> &str {
        self.inner.namespace()
    }
    fn read(
        &self,
        key: &durable_store::StorageKey,
    ) -> Result<Option<durable_store::Stored>, durable_store::StoreError> {
        self.inner.read(key)
    }
    fn write(
        &self,
        key: &durable_store::StorageKey,
        op: durable_store::WriteOp,
        writer: Option<&[u8]>,
    ) -> Result<durable_store::Revision, durable_store::StoreError> {
        if self.refuse.get() {
            return Err(durable_store::StoreError::Failed {
                context: "subject submit".into(),
                message: "quota".into(),
            });
        }
        self.inner.write(key, op, writer)
    }
    fn durability(
        &self,
        key: &durable_store::StorageKey,
        revision: durable_store::Revision,
    ) -> durable_store::Durability {
        self.inner.durability(key, revision)
    }
    fn flush(
        &self,
        key: Option<&durable_store::StorageKey>,
    ) -> Result<(), durable_store::StoreError> {
        self.inner.flush(key)
    }
    fn lease(
        &self,
        key: &durable_store::StorageKey,
    ) -> Result<durable_store::Lease, durable_store::StoreError> {
        self.inner.lease(key)
    }
    fn lease_shared(
        &self,
        key: &durable_store::StorageKey,
    ) -> Result<durable_store::SharedLease, durable_store::StoreError> {
        self.inner.lease_shared(key)
    }
    fn claim(
        &self,
        key: &durable_store::StorageKey,
        claimant: &str,
    ) -> Result<bool, durable_store::StoreError> {
        self.inner.claim(key, claimant)
    }
}
#[test]
fn background_synchronous_submit_refusal_retains_owner_then_real_retry_offers_checkpoint() {
    let inner = Rc::new(durable_store::MemoryStore::queued());
    let mirror = Rc::new(durable_store::MemoryStore::new());
    let backend = Rc::new(RefusingStore {
        inner: inner.clone(),
        refuse: Cell::new(false),
    });
    let mut host = ProbeHost::new();
    let mut owners = crate::record::RecordOwners::with_store(backend.clone());
    host.views = Rc::new(
        crate::application_view::InstanceViewStore::with_store(
            backend.clone(),
            probe::DEVICE,
            probe::ROOM,
        )
        .unwrap(),
    );
    host.records = owners.open(probe::APP, probe::ROOM).unwrap();
    host.owners = owners;
    let queue = OwnerWakeQueue::default();
    let (owner, seen, _) = prepared(&host, &queue, 5235, false);
    let checkout = owner.component.record().clone();
    let (mut frame, _, _) = frame(&queue, vec![owner]);
    commit_accepted(&inner, &mirror);
    backend.refuse.set(true);
    seen.write_next.set(true);
    seen.wake.borrow().as_ref().unwrap().wake();
    let refused = frame.service_pending_owner_models_report();
    let retained = queue.has_pending();
    let no_accepted = inner.drain().is_empty();
    let failed = host.records.save_status(&checkout.id).unwrap();
    backend.refuse.set(false);
    let retried = frame.service_pending_owner_models_report();
    let pending = queue.has_pending();
    let writes = commit_accepted(&inner, &mirror);
    let reopened = reopened_body(mirror.clone(), &checkout);
    close_memory(&mut frame, 1, &inner, &mirror);
    assert!(refused.attempted);
    assert_eq!(refused.completed, 0);
    assert!(refused.result.is_err());
    assert!(retained);
    assert!(no_accepted);
    assert!(matches!(failed, crate::record::RecordSaveStatus::Failed(_)));
    assert!(retried.attempted);
    assert_eq!(retried.completed, 1);
    assert!(retried.result.is_ok());
    assert!(!pending);
    assert_eq!(writes.len(), 1);
    assert!(writes[0].op.is_replace());
    assert_eq!(reopened, "background model write");
}
#[test]
fn background_partial_batch_report_keeps_accepted_peer_bytes_before_later_busy_refusal() {
    let backend = Rc::new(durable_store::MemoryStore::queued());
    let mirror = Rc::new(durable_store::MemoryStore::new());
    let host = memory_host(backend.clone());
    let other = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (a, sa, _) = prepared(&host, &queue, 5236, false);
    let checkout = a.component.record().clone();
    let (b, sb, _) = prepared(&other, &queue, 5237, false);
    let bid = b.component.record().id.clone();
    let (mut frame, _, _) = frame(&queue, vec![a, b]);
    commit_accepted(&backend, &mirror);
    sa.write_next.set(true);
    sa.wake.borrow().as_ref().unwrap().wake();
    sb.wake.borrow().as_ref().unwrap().wake();
    let report = other
        .records
        .with_action_state(&bid, |_, _, _| frame.service_pending_owner_models_report())
        .unwrap();
    let accepted = commit_accepted(&backend, &mirror);
    let reopened = reopened_body(mirror.clone(), &checkout);
    let retry = frame.service_pending_owner_models_report();
    let empty = frame.service_pending_owner_models_report();
    close_memory(&mut frame, 2, &backend, &mirror);
    assert!(report.attempted);
    assert_eq!(report.completed, 1);
    assert!(matches!(
        report.result,
        Err(BoundDispatchError::Record(
            crate::record::RecordOpenError::Busy
        ))
    ));
    assert_eq!(accepted.len(), 1);
    assert_eq!(reopened, "background model write");
    assert!(retry.attempted);
    assert_eq!(retry.completed, 1);
    assert!(retry.result.is_ok());
    assert!(!empty.attempted);
    assert_eq!(empty.completed, 0);
    assert!(empty.result.is_ok());
}
#[test]
fn background_exact_offer_never_settles_unrelated_same_store_typing() {
    let backend = Rc::new(durable_store::MemoryStore::queued());
    let mirror = Rc::new(durable_store::MemoryStore::new());
    let host = memory_host(backend.clone());
    let queue = OwnerWakeQueue::default();
    let (a, sa, _) = prepared(&host, &queue, 5238, false);
    let aid = a.component.record().id.clone();
    let (b, _, _) = prepared(&host, &queue, 5239, false);
    let bid = b.component.record().id.clone();
    let (mut frame, _, _) = frame(&queue, vec![a, b]);
    commit_accepted(&backend, &mirror);
    host.records
        .with(&bid, |doc| {
            doc.put(ROOT, "body", "unrelated unfinished typing")
                .unwrap()
        })
        .unwrap();
    let before = host
        .records
        .read(&bid, |doc| {
            (doc.base_heads(), doc.pending_ops(), doc.get_actor().clone())
        })
        .unwrap();
    sa.write_next.set(true);
    sa.wake.borrow().as_ref().unwrap().wake();
    let serviced = frame.service_pending_owner_models();
    let after = host
        .records
        .read(&bid, |doc| {
            (doc.base_heads(), doc.pending_ops(), doc.get_actor().clone())
        })
        .unwrap();
    let writes = commit_accepted(&backend, &mirror);
    close_memory(&mut frame, 2, &backend, &mirror);
    assert!(before.1 > 0);
    assert_eq!(serviced.unwrap(), 1);
    assert_eq!(before, after);
    assert_eq!(writes.len(), 1);
    assert_eq!(
        writes[0].key,
        durable_store::StorageKey::record(probe::APP, probe::ROOM, aid.storage_key())
    );
}
