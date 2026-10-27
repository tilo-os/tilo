//! A list's rows on their way: their slots and placeholders.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use automerge::AutoCommit;

use super::tests::{PagedList, ProbeRow, lay_out};
use super::*;
use crate::widget::SkeletonShape;

/// A list model whose rows come from another device: row 0 is on its
/// way, row 1 failed and can be asked for again.
struct RemoteRows {
    ready: Rc<Cell<bool>>,
    retried: Rc<Cell<Option<usize>>>,
}

impl ListModel for RemoteRows {
    type Key = u64;

    fn len(&self) -> usize {
        2
    }

    fn key_at(&self, index: usize) -> Option<Self::Key> {
        (index < 2).then(|| u64::try_from(index).ok()).flatten()
    }

    fn index_of(&self, key: &Self::Key) -> Option<usize> {
        usize::try_from(*key).ok().filter(|index| *index < 2)
    }

    fn row_state(&self, index: usize) -> Arrival<(), &str> {
        match index {
            0 if self.ready.get() => Arrival::Ready(()),
            0 => Arrival::Pending {
                from: crate::arriving::Source::peer("Laptop"),
            },
            _ => Arrival::Failed("offline"),
        }
    }

    fn can_retry(&self, index: usize) -> bool {
        index == 1
    }

    fn retry(&mut self, index: usize) {
        self.retried.set(Some(index));
    }
}

#[test]
fn a_pending_row_keeps_its_slot_and_the_list_paints_its_placeholder() {
    use crate::arriving::{GRACE, MIN_SHOWN, Step};
    use crate::time::testing::with_monotonic_time;
    let t0 = crate::time::now();
    let ready = Rc::new(Cell::new(false));
    let retried = Rc::new(Cell::new(None));
    let builds = Rc::new(Cell::new(0_usize));
    let counted = Rc::clone(&builds);
    let model = RemoteRows {
        ready: Rc::clone(&ready),
        retried: Rc::clone(&retried),
    };
    let mut list = with_monotonic_time(t0, || {
        let mut list = VirtualList::new(model, 30.0, move |_, _, key| {
            counted.set(counted.get().saturating_add(1));
            Box::new(ProbeRow::new(
                format!("row {key}"),
                30.0,
                false,
                Rc::default(),
            ))
        })
        .with_viewport_height(120.0);
        let _ = lay_out(&mut list, 300.0);
        list
    });
    assert_eq!(builds.get(), 0, "the builder sees only rows that are there");
    assert_eq!(list.mounted[0].state, MountedRowState::Pending);
    assert!(
        (list.mounted[0].widget.get_rect().height() - 30.0).abs() < 0.5,
        "the slot is the row's height"
    );
    let below = list.mounted[1].widget.get_rect().y0;
    with_monotonic_time(t0, || {
        assert_eq!(
            Widget::next_wake_deadline(&list),
            Some(t0 + GRACE),
            "a pending row waits for its next step, with no frames between"
        );
        assert!(!Widget::needs_animation_frame(&list));
    });
    let skeleton = t0 + GRACE;
    assert_eq!(
        list.arrivals
            .face(
                &0,
                SkeletonShape::Row(crate::widget::RowShape::TITLE),
                skeleton
            )
            .map(|face| face.step()),
        Some(Step::Skeleton)
    );

    // The failed row says why, with a retry that asks the model again.
    assert!(matches!(
        &list.mounted[1].state,
        MountedRowState::Failed(message, true) if message == "offline"
    ));
    let mut names = Vec::new();
    list.mounted[1]
        .widget
        .for_each_child(&mut |child| names.push(child.name().to_owned()));
    assert!(names.iter().any(|name| name == "Button"), "{names:?}");
    list.retry_asked.set(Some(1));
    let doc = AutoCommit::new();
    with_monotonic_time(skeleton, || {
        assert!(Widget::update(&mut list, &doc, &automerge::ROOT));
    });
    assert_eq!(retried.get(), Some(1));

    // The row lands after its skeleton showed: it stands its minimum,
    // then the row fills its slot without moving the one below.
    ready.set(true);
    let landed = skeleton + crate::time::Duration::from_millis(10);
    with_monotonic_time(landed, || {
        list.sync(&doc, &automerge::ROOT);
        let _ = lay_out(&mut list, 300.0);
    });
    assert_eq!(list.mounted[0].state, MountedRowState::Pending);
    let shows = skeleton + MIN_SHOWN;
    with_monotonic_time(landed, || {
        assert_eq!(Widget::next_wake_deadline(&list), Some(shows));
    });
    with_monotonic_time(shows, || {
        list.sync(&doc, &automerge::ROOT);
        let _ = lay_out(&mut list, 300.0);
        assert_eq!(list.mounted[0].state, MountedRowState::Ready);
        assert_eq!(builds.get(), 1);
        assert!(list.arrivals.fade(&0, shows).is_some(), "it fades in");
        assert!(Widget::needs_animation_frame(&list));
    });
    assert!(
        (list.mounted[1].widget.get_rect().y0 - below).abs() < 0.5,
        "the row below stays where it was"
    );
}

