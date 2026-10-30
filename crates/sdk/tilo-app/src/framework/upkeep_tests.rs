//! A view's upkeep as the window meets it: the work taken in before its
//! widgets sync, the frames, wakes and captures it owes, and what it asks of
//! the host.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ui_core::automerge::{AutoCommit, ROOT};
use ui_core::widget::capability::{capability, capability_mut};

use super::*;

/// `upkeep` around a plain container, as a view returns it.
fn kept(upkeep: Upkeep) -> Element {
    super::upkeep(upkeep, Box::new(ui_core::widget::Container::new()))
}

#[test]
fn the_view_takes_in_its_work_and_says_what_it_owes_the_window() {
    let synced = Rc::new(Cell::new(0));
    let busy = Rc::new(Cell::new(false));
    let presented = Rc::new(Cell::new(0));
    let due = ui_core::time::now();
    let (counted, working, waiting, drawn) = (
        Rc::clone(&synced),
        Rc::clone(&busy),
        Rc::clone(&busy),
        Rc::clone(&presented),
    );
    let mut view = kept(
        Upkeep::new()
            .syncing(move || counted.set(counted.get() + 1))
            .busy(move || working.get())
            .next_wake(move || Some(due))
            .pending_visual_work(move || waiting.get())
            .presenting(move || drawn.set(drawn.get() + 1))
            .appearance(|| None),
    );
    let doc = AutoCommit::new();
    view.sync(&doc, &ROOT);
    assert_eq!(synced.get(), 1, "the view takes in its work on a sync");
    assert!(!view.needs_animation_frame());
    assert!(!view.has_pending_visual_work());
    assert_eq!(view.next_wake_deadline(), Some(due));
    assert!(view.appearance_intent().is_none());
    busy.set(true);
    assert!(
        view.needs_animation_frame(),
        "busy work keeps frames coming"
    );
    assert!(view.has_pending_visual_work(), "a capture waits for it");
    assert_eq!(presented.get(), 0);
}

#[test]
fn the_view_shares_collaboration_only_when_it_asks_to() {
    let taken = Rc::new(Cell::new(0));
    let took = Rc::clone(&taken);
    let mut view = kept(Upkeep::new().collaboration(
        move || {
            took.set(took.get() + 1);
            Vec::new()
        },
        |_| {},
    ));
    let collaborative = capability_mut::<dyn Collaborative>(&mut *view).expect("it shares");
    assert!(collaborative.take_collab_requests().is_empty());
    assert_eq!(taken.get(), 1);
    let mut plain = kept(Upkeep::new());
    assert!(capability_mut::<dyn Collaborative>(&mut *plain).is_none());
    assert!(capability::<dyn ApplicationViewState>(&*plain).is_none());
}

#[test]
fn a_reopened_window_shows_the_presentation_its_view_kept() {
    let shown = Rc::new(RefCell::new(b"general".to_vec()));
    let (read, write) = (Rc::clone(&shown), Rc::clone(&shown));
    let mut view = kept(Upkeep::new().kept_view(
        "so.tilo.test",
        1,
        move || read.borrow().clone(),
        move |payload| {
            if payload.is_empty() {
                return Err("nothing to show".into());
            }
            *write.borrow_mut() = payload.to_vec();
            Ok(())
        },
    ));
    let saved = capability::<dyn ApplicationViewState>(&*view)
        .expect("the view keeps its presentation")
        .capture_application_view()
        .expect("captured")
        .expect("something to keep");
    assert_eq!(saved.app_id, "so.tilo.test");
    assert_eq!(saved.payload, b"general");
    *shown.borrow_mut() = b"other".to_vec();
    let doc = AutoCommit::new();
    let mut restore = |local: &ApplicationLocalView| {
        capability_mut::<dyn ApplicationViewState>(&mut *view)
            .expect("the view keeps its presentation")
            .restore_application_view(local, &doc)
    };
    assert!(matches!(
        restore(&saved),
        Ok(ApplicationFocusRestore::Generic)
    ));
    assert_eq!(*shown.borrow(), b"general");
    let foreign = ApplicationLocalView {
        app_id: "so.tilo.other".into(),
        ..saved.clone()
    };
    assert!(restore(&foreign).is_err(), "another app's view is refused");
    let empty = ApplicationLocalView {
        payload: Vec::new(),
        ..saved
    };
    assert!(restore(&empty).is_err(), "what the view refuses is refused");
}
