use super::*;
use crate::widget::TaffyTreeExt;
use crate::widget::{Button, Element};
use std::cell::Cell;
use std::rc::Rc;

/// Legacy composition deliberately does not report its breakpoint changes.
struct ResponsiveRoot {
    rect: KurboRect,
    children: Vec<Element>,
    nodes: Vec<NodeId>,
    builds: Rc<Cell<usize>>,
    editing: bool,
    trap: bool,
    modal_on_narrow: bool,
    native_calls: Option<Rc<std::cell::RefCell<Vec<NativeCall>>>>,
    scroll: Rc<Cell<(f32, f32)>>,
    oscillate: Rc<Cell<bool>>,
    paints: Rc<Cell<usize>>,
}
fn body_editor(path: Vec<crate::presence::PathSegment>, id: &str) -> Element {
    Box::new(crate::testing::OwnedText::new(
        "body",
        crate::widget::TextInput::new(path).with_id(id),
    ))
}
impl ResponsiveRoot {
    fn new(builds: Rc<Cell<usize>>) -> Self {
        Self {
            rect: KurboRect::ZERO,
            children: vec![],
            nodes: vec![],
            builds,
            editing: false,
            trap: false,
            modal_on_narrow: false,
            native_calls: None,
            scroll: Rc::new(Cell::new((0.0, 120.0))),
            oscillate: Rc::default(),
            paints: Rc::default(),
        }
    }
}
impl Widget for ResponsiveRoot {
    fn name(&self) -> &'static str {
        "ResponsiveRoot"
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        for (child, node) in self.children.iter_mut().zip(&self.nodes) {
            child.handle_event(event, &mut ctx.reborrow(ctx.path, *node));
        }
    }
    fn composition_follows_allocation(&self) -> bool {
        true
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.builds.set(self.builds.get().saturating_add(1));
        let count = if self.rect.width() >= 1100.0 {
            3
        } else if self.rect.width() >= 800.0 {
            2
        } else {
            1
        };
        self.children = (0..count)
            .map(|index| Box::new(Button::new(&format!("action-{index}"), "Action")) as Element)
            .collect();
        if self.editing {
            self.children = vec![
                body_editor(
                    vec![crate::presence::PathSegment::Field("body".into())],
                    "editor",
                ),
                Box::new(Button::new("other", "Other")),
            ];
            if self.rect.width() < 1100.0 {
                self.children.reverse();
            }
            self.children.push(Box::new(
                crate::widget::Scrollable::new_vertical()
                    .with_height(100.0)
                    .with_persisted_offset(self.scroll.clone())
                    .with_child(Box::new(
                        crate::widget::Container::new()
                            .with_no_padding()
                            .with_height(1000.0),
                    )),
            ));
        }
        if self.modal_on_narrow {
            // This fixture opens a modal whose fallback is another editor,
            // so accidental transfer of the outside selection is observable.
            if self.rect.width() < 1100.0 {
                self.children.swap(0, 1);
            }
            let mut body = crate::widget::Column::new();
            for child in self.children.drain(..2) {
                body = body.push(child);
            }
            let mut modal = crate::widget::ModalDialog::new("Modal", body);
            modal.set_open(self.rect.width() < 1100.0);
            self.children = vec![body_editor(vec![], "outside"), Box::new(modal)];
        }
        if let Some(calls) = &self.native_calls {
            self.children = vec![Box::new(crate::widget::NativeView::new(
                "Native",
                CountingNativeBackend {
                    generation: self.builds.get(),
                    calls: calls.clone(),
                },
            ))];
        }
        self.nodes = self
            .children
            .iter_mut()
            .map(|child| child.build_layout(taffy, theme, map))
            .collect();
        let style = taffy::Style {
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::length(if self.editing {
                    400.0
                } else if self.oscillate.get() && self.rect.height() == 100.0 {
                    101.0
                } else {
                    100.0
                }),
            },
            ..Default::default()
        };
        let node = taffy.parent_node(style, &self.nodes);
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: vello::kurbo::Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = KurboRect::from_origin_size(
            origin,
            (f64::from(layout.size.width), f64::from(layout.size.height)),
        );
        for (child, node) in self.children.iter_mut().zip(&self.nodes) {
            child.apply_layout(taffy, *node, origin);
        }
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        affine: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.paints.set(self.paints.get().saturating_add(1));
        for child in &mut self.children {
            child.draw(scene, affine, text, theme, presence);
        }
    }
    fn get_rect(&self) -> KurboRect {
        self.rect
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.children.iter().map(AsRef::as_ref).collect()
    }
    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        self.children
            .iter_mut()
            .map(|child| child.as_mut() as &mut dyn Widget)
            .collect()
    }
    fn traps_focus(&self) -> bool {
        self.trap
    }
    fn layout_needs_measure(&self) -> bool {
        false
    }
}
fn frame(app: &mut App, width: u32) {
    app.layout.layout_dirty = true;
    run_layout_pass(
        app,
        app.layout.root_node.unwrap(),
        width,
        400,
        1.0,
        false,
        None,
    )
    .unwrap();
}
#[test]
fn viewport_changes_recompose_like_a_fresh_app_before_paint() {
    // Case `legacy_composition_matches_fresh_at_multiple_viewports_before_paint`.
    {
        let builds = Rc::new(Cell::new(0));
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(ResponsiveRoot::new(builds.clone())),
        );
        app.init_layout();
        for (width, count) in [(1200, 3), (920, 2), (600, 1), (1200, 3), (920, 2)] {
            frame(&mut app, width);
            assert_eq!(app.root_widget.children().len(), count, "viewport {width}");
            let mut fresh = App::new_with_controlled_inputs(
                State::new(),
                Box::new(ResponsiveRoot::new(Rc::new(Cell::new(0)))),
            );
            fresh.init_layout();
            frame(&mut fresh, width);
            assert_eq!(
                app.root_widget
                    .children()
                    .iter()
                    .map(|child| child.get_rect())
                    .collect::<Vec<_>>(),
                fresh
                    .root_widget
                    .children()
                    .iter()
                    .map(|child| child.get_rect())
                    .collect::<Vec<_>>()
            );
        }
        let before = builds.get();
        frame(&mut app, 920);
        assert_eq!(
            builds.get(),
            before,
            "translation-only frames do not rebuild composition"
        );
        run_layout_pass(&mut app, NodeId::from(0usize), 920, 400, 1.0, false, None).unwrap();
        assert_eq!(
            builds.get(),
            before,
            "paint-only frames do not rebuild composition"
        );
    }

    // Case `logical_viewport_changes_recompose_even_at_unchanged_physical_size`.
    {
        let builds = Rc::new(Cell::new(0));
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(ResponsiveRoot::new(builds.clone())),
        );
        app.init_layout();
        frame(&mut app, 1200);
        let node = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, node, 1200, 400, 2.0, false, None).unwrap();
        assert_eq!(app.root_widget.children().len(), 1);
        let before = builds.get();
        let node = app.layout.root_node.unwrap();
        app.layout.layout_dirty = true;
        run_layout_pass(&mut app, node, 600, 200, 1.0, false, None).unwrap();
        assert_eq!(
            builds.get(),
            before,
            "equal logical viewport does not recompose"
        );
    }

    // Case `pending_structure_rebuild_is_measured_at_an_unchanged_viewport`.
    {
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(ResponsiveRoot::new(Rc::default())),
        );
        app.init_layout();
        frame(&mut app, 1200);
        app.layout.structure_dirty = true;
        frame(&mut app, 1200);
        assert_eq!(app.root_widget.get_rect().width(), 1200.0);
        assert!(
            app.root_widget
                .children()
                .iter()
                .all(|child| child.get_rect().height() > 0.0)
        );
    }

    // Case `oscillating_composition_is_bounded_and_headless_skips_paint`.
    {
        let root = ResponsiveRoot::new(Rc::default());
        let oscillate = root.oscillate.clone();
        let paints = root.paints.clone();
        let builds = root.builds.clone();
        let mut driver = harness(State::new(), root, 1200);
        oscillate.set(true);
        let before = builds.get();
        let painted = paints.get();
        assert!(matches!(
            driver.resize((920, 400)),
            Err(crate::widget::WidgetLayoutError::CompositionDidNotSettle)
        ));
        assert!(builds.get() - before <= 8);
        assert_eq!(
            paints.get(),
            painted,
            "failed settlement never reaches paint"
        );
        oscillate.set(false);
        driver.resize((920, 400)).unwrap();
        assert_eq!(paints.get(), painted + 1);

        let root = ResponsiveRoot::new(Rc::default());
        root.oscillate.set(true);
        let builds = root.builds.clone();
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
        app.init_layout();
        let root = app.layout.root_node.unwrap();
        assert!(run_layout_pass(&mut app, root, 1200, 400, 1.0, false, None).is_none());
        assert!(builds.get() <= 8);
        assert!(app.layout.last_layout_logical.is_none());
    }

    // Case `review_failed_headless_resize_can_return_to_last_successful_size`.
    {
        let root = ResponsiveRoot::new(Rc::default());
        let oscillate = root.oscillate.clone();
        let mut driver = harness(State::new(), root, 1200);
        oscillate.set(true);
        assert!(driver.resize((920, 400)).is_err());
        oscillate.set(false);
        driver.resize((1200, 400)).unwrap();
        assert_eq!(driver.inspection().viewport, [1200, 400]);
        assert_eq!(driver.inspection().root.children.len(), 3);
        assert_eq!(driver.inspection().root.rect[2], 1200.0);
        driver.resize((920, 400)).unwrap();
        assert_eq!(driver.inspection().root.children.len(), 2);
    }

    // Case `review_native_views_mount_only_after_successful_final_composition`.
    {
        let mut root = ResponsiveRoot::new(Rc::default());
        let calls = Rc::new(std::cell::RefCell::new(Vec::new()));
        root.native_calls = Some(calls.clone());
        let builds = root.builds.clone();
        let oscillate = root.oscillate.clone();
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
        app.init_layout();
        let host = crate::widget::NativeViewHost::headless(app.input.platform, 1.0);
        crate::widget::mount_native_views(app.root_widget.as_mut(), &host);
        for width in [1200, 920] {
            let mounts = calls
                .borrow()
                .iter()
                .filter(|call| matches!(call, NativeCall::Mount(_)))
                .count();
            app.layout.layout_dirty = true;
            let node = app.layout.root_node.unwrap();
            run_layout_pass(&mut app, node, width, 400, 1.0, false, Some(&host)).unwrap();
            let final_generation = builds.get();
            assert_eq!(
                calls
                    .borrow()
                    .iter()
                    .filter(|call| matches!(call, NativeCall::Mount(_)))
                    .count(),
                mounts + 1,
                "mount final composition exactly once"
            );
            assert!(
                calls
                    .borrow()
                    .contains(&NativeCall::Mount(final_generation))
            );
            assert!(calls.borrow().iter().any(|call| matches!(call, NativeCall::Geometry(generation, rect) if *generation == final_generation && rect.width() == f64::from(width))));
            let mut scene = Scene::new();
            app.root_widget.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut app.text_renderer,
                &app.theme,
                &PresenceCtx::default(),
            );
            assert!(
                calls
                    .borrow()
                    .contains(&NativeCall::Visible(final_generation, true))
            );
        }
        let mounts = calls
            .borrow()
            .iter()
            .filter(|call| matches!(call, NativeCall::Mount(_)))
            .count();
        oscillate.set(true);
        app.layout.layout_dirty = true;
        let node = app.layout.root_node.unwrap();
        assert!(run_layout_pass(&mut app, node, 600, 400, 1.0, false, Some(&host)).is_none());
        assert_eq!(
            calls
                .borrow()
                .iter()
                .filter(|call| matches!(call, NativeCall::Mount(_)))
                .count(),
            mounts,
            "failed intermediate compositions never attach"
        );
        oscillate.set(false);
        let node = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, node, 920, 400, 1.0, false, Some(&host)).unwrap();
        assert_eq!(
            app.root_widget.get_rect().width(),
            920.0,
            "live recovery also invalidates failed geometry"
        );
    }
}

