//! The native-input observer reads the app without touching its document.
//!
//! The snapshot a native audit asks for must describe the live state (the
//! selected category, the layout class, the document heads) without
//! committing the document's pending changes: the observer is a witness, not
//! a writer.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::SettingsApp;
use automerge::AutoCommit;
use std::collections::HashMap;
const W: f32 = 1200.0;
const H: f32 = 1800.0;
use taffy::{NodeId, TaffyTree};
use ui::framework::HostDelivery as _;
use ui::text_render::TextRenderer;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, PhysicalKey};

use crate::categories::Category;

fn root() -> automerge::ObjId {
    automerge::ROOT
}

use ui::synthetic_input::char_key;

/// A faithful stand-in for `ui::App` covering focus + the rebuild-on-filter
/// cycle. Owns the app, its live taffy tree + node map, and the focus state.
struct Harness {
    app: Box<SettingsApp>,
    doc: AutoCommit,
    taffy: TaffyTree<()>,
    node_map: HashMap<NodeId, *mut dyn Widget>,
    /// The `SettingsApp` root node (what `App::root_node` holds).
    app_node: NodeId,
    focused: Option<NodeId>,
    focus_order: Vec<NodeId>,
    viewport: (f32, f32),
    /// Dropped last: the app's stores live under this guard's config root.
    _config: crate::prefs::test_env::ScratchConfig,
}

impl Harness {
    fn new() -> Self {
        Self::at_size(W, H)
    }

    fn at_size(width: f32, height: f32) -> Self {
        let config = crate::prefs::test_env::ScratchConfig::new();
        let mut h = Harness {
            app: Box::new(config.app()),
            doc: AutoCommit::new(),
            taffy: TaffyTree::new(),
            node_map: HashMap::new(),
            app_node: NodeId::new(0),
            focused: None,
            focus_order: Vec::new(),
            viewport: (width, height),
            _config: config,
        };
        h.layout();
        // AppShell first learns the real width in apply_layout and requests
        // another structural pass when it crosses the narrow breakpoint.
        // The live host drains that request before accepting settled input.
        for _ in 0..4 {
            if !h.app.take_structure_dirty() {
                return h;
            }
            h.rebuild();
        }
        panic!("native Settings harness did not settle initial structure");
    }

    /// Run the host's per-frame cycle: `sync` (seed) → `build_layout` →
    /// `compute_layout(measure)` → `apply_layout` → `sync` (settle) → refresh
    /// the focusable order. Mirrors `shots::render_to_path` + `App::init_layout`.
    fn layout(&mut self) {
        let theme = ui::Theme::dark();
        self.app.sync(&self.doc, &root());
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let app_node = self.app.build_layout(&mut taffy, &theme, &mut node_map);
        let outer = taffy
            .new_with_children(
                taffy::Style {
                    size: taffy::Size {
                        width: taffy::Dimension::length(self.viewport.0),
                        height: taffy::Dimension::length(self.viewport.1),
                    },
                    ..Default::default()
                },
                &[app_node],
            )
            .unwrap();
        self.app.sync(&self.doc, &root());
        let mut tr = TextRenderer::new();
        let mut bindings = ui::bound_dispatch::BoundNodeMap::new();
        // SAFETY: this harness exclusively owns this unchanged app and its
        // exact live layout pointers for both admission and measurement.
        unsafe {
            bindings.rebuild(&*self.app, &taffy, outer, &node_map);
            ui::widget::compute_widget_layout(
                &mut taffy,
                outer,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(self.viewport.0),
                    height: taffy::AvailableSpace::Definite(self.viewport.1),
                },
                &node_map,
                &bindings,
                &mut tr,
            )
            .unwrap();
        }
        self.app.apply_layout(&taffy, app_node, Point::ORIGIN);
        // Settling sync — the just-built leaf controls (search field, rows)
        // read back their bound CRDT values (this is what the shots harness
        // and the live app do after a structural rebuild).
        self.app.sync(&self.doc, &root());

