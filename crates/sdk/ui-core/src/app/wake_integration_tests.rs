use super::*;
use crate::view::{FreezeThaw, ViewContribution};
use crate::wake::{WakeEvent, WakeHandle, WakeScope};
use crate::widget::TaffyTreeExt;
use crate::widget::{AppModel, ModelPath};
use crate::{LatestRequest, LocalState};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

fn bound_scope() -> (WakeScope, Receiver<WakeEvent>) {
    let (tx, rx) = mpsc::channel();
    let mut scope = WakeScope::new();
    scope
        .bind(move |event| {
            let _ = tx.send(event);
        })
        .unwrap();
    (scope, rx)
}

type Captures = Rc<RefCell<Vec<(&'static str, LatestRequest<u32>)>>>;

struct CaptureRoot {
    captures: Captures,
}

impl CaptureRoot {
    fn capture(&self, label: &'static str) {
        self.captures
            .borrow_mut()
            .push((label, LatestRequest::new()));
    }
}

impl Drop for CaptureRoot {
    fn drop(&mut self) {
        self.capture("drop");
    }
}

impl Widget for CaptureRoot {
    crate::provides!(dyn crate::view::capability::ViewState);

    fn sync(&mut self, _: &AppModel, _: &ModelPath) {
        self.capture("sync");
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.capture("layout");
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }

    fn handle_event(&mut self, _: &WindowEvent, _ctx: &mut crate::widget::EventCtx<'_>) {
        self.capture("event");
    }

    fn name(&self) -> &'static str {
        "capture-root"
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        vello::kurbo::Rect::ZERO
    }
}

impl crate::view::capability::ViewState for CaptureRoot {
    fn view_state(&self) -> Option<ViewContribution> {
        self.capture("capture-view");
        Some(ViewContribution::default())
    }
    fn restore_view_state(&mut self, _: &ViewContribution) {
        self.capture("restore-view");
    }
}