fn editing_fixture() -> (State, ResponsiveRoot) {
    use automerge::transaction::Transactable;
    let mut state = State::new();
    let body = state
        .doc
        .put_object(&automerge::ROOT, "body", automerge::ObjType::Text)
        .unwrap();
    state.doc.splice_text(&body, 0, 0, "hello world").unwrap();
    let mut root = ResponsiveRoot::new(Rc::default());
    root.editing = true;
    (state, root)
}

#[test]
fn resize_restores_editor_identity_selection_and_focus() {
    // Case `resize_restores_fresh_editor_by_identity_with_selection_and_scroll`.
    {
        let (state, root) = editing_fixture();
        let scroll = root.scroll.clone();
        let mut app = App::new_with_controlled_inputs(state, Box::new(root));
        app.init_layout();
        frame(&mut app, 1200);
        app.set_focused_node(Some(app.focusable_order[0]));
        app.mutate_node(app.focused_node.unwrap(), |widget| {
            crate::undo::set_state(widget, "5:b:1");
        })
        .unwrap();
        for width in [920, 600, 1200] {
            frame(&mut app, width);
            let node = app.focused_node.expect("editor remains focused");
            let expected_index = usize::from(width < 1100);
            assert_eq!(
                node, app.focusable_order[expected_index],
                "focus follows identity across reordered controls"
            );
            assert_eq!(
                app.inspect_node(node, |widget| widget.accessibility().unwrap().semantic_key)
                    .unwrap()
                    .as_deref(),
                Some("editor")
            );
            assert_eq!(
                app.inspect_node(node, |widget| crate::undo::get_state(widget))
                    .unwrap()
                    .as_deref(),
                Some("5:b:1")
            );
            assert_eq!(
                app.inspect_node(node, |widget| widget.accessibility().unwrap().value)
                    .unwrap()
                    .as_deref(),
                Some("hello world")
            );
            assert_eq!(scroll.get(), (0.0, 120.0));
            assert!(
                app.inspect_node(node, |widget| widget.get_rect().width())
                    .unwrap()
                    > 0.0
            );
        }
    }

    // Case `headless_resize_matches_fresh_and_preserves_selected_edit_target`.
    {
        let builds = Rc::new(Cell::new(0));
        let mut driver = harness(State::new(), ResponsiveRoot::new(builds.clone()), 1200);
        for width in [920, 600, 1200] {
            driver.resize((width, 400)).unwrap();
            let fresh = harness(State::new(), ResponsiveRoot::new(Rc::default()), width);
            assert_eq!(driver.inspection().root.rect, fresh.inspection().root.rect);
            assert_eq!(
                driver
                    .inspection()
                    .root
                    .children
                    .iter()
                    .map(|child| child.rect)
                    .collect::<Vec<_>>(),
                fresh
                    .inspection()
                    .root
                    .children
                    .iter()
                    .map(|child| child.rect)
                    .collect::<Vec<_>>()
            );
        }
        let before = builds.get();
        driver.resize((1200, 400)).unwrap();
        assert_eq!(builds.get(), before);

        let (state, root) = editing_fixture();
        let scroll = root.scroll.clone();
        let mut editor = harness(state, root, 1200);
        editor.tab(false).expect("the editor is the first tab stop");
        editor.set_modifiers(&["ctrl".into()]);
        editor.key("a");
        editor.set_modifiers(&[]);
        editor.resize((920, 400)).unwrap();
        assert_eq!(
            editor
                .focused_access_node()
                .unwrap()
                .semantic_key
                .as_deref(),
            Some("editor")
        );
        assert_eq!(scroll.get(), (0.0, 120.0));
        editor.type_text("X");
        assert_eq!(
            editor.focused_access_node().unwrap().value.as_deref(),
            Some("X"),
            "typing replaces the selection retained across fresh composition"
        );
    }

    // Case `review_active_trap_retains_keyed_selection_after_replacing_and_reordering_controls`.
    {
        let (state, mut root) = editing_fixture();
        root.trap = true;
        let mut app = App::new_with_controlled_inputs(state, Box::new(root));
        app.init_layout();
        frame(&mut app, 1200);
        let editor = app.focusable_order[0];
        app.set_focused_node(Some(editor));
        app.mutate_node(editor, |widget| crate::undo::set_state(widget, "5:b:1"))
            .unwrap();
        for width in [920, 600, 1200] {
            frame(&mut app, width);
            assert!(app.accessibility.focus_trap_active);
            let focused = app.focused_node.expect("modal editor remains focused");
            assert!(app.focusable_order.contains(&focused));
            assert_eq!(
                app.inspect_node(focused, |widget| widget
                    .accessibility()
                    .unwrap()
                    .semantic_key)
                    .unwrap()
                    .as_deref(),
                Some("editor")
            );
            assert!(
                app.inspect_node(focused, |widget| widget.is_focused())
                    .unwrap()
            );
            assert_eq!(
                app.inspect_node(focused, |widget| crate::undo::get_state(widget))
                    .unwrap()
                    .as_deref(),
                Some("5:b:1")
            );
        }
    }

    // Case `review_new_modal_does_not_restore_outside_focus_or_copy_selection_to_fallback`.
    {
        let (state, mut root) = editing_fixture();
        root.modal_on_narrow = true;
        let mut app = App::new_with_controlled_inputs(state, Box::new(root));
        app.init_layout();
        frame(&mut app, 1200);
        assert!(!app.accessibility.focus_trap_active);
        let outside = app.focusable_order[0];
        app.set_focused_node(Some(outside));
        app.mutate_node(outside, |widget| crate::undo::set_state(widget, "5:b:1"))
            .unwrap();
        frame(&mut app, 920);
        assert!(app.accessibility.focus_trap_active);
        let focused = app.focused_node.expect("modal owns focus");
        assert!(app.focusable_order.contains(&focused));
        assert!(
            app.inspect_node(focused, |widget| widget.is_focused())
                .unwrap()
        );
        assert_ne!(
            app.inspect_node(focused, |widget| widget
                .accessibility()
                .unwrap()
                .semantic_key)
                .unwrap()
                .as_deref(),
            Some("outside")
        );
        assert_eq!(
            app.inspect_node(focused, |widget| crate::undo::get_state(widget))
                .unwrap()
                .as_deref(),
            Some("0"),
            "fallback editor keeps its own caret and no selection"
        );
        let outside = app
            .layout
            .node_map
            .keys()
            .copied()
            .find(|&node| {
                app.inspect_node(node, |widget| {
                    widget
                        .accessibility()
                        .and_then(|access| access.semantic_key)
                        .as_deref()
                        == Some("outside")
                })
                .unwrap_or(false)
            })
            .unwrap();
        assert!(
            !app.inspect_node(outside, |widget| widget.is_focused())
                .unwrap()
        );
    }
}

