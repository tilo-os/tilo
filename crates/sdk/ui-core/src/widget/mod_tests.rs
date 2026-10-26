//! Unit tests of the widget module's shared traits and helpers.

use super::*;
use std::cell::Cell;
use std::rc::Rc;

struct CollabProbe {
    events: std::rc::Rc<std::cell::Cell<u32>>,
    child: Option<Box<dyn Widget>>,
}

impl Collaborative for CollabProbe {
    fn take_collab_requests(&mut self) -> Vec<crate::collaboration::CollabRequest> {
        vec![crate::collaboration::CollabRequest::ClearContinuation {
            app_id: "probe".into(),
        }]
    }

    fn handle_collab_event(&mut self, _: &crate::collaboration::CollabEvent) {
        self.events.set(self.events.get().saturating_add(1));
    }
}

impl AudiencePresentationProvider for CollabProbe {
    fn on_audience_presentation_changed(&mut self, _active: bool) {
        self.events.set(self.events.get().saturating_add(1));
    }
}

impl Widget for CollabProbe {
    crate::provides!(dyn Collaborative, dyn AudiencePresentationProvider);

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        _: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.leaf_node(taffy::Style::default())
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let Some(child) = self.child.as_mut() else {
            return Vec::new();
        };
        let child: &mut (dyn Widget + '_) = child.as_mut();
        vec![child]
    }

    fn name(&self) -> &'static str {
        "CollabProbe"
    }
}

#[test]
fn widget_trait_defaults_and_native_input_probe_precedence() {
    fn ordinary_window_close_defaults_to_immediate_exit() {
        let mut spacer = Spacer::new();
        assert_eq!(
            crate::component_lifecycle::on_window_close_requested(&mut spacer),
            WindowCloseAdmission::Allow
        );
        assert!(!crate::component_lifecycle::take_window_close_intent(
            &mut spacer
        ));
    }

    fn native_input_snapshot_defaults_to_none() {
        let state = crate::State::new();
        assert!(
            Spacer::new()
                .native_input_snapshot(&state.doc, &state.root)
                .is_none()
        );
    }

    fn native_input_snapshot_root_fallback_and_explicit_probe_precedence() {
        struct Observed(std::rc::Rc<std::cell::Cell<usize>>);
        impl Widget for Observed {
            fn native_input_snapshot(
                &self,
                doc: &crate::widget::AppModel,
                path: &crate::widget::ModelPath,
            ) -> Option<serde_json::Value> {
                use automerge::ReadDoc;
                let (doc, path) = crate::doc::document_ref(doc, path);
                self.0.set(self.0.get() + 1);
                Some(serde_json::json!({"keys": doc.keys(path).collect::<Vec<_>>() }))
            }
            fn build_layout(
                &mut self,
                _: &mut TaffyTree<()>,
                _: &Theme,
                _: &mut HashMap<NodeId, *mut dyn Widget>,
            ) -> NodeId {
                unreachable!()
            }
            fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, _: vello::kurbo::Point) {
                unreachable!()
            }
            fn draw(
                &mut self,
                _: &mut Scene,
                _: vello::kurbo::Affine,
                _: &mut TextRenderer,
                _: &Theme,
                _: &PresenceCtx,
            ) {
                unreachable!()
            }
            fn handle_event(&mut self, _: &WindowEvent, _ctx: &mut crate::widget::EventCtx<'_>) {
                unreachable!()
            }
            fn name(&self) -> &str {
                "Observed"
            }
        }
        use automerge::transaction::Transactable;
        let mut state = crate::State::new();
        state
            .doc
            .put(&state.root, "fixture", "actual state")
            .unwrap();
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut app = crate::App::new(state, Box::new(Observed(calls.clone())));
        assert_eq!(
            app.native_input_snapshot().unwrap()["keys"],
            serde_json::json!(["fixture"])
        );
        assert_eq!(calls.get(), 1);
        app.set_native_input_probe(|| serde_json::json!({"explicit": true}));
        assert_eq!(
            app.native_input_snapshot().unwrap(),
            serde_json::json!({"explicit": true})
        );
        assert_eq!(
            calls.get(),
            1,
            "explicit probe must suppress root observation"
        );
    }

    /// Pinned so a future edit that removes the default blanket-breaks
    /// every widget impl that hasn't opted into a stable test id.
    fn widget_test_id_default_is_none() {
        let s = Spacer::new();
        assert!(
            Widget::test_id(&s).is_none(),
            "default Widget::test_id should be None"
        );
    }

    ordinary_window_close_defaults_to_immediate_exit();
    native_input_snapshot_defaults_to_none();
    native_input_snapshot_root_fallback_and_explicit_probe_precedence();
    widget_test_id_default_is_none();
}

#[test]
fn widget_capabilities_forward_through_the_tree_and_emitters_stay_bounded() {
    fn collaboration_capabilities_are_discovered_through_the_widget_tree() {
        let events = Rc::new(Cell::new(0));
        let child = CollabProbe {
            events: Rc::clone(&events),
            child: None,
        };
        let mut root = CollabProbe {
            events: Rc::clone(&events),
            child: Some(Box::new(child)),
        };

        assert_eq!(take_collab_requests(&mut root).len(), 2);
        handle_collab_event(
            &mut root,
            &crate::collaboration::CollabEvent::FollowChanged { peer_id: None },
        );
        assert_eq!(events.get(), 2);
    }

    fn audience_presentation_capability_forwards_through_dyn_widget() {
        let events = Rc::new(Cell::new(0));
        let mut widget: Box<dyn Widget> = Box::new(CollabProbe {
            events: Rc::clone(&events),
            child: None,
        });

        widget.on_audience_presentation_changed(true);
        assert_eq!(events.get(), 1);
    }

    fn action_emitter_backlog_is_bounded_and_keeps_the_newest_intent() {
        let mut emitter = ActionEmitter::new(test_bool_action_binding());
        for _ in 0..ACTION_EMITTER_QUEUE_LIMIT.saturating_add(1) {
            emitter.emit(false).expect("valid boolean intent");
        }
        emitter.emit(true).expect("valid boolean intent");
        let mut count = 0;
        let mut last = None;
        while let Some(invocation) = emitter.take() {
            count += 1;
            last = invocation.arguments.get("value").cloned();
        }
        assert_eq!(count, ACTION_EMITTER_QUEUE_LIMIT);
        assert_eq!(last, Some(serde_json::Value::Bool(true)));
    }

    collaboration_capabilities_are_discovered_through_the_widget_tree();
    audience_presentation_capability_forwards_through_dyn_widget();
    action_emitter_backlog_is_bounded_and_keeps_the_newest_intent();
}

#[test]
fn collaboration_capabilities_visit_parent_and_child_exactly_once() {
    let events = std::rc::Rc::new(std::cell::Cell::new(0));
    let child = CollabProbe {
        events: std::rc::Rc::clone(&events),
        child: None,
    };
    let mut root = CollabProbe {
        events: std::rc::Rc::clone(&events),
        child: Some(Box::new(child)),
    };

    assert_eq!(take_collab_requests(&mut root).len(), 2);
    handle_collab_event(
        &mut root,
        &crate::collaboration::CollabEvent::FollowChanged { peer_id: None },
    );
    assert_eq!(events.get(), 2);
}
