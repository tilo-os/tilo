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
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NativeKeyCode, PhysicalKey, SmolStr};

use crate::categories::Category;

fn root() -> automerge::ObjId {
    automerge::ROOT
}

/// A printable-character `KeyboardInput`, built the same way `TextInput`'s own
/// tests do (`KeyEvent` has a `pub(crate)` platform field, so it can't be
/// struct-literalled — zero it then overwrite the public fields).
fn char_key_event(s: &str) -> WindowEvent {
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = Key::Character(SmolStr::new(s));
    ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = Some(SmolStr::new(s));
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event: ev,
        is_synthetic: false,
    }
}

/// A faithful stand-in for `ui::App` covering focus + the rebuild-on-filter
/// cycle. Owns the app, its live taffy tree + node map, and the focus state.
struct Harness {
    app: SettingsApp,
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
            app: config.app(),
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
        let measure_map = node_map.clone();
        let mut tr = TextRenderer::new();
        taffy
            .compute_layout_with_measure(
                outer,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(self.viewport.0),
                    height: taffy::AvailableSpace::Definite(self.viewport.1),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, &mut tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .unwrap();
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
        walk(self.app_node, &self.app, &self.taffy, &mut order);
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
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        };
        let press = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        let release = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
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
            while let Some(intent) = ui::widget::controls::take_control_invocation(&mut self.app) {
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
        let ev = char_key_event(c);
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
        if Widget::take_structure_dirty(&mut self.app) {
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
            if w.name() == "TextInput" {
                *out = Some(w);
                return;
            }
            for c in w.children() {
                walk(c, out);
            }
        }
        let mut out = None;
        walk(&self.app, &mut out);
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
        walk(&self.app, &mut n);
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
        assert_eq!(before["search_focused"], true);
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
        let mut ev = char_key_event("");
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
    let network = named_widget_identity(&h.app, "NetworkPane").unwrap();
    h.click(h.search_field_rect().center());
    h.type_char("z");
    assert_eq!(named_widget_identity(&h.app, "NetworkPane"), Some(network));
    h.app.select(super::Category::About);
    h.rebuild();
    assert_eq!(h.app.selected(), super::Category::About);
    assert_eq!(named_widget_identity(&h.app, "NetworkPane"), None);
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
    let mut ev = char_key_event("");
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