        self.taffy = taffy;
        self.node_map = node_map;
        self.app_node = app_node;
        self.refresh_focus_order();
    }

    /// Replicate `App::update_focusable_order`'s `collect_focusable`: a
    /// pre-order walk zipping `widget.children()` with `taffy.children`,
    /// collecting the nodes of focusable widgets. Because it walks the MAIN
    /// taffy tree, a widget inside `NavSplit`'s isolated inner tree is
    /// unreachable here — which is exactly why the search field must be a
    /// top-level child to be focusable.
    fn refresh_focus_order(&mut self) {
        fn walk(node: NodeId, w: &dyn Widget, taffy: &TaffyTree<()>, out: &mut Vec<NodeId>) {
            if w.is_focusable() && w.is_enabled() {
                out.push(node);
            }
            if let Ok(child_nodes) = taffy.children(node) {
                for (cw, &cn) in w.children().iter().zip(child_nodes.iter()) {
                    walk(cn, *cw, taffy, out);
                }
            }
        }
        let mut order = Vec::new();
        walk(self.app_node, &*self.app, &self.taffy, &mut order);
        self.focus_order = order;
    }

    /// Apply focus exactly like `App::set_focused_node`: `set_focus(false)` on
    /// the old node, `set_focus(true)` on the new one, via the node map.
    fn set_focused(&mut self, new: Option<NodeId>) {
        if self.focused == new {
            return;
        }
        if let Some(old) = self.focused
            && let Some(&w) = self.node_map.get(&old)
        {
            unsafe { (*w).set_focus(false) };
        }
        self.focused = new;
        if let Some(id) = new
            && let Some(&w) = self.node_map.get(&id)
        {
            unsafe { (*w).set_focus(true) };
        }
    }

    /// Drive a full left click (cursor → press → release) at `p` through the
    /// production event path, then apply any requested focus target — exactly
    /// what `App`'s window-event handler does at the end of dispatch.
    fn click(&mut self, p: Point) {
        let mods = Modifiers::default();
        let cursor = WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        };
        let press = WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        let release = WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        };
        let mut focus_target = None;
        for ev in [&cursor, &press, &release] {
            let mut handled = false;
            self.app.handle_event(
                ev,
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &mods,
                    state: ui::widget::Edits::lend(&mut self.doc),
                    path: &root(),
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: self.app_node,
                    taffy: &self.taffy,
                },
            );
            // Match the live host's action-bound control dispatch boundary,
            // also exercised by tests/control_interaction.rs. Back is a View
            // action; emitting its intent alone does not activate it.
            while let Some(intent) = ui::widget::controls::take_control_invocation(&mut *self.app) {
                let control = intent
                    .address
                    .resource
                    .strip_prefix("tilo.control:")
                    .unwrap();
                let payload = serde_json::to_vec(control).unwrap();
                let started = self
                    .app
                    .start_encoded(
                        tilo_actions::OperationKind::View,
                        "tilo.ui.control.invoke",
                        &payload,
                        &mut self.doc,
                        &root(),
                    )
                    .unwrap();
                assert!(started.is_some(), "Settings handled its control action");
            }
        }
        if let Some(id) = focus_target {
            self.set_focused(Some(id));
        }
    }

    /// Type one character through the production path, then run the frame's
    /// sync + (if the tree changed) the ordinal-preserving rebuild — the same
    /// thing `App` does with `autofocus_on_structure_change` set.
    fn type_char(&mut self, c: &str) {
        let mods = Modifiers::default();
        let ev = char_key(c);
        let mut handled = false;
        let mut focus_target = None;
        self.app.handle_event(
            &ev,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: ui::widget::Edits::lend(&mut self.doc),
                path: &root(),
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: self.app_node,
                taffy: &self.taffy,
            },
        );
        if let Some(id) = focus_target {
            self.set_focused(Some(id));
        }
        // Frame: sync consumes the field's SEARCH_KEY write; a changed query
        // flags a structure rebuild, which we run preserving focus by ordinal.
        self.app.sync(&self.doc, &root());
        if Widget::take_structure_dirty(&mut *self.app) {
            self.rebuild();
        }
    }

    /// The `App::rebuild_structure` behaviour under
    /// `autofocus_on_structure_change`: capture the focused ordinal + its
    /// ephemeral caret state, rebuild, then restore focus to the same ordinal
    /// and paste the caret back.
    fn rebuild(&mut self) {
        let prev_idx = self
            .focused
            .and_then(|id| self.focus_order.iter().position(|&x| x == id));
        let prev_state = self
            .focused
            .and_then(|id| self.node_map.get(&id).copied())
            .and_then(|w| unsafe { ui::undo::get_state(&(*w)) });
        // The old node ids are about to be invalidated (fresh taffy tree).
        self.focused = None;
        self.layout();
        if let Some(idx) = prev_idx
            && let Some(&id) = self.focus_order.get(idx)
        {
            self.set_focused(Some(id));
            if let Some(s) = &prev_state
                && let Some(&w) = self.node_map.get(&id)
            {
                unsafe { ui::undo::set_state(&mut (*w), s) };
            }
        }
    }

    /// The first `TextInput` in a pre-order walk — the top-strip search field
    /// (the strip is the app's first child, so it precedes any pane input).
    fn search_field(&self) -> Option<&dyn Widget> {
        fn walk<'a>(w: &'a dyn Widget, out: &mut Option<&'a dyn Widget>) {
            if out.is_some() {
                return;
            }
            if w.test_id() == Some("settings.search") {
                *out = Some(w);
                return;
            }
            for c in w.children() {
                walk(c, out);
            }
        }
        let mut out = None;
        walk(&*self.app, &mut out);
        out
    }

    fn search_field_rect(&self) -> vello::kurbo::Rect {
        self.search_field()
            .expect("a search TextInput exists")
            .get_rect()
    }

    fn sidebar_row_count(&self) -> usize {
        fn walk(w: &dyn Widget, n: &mut usize) {
            if let Some(list) = w
                .as_any()
                .and_then(|value| value.downcast_ref::<ui::widget::NavList>())
            {
                *n = n.saturating_add(list.visible_item_ids().len());
            }
            for c in w.children() {
                walk(c, n);
            }
        }
        let mut n = 0;
        walk(&*self.app, &mut n);
        n
    }
}

