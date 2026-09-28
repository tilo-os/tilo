//! Contact export-selection dialog.
//!
//! Owns modal state, responsive geometry, drawing and pointer/wheel routing.
//! The root app only handles the resulting request to open the native Save
//! portal, keeping host I/O out of this reusable interaction surface.

use std::collections::HashSet;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::a11y::{AccessNode, AccessState, Action, Role};
use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::app::{chip, draw_checkbox, menu_panel, toolbar_button};
use crate::model::{ContactId, ContactStore, SortKey};
use crate::vcard::VCardVersion;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ExportRequest {
    pub(crate) ids: Vec<ContactId>,
    pub(crate) version: VCardVersion,
}

pub(crate) struct ExportDialog {
    selection: HashSet<ContactId>,
    scroll: f64,
    open: bool,
    focused: ExportFocus,
    /// Keep hit targets and semantic bounds in sync with enlarged text.
    text_scale: f64,
}

/// Hand-drawn modal focus is kept by durable contact ID, never an incidental
/// row offset, so sorting or a collaborator's edit cannot retarget activation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExportFocus {
    SelectAll,
    SelectNone,
    Row(ContactId),
    Cancel,
    ExportV3,
    ExportV4,
}

const EXPORT_OVERSCAN_ROWS: usize = 4;
const EXPORT_ROW_H: f64 = 40.0;
const EXPORT_HEADER_H: f64 = 64.0;
const EXPORT_FOOTER_H: f64 = 64.0;
const EXPORT_MIN_H: f64 = 320.0;
const EXPORT_MAX_H: f64 = 560.0;

#[derive(Debug)]
pub(crate) struct Geometry {
    pub(crate) panel: Rect,
    pub(crate) select_all: Rect,
    pub(crate) select_none: Rect,
    pub(crate) list_body: Rect,
    pub(crate) rows: Vec<(ContactId, Rect, Rect)>,
    pub(crate) row_start: usize,
    pub(crate) row_count: usize,
    visible_first: usize,
    visible_last: usize,
    pub(crate) export_v3: Rect,
    pub(crate) export_v4: Rect,
    pub(crate) cancel: Rect,
    content_h: f64,
    row_h: f64,
    text_scale: f64,
    compact: bool,
}

impl ExportDialog {
    pub(crate) fn new() -> Self {
        Self {
            selection: HashSet::new(),
            scroll: 0.0,
            open: false,
            focused: ExportFocus::ExportV4,
            text_scale: 1.0,
        }
    }

    pub(crate) fn set_text_scale(&mut self, scale: f32) {
        self.text_scale = f64::from(scale).clamp(1.0, 2.0);
    }

