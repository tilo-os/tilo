//! The theme sidebar — a weather-style saved-place list, one selectable
//! [`ThemeRow`] per theme (name + palette swatch), with the applied theme
//! badged "Active". Right-clicking a row opens a [`ui::widget::ContextMenu`]
//! (Duplicate always; Rename + Delete for custom themes); the selected
//! custom theme's name is edited inline with a [`ui::widget::TextInput`].
//!
//! All catalog + selection state is pushed in from [`crate::app::ThemesApp`]
//! via [`Gallery::set_view`]; user intent leaves through the CRDT command
//! bus (`select:` / `duplicate:` / `rename:` / `delete:` commands) and the
//! `ed_name` scratch key (inline rename).

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{ContextMenu, MenuItem, TextInput, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::doc;
use crate::draw;
use crate::model::ThemeSpec;
use crate::widgets::ThemeRow;

const HEADER_H: f64 = 34.0;
const PAD: f32 = 14.0;
const GAP: f32 = 8.0;

/// Panic-free node construction for [`Widget::build_layout`].
/// `TaffyTree::new_with_children` is typed `-> TaffyResult<NodeId>` for API
/// generality, but only fails for a child `NodeId` belonging to a
/// *different* tree — never the case here, since every child passed in was
/// just built in this same `taffy` tree by the calls immediately above.
/// `Widget::build_layout` returns a bare `NodeId` (not a `Result`) across
/// every implementor, so surfacing a theoretical error here would mean
/// threading a `Result` through the whole toolkit for a branch that can't
/// be taken.
#[allow(
    clippy::unwrap_used,
    reason = "new_with_children cannot fail for children built in this same tree; see doc comment"
)]
fn parent_node(taffy: &mut TaffyTree<()>, style: Style, children: &[NodeId]) -> NodeId {
    taffy.new_with_children(style, children).unwrap()
}

pub struct Gallery {
    rows: Vec<ThemeRow>,
    /// Inline editor for the selected custom theme's name (local mode; the
    /// gallery mirrors its edits into the `ed_name` scratch key).
    name_input: TextInput,
    /// Right-click actions menu.
    menu: ContextMenu,
    /// Command written to the bus for each menu action row, by index.
    menu_cmds: Vec<String>,

    selected: String,
    active_id: String,
    /// The selected theme is an editable custom theme (so the inline name
    /// editor is shown over its row).
    selected_custom: bool,
    /// Id whose name currently seeds `name_input`; re-seeded on change only
    /// so typing isn't clobbered mid-edit.
    seeded_id: Option<String>,
    /// A `rename` command asked to focus the inline editor next frame.
    focus_pending: bool,

    rect: Rect,
    node: Option<NodeId>,
    name_input_node: Option<NodeId>,
    count: usize,
    last_cursor: Point,
    structure_dirty: bool,
}

impl Default for Gallery {
    fn default() -> Self {
        Self::new()
    }
}