/// The native observer tracks search rebuilds/backspace and never commits pending changes.
#[test]
fn native_observer_tracks_search_without_committing_pending_changes() {
    // Case `native_observer_tracks_search_rebuild_and_backspace`.
    {
        let mut h = Harness::new();
        let p = h.search_field_rect().center();
        h.click(p);
        let before = h.app.native_input_snapshot(&h.doc, &root()).unwrap();
        assert_eq!(before["ready"], true);
        assert_eq!(before["rendered_result_count"], h.sidebar_row_count());
        assert_eq!(
            before["search_focused"], true,
            "observer={before:#}; focused={:?}; clicked={p:?}",
            h.focused
        );
        let original_heads = h.doc.get_heads();
        h.type_char("z");
        let typed = h.app.native_input_snapshot(&h.doc, &root()).unwrap();
        assert_eq!(typed["local_search_text"], "z");
        assert_eq!(typed["synchronized_search_query"], "z");
        assert_eq!(typed["search_widget_text"], "z");
        assert_eq!(typed["search_caret_byte"], 1);
        assert_eq!(typed["search_focused"], true);
        assert_eq!(typed["rendered_result_count"], h.sidebar_row_count());
        assert_ne!(
            typed["rendered_category_ids"],
            before["rendered_category_ids"]
        );
        // Native key construction, delivered through the same root widget route.
        let mut ev = char_key("");
        if let WindowEvent::KeyboardInput { event, .. } = &mut ev {
            event.logical_key = Key::Named(winit::keyboard::NamedKey::Backspace);
            event.text = None;
        }
        let mut handled = false;
        h.app.handle_event(
            &ev,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: ui::widget::Edits::lend(&mut h.doc),
                path: &root(),
                handled: &mut handled,
                focus_target: &mut None,
                node_id: h.app_node,
                taffy: &h.taffy,
            },
        );
        let immediate = h.app.native_input_snapshot(&h.doc, &root()).unwrap();
        assert_eq!(immediate["local_search_text"], "");
        assert_eq!(immediate["synchronized_search_query"], "z");
        h.app.sync(&h.doc, &root());
        if h.app.take_structure_dirty() {
            h.rebuild();
        }
        let after = h.app.native_input_snapshot(&h.doc, &root()).unwrap();
        assert_eq!(after["search_caret_byte"], 0);
        assert_eq!(after["search_focused"], true);
        assert_eq!(
            after["rendered_category_ids"],
            before["rendered_category_ids"]
        );
        assert_eq!(h.doc.get_heads(), original_heads);
        assert_eq!(
            after["effective_document_heads"],
            before["effective_document_heads"]
        );
    }

    // Case `native_observer_does_not_commit_original_pending_changes`.
    {
        use automerge::transaction::Transactable;
        let config = crate::prefs::test_env::ScratchConfig::new();
        let app = config.app();
        let mut doc = AutoCommit::new();
        doc.put(root(), "diagnostic_test", "pending").unwrap();
        let debug_before = format!("{doc:?}");
        let pending = doc.pending_ops();
        let a = app.native_input_snapshot(&doc, &root()).unwrap();
        let b = app.native_input_snapshot(&doc, &root()).unwrap();
        assert_eq!(a, b);
        assert_eq!(a["ready"], false);
        assert_eq!(a["original_pending_ops"], pending);
        assert_eq!(format!("{doc:?}"), debug_before);
        assert_eq!(doc.pending_ops(), pending);
        assert!(!a["effective_document_heads"].as_array().unwrap().is_empty());
        assert!(a.get("document_content").is_none());
    }
}