    pub(crate) fn open(&mut self, selected: impl IntoIterator<Item = ContactId>) {
        self.selection = selected.into_iter().collect();
        self.scroll = 0.0;
        self.open = true;
        self.focused = ExportFocus::ExportV4;
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    #[cfg(test)]
    pub(crate) fn selection(&self) -> &HashSet<ContactId> {
        &self.selection
    }

    #[allow(
        clippy::too_many_lines,
        reason = "responsive export geometry keeps its virtualized rows and footer hit targets synchronized"
    )]
    pub(crate) fn geometry(&self, window: Rect, store: &ContactStore, sort: SortKey) -> Geometry {
        let ids = store.filtered_sorted("", None, sort);
        // At 200% the original single-line header/footer cannot contain their
        // labels. Use a stacked layout rather than painting enlarged text into
        // controls sized for 100%.
        let compact = self.text_scale >= 1.75 || window.width() < 440.0;
        let header_h = if compact { 176.0 } else { EXPORT_HEADER_H };
        let footer_h = if compact { 120.0 } else { EXPORT_FOOTER_H };
        let row_h = if compact { 56.0 } else { EXPORT_ROW_H };
        let width = 460.0_f64.min((window.width() - 32.0).max(280.0));
        let max_height = (window.height() * 0.8)
            .clamp(EXPORT_MIN_H, EXPORT_MAX_H)
            .min(window.height() - 40.0);
        // Small address books should not get a large inert well below the
        // last checkbox.  The list grows with its content through the normal
        // minimum dialog height, then caps and scrolls for large directories.
        let content_height = header_h + ids.len() as f64 * row_h + footer_h;
        let min_height = if compact {
            header_h + row_h * 2.0 + footer_h
        } else {
            EXPORT_MIN_H
        };
        let height = content_height.clamp(min_height.min(max_height), max_height);
        let panel = Rect::from_center_size(window.center(), (width, height));
        let (select_all, select_none) = if compact {
            let gap = 8.0;
            let button_w = (panel.width() - 32.0 - gap) / 2.0;
            let y = panel.y0 + 124.0;
            (
                Rect::new(panel.x0 + 16.0, y, panel.x0 + 16.0 + button_w, y + 36.0),
                Rect::new(
                    panel.x0 + 16.0 + button_w + gap,
                    y,
                    panel.x1 - 16.0,
                    y + 36.0,
                ),
            )
        } else {
            let y = panel.y0 + 18.0;
            let select_none = Rect::new(panel.x1 - 112.0, y, panel.x1 - 16.0, y + 28.0);
            let select_all = Rect::new(select_none.x0 - 104.0, y, select_none.x0 - 8.0, y + 28.0);
            (select_all, select_none)
        };
        let list_body = Rect::new(
            panel.x0 + 16.0,
            panel.y0 + header_h,
            panel.x1 - 16.0,
            panel.y1 - footer_h,
        );
        let first_visible = (self.scroll / row_h).floor().max(0.0) as usize;
        let visible = (list_body.height() / row_h).ceil().max(1.0) as usize;
        let row_start = first_visible
            .saturating_sub(EXPORT_OVERSCAN_ROWS)
            .min(ids.len());
        let row_end = first_visible
            .saturating_add(visible)
            .saturating_add(EXPORT_OVERSCAN_ROWS)
            .min(ids.len());
        let visible_last = first_visible.saturating_add(visible).min(ids.len());
        let rows = ids
            .iter()
            .enumerate()
            .skip(row_start)
            .take(row_end.saturating_sub(row_start))
            .map(|(index, id)| {
                let y = list_body.y0 - self.scroll + row_h * index as f64;
                let row = Rect::new(list_body.x0, y, list_body.x1, y + row_h);
                let checkbox = Rect::new(
                    row.x0 + 4.0,
                    row.center().y - if compact { 12.0 } else { 10.0 },
                    row.x0 + if compact { 28.0 } else { 24.0 },
                    row.center().y + if compact { 12.0 } else { 10.0 },
                );
                (*id, row, checkbox)
            })
            .collect();
        let (cancel, export_v3, export_v4) = if compact {
            let gap = 8.0;
            let action_w = (panel.width() - 32.0 - gap) / 2.0;
            let action_y = panel.y1 - 48.0;
            (
                Rect::new(
                    panel.x0 + 16.0,
                    panel.y1 - 104.0,
                    panel.x1 - 16.0,
                    panel.y1 - 64.0,
                ),
                Rect::new(
                    panel.x0 + 16.0,
                    action_y,
                    panel.x0 + 16.0 + action_w,
                    panel.y1 - 8.0,
                ),
                Rect::new(
                    panel.x0 + 16.0 + action_w + gap,
                    action_y,
                    panel.x1 - 16.0,
                    panel.y1 - 8.0,
                ),
            )
        } else {
            let by = panel.y1 - 44.0;
            let export_v4 = Rect::new(panel.x1 - 136.0, by, panel.x1 - 16.0, by + 32.0);
            let export_v3 = Rect::new(export_v4.x0 - 128.0, by, export_v4.x0 - 8.0, by + 32.0);
            let cancel = Rect::new(panel.x0 + 16.0, by, panel.x0 + 104.0, by + 32.0);
            (cancel, export_v3, export_v4)
        };
        Geometry {
            panel,
            select_all,
            select_none,
            list_body,
            rows,
            row_start,
            row_count: ids.len(),
            visible_first: first_visible,
            visible_last,
            export_v3,
            export_v4,
            cancel,
            content_h: ids.len() as f64 * row_h,
            row_h,
            text_scale: self.text_scale,
            compact,
        }
    }

    pub(crate) fn draw(
        &self,
        ctx: &mut PaintCtx<'_>,
        window: Rect,
        store: &ContactStore,
        sort: SortKey,
        pointer: Point,
    ) {
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            colors.surfaces.overlay,
            None,
            &window,
        );
        let geometry = self.geometry(window, store, sort);
        menu_panel(ctx.scene, ctx.transform, theme, geometry.panel);
        self.draw_header(ctx.scene, ctx.transform, ctx.text, theme, &geometry);
        self.draw_rows(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            store,
            pointer,
            &geometry,
        );
        self.draw_footer_buttons(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &geometry,
            pointer,
        );
    }

    /// The title, selection count, and Select all/none chips (part of
    /// [`Self::draw`]).
    fn draw_header(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        geometry: &Geometry,
    ) {
        let colors = &theme.tokens.colors;
        text.draw_text_weighted(
            scene,
            "Export contacts",
            16.0,
            colors.text_primary,
            transform * Affine::translate((geometry.panel.x0 + 18.0, geometry.panel.y0 + 18.0)),
            None,
            TextWeight::Bold,
        );
        text.draw_text(
            scene,
            &format!("{} selected", self.selection.len()),
            12.0,
            colors.text_dim,
            transform
                * Affine::translate((
                    geometry.panel.x0 + 18.0,
                    geometry.panel.y0 + if geometry.compact { 62.0 } else { 40.0 },
                )),
            None,
        );
        if geometry.compact {
            text.draw_text(
                scene,
                "Choose vCard format",
                12.0,
                colors.text_dim,
                transform * Affine::translate((geometry.panel.x0 + 18.0, geometry.panel.y0 + 88.0)),
                None,
            );
            text.draw_text(
                scene,
                &format!(
                    "{}–{} of {}",
                    geometry.visible_first.saturating_add(1),
                    geometry.visible_last,
                    geometry.row_count
                ),
                12.0,
                colors.text_dim,
                transform
                    * Affine::translate((geometry.panel.x1 - 106.0, geometry.panel.y0 + 62.0)),
                None,
            );
        }
        chip(
            scene,
            transform,
            text,
            theme,
            geometry.select_all,
            "Select all",
            false,
        );
        chip(
            scene,
            transform,
            text,
            theme,
            geometry.select_none,
            "Select none",
            false,
        );
        let focused = match self.focused {
            ExportFocus::SelectAll => geometry.select_all,
            ExportFocus::SelectNone => geometry.select_none,
            _ => return,
        };
        scene.stroke(
            &vello::kurbo::Stroke::new(2.0),
            transform,
            colors.border_focus,
            None,
            &RoundedRect::from_rect(focused.inflate(2.0, 2.0), f64::from(theme.tokens.radii.md)),
        );
    }

    /// The scrolling, clipped contact list (part of [`Self::draw`]).
    fn draw_rows(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        store: &ContactStore,
        pointer: Point,
        geometry: &Geometry,
    ) {
        let colors = &theme.tokens.colors;
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &geometry.list_body,
        );
        for (id, row, checkbox) in &geometry.rows {
            if row.y1 < geometry.list_body.y0 || row.y0 > geometry.list_body.y1 {
                continue;
            }
            let Some(contact) = store.get(*id) else {
                continue;
            };
            if row.contains(pointer) {
                // Hover is the quiet lift (HIG §9.3), derived against the
                // Panel ground the dialog's rows sit on.
                let hover = crate::app::row_tier_on(
                    ui::chrome::panel_ground(theme),
                    ui::chrome::SelectionState::new(false, true, false),
                );
                scene.fill(
                    Fill::NonZero,
                    transform,
                    hover.fill,
                    None,
                    &RoundedRect::from_rect(*row, f64::from(theme.tokens.radii.md)),
                );
            }
            draw_checkbox(
                scene,
                transform,
                theme,
                *checkbox,
                self.selection.contains(id),
            );
            if self.focused == ExportFocus::Row(*id) {
                scene.stroke(
                    &vello::kurbo::Stroke::new(2.0),
                    transform,
                    colors.border_focus,
                    None,
                    &RoundedRect::from_rect(
                        row.inflate(-1.0, -1.0),
                        f64::from(theme.tokens.radii.md),
                    ),
                );
            }
            text.draw_text(
                scene,
                &contact.display_name(),
                14.0,
                colors.text_primary,
                transform
                    * Affine::translate((
                        checkbox.x1 + 12.0,
                        row.center().y - 9.0 * geometry.text_scale,
                    )),
                Some((row.x1 - checkbox.x1 - 20.0) as f32),
            );
        }
        scene.pop_layer();
    }

    /// The Cancel / Export 3.0 / Export 4.0 buttons (part of [`Self::draw`]).
    fn draw_footer_buttons(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        geometry: &Geometry,
        pointer: Point,
    ) {
        toolbar_button(
            &mut PaintCtx::new(scene, transform, text, theme),
            geometry.cancel,
            "Cancel",
            false,
            geometry.cancel.contains(pointer),
            false,
        );
        toolbar_button(
            &mut PaintCtx::new(scene, transform, text, theme),
            geometry.export_v3,
            "vCard 3.0",
            false,
            geometry.export_v3.contains(pointer),
            false,
        );
        toolbar_button(
            &mut PaintCtx::new(scene, transform, text, theme),
            geometry.export_v4,
            "vCard 4.0",
            true,
            geometry.export_v4.contains(pointer),
            false,
        );
        let focused = match self.focused {
            ExportFocus::Cancel => geometry.cancel,
            ExportFocus::ExportV3 => geometry.export_v3,
            ExportFocus::ExportV4 => geometry.export_v4,
            _ => return,
        };
        scene.stroke(
            &vello::kurbo::Stroke::new(2.0),
            transform,
            theme.tokens.colors.border_focus,
            None,
            &RoundedRect::from_rect(focused.inflate(2.0, 2.0), f64::from(theme.tokens.radii.md)),
        );
    }

    pub(crate) fn handle_event(
        &mut self,
        event: &WindowEvent,
        pointer: Point,
        window: Rect,
        store: &ContactStore,
        sort: SortKey,
    ) -> (bool, Option<ExportRequest>) {
        if let WindowEvent::MouseWheel { delta, .. } = event {
            let delta = match delta {
                MouseScrollDelta::LineDelta(_, y) => f64::from(-y * 48.0),
                MouseScrollDelta::PixelDelta(point) => -point.y,
            };
            let geometry = self.geometry(window, store, sort);
            self.scroll = (self.scroll + delta).clamp(
                0.0,
                (geometry.content_h - geometry.list_body.height()).max(0.0),
            );
            return (true, None);
        }
        if !matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) {
            return (false, None);
        }
        let geometry = self.geometry(window, store, sort);
        if geometry.select_all.contains(pointer) {
            self.focused = ExportFocus::SelectAll;
            self.selection = store.filtered_sorted("", None, sort).into_iter().collect();
        } else if geometry.select_none.contains(pointer) {
            self.focused = ExportFocus::SelectNone;
            self.selection.clear();
        } else if geometry.list_body.contains(pointer) {
            if let Some((id, _, _)) = geometry
                .rows
                .iter()
                .find(|(_, row, _)| row.contains(pointer))
            {
                self.focused = ExportFocus::Row(*id);
                if !self.selection.remove(id) {
                    self.selection.insert(*id);
                }
            }
        } else if geometry.cancel.contains(pointer) || !geometry.panel.contains(pointer) {
            self.focused = ExportFocus::Cancel;
            self.close();
        } else if geometry.export_v3.contains(pointer) || geometry.export_v4.contains(pointer) {
            self.focused = if geometry.export_v4.contains(pointer) {
                ExportFocus::ExportV4
            } else {
                ExportFocus::ExportV3
            };
            let version = if geometry.export_v4.contains(pointer) {
                VCardVersion::V4
            } else {
                VCardVersion::V3
            };
            let ids = store
                .filtered_sorted("", None, sort)
                .into_iter()
                .filter(|id| self.selection.contains(id))
                .collect();
            self.close();
            return (true, Some(ExportRequest { ids, version }));
        }
        (true, None)
    }

    /// Keyboard route for the hand-drawn dialog. Tab stays inside the modal;
    /// rows also support arrows, Space and Enter like native checkboxes.
    pub(crate) fn handle_key(
        &mut self,
        key: &KeyEvent,
        backward: bool,
        window: Rect,
        store: &ContactStore,
        sort: SortKey,
    ) -> (bool, Option<ExportRequest>) {
        if key.state != ElementState::Pressed {
            return (false, None);
        }
        match &key.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.close();
                (true, None)
            }
            Key::Named(NamedKey::Tab) => {
                self.step_focus(store, sort, if backward { -1 } else { 1 });
                self.scroll_focus_into_view(window, store, sort);
                (true, None)
            }
            Key::Named(NamedKey::ArrowDown) => {
                if matches!(self.focused, ExportFocus::Row(_)) {
                    self.step_row_focus(store, sort, 1);
                    self.scroll_focus_into_view(window, store, sort);
                    (true, None)
                } else {
                    (false, None)
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                if matches!(self.focused, ExportFocus::Row(_)) {
                    self.step_row_focus(store, sort, -1);
                    self.scroll_focus_into_view(window, store, sort);
                    (true, None)
                } else {
                    (false, None)
                }
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                let request = self.activate_focus(store, sort);
                (true, request)
            }
            _ => (false, None),
        }
    }

    /// Build a bounded semantic subtree. The address book can be very large,
    /// so assistive technology receives only visible rows plus correct total
    /// positions—the same performance contract as the painted list.
    pub(crate) fn accessibility(
        &self,
        window: Rect,
        store: &ContactStore,
        sort: SortKey,
    ) -> AccessNode {
        let geometry = self.geometry(window, store, sort);
        let rows = geometry
            .rows
            .iter()
            .enumerate()
            .filter_map(|(offset, (id, row, _))| {
                let contact = store.get(*id)?;
                Some(
                    AccessNode::new(Role::Checkbox)
                        .with_name(contact.display_name())
                        .with_action_target(format!("contacts.export.row:{id}"))
                        .with_bounds(*row)
                        .with_position(geometry.row_start + offset + 1, geometry.row_count)
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.focused == ExportFocus::Row(*id))
                                .checked(self.selection.contains(id)),
                        ),
                )
            })
            .collect();
        AccessNode::new(Role::Dialog)
            .with_name("Export contacts")
            .with_description("Choose contacts and a vCard version to export.")
            .with_bounds(geometry.panel)
            .with_state(AccessState::new().modal(true))
            .with_children(vec![
                AccessNode::new(Role::Button)
                    .with_name("Select all contacts")
                    .with_action_target("contacts.export.select-all")
                    .with_bounds(geometry.select_all)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.focused == ExportFocus::SelectAll),
                    ),
                AccessNode::new(Role::Button)
                    .with_name("Select no contacts")
                    .with_action_target("contacts.export.select-none")
                    .with_bounds(geometry.select_none)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.focused == ExportFocus::SelectNone),
                    ),
                AccessNode::new(Role::List)
                    .with_name("Contacts to export")
                    .with_children(rows),
                AccessNode::new(Role::Button)
                    .with_name("Cancel export")
                    .with_action_target("contacts.export.cancel")
                    .with_bounds(geometry.cancel)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.focused == ExportFocus::Cancel),
                    ),
                AccessNode::new(Role::Button)
                    .with_name("Export vCard 3.0")
                    .with_action_target("contacts.export.v3")
                    .with_bounds(geometry.export_v3)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.focused == ExportFocus::ExportV3),
                    ),
                AccessNode::new(Role::Button)
                    .with_name("Export vCard 4.0")
                    .with_action_target("contacts.export.v4")
                    .with_bounds(geometry.export_v4)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.focused == ExportFocus::ExportV4),
                    ),
            ])
    }

    pub(crate) fn perform_accessibility_action(
        &mut self,
        target: &str,
        action: &Action,
        window: Rect,
        store: &ContactStore,
        sort: SortKey,
    ) -> (bool, Option<ExportRequest>) {
        let focus = match target {
            "contacts.export.select-all" => Some(ExportFocus::SelectAll),
            "contacts.export.select-none" => Some(ExportFocus::SelectNone),
            "contacts.export.cancel" => Some(ExportFocus::Cancel),
            "contacts.export.v3" => Some(ExportFocus::ExportV3),
            "contacts.export.v4" => Some(ExportFocus::ExportV4),
            _ => target
                .strip_prefix("contacts.export.row:")
                .and_then(|raw| raw.parse::<ContactId>().ok())
                .filter(|id| store.get(*id).is_some())
                .map(ExportFocus::Row),
        };
        let Some(focus) = focus else {
            return (false, None);
        };
        self.focused = focus;
        self.scroll_focus_into_view(window, store, sort);
        if matches!(action, Action::Activate | Action::ToggleCheck) {
            return (true, self.activate_focus(store, sort));
        }
        (matches!(action, Action::Focus), None)
    }

    fn activate_focus(&mut self, store: &ContactStore, sort: SortKey) -> Option<ExportRequest> {
        match self.focused {
            ExportFocus::SelectAll => {
                self.selection = store.filtered_sorted("", None, sort).into_iter().collect();
            }
            ExportFocus::SelectNone => self.selection.clear(),
            ExportFocus::Row(id) => {
                if !self.selection.remove(&id) {
                    self.selection.insert(id);
                }
            }
            ExportFocus::Cancel => self.close(),
            ExportFocus::ExportV3 | ExportFocus::ExportV4 => {
                let version = if self.focused == ExportFocus::ExportV4 {
                    VCardVersion::V4
                } else {
                    VCardVersion::V3
                };
                let ids = store
                    .filtered_sorted("", None, sort)
                    .into_iter()
                    .filter(|id| self.selection.contains(id))
                    .collect();
                self.close();
                return Some(ExportRequest { ids, version });
            }
        }
        None
    }

    fn step_focus(&mut self, store: &ContactStore, sort: SortKey, delta: isize) {
        let ids = store.filtered_sorted("", None, sort);
        let mut stops = vec![ExportFocus::SelectAll, ExportFocus::SelectNone];
        stops.extend(ids.into_iter().map(ExportFocus::Row));
        stops.extend([
            ExportFocus::Cancel,
            ExportFocus::ExportV3,
            ExportFocus::ExportV4,
        ]);
        let current = stops
            .iter()
            .position(|focus| *focus == self.focused)
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(stops.len() as isize) as usize;
        if let Some(focus) = stops.get(next).copied() {
            self.focused = focus;
        }
    }

    fn step_row_focus(&mut self, store: &ContactStore, sort: SortKey, delta: isize) {
        let ids = store.filtered_sorted("", None, sort);
        let Some(ExportFocus::Row(id)) = Some(self.focused) else {
            return;
        };
        let Some(index) = ids.iter().position(|candidate| *candidate == id) else {
            return;
        };
        let next = (index as isize + delta).clamp(0, ids.len().saturating_sub(1) as isize) as usize;
        if let Some(id) = ids.get(next) {
            self.focused = ExportFocus::Row(*id);
        }
    }

    fn scroll_focus_into_view(&mut self, window: Rect, store: &ContactStore, sort: SortKey) {
        let ExportFocus::Row(id) = self.focused else {
            return;
        };
        let ids = store.filtered_sorted("", None, sort);
        let Some(index) = ids.iter().position(|candidate| *candidate == id) else {
            return;
        };
        let geometry = self.geometry(window, store, sort);
        let top = index as f64 * geometry.row_h;
        let bottom = top + geometry.row_h;
        if top < self.scroll {
            self.scroll = top;
        }
        if bottom > self.scroll + geometry.list_body.height() {
            self.scroll = bottom - geometry.list_body.height();
        }
        self.scroll = self.scroll.clamp(
            0.0,
            (geometry.content_h - geometry.list_body.height()).max(0.0),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Contact, sample_contacts};

    fn store() -> ContactStore {
        let mut store = ContactStore::new();
        for contact in sample_contacts() {
            store.upsert(contact);
        }
        store
    }

    /// The export dialog stays inside narrow and 200% windows, fits short books and caps large ones to a viewport.
    #[test]
    fn export_dialog_geometry_fits_every_book_and_width() {
        // --- phase: narrow_geometry_stays_inside_the_window ---
        {
            let dialog = ExportDialog::new();
            let window = Rect::new(0.0, 0.0, 320.0, 500.0);
            let geometry = dialog.geometry(window, &store(), SortKey::Name);
            assert!(window.contains(geometry.panel.origin()));
            assert!(geometry.panel.x1 <= window.x1);
        }

        // --- phase: true_two_hundred_percent_narrow_layout_stacks_without_overlap ---
        {
            let mut dialog = ExportDialog::new();
            dialog.set_text_scale(2.0);
            let window = Rect::new(0.0, 0.0, 390.0, 844.0);
            let geometry = dialog.geometry(window, &store(), SortKey::Name);

            assert!(geometry.compact);
            assert!(geometry.panel.y0 >= window.y0);
            assert!(geometry.panel.y1 <= window.y1);
            assert!(geometry.select_all.y1 <= geometry.list_body.y0);
            assert!(geometry.select_none.y1 <= geometry.list_body.y0);
            assert!(geometry.list_body.y1 <= geometry.cancel.y0);
            assert!(geometry.cancel.y1 <= geometry.export_v3.y0);
            assert!(geometry.cancel.y1 <= geometry.export_v4.y0);
            assert!(geometry.export_v3.x1 <= geometry.export_v4.x0);
            assert!(geometry.rows.iter().all(|(_, row, _)| row.height() >= 56.0));
        }

        // --- phase: short_books_fit_their_rows_without_a_dead_middle ---
        {
            let store = store();
            let dialog = ExportDialog::new();
            let geometry =
                dialog.geometry(Rect::new(0.0, 0.0, 1_280.0, 800.0), &store, SortKey::Name);
            assert_eq!(geometry.row_count, 7);
            assert!(
                (geometry.panel.height()
                    - (EXPORT_HEADER_H + 7.0 * EXPORT_ROW_H + EXPORT_FOOTER_H))
                    .abs()
                    < f64::EPSILON
            );
            assert!((geometry.list_body.height() - 7.0 * EXPORT_ROW_H).abs() < f64::EPSILON);
            assert!(
                geometry.panel.height() < EXPORT_MAX_H,
                "a short list does not consume the large-directory dialog height"
            );
        }

        // --- phase: large_books_cap_height_and_materialize_only_a_viewport ---
        {
            let mut store = ContactStore::new();
            for index in 0..1_000 {
                store.upsert(Contact {
                    id: uuid::Uuid::new_v4(),
                    first: format!("Directory {index:04}"),
                    ..Contact::default()
                });
            }
            let dialog = ExportDialog::new();
            let geometry =
                dialog.geometry(Rect::new(0.0, 0.0, 1_280.0, 800.0), &store, SortKey::Name);
            assert!((geometry.panel.height() - EXPORT_MAX_H).abs() < f64::EPSILON);
            assert!(geometry.rows.len() < geometry.row_count);
        }
    }

    /// Opening resets selection and visibility; row selection and version choice emit exactly one export request.
    #[test]
    fn export_dialog_selection_emits_one_request() {
        // --- phase: opening_replaces_selection_and_resets_visibility ---
        {
            let store = store();
            let ids = store.filtered_sorted("", None, SortKey::Name);
            let mut dialog = ExportDialog::new();
            dialog.open(ids.iter().take(2).copied());
            assert!(dialog.is_open());
            assert_eq!(dialog.selection().len(), 2);
            dialog.close();
            assert!(!dialog.is_open());
        }

        // --- phase: row_selection_and_version_choice_emit_one_export_request ---
        {
            let store = store();
            let ids = store.filtered_sorted("", None, SortKey::Name);
            let window = Rect::new(0.0, 0.0, 960.0, 700.0);
            let mut dialog = ExportDialog::new();
            dialog.open(ids.iter().copied());
            let press = WindowEvent::MouseInput {
                // winit intentionally exposes no constructor for synthetic ids.
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Pressed,
                button: MouseButton::Left,
            };

            let geometry = dialog.geometry(window, &store, SortKey::Name);
            dialog.handle_event(
                &press,
                geometry.select_none.center(),
                window,
                &store,
                SortKey::Name,
            );
            let geometry = dialog.geometry(window, &store, SortKey::Name);
            let first = geometry.rows[0];
            dialog.handle_event(&press, first.1.center(), window, &store, SortKey::Name);
            let geometry = dialog.geometry(window, &store, SortKey::Name);
            let (_, request) = dialog.handle_event(
                &press,
                geometry.export_v4.center(),
                window,
                &store,
                SortKey::Name,
            );

            assert_eq!(
                request,
                Some(ExportRequest {
                    ids: vec![first.0],
                    version: VCardVersion::V4,
                })
            );
            assert!(!dialog.is_open());
        }
    }
}