#[test]
fn factory_and_callbacks_keep_their_owner_and_restore_parent_context() {
    // Case `factory_state_stays_owned_before_binding_and_after_return`.
    {
        let mut captured = None;
        let mut app = App::from_factory_with_controlled_inputs(State::new(), || {
            let state = LocalState::new(0);
            state.writer().set(1);
            captured = Some(state);
            Box::new(crate::widget::Text::new("root"))
        });
        let state = captured.unwrap();
        let (tx, rx) = mpsc::channel();
        app.wake_scope
            .as_mut()
            .unwrap()
            .bind(move |event| {
                let _ = tx.send(event);
            })
            .unwrap();
        let pending = rx.try_recv().unwrap();
        app.needs_sync = false;
        assert!(app.accept_wake(&pending));
        assert!(app.needs_sync);
        assert_eq!(state.get(), 1);
        drop(app);
        assert!(!state.writer().try_set(2));
    }

    // Case `transfer_preserves_the_exact_owner_and_rejects_foreign_events`.
    {
        let (scope, rx) = bound_scope();
        let (other, other_rx) = bound_scope();
        let state = {
            let _entry = scope.enter();
            LocalState::new(0)
        };
        let mut app = App::from_scoped_parts_with_controlled_inputs(
            scope,
            State::new(),
            Box::new(crate::widget::Text::new("root")),
        );
        state.writer().set(1);
        let current = rx.try_recv().unwrap();
        other.handle().wake();
        let foreign = other_rx.try_recv().unwrap();
        app.needs_sync = false;
        assert!(!app.accept_wake(&foreign));
        assert!(!app.needs_sync);
        assert!(app.accept_wake(&current));
        assert!(app.needs_sync);
        app.close_wake_scope();
        app.needs_sync = false;
        assert!(!app.accept_wake(&current));
        assert!(!app.needs_sync);
    }

    // Case `collaboration_queue_wake_does_not_force_document_sync`.
    {
        let (scope, rx) = bound_scope();
        let handle = scope.handle();
        let mut app = App::from_scoped_parts_with_controlled_inputs(
            scope,
            State::new(),
            Box::new(crate::widget::Text::new("root")),
        );
        app.needs_sync = false;
        handle.notify_events();
        assert!(app.accept_wake(&rx.try_recv().unwrap()));
        assert!(!app.needs_sync);
        app.needs_sync = true;
        handle.notify_events();
        assert!(app.accept_wake(&rx.try_recv().unwrap()));
        assert!(
            app.needs_sync,
            "queue wake must not erase an earlier model invalidation"
        );
    }

    // Case `fallible_factory_error_closes_captures_and_restores_parent_context`.
    {
        let (outer, rx) = bound_scope();
        let _outer = outer.enter();
        let mut captured = None;
        let result: Result<App, &'static str> = App::try_from_factory(State::new(), || {
            captured = Some(LocalState::new(0));
            Err("factory failed")
        });
        assert!(matches!(result, Err("factory failed")));
        assert!(!captured.unwrap().writer().try_set(1));
        WakeHandle::new().wake();
        assert!(outer.accepts(&rx.try_recv().unwrap()));
    }

    // Case `factory_unwind_closes_captures_and_restores_parent_context`.
    {
        let (outer, rx) = bound_scope();
        let _outer = outer.enter();
        let mut captured = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            App::from_factory(State::new(), || {
                captured = Some(LocalState::new(0));
                panic!("factory panicked");
            })
        }));
        assert!(result.is_err());
        assert!(!captured.unwrap().writer().try_set(1));
        WakeHandle::new().wake();
        assert!(outer.accepts(&rx.try_recv().unwrap()));
    }

    // Case `callback_created_requests_keep_app_owner_when_begun_elsewhere`.
    {
        let (scope, rx) = bound_scope();
        let (other, other_rx) = bound_scope();
        let captures = Captures::default();
        let root = {
            let _entry = scope.enter();
            Box::new(CaptureRoot {
                captures: Rc::clone(&captures),
            })
        };
        let mut app = App::from_scoped_parts_with_controlled_inputs(scope, State::new(), root);
        {
            let _other = other.enter();
            app.init_layout();
            app.refresh_focus_order();
            let view = app.capture_view();
            app.restore_view(&view);
        }
        let requests = std::mem::take(&mut *captures.borrow_mut());
        for label in ["layout", "sync", "capture-view", "restore-view"] {
            assert!(requests.iter().any(|(actual, _)| *actual == label));
        }
        for (_, mut request) in requests {
            let completion = {
                let _other = other.enter();
                request.begin()
            };
            assert!(
                std::thread::spawn(move || completion.complete(7))
                    .join()
                    .unwrap()
            );
            assert!(app.accept_wake(&rx.try_recv().unwrap()));
            assert_eq!(request.try_take(), Some(7));
        }
        assert!(matches!(other_rx.try_recv(), Err(TryRecvError::Empty)));
        {
            let _other = other.enter();
            drop(app);
        }
        let mut after_drop = std::mem::take(&mut *captures.borrow_mut());
        assert!(after_drop.iter().any(|(label, _)| *label == "drop"));
        for (_, request) in &mut after_drop {
            assert!(request.try_begin().is_none());
        }
        assert!(matches!(other_rx.try_recv(), Err(TryRecvError::Empty)));
    }

    // Case `detached_app_callbacks_mask_an_ambient_host`.
    {
        let (outer, rx) = bound_scope();
        let _outer = outer.enter();
        let captures = Captures::default();
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CaptureRoot {
                captures: Rc::clone(&captures),
            }),
        );
        app.refresh_focus_order();
        let mut requests = std::mem::take(&mut *captures.borrow_mut());
        assert!(!requests.is_empty());
        for (_, request) in &mut requests {
            assert!(request.begin().complete(1));
            assert_eq!(request.try_take(), Some(1));
        }
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
        WakeHandle::new().wake();
        assert!(outer.accepts(&rx.try_recv().unwrap()));
    }
}

#[test]
#[should_panic(expected = "live App requires from_factory")]
fn detached_native_startup_fails_before_creating_an_event_loop() {
    App::new_with_controlled_inputs(State::new(), Box::new(crate::widget::Text::new("root"))).run();
}

#[test]
fn embedded_presentation_wakes_keep_the_directory_until_a_model_wake() {
    let mut app = App::from_factory_with_controlled_inputs(State::new(), || {
        Box::new(crate::widget::Text::new("root"))
    });
    let (tx, rx) = mpsc::channel();
    app.wake_scope
        .as_mut()
        .unwrap()
        .bind(move |event| {
            tx.send(event).unwrap();
        })
        .unwrap();
    app.pump_hosted_model_as(false);
    let revision = app.action_directory_revision();
    for _ in rx.try_iter() {}
    app.wake_scope.as_ref().unwrap().handle().notify_events();
    assert!(!rx.recv().unwrap().requires_sync());
    assert!(!app.pump_hosted_model_as(false).synced);
    assert_eq!(app.action_directory_revision(), revision);
    app.wake_scope.as_ref().unwrap().handle().wake();
    assert!(rx.recv().unwrap().requires_sync());
    assert!(app.pump_hosted_model_as(false).synced);
    assert_ne!(app.action_directory_revision(), revision);
}