fn named_widget_identity(widget: &dyn Widget, name: &str) -> Option<usize> {
    if widget.name() == name {
        return Some(std::ptr::from_ref(widget).cast::<()>() as usize);
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| named_widget_identity(child, name))
}

#[test]
fn search_preserves_production_network_pane_but_category_switch_replaces_it() {
    let mut h = Harness::new();
    let network = named_widget_identity(&*h.app, "NetworkPane").unwrap();
    h.click(h.search_field_rect().center());
    h.type_char("z");
    assert_eq!(named_widget_identity(&*h.app, "NetworkPane"), Some(network));
    h.app.select(super::Category::About);
    h.rebuild();
    assert_eq!(h.app.selected(), super::Category::About);
    assert_eq!(named_widget_identity(&*h.app, "NetworkPane"), None);
}

#[test]
fn narrow_search_retains_focus_through_filter_and_backspace() {
    let mut h = Harness::at_size(440.0, 820.0);
    assert!(h.app.is_narrow());
    // Reach the list using the real narrow back control, not a screenshot override.
    let back = h.app.back_button_rect();
    assert!(back.width() > 0.0);
    h.click(back.center());
    h.app.sync(&h.doc, &root());
    if h.app.take_structure_dirty() {
        h.rebuild();
    }
    assert!(h.app.sidebar_open());
    h.click(h.search_field_rect().center());
    h.type_char("z");
    let typed = h.app.native_input_snapshot(&h.doc, &root()).unwrap();
    assert_eq!(typed["search_focused"], true);
    assert_eq!(typed["search_caret_byte"], 1);
    assert_eq!(typed["local_search_text"], "z");
    assert_eq!(typed["sidebar_open"], true);
    assert_eq!(typed["narrow"], true);
    let mut ev = char_key("");
    if let WindowEvent::KeyboardInput { event, .. } = &mut ev {
        event.logical_key = Key::Named(winit::keyboard::NamedKey::Backspace);
        event.text = None;
    }
    h.app.handle_event(
        &ev,
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &Modifiers::default(),
            state: ui::widget::Edits::lend(&mut h.doc),
            path: &root(),
            handled: &mut false,
            focus_target: &mut None,
            node_id: h.app_node,
            taffy: &h.taffy,
        },
    );
    h.app.sync(&h.doc, &root());
    if h.app.take_structure_dirty() {
        h.rebuild();
    }
    let restored = h.app.native_input_snapshot(&h.doc, &root()).unwrap();
    assert_eq!(restored["search_focused"], true);
    assert_eq!(restored["search_caret_byte"], 0);
    assert_eq!(restored["local_search_text"], "");
}

#[test]
fn native_observer_reports_the_selected_category_and_layout_class() {
    let config = crate::prefs::test_env::ScratchConfig::new();
    let app = config.app().with_category(Category::Appearance);
    let app = ui::headless::Framed::new(
        ui::State::new(),
        app,
        ui::headless::viewport((1200.0, 760.0)),
        ui::Theme::dark(),
    );
    let doc = AutoCommit::new();
    let snapshot = app.native_input_snapshot(&doc, &root()).unwrap();
    assert_eq!(snapshot["ready"], true);
    assert_eq!(snapshot["selected_category"], "appearance");
    assert_eq!(snapshot["narrow"], false);
    assert_eq!(snapshot["sidebar_open"], true);
    assert_eq!(snapshot["nav_id"], crate::sidebar::NAV_ID);
}

