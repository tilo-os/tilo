//! Search field focus + filter, driven through the production event path.
//!
//! This is the regression guard for the "clicking the search bar does nothing"
//! bug. The root cause was that the field lived inside the `NavSplit`, whose
//! children are laid out in an isolated inner taffy tree the host's focus walk
//! never visits — so the field was never in `focusable_order`, a click's focus
//! could not be restored after the per-keystroke filter rebuild, and typing
//! died after one character. The fix hoists the field into the app's top strip,
//! in the *main* taffy tree.
//!
//! The harness below is a faithful, minimal re-implementation of the pieces of
//! `ui::App` this behaviour depends on — the same `collect_focusable` walk, the
//! same `focus_target` → `set_focus` application, and the same
//! ordinal-preserving `rebuild_structure` that `autofocus_on_structure_change`
//! runs. Every assertion rides the real `SettingsApp` widget tree and the real
//! `TextInput`/`Container`/`Row`/`NavSplit` event path; nothing is mocked.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;

use automerge::AutoCommit;
use settings::SettingsApp;
use taffy::{NodeId, TaffyTree};
use ui::text_render::TextRenderer;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NativeKeyCode, PhysicalKey, SmolStr};

const W: f32 = 1200.0;
const H: f32 = 760.0;

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
    /// Dropped after the app whose account stores it holds.
    _config: crate::support::ScratchConfig,
}

impl Harness {
    fn new() -> Self {
        let config = crate::support::ScratchConfig::new();
        let mut h = Harness {
            app: config.app(),
            doc: AutoCommit::new(),
            taffy: TaffyTree::new(),
            node_map: HashMap::new(),
            app_node: NodeId::new(0),
            focused: None,
            focus_order: Vec::new(),
            _config: config,
        };
        h.layout();
        h
    }

    /// Run the host's per-frame cycle: `sync` (seed) → `build_layout` →
    /// `compute_layout(measure)` → `apply_layout` → `sync` (settle) → refresh
    /// the focusable order. Mirrors `shots::render_to_path` + `App::init_layout`.
    fn layout(&mut self) {
        let theme = ui::Theme::dark();
        self.app.sync(&mut self.doc, &root());
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let app_node = self.app.build_layout(&mut taffy, &theme, &mut node_map);
        let outer = taffy
            .new_with_children(
                taffy::Style {
                    size: taffy::Size {
                        width: taffy::Dimension::length(W),
                        height: taffy::Dimension::length(H),
                    },
                    ..Default::default()
                },
                &[app_node],
            )
            .unwrap();
        self.app.sync(&mut self.doc, &root());
        let measure_map = node_map.clone();
        let mut tr = TextRenderer::new();
        taffy
            .compute_layout_with_measure(
                outer,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(W),
                    height: taffy::AvailableSpace::Definite(H),
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
        self.app.sync(&mut self.doc, &root());

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
                    state: &mut self.doc,
                    path: &root(),
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: self.app_node,
                    taffy: &self.taffy,
                },
            );
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
                state: &mut self.doc,
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
        self.app.sync(&mut self.doc, &root());
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
            if w.name() == "SidebarRow" {
                *n = n.saturating_add(1);
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

/// The search field is top-level focusable, focuses on click and keeps focus while typing.
#[test]
fn search_field_is_focusable_and_keeps_focus_while_filtering() {
    // Case `search_field_is_a_top_level_focusable`.
    {
        // The structural fix: the field lives in the main taffy tree, so the
        // host's focus walk actually reaches it. (Buried in `NavSplit` it never
        // would.)
        let h = Harness::new();
        assert!(
            h.search_field().is_some(),
            "the search field must exist in the composed tree"
        );
        let has_text_input = h.focus_order.iter().any(|&id| {
            h.node_map
                .get(&id)
                .is_some_and(|&w| unsafe { (*w).name() == "TextInput" })
        });
        assert!(
            has_text_input,
            "the search field must appear in the focusable order (proving it is not \
         trapped inside NavSplit's isolated inner tree)"
        );
    }

    // Case `clicking_search_field_focuses_it`.
    {
        let mut h = Harness::new();
        assert!(
            !h.search_field().unwrap().is_focused(),
            "the field starts unfocused (no autofocus on launch)"
        );
        let rect = h.search_field_rect();
        assert!(
            rect.width() > 0.0 && rect.height() > 0.0,
            "field has a real rect"
        );

        h.click(rect.center());

        assert!(
            h.search_field().unwrap().is_focused(),
            "clicking the search field must focus it"
        );
    }

    // Case `typing_into_search_filters_and_keeps_focus`.
    {
        let mut h = Harness::new();
        let before = h.sidebar_row_count();
        assert!(before > 3, "the unfiltered sidebar lists every category");

        // Click to focus, then type a query that filters to a couple of panes.
        let rect = h.search_field_rect();
        h.click(rect.center());
        assert!(
            h.search_field().unwrap().is_focused(),
            "click focused the field"
        );

        for c in ["n", "e", "t"] {
            h.type_char(c);
        }

        // The query reached the app and drove the filter (real sidebar rebuild).
        assert_eq!(
            h.app.search_query(),
            "net",
            "each typed char reached the query"
        );
        let after = h.sidebar_row_count();
        assert!(
            after < before && after >= 1,
            "typing filtered the sidebar (before={before}, after={after})"
        );

        // The field survived every per-keystroke rebuild: it is still focused and
        // still holds the full typed text (the caret never got dropped, so all
        // three characters landed — the exact thing the old bug broke).
        let field = h
            .search_field()
            .expect("field still present after filtering");
        assert!(field.is_focused(), "focus survived the filter rebuilds");
        assert_eq!(
            field.text_review().map(|(_, t)| t),
            Some("net".to_string()),
            "the field holds the full query it accumulated across rebuilds"
        );
    }
}