fn harness(state: State, root: ResponsiveRoot, width: u32) -> crate::headless::FrameHarness {
    crate::headless::FrameHarness::new_with_controlled_inputs(
        state,
        Box::new(root),
        (width, 400),
        Theme::dark(),
        crate::font_catalog::fixture_renderer(),
        crate::permissions::UserId("resize-test".into()),
    )
}

#[derive(Clone, Debug, PartialEq)]
enum NativeCall {
    Mount(usize),
    Unmount(usize),
    Geometry(usize, KurboRect),
    Visible(usize, bool),
}
struct CountingNativeBackend {
    generation: usize,
    calls: Rc<std::cell::RefCell<Vec<NativeCall>>>,
}
impl crate::widget::NativeViewBackend for CountingNativeBackend {
    fn mount(&mut self, _: &crate::widget::NativeViewHost<'_>) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(NativeCall::Mount(self.generation));
        Ok(())
    }
    fn unmount(&mut self) {
        self.calls
            .borrow_mut()
            .push(NativeCall::Unmount(self.generation));
    }
    fn set_geometry(&mut self, geometry: crate::widget::NativeViewGeometry) {
        self.calls
            .borrow_mut()
            .push(NativeCall::Geometry(self.generation, geometry.logical));
    }
    fn set_visible(&mut self, visible: bool) {
        self.calls
            .borrow_mut()
            .push(NativeCall::Visible(self.generation, visible));
    }
    fn set_focus(&mut self, _: bool) {}
    fn set_enabled(&mut self, _: bool) {}
}

#[test]
fn native_slider_rebuilds_its_target_style_when_input_capabilities_change() {
    let value = crate::LocalState::new(0.5);
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(crate::widget::Slider::from_state(&value)),
    );
    app.init_layout();
    frame(&mut app, 600);
    for environment in [
        crate::InteractionEnvironment::TOUCH,
        crate::InteractionEnvironment::POINTER,
    ] {
        app.set_interaction_environment(environment);
        frame(&mut app, 600);
        assert_eq!(
            app.root_widget.get_rect().height(),
            f64::from(environment.minimum_target()),
            "the native host applies the current input target, not its old cached style"
        );
        assert!(!app.root_widget.layout_needs_measure());
        assert_eq!(value.get(), 0.5, "layout never changes the value");
    }
}