fn keyboard_map_pressed(widget: &dyn Widget, key: PhysicalKey) -> Option<bool> {
    if let Some(map) = widget
        .as_any()
        .and_then(|widget| widget.downcast_ref::<ui::KeyboardMap>())
    {
        return Some(map.is_pressed(key));
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| keyboard_map_pressed(child, key))
}

fn dispatch_preview_key(h: &mut Harness, state: ElementState) {
    let logical = winit::keyboard::Key::Character("z".into());
    dispatch_preview_event(
        h,
        &WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            is_synthetic: false,
            event: winit::event::KeyEvent::synthetic(
                winit::event::RawKeyEvent {
                    physical_key: PhysicalKey::Code(winit::keyboard::KeyCode::KeyY),
                    state,
                },
                logical.clone(),
                Some("z".into()),
                winit::keyboard::KeyLocation::Standard,
                false,
                Some("z".into()),
                logical,
            ),
        },
    );
}

fn dispatch_preview_event(h: &mut Harness, event: &WindowEvent) {
    let mods = Modifiers::default();
    let mut handled = false;
    let mut focus = None;
    h.app.handle_event(
        event,
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &mods,
            state: ui::widget::Edits::lend(&mut h.doc),
            path: &root(),
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: h.app_node,
            taffy: &h.taffy,
        },
    );
}

#[test]
fn keyboard_map_observes_foreground_physical_keys_while_search_keeps_editing() {
    let mut h = Harness::new();
    h.app.select(Category::MouseKeyboard);
    h.rebuild();
    h.click(h.search_field_rect().center());
    let compiled = crate::keyboard_preview::compilation_count();
    let key = PhysicalKey::Code(winit::keyboard::KeyCode::KeyY);
    let heads = h.doc.get_heads();
    assert_eq!(keyboard_map_pressed(&*h.app, key), Some(false));
    dispatch_preview_key(&mut h, ElementState::Pressed);
    assert_eq!(keyboard_map_pressed(&*h.app, key), Some(true));
    assert_eq!(h.app.search_state.with(Clone::clone), "z");
    assert_eq!(h.doc.get_heads(), heads, "pressed positions stay ephemeral");
    // Search invalidates the retained content. The same held physical position
    // survives that ordinary recomposition while editing remains focused.
    h.app.sync(&h.doc, &root());
    h.rebuild();
    assert_eq!(keyboard_map_pressed(&*h.app, key), Some(true));
    assert_eq!(
        crate::keyboard_preview::compilation_count(),
        compiled,
        "search recomposition must retain the bounded compiled preview"
    );
    let snapshot = h.app.native_input_snapshot(&h.doc, &root()).unwrap();
    assert_eq!(snapshot["search_focused"], true);
    dispatch_preview_key(&mut h, ElementState::Released);
    assert_eq!(keyboard_map_pressed(&*h.app, key), Some(false));
    assert_eq!(h.app.search_state.with(Clone::clone), "z");
    dispatch_preview_key(&mut h, ElementState::Pressed);
    dispatch_preview_event(&mut h, &WindowEvent::Focused(false));
    assert_eq!(keyboard_map_pressed(&*h.app, key), Some(false));
    dispatch_preview_event(&mut h, &WindowEvent::Focused(true));
    dispatch_preview_key(&mut h, ElementState::Pressed);
    h.app.select(Category::About);
    h.rebuild();
    assert_eq!(keyboard_map_pressed(&*h.app, key), None);
    h.app.select(Category::MouseKeyboard);
    h.rebuild();
    assert_eq!(keyboard_map_pressed(&*h.app, key), Some(false));
}

fn keyboard_map_symbols(
    widget: &dyn Widget,
    key: PhysicalKey,
) -> Option<Vec<winit::keyboard::KeyboardLayoutSymbol>> {
    if let Some(map) = widget
        .as_any()
        .and_then(|widget| widget.downcast_ref::<ui::KeyboardMap>())
    {
        return map.active_symbols(key);
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| keyboard_map_symbols(child, key))
}

