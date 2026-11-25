//! The owner models a worker wakes for the components a driven app hosts: a save-progress wake
//! shows the status its owner does not already show and nothing else, a status change rebuilds no
//! layout tree, an idle pass serves a woken owner so the window rests, and a capture of a resting
//! window runs no frame.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixture: a failure is the assertion"
)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use automerge::{AutoCommit, ROOT, transaction::Transactable};

use crate::application::{AppComponent, LaunchTarget, ProcessContext, RestoreInput};
use crate::application_test_support::{self as probe, ProbeHarness};
use crate::bound_dispatch::{BoundDispatchError, ComponentRequest};
use crate::headless::Driver;
use crate::owner_wake::OwnerWakeQueue;
use crate::state::State;
use crate::widget::{AppKind, AppWidget, BoundAppWidget, IconDescriptor, Widget};

#[derive(Clone, Default)]
struct Seen {
    syncs: Rc<Cell<usize>>,
    updates: Rc<Cell<usize>>,
    saves: Rc<Cell<usize>>,
    saved: Rc<Cell<bool>>,
    heard: Rc<RefCell<Vec<bool>>>,
    write_next: Rc<Cell<bool>>,
    wake: Rc<RefCell<Option<crate::WakeHandle>>>,
    panic: Rc<Cell<bool>>,
}

struct OwnerProbe {
    seen: Seen,
    actions: crate::local_dispatch::LocalDispatch,
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

    // Nothing it syncs changes its size: a window over it re-solves its layout only for what it
    // shows itself.
    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn layout_needs_measure(&self) -> bool {
        false
    }

    fn sync(&mut self, model: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        let (doc, root) = crate::doc::document_ref(model, path);
        // The owner syncs over its own record, which holds only its content: who the record is
        // the workspace's store keeps.
        assert!(
            automerge::ReadDoc::get(doc, root, "body")
                .unwrap()
                .is_some(),
            "the owner syncs over its record"
        );
        self.seen.syncs.set(self.seen.syncs.get() + 1);
        assert!(
            !self.seen.panic.replace(false),
            "real owner callback failed"
        );
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
        self.actions.take_committed()
    }
}

