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
    saves: Rc<Cell<usize>>,
    body: Rc<RefCell<String>>,
    wake: Rc<RefCell<Option<crate::WakeHandle>>>,
    late: Rc<RefCell<Option<crate::WakeHandle>>>,
    panic: Rc<Cell<bool>>,
    order: Rc<RefCell<Vec<u64>>>,
}
struct OwnerProbe {
    seen: Seen,
    id: u64,
}
impl Widget for OwnerProbe {
    crate::testing::inert_widget_methods!(
        leaf_layout,
        apply_layout,
        draw,
        get_rect,
        name = "shared-owner-probe"
    );
    fn sync(&mut self, model: &mut crate::widget::AppModel, path: &crate::widget::ModelPath) {
        let (doc, _) = crate::doc::document(model, path);
        assert!(crate::record::identity(doc).is_some());
        self.seen.syncs.set(self.seen.syncs.get() + 1);
        self.seen.order.borrow_mut().push(self.id);
        if self.seen.panic.replace(false) {
            panic!("real owner callback failed");
        }
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
    Ok(AppComponent::new(
        context,
        Box::new(OwnerProbe { seen, id }),
        tilo_actions::ActionRegistry::new(),
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
    let mut bound = BoundAppWidget { component };
    bound.sync(&mut AutoCommit::new(), &ROOT);
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
    crate::testing::inert_widget_methods!(
        leaf_layout,
        apply_layout,
        draw,
        get_rect,
        name = "hidden-owner-host"
    );
    fn sync(&mut self, _: &mut crate::widget::AppModel, _: &crate::widget::ModelPath) {}
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