fn keyboard_map_value(widget: &dyn Widget) -> Option<String> {
    if let Some(map) = widget
        .as_any()
        .and_then(|widget| widget.downcast_ref::<ui::KeyboardMap>())
    {
        return map.accessibility()?.value;
    }
    widget.children().into_iter().find_map(keyboard_map_value)
}

#[test]
fn focused_seat_configuration_survives_page_recomposition_without_off_page_key_capture() {
    let mut h = Harness::new();
    let key = PhysicalKey::Code(winit::keyboard::KeyCode::KeyY);
    let snapshot = |text: &str| {
        std::sync::Arc::new(winit::keyboard::KeyboardLayout {
            depressed: 1,
            latched: 0,
            locked: 0,
            group: 1,
            keys: vec![winit::keyboard::KeyboardLayoutKey {
                physical: key,
                symbols: vec![winit::keyboard::KeyboardLayoutSymbol {
                    name: text.into(),
                    text: text.into(),
                }],
            }],
        })
    };
    let original = snapshot("z");
    h.app
        .observe_physical_input(ui::widget::PhysicalInput::Layout(Some(original)));
    h.app.select(Category::MouseKeyboard);
    h.rebuild();
    assert_eq!(keyboard_map_symbols(&*h.app, key).unwrap()[0].text, "z");
    let value = keyboard_map_value(&*h.app).unwrap();
    assert!(value.contains("AD06: z"));
    assert!(
        !value.contains("AD06: y"),
        "live accessibility must not claim the preferred US symbol"
    );
    h.app.select(Category::About);
    h.rebuild();
    let active = snapshot("Z");
    h.app
        .observe_physical_input(ui::widget::PhysicalInput::Layout(Some(active)));
    h.app
        .observe_physical_input(ui::widget::PhysicalInput::Key {
            key,
            state: ElementState::Pressed,
        });
    h.app.select(Category::MouseKeyboard);
    h.rebuild();
    assert_eq!(keyboard_map_symbols(&*h.app, key).unwrap()[0].text, "Z");
    assert!(keyboard_map_value(&*h.app).unwrap().contains("AD06: Z"));
    assert_eq!(
        keyboard_map_pressed(&*h.app, key),
        Some(false),
        "off-page presses never enter the map"
    );
    let heads = h.doc.get_heads();
    h.app
        .observe_physical_input(ui::widget::PhysicalInput::Focused(false));
    assert_eq!(
        keyboard_map_symbols(&*h.app, key),
        None,
        "actual blur revokes native configuration"
    );
    let value = keyboard_map_value(&*h.app).unwrap();
    assert!(!value.contains("AD06: y"));
    assert!(!value.contains("AD06: Z"));
    assert_eq!(
        h.doc.get_heads(),
        heads,
        "configuration and press feedback stay out of CRDT state"
    );
}

#[test]
fn narrow_keyboard_inspection_has_touch_sized_selector_and_live_readable_details() {
    fn find(widget: &dyn Widget, id: &str) -> Option<(vello::kurbo::Rect, String)> {
        if widget.test_id() == Some(id) {
            return Some((
                widget.get_rect(),
                widget.accessibility().unwrap().name.unwrap_or_default(),
            ));
        }
        widget
            .children()
            .into_iter()
            .find_map(|child| find(child, id))
    }
    let mut h = Harness::at_size(600.0, 1800.0);
    h.app.select(Category::MouseKeyboard);
    h.rebuild();
    let (selector, _) = find(&*h.app, "input.map.inspect-key").unwrap();
    let minimum = f64::from(ui::Theme::dark().tokens.metrics.min_pointer_target);
    assert!(selector.width() >= minimum && selector.height() >= minimum);
    let (map, _) = find(&*h.app, "input.keyboard_map").unwrap();
    assert!(map.width() > 300.0, "the visual map remains prominent");
    let key = PhysicalKey::Code(winit::keyboard::KeyCode::KeyQ);
    for text in ["λ", "Λ"] {
        h.app
            .observe_physical_input(ui::widget::PhysicalInput::Layout(Some(
                std::sync::Arc::new(winit::keyboard::KeyboardLayout {
                    depressed: 1,
                    latched: 0,
                    locked: 0,
                    group: 1,
                    keys: vec![winit::keyboard::KeyboardLayoutKey {
                        physical: key,
                        symbols: vec![winit::keyboard::KeyboardLayoutSymbol {
                            name: text.into(),
                            text: text.into(),
                        }],
                    }],
                }),
            )));
        h.layout();
        let (rect, content) = find(&*h.app, "input.keyboard-map.details").unwrap();
        assert!(
            content.contains(text),
            "native modifier symbols update the actual Text: {content}"
        );
        assert!(!content.contains(": q"), "no preview fallback in live mode");
        assert!(
            rect.height()
                >= f64::from(
                    ui::Theme::dark()
                        .text(ui::tokens::TextRole::Body)
                        .line_height_px()
                )
        );
    }
    h.app
        .observe_physical_input(ui::widget::PhysicalInput::Focused(false));
    h.layout();
    assert!(
        !find(&*h.app, "input.keyboard-map.details")
            .unwrap()
            .1
            .contains("Λ")
    );
}