#[test]
fn a_paged_list_requests_its_window_and_keeps_each_row_while_it_arrives() {
    crate::time::testing::with_monotonic_time(crate::time::now(), || {
        let states = Rc::new(RefCell::new(HashMap::new()));
        let requests = Rc::new(RefCell::new(Vec::new()));
        let changed = Rc::new(Cell::new(false));
        let pending = Rc::new(Cell::new(true));
        let builds = Rc::new(RefCell::new(Vec::new()));
        let captured_builds = Rc::clone(&builds);
        let model = PagedList {
            count: 100_000_000,
            states: Rc::clone(&states),
            requests: Rc::clone(&requests),
            changed: Rc::clone(&changed),
            pending: Rc::clone(&pending),
        };
        let mut list = VirtualList::new(model, 20.0, move |_, _, key| {
            let label = format!("ready {key}");
            captured_builds.borrow_mut().push(label.clone());
            Box::new(ProbeRow::new(label, 20.0, false, Rc::new(Cell::new(0))))
        })
        .with_viewport_height(40.0)
        .with_semantic_key(u64::to_string);

        let _ = lay_out(&mut list, 300.0);
        assert!(list.mounted_len() <= 6);
        assert!(
            requests.borrow().iter().all(|range| range.len() <= 6),
            "fetch requests stay bounded to the sparse viewport"
        );
        assert_eq!(requests.borrow().len(), 1, "one request per window");
        let _ = lay_out(&mut list, 300.0);
        assert_eq!(
            requests.borrow().len(),
            1,
            "repeat layout does not enqueue the same window again"
        );
        let semantics = Widget::accessibility(&list).unwrap();
        assert!(semantics.children[0].state.busy);
        assert!(!Widget::perform_semantic_action(
            &mut list,
            "key:0",
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert!(Widget::needs_animation_frame(&list));

        states.borrow_mut().insert(0, 1);
        pending.set(false);
        changed.set(true);
        assert!(Widget::update(
            &mut list,
            &AutoCommit::new(),
            &automerge::ROOT
        ));
        let _ = lay_out(&mut list, 300.0);
        assert_eq!(
            requests.borrow().len(),
            2,
            "a model wakeup may refill the same window once"
        );
        let semantics = Widget::accessibility(&list).unwrap();
        assert!(!semantics.children[0].state.busy);
        assert!(!semantics.children[0].state.invalid);
        assert!(builds.borrow().iter().any(|label| label == "ready 0"));
        assert!(Widget::perform_semantic_action(
            &mut list,
            "key:0",
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        ));

        states.borrow_mut().insert(1, 2);
        changed.set(true);
        assert!(Widget::update(
            &mut list,
            &AutoCommit::new(),
            &automerge::ROOT
        ));
        let _ = lay_out(&mut list, 300.0);
        let semantics = Widget::accessibility(&list).unwrap();
        assert!(semantics.children[1].state.invalid);
        assert_eq!(
            semantics.children[1].description.as_deref(),
            Some("Fetch failed")
        );
        assert!(!Widget::needs_animation_frame(&list));
    });
}
