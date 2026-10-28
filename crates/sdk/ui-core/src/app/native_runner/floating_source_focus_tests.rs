use super::*;

type FocusEvents = std::rc::Rc<std::cell::RefCell<Vec<(Option<String>, bool)>>>;

#[derive(Default)]
struct FocusProbe {
    events: FocusEvents,
    drawable_ids: Vec<&'static str>,
    request: Option<crate::floating_surface::FloatingSurfaceRequest>,
}

impl crate::widget::AppWidget for FocusProbe {
    fn app_kind(&self) -> crate::widget::AppKind {
        crate::widget::AppKind::Editor
    }
    fn app_title(&self) -> &str {
        "Focus probe"
    }
    fn app_icon(&self) -> crate::widget::IconDescriptor {
        crate::widget::IconDescriptor::Named("chat".into())
    }
}

impl crate::widget::FloatingSurfaceProvider for FocusProbe {
    fn take_floating_surface_request(
        &mut self,
    ) -> Option<crate::floating_surface::FloatingSurfaceRequest> {
        self.request.take()
    }

    fn on_floating_surface_ime_source_changed(&mut self, id: Option<&str>) {
        self.events
            .borrow_mut()
            .push((id.map(|id| format!("ime:{id}")), false));
    }

    fn draw_floating_surface(
        &mut self,
        id: &str,
        _scene: &mut Scene,
        _transform: Affine,
        _text: &mut TextRenderer,
        _theme: &Theme,
        _presence: &crate::PresenceCtx,
    ) -> bool {
        if !self.drawable_ids.contains(&id) {
            return false;
        }
        self.events
            .borrow_mut()
            .push((Some(format!("draw:{id}")), false));
        true
    }

    fn on_floating_surface_event_source(&mut self, id: Option<&str>) {
        self.events
            .borrow_mut()
            .push((id.map(|id| format!("input:{id}")), false));
    }
    fn floating_surface_accessibility(&self, id: &str) -> Option<crate::a11y::AccessNode> {
        (id == "a").then(|| {
            crate::a11y::AccessNode::new(crate::a11y::Role::Button)
                .with_hashed_key("source-a-send")
                .with_action_target("source-a-send")
        })
    }
    fn perform_floating_surface_accessibility_action(
        &mut self,
        id: &str,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &automerge::ObjId,
    ) -> bool {
        let accepted = id == "a"
            && target == "source-a-send"
            && matches!(action, crate::a11y::Action::Activate);
        if accepted {
            self.events.borrow_mut().push((Some(target.into()), true));
        }
        accepted
    }
    fn floating_surface_ime(&self, id: &str) -> Option<(crate::ime::InputContent, KurboRect)> {
        (id == "a").then_some((
            crate::ime::InputContent::TEXT,
            KurboRect::new(-700.0, 500.0, -699.0, 520.0),
        ))
    }
    fn on_floating_surface_focus_changed(&mut self, id: Option<&str>, focused: bool) {
        self.events
            .borrow_mut()
            .push((id.map(str::to_owned), focused));
    }
}

impl Widget for FocusProbe {
    crate::provides!(dyn crate::widget::FloatingSurfaceProvider);

    fn sync(&mut self, _: &crate::widget::AppModel, _: &crate::widget::ModelPath) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        _: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use crate::widget::TaffyTreeExt as _;
        taffy.leaf_node(taffy::Style::default())
    }

    fn name(&self) -> &'static str {
        "FocusProbe"
    }
}

#[test]
fn retained_inactive_tab_forwards_exact_draw_and_ime_source_through_the_host_chain() {
    use crate::widget::{GroupRow, TabDescriptor, TileContent, WindowGroup, WindowWidth};
    let probe = FocusProbe {
        drawable_ids: vec!["a", "b"],
        request: Some(crate::floating_surface::FloatingSurfaceRequest {
            id: "b".into(),
            title: "Conversation".into(),
            role: crate::floating_surface::FloatingRole::Conversation,
            source: crate::floating_surface::FloatingSource::AppRegion {
                app_id: "chat".into(),
                region: KurboRect::new(0.0, 0.0, 500.0, 500.0),
            },
            preferred_size: vello::kurbo::Size::new(500.0, 500.0),
            aspect_ratio: None,
            initial_rect: None,
            presentation_rect: None,
            tracking_source: None,
        }),
        ..Default::default()
    };
    let events = std::rc::Rc::clone(&probe.events);
    let group = WindowGroup::new(
        1,
        vec![
            (
                TabDescriptor::new(1, "Primary"),
                TileContent::App(Box::new(FocusProbe::default())),
            ),
            (
                TabDescriptor::new(2, "Conversation"),
                TileContent::App(Box::new(probe)),
            ),
        ],
        1,
        WindowWidth::Normal,
    );
    let mut root = GroupRow::new(vec![group]);
    assert_eq!(root.take_floating_surface_request().unwrap().id, "b");
    assert!(root.take_floating_surface_request().is_none());
    root.on_floating_surface_ime_source_changed(Some("b"));
    forward_native_surface_focus(&mut root, Some("b"), &WindowEvent::Focused(true));
    // The selected tab does not own either floating presentation. The
    // exact pass must reach the retained app through its AppWidget loan.
    let mut scene = Scene::new();
    let mut text = TextRenderer::new();
    assert!(root.draw_floating_surface(
        "b",
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &Theme::default(),
        &crate::PresenceCtx::empty()
    ));
    assert!(!root.draw_floating_surface(
        "missing",
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &Theme::default(),
        &crate::PresenceCtx::empty()
    ));
    root.on_floating_surface_ime_source_changed(None);
    assert_eq!(
        *events.borrow(),
        vec![
            (Some("ime:b".into()), false),
            (Some("b".into()), true),
            (Some("draw:b".into()), false),
            (None, false),
        ]
    );
}