impl Gallery {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            name_input: TextInput::local().with_plain_chrome().with_font_size(14.0),
            menu: ContextMenu::new(),
            menu_cmds: Vec::new(),
            selected: String::new(),
            active_id: String::new(),
            selected_custom: false,
            seeded_id: None,
            focus_pending: false,
            rect: Rect::ZERO,
            node: None,
            name_input_node: None,
            count: 0,
            last_cursor: Point::ZERO,
            structure_dirty: false,
        }
    }

    /// Whether an inline name editor is shown this frame (selected theme is
    /// custom). Drives whether the `name_input` node is part of the tree.
    fn editor_present(&self) -> bool {
        self.selected_custom
    }

    /// Push the catalog + selection + applied-theme id from the app.
    pub fn set_view(
        &mut self,
        specs: &[ThemeSpec],
        selected: &str,
        active_id: &str,
        selected_custom: bool,
    ) {
        let editor_before = self.editor_present();
        self.count = specs.len();
        self.selected = selected.to_string();
        self.active_id = active_id.to_string();
        self.selected_custom = selected_custom;

        let ids_match = self.rows.len() == specs.len()
            && self.rows.iter().zip(specs).all(|(r, s)| r.id() == s.id);
        if !ids_match {
            self.rows = specs
                .iter()
                .map(|s| ThemeRow::from_spec(s, s.id == selected))
                .collect();
            self.structure_dirty = true;
        }
        for (r, s) in self.rows.iter_mut().zip(specs) {
            let is_sel = s.id == selected;
            let suppress = is_sel && selected_custom;
            r.update(s, is_sel, s.id == active_id, suppress);
        }

        // Seed the inline editor with the selected custom theme's name — only
        // when the selection changes, so live typing isn't overwritten.
        if selected_custom {
            if self.seeded_id.as_deref() != Some(selected) {
                let name = specs
                    .iter()
                    .find(|s| s.id == selected)
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                self.name_input.set_text(&name);
                self.seeded_id = Some(selected.to_string());
            }
        } else {
            self.seeded_id = None;
        }

        if editor_before != self.editor_present() {
            self.structure_dirty = true;
        }
    }

    /// A `rename` command: focus the inline editor on the next frame.
    pub fn request_name_focus(&mut self) {
        self.focus_pending = true;
    }

    /// Open the actions menu for the selected row — used by the `--shots`
    /// renderer to capture the menu without a live right-click.
    pub fn open_menu_for_shot(&mut self) {
        let sel = self.selected.clone();
        let target = self
            .rows
            .iter()
            .find(|r| r.id() == sel)
            .map(|r| (r.get_rect(), r.is_custom()));
        if let Some((rr, custom)) = target {
            self.open_menu(rr.x0 + 90.0, rr.center().y, &sel, custom);
        }
    }

    /// Build the actions menu for `id` at a logical position and open it.
    fn open_menu(&mut self, x: f64, y: f64, id: &str, custom: bool) {
        self.menu = ContextMenu::new();
        self.menu_cmds.clear();
        self.menu.push(MenuItem::action("Duplicate", || {}));
        self.menu_cmds.push(format!("duplicate:{id}"));
        if custom {
            self.menu.push(MenuItem::action("Rename", || {}));
            self.menu_cmds.push(format!("rename:{id}"));
            self.menu.push(MenuItem::action("Delete", || {}));
            self.menu_cmds.push(format!("delete:{id}"));
        }
        self.menu.open_at([x as f32, y as f32]);
    }

    /// The scene-space rect of the selected custom theme's name field.
    fn editor_rect(&self) -> Option<Rect> {
        if !self.selected_custom {
            return None;
        }
        self.rows
            .iter()
            .find(|r| r.id() == self.selected)
            .map(super::widgets::ThemeRow::name_rect)
    }
}

impl Widget for Gallery {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.name_input.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut child_nodes: Vec<NodeId> = self
            .rows
            .iter_mut()
            .map(|r| r.build_layout(taffy, theme, node_map))
            .collect();

        // The inline name editor rides as an out-of-flow child (so it doesn't
        // claim a row slot) whose real rect is assigned in `apply_layout`. It
        // must be a taffy child for the App's focus order to include it.
        if self.editor_present() {
            let n = self.name_input.build_layout(taffy, theme, node_map);
            let _ = taffy.set_style(
                n,
                Style {
                    position: taffy::Position::Absolute,
                    size: Size {
                        width: Dimension::length(1.0),
                        height: Dimension::length(1.0),
                    },
                    ..taffy.style(n).cloned().unwrap_or_default()
                },
            );
            self.name_input_node = Some(n);
            child_nodes.push(n);
        } else {
            self.name_input_node = None;
        }