impl crate::undo::UndoPoints for OwnerProbe {
    fn get_state(&self) -> Option<String> {
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

    fn resource_save_status(&mut self, status: crate::resource::ResourceSaveStatus) -> bool {
        self.seen.saves.set(self.seen.saves.get() + 1);
        self.seen
            .saved
            .set(status == crate::resource::ResourceSaveStatus::Saved);
        self.seen
            .heard
            .borrow_mut()
            .push(status == crate::resource::ResourceSaveStatus::Saved);
        true
    }
}

thread_local! { static NEXT: RefCell<Option<Seen>> = const { RefCell::new(None) }; }

fn factory(
    mut context: ProcessContext,
    _: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, crate::launch::LaunchError> {
    context
        .initialize_new_resource(|doc| {
            doc.put(ROOT, "body", "canonical owner body")
                .map_err(|e| e.to_string())?;
            doc.commit();
            Ok(())
        })
        .map_err(crate::launch::LaunchError::Record)?;
    let seen = NEXT.with_borrow_mut(|next| next.take().unwrap());
    *seen.wake.borrow_mut() = Some(context.wake_handle());
    let mut registry = tilo_actions::ActionRegistry::new();
    registry.register::<BackgroundWrite>("probe.background");
    Ok(AppComponent::new(
        context,
        Box::new(OwnerProbe {
            seen,
            actions: crate::local_dispatch::LocalDispatch::new(),
        }),
        registry,
    ))
}

fn prepared(
    host: &ProbeHarness,
    queue: &OwnerWakeQueue,
    id: u64,
) -> (BoundAppWidget, Seen, Arc<Mutex<Vec<bool>>>) {
    let seen = Seen::default();
    NEXT.with_borrow_mut(|next| *next = Some(seen.clone()));
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

/// A host that retains and closes its owners but does not present or synchronize them in its
/// ordinary visible pass, so only the owner service reaches them.
struct HiddenOwners {
    owners: Vec<BoundAppWidget>,
    visits: Rc<Cell<usize>>,
    refuse: Rc<Cell<bool>>,
}

impl Widget for HiddenOwners {
    crate::provides!(dyn crate::bound_dispatch::BoundTraversal);
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
}

impl crate::bound_dispatch::BoundTraversal for HiddenOwners {
    fn visit_components(
        &mut self,
        request: &mut ComponentRequest<'_>,
    ) -> Result<(), BoundDispatchError> {
        self.visits.set(self.visits.get() + 1);
        if self.refuse.get() {
            return Err(BoundDispatchError::CrossResourceReentry);
        }
        for owner in &mut self.owners {
            crate::bound_dispatch::visit_components(owner, request)?;
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
) -> (Driver, Rc<Cell<usize>>, Rc<Cell<bool>>) {
    let visits = Rc::new(Cell::new(0));
    let refuse = Rc::new(Cell::new(false));
    let inputs = probe::runner_inputs();
    let mut driver = Driver::new_with_controlled_inputs(
        State::new(),
        Box::new(HiddenOwners {
            owners,
            visits: Rc::clone(&visits),
            refuse: Rc::clone(&refuse),
        }),
        (320, 180),
        inputs.theme,
        inputs.text_renderer,
        probe::author(),
    );
    driver.attach_owner_wake_queue(queue.clone()).unwrap();
    // Original initialization notifications are already reconciled by each owner's actual
    // canonical sync above; finish their host intake explicitly.
    driver.service_pending_owner_models().unwrap();
    (driver, visits, refuse)
}

fn close(driver: &mut Driver, expected: usize) {
    driver.close_components();
    let reports = driver.take_component_close_reports();
    assert_eq!(reports.len(), expected);
    assert!(reports.iter().all(|r| r.resource_error.is_none()
        && r.view_error.is_none()
        && r.teardown_error.is_none()));
}

// Land every owner's first write and take up every wake its store calls.
//
// A store resolves a write on its own thread and only then calls the wake that tells the owner to
// show it saved, so a sample can read the record as saved while that wake is still on its way, and
// a wake that arrives while a test is setting up the batch under test joins it. A flush returns
// only once every queued write has had its wake called, so flushing, then servicing, and repeating
// until the store has called nothing more leaves a queue that is empty of store work rather than
// one that merely looks empty.
fn settle_first_writes(host: &ProbeHarness, driver: &mut Driver, seen: &[Seen]) {
    let deadline = crate::time::now() + tilo_test_env::hang_bound();
    loop {
        host.records.try_flush().unwrap();
        driver.service_pending_owner_models().unwrap();
        if !driver.has_pending_owner_models() && seen.iter().all(|owner| owner.saved.get()) {
            return;
        }
        assert!(
            crate::time::now() < deadline,
            "every owner hears its first write saved"
        );
    }
}

/// A save-progress wake shows the status its owner does not already show, and nothing else: no
/// model callback runs (no update, no sync) and no layout is solved for it. A wake that samples the
/// status the owner already shows tells nothing, and draws nothing.
#[test]
fn a_save_status_wake_shows_it_without_an_owner_service_or_layout() {
    let host = ProbeHarness::new();
    let queue = OwnerWakeQueue::default();
    let (owner, seen, _) = prepared(&host, &queue, 6100);
    let (mut driver, _, _) = frame(&queue, vec![owner]);
    settle_first_writes(&host, &mut driver, std::slice::from_ref(&seen));
    driver.idle();
    let (updates, syncs, saves) = (seen.updates.get(), seen.syncs.get(), seen.saves.get());
    let layouts = driver.relayout_frames();
    let wake = seen.wake.borrow().clone().unwrap();

    wake.wake_save_progress();
    assert_eq!(driver.service_pending_owner_models().unwrap(), 1);
    assert_eq!(
        seen.saves.get(),
        saves,
        "a status the owner already shows is not told again"
    );
    assert_eq!(
        (seen.updates.get(), seen.syncs.get()),
        (updates, syncs),
        "no model callback runs for a save"
    );
    assert!(!driver.app().unsettled(), "the window's layout stands");
    assert_eq!(driver.relayout_frames(), layouts, "nor is one solved");

    wake.wake_save_progress();
    wake.wake();
    assert_eq!(driver.service_pending_owner_models().unwrap(), 1);
    assert_eq!(
        seen.syncs.get(),
        syncs + 1,
        "a wake for anything else takes the whole service"
    );
    close(&mut driver, 1);
}

/// A window shows its record saved by drawing what the status changed: one sync takes the new
/// status up, no wake is raised for it, no layout tree is rebuilt and no size is re-solved.
#[test]
fn a_save_status_change_is_no_structure_change() {
    // The workspace has not answered the record's founding write when the window first syncs,
    // so the window shows it being written; one that answers at once founds the record before
    // the window ever looks.
    let host = ProbeHarness::holding_writes();
    let queue = OwnerWakeQueue::default();
    let (mut owner, seen, wakes) = prepared(&host, &queue, 7000);
    let _ = owner.take_structure_change();
    host.records.try_flush().unwrap();
    let woken = wakes.lock().unwrap().len();
    let syncs = seen.syncs.get();
    owner.sync(&AutoCommit::new(), &ROOT);
    assert_eq!(
        seen.syncs.get(),
        syncs + 1,
        "the sync that takes up the new status is the only one: a status delivered before it costs no second sync"
    );
    assert_eq!(
        wakes.lock().unwrap().len(),
        woken,
        "taking up the new status wakes no one"
    );
    let heard = seen.heard.borrow().clone();
    assert!(
        heard.windows(2).any(|pair| pair == [false, true]),
        "the window heard its record being written, then saved: {heard:?}"
    );
    assert_eq!(
        owner.take_structure_change(),
        crate::widget::StructureChange::None,
        "a save status change rebuilds no layout tree"
    );
    assert!(owner.needs_redraw(), "what shows the status is drawn again");
    assert!(
        !owner.layout_needs_measure(),
        "a status that changed no size re-solves no layout"
    );
    let (mut driver, _, _) = frame(&queue, vec![owner]);
    close(&mut driver, 1);
}

/// A worker wakes its component's owner from another thread, and the window that shows it is idle:
/// its idle pass serves the model as the live window does, so the wake is taken up and the window
/// comes to rest.
#[test]
fn an_idle_pass_serves_the_owner_a_worker_woke_so_the_window_rests() {
    let host = ProbeHarness::new();
    let queue = OwnerWakeQueue::default();
    let (owner, seen, _) = prepared(&host, &queue, 9100);
    let (mut driver, _, _) = frame(&queue, vec![owner]);
    settle_first_writes(&host, &mut driver, std::slice::from_ref(&seen));
    let syncs = seen.syncs.get();
    let wake = seen.wake.borrow().clone().unwrap();
    wake.wake();
    assert!(
        driver.frame_schedule().queued_work,
        "a woken owner is work the window owes"
    );
    driver.idle();
    assert!(
        seen.syncs.get() > syncs,
        "the idle pass ran the owner's model"
    );
    assert!(!driver.has_pending_owner_models(), "the wake was taken up");
    assert!(
        !driver.frame_schedule().queued_work,
        "a window at rest owes no frame: {:?}",
        driver.queued_work_sources()
    );
    close(&mut driver, 1);
}

/// A capture of a window that rests draws nothing: it runs the idle pass and finds what it needs
/// already there, as a live window draws no frame between two interactions.
#[test]
fn a_capture_of_a_still_window_runs_no_frame() {
    let host = ProbeHarness::new();
    let queue = OwnerWakeQueue::default();
    let (owner, seen, _) = prepared(&host, &queue, 9104);
    let (mut driver, _, _) = frame(&queue, vec![owner]);
    settle_first_writes(&host, &mut driver, std::slice::from_ref(&seen));
    driver.idle();
    let frames = driver.relayout_frames();
    assert!(driver.settle_for_capture(crate::time::Duration::from_millis(50)));
    assert_eq!(
        driver.relayout_frames(),
        frames,
        "a window at rest lays nothing out again"
    );
    close(&mut driver, 1);
}