#[test]
fn secondary_accessibility_dispatch_reaches_only_the_declared_source_control() {
    let mut root = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(
        FocusProbe::default(),
    )));
    let access = root.floating_surface_accessibility("a").unwrap();
    let tree =
        crate::accesskit_bridge::surface_tree_update(Some(&access), vello::kurbo::Point::ZERO, 1.0);
    let target_node = tree
        .nodes
        .iter()
        .find(|(_, node)| node.role() == accesskit::Role::Button)
        .map(|(id, _)| *id)
        .unwrap();
    let request = accesskit::ActionRequest {
        action: accesskit::Action::Click,
        target_node,
        target_tree: accesskit::TreeId::ROOT,
        data: None,
    };
    let mut state = automerge::AutoCommit::new();
    assert!(!dispatch_native_surface_accessibility(
        &mut root,
        "b",
        &request,
        &mut crate::widget::Edits::lend(&mut state),
        &automerge::ROOT,
    ));
    assert!(dispatch_native_surface_accessibility(
        &mut root,
        "a",
        &request,
        &mut crate::widget::Edits::lend(&mut state),
        &automerge::ROOT,
    ));
}

#[test]
fn reused_native_surface_restores_focus_without_a_new_focus_event() {
    let probe = FocusProbe::default();
    let events = std::rc::Rc::clone(&probe.events);
    let mut root = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(probe)));
    let mut lifecycle = NativeFloatingSurfaceLifecycle {
        adopted: true,
        suspended: false,
        focused: true,
    };
    forward_native_surface_focus(&mut root, Some("a"), &WindowEvent::Focused(true));

    for cycle in 1..=2 {
        lifecycle.suspend(&mut root, "a");
        assert!(lifecycle.suspended);
        assert!(!lifecycle.focused);
        assert!(!lifecycle.adopted);
        assert_eq!(events.borrow().last(), Some(&(Some("a".into()), false)));

        // The retained native window is still the seat's focus owner.
        // There is no second Focused(true) edge for the runner to receive.
        lifecycle.resume(&mut root, "a", true);
        assert!(
            lifecycle.focused,
            "Float cycle {cycle} must reconcile the retained window's actual focus"
        );
        assert!(!lifecycle.suspended);
        assert!(!lifecycle.adopted);
        assert_eq!(
            events.borrow().last(),
            Some(&(Some("a".into()), true)),
            "the retained editor must receive the same focus as its native window"
        );
        lifecycle.adopted = true;
    }
}

#[test]
fn reused_native_surface_does_not_restore_focus_after_the_host_moves_it() {
    let probe = FocusProbe::default();
    let events = std::rc::Rc::clone(&probe.events);
    let mut root = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(probe)));
    let mut lifecycle = NativeFloatingSurfaceLifecycle {
        focused: true,
        ..Default::default()
    };
    lifecycle.suspend(&mut root, "a");
    events.borrow_mut().clear();

    lifecycle.resume(&mut root, "a", false);

    assert!(!lifecycle.focused);
    assert!(!lifecycle.suspended);
    assert_eq!(
        *events.borrow(),
        vec![(Some("a".into()), false)],
        "resume must report the host's current focus, even if the editor was previously focused"
    );
}

#[test]
fn secondary_window_ime_uses_source_focus_and_local_coordinates() {
    let probe = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(
        FocusProbe::default(),
    )));
    let source = KurboRect::new(-800.0, 200.0, -300.0, 700.0);
    let state = secondary_ime_state(&probe, "a", source, true).unwrap();
    assert_eq!(
        (state.x, state.y, state.width, state.height),
        (100, 300, 1, 20)
    );
    assert!(secondary_ime_state(&probe, "a", source, false).is_none());
    assert!(secondary_ime_state(&probe, "b", source, true).is_none());
}

#[test]
fn secondary_accesskit_event_keeps_originating_native_window() {
    let window_id = winit::window::WindowId::dummy();
    let event: UserEvent = accesskit_winit::Event {
        window_id,
        window_event: accesskit_winit::WindowEvent::InitialTreeRequested,
    }
    .into();
    assert!(
        matches!(event, UserEvent::AccessKit { window_id: source, event: accesskit_winit::WindowEvent::InitialTreeRequested } if source == window_id)
    );
}

#[test]
fn native_focus_forwarding_preserves_surface_identity() {
    let probe = FocusProbe::default();
    let events = std::rc::Rc::clone(&probe.events);
    let mut root = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(probe)));
    forward_native_surface_focus(&mut root, Some("agent-a"), &WindowEvent::Focused(true));
    forward_native_surface_focus(&mut root, Some("agent-b"), &WindowEvent::Focused(true));
    forward_native_surface_focus(&mut root, Some("agent-a"), &WindowEvent::Focused(false));
    assert_eq!(
        *events.borrow(),
        vec![
            (Some("agent-a".into()), true),
            (Some("agent-b".into()), true),
            (Some("agent-a".into()), false),
        ]
    );
}