#[test]
fn keyboard_map_read_matches_actual_focused_native_symbols_and_revokes_on_blur() {
    let mut h = Harness::new();
    h.app.select(Category::MouseKeyboard);
    h.rebuild();
    let key = PhysicalKey::Code(winit::keyboard::KeyCode::KeyQ);
    h.app
        .observe_physical_input(ui::widget::PhysicalInput::Focused(true));
    h.app
        .observe_physical_input(ui::widget::PhysicalInput::Layout(Some(
            std::sync::Arc::new(winit::keyboard::KeyboardLayout {
                depressed: 1,
                latched: 2,
                locked: 4,
                group: 1,
                keys: vec![winit::keyboard::KeyboardLayoutKey {
                    physical: key,
                    symbols: vec![winit::keyboard::KeyboardLayoutSymbol {
                        name: "Greek_LAMBDA".into(),
                        text: "Λ".into(),
                    }],
                }],
            }),
        )));
    let heads = h.doc.get_heads();
    let actual = h.app.keyboard_inspection().unwrap();
    assert_eq!(actual["available"], true);
    assert_eq!(actual["mode"], "live");
    assert!(
        actual["source"].is_null(),
        "preferred/preview source is not live native evidence"
    );
    assert_eq!(actual["native_group"], 1);
    assert_eq!(actual["native_modifiers"], serde_json::json!([1, 2, 4]));
    assert_eq!(actual["selected_symbols"], serde_json::json!(["Λ"]));
    h.app
        .observe_physical_input(ui::widget::PhysicalInput::Key {
            key,
            state: ElementState::Pressed,
        });
    assert_eq!(
        h.app.keyboard_inspection().unwrap()["pressed_physical_keys"],
        serde_json::json!(["AD01"])
    );
    h.app
        .observe_physical_input(ui::widget::PhysicalInput::Key {
            key,
            state: ElementState::Released,
        });
    assert_eq!(
        h.app.keyboard_inspection().unwrap()["pressed_physical_keys"],
        serde_json::json!([])
    );
    h.app
        .observe_physical_input(ui::widget::PhysicalInput::Focused(false));
    let blurred = h.app.keyboard_inspection().unwrap();
    assert_eq!(blurred["available"], false);
    assert_eq!(blurred["native_layout_available"], false);
    assert_eq!(blurred["selected_symbols"], serde_json::json!([]));
    h.app
        .observe_physical_input(ui::widget::PhysicalInput::Focused(true));
    h.app
        .observe_physical_input(ui::widget::PhysicalInput::Layout(Some(
            std::sync::Arc::new(winit::keyboard::KeyboardLayout {
                depressed: 0,
                latched: 0,
                locked: 0,
                group: 0,
                keys: vec![],
            }),
        )));
    let missing = h.app.keyboard_inspection().unwrap();
    assert_eq!(
        missing["available"], false,
        "missing selected native position is not usable evidence"
    );
    assert_eq!(missing["selected_symbols"], serde_json::json!([]));
    assert_eq!(
        h.doc.get_heads(),
        heads,
        "reading and physical feedback never edit the document"
    );
}