        let node = parent_node(
            taffy,
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                gap: Size {
                    width: length(0.0_f32),
                    height: length(GAP),
                },
                padding: taffy::Rect {
                    left: length(PAD),
                    right: length(PAD),
                    top: length(HEADER_H as f32),
                    bottom: length(PAD),
                },
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &child_nodes,
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else { return };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        if let Ok(children) = taffy.children(node) {
            for (row, child) in self.rows.iter_mut().zip(children.iter()) {
                row.apply_layout(taffy, *child, new_origin);
            }
        }
        // Position the inline editor over the selected custom row's name.
        if let Some(r) = self.editor_rect() {
            self.name_input.apply_layout_external(r);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        p: &PresenceCtx,
    ) {
        draw::fill_rect(scene, t, self.rect, theme.bg_app);
        draw::line(
            scene,
            t,
            Point::new(self.rect.x1, self.rect.y0),
            Point::new(self.rect.x1, self.rect.y1),
            theme.border_standard,
            1.0,
        );
        // No app-name wordmark: the OS titlebar shows "Themes". The gallery
        // header is just the functional theme count, and the list flows up into
        // the reclaimed space.
        draw::text_w(
            tr,
            scene,
            t,
            &format!("{} themes", self.count),
            12.0,
            theme.text_dim,
            self.rect.x0 + f64::from(PAD),
            self.rect.y0 + 12.0,
            TextWeight::Semibold,
        );
        for r in &mut self.rows {
            r.draw(scene, t, tr, theme, p);
        }
        // Inline name editor over the selected custom row.
        if self.editor_present() {
            self.name_input.draw(scene, t, tr, theme, p);
        }
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        p: &PresenceCtx,
    ) {
        // The actions menu floats above every row.
        self.menu.draw(scene, t, tr, theme, p);
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        m: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Point::new(position.x / scale, position.y / scale);
        }

        // A rename asked to focus the inline editor.
        if self.focus_pending {
            self.focus_pending = false;
            if self.editor_present() {
                Widget::set_focus(&mut self.name_input, true);
            }
        }

        // An open menu owns input first; a chosen row becomes a command.
        if self.menu.is_open() {
            self.menu.handle_event(
                event,
                scale,
                m,
                state,
                path,
                handled,
                focus_target,
                node,
                taffy,
            );
            if let Some(idx) = self.menu.take_selection() {
                if let Some(cmd) = self.menu_cmds.get(idx) {
                    doc::put_str(state, path, doc::KEY_COMMAND, cmd);
                }
                *handled = true;
            }
            if *handled {
                return;
            }
        }

        // Right-click a row → open its actions menu.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Right,
            ..
        } = event
        {
            let hit = self
                .rows
                .iter()
                .find(|r| r.get_rect().contains(self.last_cursor))
                .map(|r| (r.id().to_string(), r.is_custom()));
            if let Some((id, custom)) = hit {
                self.open_menu(self.last_cursor.x, self.last_cursor.y, &id, custom);
                *handled = true;
                return;
            }
        }

        // Inline name editor gets first crack at pointer/keys so clicking the
        // name focuses it (rather than re-selecting the already-selected row).
        if self.editor_present()
            && let Some(n) = self.name_input_node
        {
            self.name_input.handle_event(
                event,
                scale,
                m,
                state,
                path,
                handled,
                focus_target,
                n,
                taffy,
            );
            // Mirror any edit into the scratch key the app reconciles.
            if let Some(name) = self.name_input.take_dirty() {
                doc::put_str(state, path, doc::KEY_ED_NAME, &name);
            }
            if *handled {
                return;
            }
        }

        // Rows (selection + hover) last.
        if let Ok(children) = taffy.children(node) {
            for (row, child) in self.rows.iter_mut().zip(children.iter()) {
                row.handle_event(
                    event,
                    scale,
                    m,
                    state,
                    path,
                    handled,
                    focus_target,
                    *child,
                    taffy,
                );
                if *handled {
                    break;
                }
            }
        }
    }

    fn name(&self) -> &'static str {
        "Gallery"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut v: Vec<&dyn Widget> = self.rows.iter().map(|r| r as &dyn Widget).collect();
        if self.editor_present() {
            v.push(&self.name_input as &dyn Widget);
        }
        v
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut v: Vec<&mut (dyn Widget + '_)> = self
            .rows
            .iter_mut()
            .map(|r| r as &mut (dyn Widget + '_))
            .collect();
        if self.selected_custom {
            v.push(&mut self.name_input as &mut (dyn Widget + '_));
        }
        v
    }

    fn take_structure_dirty(&mut self) -> bool {
        let d = self.structure_dirty;
        self.structure_dirty = false;
        d
    }
}
