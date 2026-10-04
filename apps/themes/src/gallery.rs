//! The theme sidebar — a weather-style saved-place list, one selectable
//! [`ThemeRow`] per theme (name + palette swatch), with the applied theme
//! badged "Active". Right-clicking a row opens a [`ui::widget::ContextMenu`]
//! (Duplicate always; Rename + Delete for custom themes); the selected
//! custom theme's name is edited inline with a [`ui::widget::TextInput`].
//!
//! All catalog + selection state is pushed in from [`crate::app::ThemesApp`]
//! via [`Gallery::set_view`]; user intent stays local until the app turns a
//! committed operation into its canonical durable action.

use std::collections::HashMap;
use ui::widget::{AppModel, ModelPath};

use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::tokens::Spacing;
use ui::widget::{ContextMenu, MenuItem, TextInput, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::draw;
use crate::model::ThemeSpec;
use crate::widgets::ThemeRow;

/// Height (logical px) of the header band above the first row at 1×: the
/// theme count sits one `s3` below the top with the rows starting under it.
/// Under large text the band grows with the count's line box (see
/// [`header_band`]); at 1× it is exactly this.
const HEADER_H: f32 = 34.0;
/// Inset of the list from the sidebar's edges — the `s4` step, the same
/// gutter the collapsed content and the top bar's toggle use, so the rows
/// and the toggle above them share a left edge.
const PAD: f32 = Spacing::BASE.s4;
/// Gap between rows — the `s2` step.
const GAP: f32 = Spacing::BASE.s2;

/// The header band for `theme`: the count's line box (its `sm` type is
/// already text-scaled; a nominal 1.2 line height) one `s3` below the top
/// and one `s1` above the first row, never below [`HEADER_H`].
fn header_band(theme: &Theme) -> f32 {
    let sp = &theme.tokens.spacing;
    let line = theme.tokens.typography.scale.sm * 1.2;
    (sp.s3 + line + sp.s1).max(HEADER_H)
}

const NAME_CONFLICT_HINT: &str = "Name changed. Enter: apply draft. Escape: use current name.";

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum RenameState {
    Current,
    Conflict,
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
    canonical_name: String,
    rename_state: RenameState,
    /// A `rename` command asked to focus the inline editor next frame.
    focus_pending: bool,
    commands: Vec<String>,
    edited_name: Option<String>,

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
            name_input: TextInput::local()
                .with_kind(ui::widget::TextInputKind::Name)
                .with_plain_chrome()
                .with_font_size(14.0),
            menu: ContextMenu::new(),
            menu_cmds: Vec::new(),
            selected: String::new(),
            active_id: String::new(),
            selected_custom: false,
            seeded_id: None,
            canonical_name: String::new(),
            rename_state: RenameState::Current,
            focus_pending: false,
            commands: Vec::new(),
            edited_name: None,
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

        // Refresh clean cached names, but keep an unfinished local rename
        // when History or another writer changes the same canonical name.
        if selected_custom {
            let name = specs
                .iter()
                .find(|s| s.id == selected)
                .map(|s| s.name.clone())
                .unwrap_or_default();
            if self.seeded_id.as_deref() != Some(selected) {
                self.name_input.set_text(&name);
                self.edited_name = None;
                self.rename_state = RenameState::Current;
                self.seeded_id = Some(selected.to_string());
            } else if self.canonical_name != name {
                let dirty =
                    self.edited_name.is_some() || self.name_input.value() != self.canonical_name;
                if dirty && self.name_input.value() != name {
                    self.rename_state = RenameState::Conflict;
                    self.edited_name = None;
                } else {
                    self.name_input.set_text(&name);
                    self.rename_state = RenameState::Current;
                }
            }
            self.canonical_name = name;
        } else {
            self.seeded_id = None;
            self.rename_state = RenameState::Current;
            self.edited_name = None;
        }

        if editor_before != self.editor_present() {
            self.structure_dirty = true;
        }
    }

    /// A `rename` command: focus the inline editor on the next frame.
    pub fn request_name_focus(&mut self) {
        self.focus_pending = true;
    }

    pub fn take_command(&mut self) -> Option<String> {
        (!self.commands.is_empty()).then(|| self.commands.remove(0))
    }

    pub fn take_edited_name(&mut self) -> Option<String> {
        if self.rename_state == RenameState::Conflict {
            None
        } else {
            self.edited_name.take()
        }
    }

    /// Stage a local inline-name edit. Used by the app's headless interaction
    /// harness as well as the real `TextInput` event path.
    pub fn stage_edited_name(&mut self, name: impl Into<String>) {
        let name = name.into();
        self.name_input.set_text(&name);
        self.edited_name = Some(name);
    }

    /// A conflicting local rename needs a deliberate commit or cancel.
    pub(crate) fn resolve_name_conflict(&mut self, key: &Key) -> bool {
        if self.rename_state != RenameState::Conflict {
            return false;
        }
        match key {
            Key::Named(NamedKey::Enter) => {
                self.edited_name = Some(self.name_input.value().to_string());
                self.rename_state = RenameState::Current;
                true
            }
            Key::Named(NamedKey::Escape) => {
                self.name_input.set_text(&self.canonical_name);
                self.edited_name = None;
                self.rename_state = RenameState::Current;
                true
            }
            _ => false,
        }
    }

    #[cfg(test)]
    pub(crate) fn displayed_name(&self) -> &str {
        self.name_input.value()
    }

    /// Whether the actions menu is open.
    #[cfg(test)]
    pub(crate) fn menu_is_open(&self) -> bool {
        self.menu.is_open()
    }

    /// Queue `command` as if a row or the actions menu had reported it.
    #[cfg(test)]
    pub(crate) fn queue_command(&mut self, command: &str) {
        self.commands.push(command.to_owned());
    }

    /// Open the actions menu for the selected row — staged by the menu
    /// screenshot scenario, which has no live right-click.
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
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
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
                    top: length(header_band(theme)),
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
        // The gallery's trailing divider is a hairline derived from its own
        // fill (HIG §9.5); half a pixel inside the edge so the 1 px rule lands
        // on one pixel column instead of straddling two.
        draw::line(
            scene,
            t,
            Point::new(self.rect.x1 - 0.5, self.rect.y0),
            Point::new(self.rect.x1 - 0.5, self.rect.y1),
            ui::surface::edge_on(theme.bg_app),
            1.0,
        );
        // No app-name wordmark: the OS titlebar shows "Themes". The gallery
        // header is just the functional theme count, and the list flows up into
        // the reclaimed space.
        if self.rename_state == RenameState::Conflict {
            tr.draw_text(
                scene,
                NAME_CONFLICT_HINT,
                11.,
                theme.text_dim,
                t * Affine::translate((self.rect.x0 + f64::from(PAD), self.rect.y0 + 3.)),
                Some((self.rect.width() - 2. * f64::from(PAD)) as f32),
            );
        } else {
            // The count is the theme's small type, handed to the renderer once
            // (the theme size already carries the text scale).
            let count_size = tr
                .renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm))
                .size;
            PaintCtx::new(scene, t, tr, theme).text(
                &format!("{} themes", self.count),
                &TextStyle::new(count_size).with_weight(TextWeight::Semibold),
                theme.text_dim,
                Point::new(
                    self.rect.x0 + f64::from(PAD),
                    self.rect.y0 + f64::from(theme.tokens.spacing.s3),
                ),
                None,
            );
        }
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
            self.menu.handle_event(event, ctx);
            if let Some(idx) = self.menu.take_selection() {
                if let Some(cmd) = self.menu_cmds.get(idx) {
                    self.commands.push(cmd.clone());
                }
                *ctx.handled = true;
            }
            if *ctx.handled {
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
                *ctx.handled = true;
                return;
            }
        }

        // Inline name editor gets first crack at pointer/keys so clicking the
        // name focuses it (rather than re-selecting the already-selected row).
        if self.editor_present()
            && let Some(n) = self.name_input_node
        {
            if self.name_input.is_focused()
                && let WindowEvent::KeyboardInput { event: key, .. } = event
                && key.state == ElementState::Pressed
                && self.resolve_name_conflict(&key.logical_key)
            {
                *ctx.handled = true;
                return;
            }
            self.name_input
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            // Keep name input local until the app commits its canonical action.
            if let Some(name) = self.name_input.take_dirty() {
                self.edited_name = Some(name);
            }
            if *ctx.handled {
                return;
            }
        }

        // Rows (selection + hover) last.
        if let Ok(children) = ctx.taffy.children(ctx.node_id) {
            for (row, child) in self.rows.iter_mut().zip(children.iter()) {
                row.handle_event(event, &mut ctx.reborrow(ctx.path, *child));
                if *ctx.handled {
                    if row.take_select_request() {
                        self.commands.push(format!("select:{}", row.id()));
                    }
                    break;
                }
            }
        }
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        (self.rename_state == RenameState::Conflict)
            .then(|| ui::a11y::AccessNode::new(ui::a11y::Role::Group).with_name(NAME_CONFLICT_HINT))
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
