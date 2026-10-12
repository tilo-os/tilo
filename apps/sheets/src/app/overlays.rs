//! Everything that floats over the sheet: the File, Format and More menus, the
//! sheet-tab menu and its colour popover, the document-settings sheet and the
//! command palette.
//!
//! Menus are the toolkit's `ContextMenu`, the tab colours a `Popover` holding a
//! `ColorSwatchPicker`, the settings an immediate-mode modal sheet
//! (`modal_dialog`) with chips and a segmented control inside, and the palette
//! a `CommandPalette` over the modal scrim. All of them paint in the window's
//! top layer (`Widget::draw_overlay`), so they sit above the grid, the bars and
//! the status bubble.

use super::{
    Affine, ContextMenu, Currency, DATE_STYLES, MenuItem, MenuRow, Point, PresenceCtx, Rect, Scene,
    SettingChip, SettingKind, SettingsPanel, SheetsApp, TAB_MENU_ACTIONS, TAB_PALETTE_LEN,
    TIME_STYLES, TabAction, TextRenderer, TextRole, Theme, Widget, WindowEvent, tab_action_label,
};
use ui::num::f64_of_usize;
use ui::widget::modal_dialog::{self, ModalAction, ModalHit, ModalLayout, ModalSpec};
use ui::widget::{
    ChipFace, ChipLead, ColorSwatch, ColorSwatchPicker, ControlSize, EventCtx, MenuItem as Row,
    SegmentedFace, SegmentedStyle,
};

/// A row of the sheet-tab menu.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum TabMenuRow {
    /// One of the plain tab actions.
    Act(TabAction),
    /// Open the colour popover for the tab.
    PickColour,
    /// Clear the tab's colour.
    ClearColour,
}

/// What the document-settings sheet laid out last frame: the sheet itself and
/// every option's hit rect, so a click between frames lands on what was shown.
pub(super) struct SettingsSheet {
    pub modal: ModalLayout,
    pub options: Vec<SettingChip>,
}

/// One painted piece of the settings body, in coordinates relative to the
/// sheet's body.
enum SettingsBlock {
    Heading(&'static str, Rect),
    Chip(SettingKind, Rect),
    Strip(Vec<(SettingKind, Rect)>),
    Note(&'static str, Rect),
}

/// The footer line of the settings sheet: what the options apply to, and what
/// "Default" means.
const SETTINGS_NOTE: &str =
    "These settings apply to this workbook only. Default follows your system settings.";

impl SheetsApp {
    // --- the command menus ---------------------------------------------------

    /// The File menu's rows: New sheet, then the three imports, then the
    /// three exports.
    pub(super) fn file_menu_rows() -> Vec<MenuRow> {
        let mut rows = Vec::new();
        for (index, (label, item)) in Self::file_menu_items().into_iter().enumerate() {
            if index == 1 || index == 4 {
                rows.push((String::new(), None, None));
            }
            rows.push((label.to_owned(), Some(item), None));
        }
        rows
    }

    /// The Format menu's rows, the active cell's format checked.
    pub(super) fn format_menu_rows(&self) -> Vec<MenuRow> {
        let active = self.current_active_format.category();
        let mut rows = Vec::new();
        for (label, item) in self.format_menu_items() {
            if item == MenuItem::ClearFormat {
                rows.push((String::new(), None, None));
                rows.push((label.to_owned(), Some(item), None));
            } else {
                let on = matches!(&item, MenuItem::SetFormat(f) if f.category() == active);
                rows.push((label.to_owned(), Some(item), Some(on)));
            }
        }
        rows
    }

    /// Where a floating surface may stand: the window, kept `s2` off every
    /// edge so a menu never touches it.
    pub(super) fn float_bounds(&self) -> Rect {
        self.rect.inset(-self.metrics.s2)
    }

    /// `anchor` with `gap` of air above and below, so a menu or popover stands
    /// clear of what opened it (and of that thing's focus ring).
    fn float_anchor(anchor: Rect, gap: f64) -> Rect {
        Rect::new(anchor.x0, anchor.y0 - gap, anchor.x1, anchor.y1 + gap)
    }

    /// Close every floating surface.
    pub(super) fn close_overlays(&mut self) {
        self.tab_menu.close();
        self.tab_menu_target = None;
        self.tab_colour.close();
        self.settings_panel = SettingsPanel::None;
        self.pending.redraw = true;
    }

    /// Re-anchor whatever is open after a reflow, so a menu follows its button
    /// and the tab menu follows its tab.
    pub(super) fn relayout_overlays(&mut self) {
        if let Some(target) = self.tab_menu_target {
            let tab = self.tab_anchor(target);
            self.tab_menu.reanchor(
                Self::float_anchor(tab, self.metrics.s3),
                self.float_bounds(),
            );
            self.tab_colour
                .set_anchor(Self::float_anchor(tab, self.metrics.s2));
            self.tab_colour.set_viewport(self.float_bounds());
        }
    }

    /// The rect of tab `idx`, or the add button's when it has none.
    fn tab_anchor(&self, idx: usize) -> Rect {
        self.tab_rects
            .iter()
            .find(|(_, i)| *i == idx)
            .map_or(self.add_tab_rect, |(rect, _)| *rect)
    }

    // --- the sheet-tab menu --------------------------------------------------

    /// Open the right-click menu for sheet tab `idx`, closing any other
    /// overlay first.
    pub(super) fn open_tab_menu(&mut self, idx: usize) {
        self.close_overlays();
        self.tab_rename = None;
        let mut context = ContextMenu::new();
        let mut rows: Vec<Option<TabMenuRow>> = Vec::new();
        for act in TAB_MENU_ACTIONS {
            if !matches!(act, TabAction::Delete) {
                context.push(Row::selection(tab_action_label(act)));
                rows.push(Some(TabMenuRow::Act(act)));
            }
        }
        context.push(Row::separator());
        rows.push(None);
        context.push(Row::selection("Tab colour"));
        rows.push(Some(TabMenuRow::PickColour));
        if self.sheet_colors.get(idx).copied().flatten().is_some() {
            context.push(Row::selection("Remove tab colour"));
            rows.push(Some(TabMenuRow::ClearColour));
        }
        context.push(Row::separator());
        rows.push(None);
        context.push(Row::destructive_selection(tab_action_label(
            TabAction::Delete,
        )));
        rows.push(Some(TabMenuRow::Act(TabAction::Delete)));
        let tab = self.tab_anchor(idx);
        context.anchor_row(tab);
        context.open_anchored(
            Self::float_anchor(tab, self.metrics.s3),
            self.float_bounds(),
        );
        self.tab_menu = context;
        self.tab_menu_rows = rows;
        self.tab_menu_target = Some(idx);
        self.tab_colour
            .set_anchor(Self::float_anchor(tab, self.metrics.s2));
        self.tab_colour.set_viewport(self.float_bounds());
        self.pending.redraw = true;
    }

    /// Run the row the open tab menu just reported, if any.
    fn drain_tab_menu(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let Some(target) = self.tab_menu_target else {
            return;
        };
        if let Some(row) = self.tab_menu.take_selection() {
            match self.tab_menu_rows.get(row).copied().flatten() {
                Some(TabMenuRow::Act(act)) => {
                    self.tab_menu_target = None;
                    self.run_tab_action(doc, target, act);
                }
                Some(TabMenuRow::ClearColour) => {
                    self.tab_menu_target = None;
                    self.run_tab_action(doc, target, TabAction::SetColor(None));
                }
                Some(TabMenuRow::PickColour) => {
                    // The menu closed itself; the colour popover takes the
                    // same anchor and the target stays.
                    let tab = self.tab_anchor(target);
                    self.tab_colour
                        .set_anchor(Self::float_anchor(tab, self.metrics.s2));
                    self.tab_colour.set_viewport(self.float_bounds());
                    self.tab_colour.set_open(true);
                }
                None => {}
            }
        }
        if !self.tab_menu.is_open() && !self.tab_colour.is_open() {
            self.tab_menu_target = None;
        }
        self.pending.redraw = true;
    }

    /// Run the colour the open popover just chose, if any.
    fn drain_tab_colour(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let Some(target) = self.tab_menu_target else {
            return;
        };
        if let Some(choice) = self.tab_colour.content_mut().take_choice() {
            self.tab_colour.close();
            self.tab_menu_target = None;
            let slot = u32::try_from(choice).ok();
            self.run_tab_action(doc, target, TabAction::SetColor(slot));
        }
        if !self.tab_menu.is_open() && !self.tab_colour.is_open() {
            self.tab_menu_target = None;
        }
        self.pending.redraw = true;
    }

    /// Keep the colour popover's swatches in step with the theme and the
    /// tab's current colour. The picker is rebuilt only when they change.
    fn sync_tab_colour_picker(&mut self, theme: &Theme) {
        let Some(target) = self.tab_menu_target else {
            return;
        };
        let presence = &theme.tokens.colors.presence;
        let swatches: Vec<ColorSwatch> = (0..TAB_PALETTE_LEN)
            .map(|slot| {
                let colour = presence.series(slot);
                let name = ui::tokens::colors::PresenceHue::ALL
                    .iter()
                    .find(|hue| presence.hue(**hue) == colour)
                    .map_or_else(
                        || format!("Colour {}", slot + 1),
                        |hue| {
                            let mut name = hue.id().to_owned();
                            if let Some(first) = name.get_mut(..1) {
                                first.make_ascii_uppercase();
                            }
                            name
                        },
                    );
                ColorSwatch::named(colour, name)
            })
            .collect();
        let current = self
            .sheet_colors
            .get(target)
            .copied()
            .flatten()
            .and_then(|slot| usize::try_from(slot).ok());
        let stale = swatches.iter().enumerate().any(|(slot, swatch)| {
            self.tab_colour
                .content()
                .swatch(slot)
                .is_none_or(|have| have.color != swatch.color)
        });
        // A choice made elsewhere (or a sync that landed after the popover
        // opened) changes which swatch is marked, so the picker follows it.
        let shown = Some((target, current));
        if stale || self.tab_colour_shown != shown {
            self.tab_colour_shown = shown;
            let mut picker = ColorSwatchPicker::new(swatches, TAB_PALETTE_LEN);
            if let Some(slot) = current {
                picker = picker.with_selected(slot % TAB_PALETTE_LEN);
            }
            *self.tab_colour.content_mut() = picker;
        }
    }

    // --- routing -------------------------------------------------------------

    /// Route an event to whatever floats over the sheet. A floating surface is
    /// modal: while one is open it takes every event. The command bar's own
    /// menus go with the bar, which takes the event first. Returns whether
    /// the event was consumed.
    pub(super) fn route_overlays(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        if self.tab_colour.is_open() {
            self.tab_colour
                .handle_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id));
            self.drain_tab_colour(&mut ctx.state);
            *ctx.handled = true;
            return true;
        }
        if self.tab_menu.is_open() {
            self.tab_menu
                .handle_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id));
            self.drain_tab_menu(&mut ctx.state);
            *ctx.handled = true;
            return true;
        }
        // The palette and the settings sheet are modal over the bar too.
        !self.palette.is_open()
            && self.settings_panel == SettingsPanel::None
            && self.route_command_bar(event, ctx)
    }

    // --- the document-settings sheet ------------------------------------------

    /// The choices of a settings strip: "Default" (inherit) first, then each
    /// style in display order.
    fn setting_strip(kind: usize) -> Vec<(&'static str, SettingKind)> {
        let mut strip = vec![(
            "Default",
            match kind {
                0 => SettingKind::Date(None),
                _ => SettingKind::Time(None),
            },
        )];
        if kind == 0 {
            strip.extend(
                DATE_STYLES
                    .iter()
                    .map(|(label, style)| (*label, SettingKind::Date(Some(*style)))),
            );
        } else {
            strip.extend(
                TIME_STYLES
                    .iter()
                    .map(|(label, style)| (*label, SettingKind::Time(Some(*style)))),
            );
        }
        strip
    }

    /// Lay the settings body out in `width`, relative to the body's top-left:
    /// a heading over each control, the currencies as wrapping chips, the date
    /// and time styles as segmented strips, and the closing note. Returns the
    /// blocks and the body's height.
    fn settings_blocks(
        tr: &mut TextRenderer,
        theme: &Theme,
        width: f64,
    ) -> (Vec<SettingsBlock>, f64) {
        let sp = &theme.tokens.spacing;
        let (related, group) = (f64::from(sp.s2), f64::from(sp.s6));
        let heading_h = f64::from(
            tr.measure_style("Ag", None, &theme.text(TextRole::Subheading))
                .1,
        );
        let chip_h = ChipFace::height(theme);
        let strip_h = SegmentedFace::height(ControlSize::Small, theme);
        let mut blocks = Vec::new();
        let mut y = 0.0;

        // Currency: Default first, then every code, wrapping inside the body.
        blocks.push(SettingsBlock::Heading(
            "Currency",
            Rect::new(0.0, y, width, y + heading_h),
        ));
        y += heading_h + related;
        let mut x = 0.0;
        let labels: Vec<(String, SettingKind)> =
            std::iter::once(("Default".to_owned(), SettingKind::Currency(None)))
                .chain(
                    Currency::ALL
                        .iter()
                        .map(|c| (c.code().to_owned(), SettingKind::Currency(Some(*c)))),
                )
                .collect();
        for (label, kind) in labels {
            let w = ChipFace::intrinsic_width(&label, ChipLead::None, false, tr, theme);
            if x > 0.0 && x + w > width {
                x = 0.0;
                y += chip_h + related;
            }
            blocks.push(SettingsBlock::Chip(
                kind,
                Rect::new(x, y, x + w, y + chip_h),
            ));
            x += w + related;
        }
        y += chip_h + group;

        // Date and time styles: one segmented strip each.
        for (heading, strip_kind) in [("Date format", 0), ("Time format", 1)] {
            blocks.push(SettingsBlock::Heading(
                heading,
                Rect::new(0.0, y, width, y + heading_h),
            ));
            y += heading_h + related;
            let strip = Self::setting_strip(strip_kind);
            let labels: Vec<&str> = strip.iter().map(|(label, _)| *label).collect();
            let wanted = SegmentedFace::intrinsic_width(&labels, ControlSize::Small, tr, theme);
            let total = wanted.min(width);
            let each = total / f64_of_usize(strip.len().max(1));
            let cells = strip
                .iter()
                .enumerate()
                .map(|(i, (_, kind))| {
                    let x0 = each * f64_of_usize(i);
                    (*kind, Rect::new(x0, y, x0 + each, y + strip_h))
                })
                .collect();
            blocks.push(SettingsBlock::Strip(cells));
            y += strip_h + group;
        }

        let note = theme.text(TextRole::Caption);
        let (_, note_h) = tr.measure_style(SETTINGS_NOTE, Some(width as f32), &note);
        blocks.push(SettingsBlock::Note(
            SETTINGS_NOTE,
            Rect::new(0.0, y, width, y + f64::from(note_h)),
        ));
        y += f64::from(note_h);
        (blocks, y)
    }

    /// Lay the settings sheet out and remember where everything landed, so a
    /// click between frames lands on what was painted.
    fn layout_settings_sheet(
        &self,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> (SettingsSheet, Vec<SettingsBlock>) {
        let width = modal_dialog::body_width(self.rect, modal_dialog::MODAL_MAX_WIDTH, theme);
        let (mut blocks, height) = Self::settings_blocks(tr, theme, width);
        let actions = [ModalAction::primary("Done")];
        let spec = ModalSpec::new("Document settings")
            .actions(&actions)
            .body_height(height);
        let modal = modal_dialog::layout(self.rect, &spec, tr, theme);
        let origin = modal.body.origin().to_vec2();
        let mut options = Vec::new();
        for block in &mut blocks {
            match block {
                SettingsBlock::Heading(_, rect) | SettingsBlock::Note(_, rect) => {
                    *rect = *rect + origin;
                }
                SettingsBlock::Chip(kind, rect) => {
                    *rect = *rect + origin;
                    options.push(SettingChip {
                        rect: *rect,
                        kind: *kind,
                    });
                }
                SettingsBlock::Strip(cells) => {
                    for (kind, rect) in cells {
                        *rect = *rect + origin;
                        options.push(SettingChip {
                            rect: *rect,
                            kind: *kind,
                        });
                    }
                }
            }
        }
        (SettingsSheet { modal, options }, blocks)
    }

    fn paint_settings_sheet(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.settings_panel == SettingsPanel::None {
            self.settings_sheet = None;
            return;
        }
        let (sheet, blocks) = self.layout_settings_sheet(tr, theme);
        let mouse = self.last_mouse;
        let hovered = move |rect: Rect| rect.contains(mouse);
        let actions = [ModalAction::primary("Done")
            .hovered(sheet.modal.actions.first().is_some_and(|r| hovered(*r)))];
        let spec = ModalSpec::new("Document settings")
            .actions(&actions)
            .close_hovered(sheet.modal.close.is_some_and(hovered));
        modal_dialog::paint_backdrop(scene, transform, tr, theme, &sheet.modal, &spec);

        // The body stands on the sheet, so it is measured and inked against
        // the sheet's ground, not the window's.
        let on_sheet = theme.on_ground(ui::chrome::panel_ground(theme));
        let (ink, dim) = modal_dialog::ink(theme);
        for block in &blocks {
            match block {
                SettingsBlock::Heading(text, rect) => {
                    tr.draw_style(
                        scene,
                        text,
                        ink,
                        transform * Affine::translate((rect.x0, rect.y0)),
                        None,
                        &on_sheet.text(TextRole::Subheading),
                    );
                }
                SettingsBlock::Note(text, rect) => {
                    tr.draw_style(
                        scene,
                        text,
                        dim,
                        transform * Affine::translate((rect.x0, rect.y0)),
                        Some(rect.width() as f32),
                        &on_sheet.text(TextRole::Caption),
                    );
                }
                SettingsBlock::Chip(kind, rect) => {
                    let (label, selected) = self.chip_label_and_state(*kind);
                    ChipFace::new(*rect, &label)
                        .selected(selected)
                        .hovered(hovered(*rect))
                        .paint(scene, transform, tr, &on_sheet);
                }
                SettingsBlock::Strip(cells) => {
                    let rects: Vec<Rect> = cells.iter().map(|(_, rect)| *rect).collect();
                    let labels: Vec<String> = cells
                        .iter()
                        .map(|(kind, _)| self.chip_label_and_state(*kind).0)
                        .collect();
                    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                    let selected = cells
                        .iter()
                        .position(|(kind, _)| self.chip_label_and_state(*kind).1)
                        .unwrap_or(0);
                    let over = cells.iter().position(|(_, rect)| hovered(*rect));
                    let face = SegmentedFace::new(&rects, &label_refs, selected)
                        .style(SegmentedStyle::Segmented)
                        .control_size(ControlSize::Small)
                        .hovered(over);
                    ui::widget::ToggleGroup::paint(scene, transform, tr, &on_sheet, &face);
                }
            }
        }
        modal_dialog::paint_actions(scene, transform, tr, theme, &sheet.modal, &spec);
        self.settings_sheet = Some(sheet);
    }

    /// A press while the settings sheet is open: an option applies, Done or the
    /// close control dismisses, a click on the scrim dismisses, anything on
    /// the sheet itself is swallowed. Always consumed (the sheet is modal).
    pub(super) fn try_settings_panel_click(
        &mut self,
        p: Point,
        state: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) -> bool {
        if self.settings_panel == SettingsPanel::None {
            return false;
        }
        if let Some(kind) = self.setting_chip_at(p) {
            self.apply_setting(state, kind);
        } else if let Some(sheet) = &self.settings_sheet {
            match sheet.modal.hit(p) {
                Some(ModalHit::Close | ModalHit::Action(_)) => {
                    self.settings_panel = SettingsPanel::None;
                }
                Some(ModalHit::Scrim) if sheet.modal.dismiss_on_scrim => {
                    self.settings_panel = SettingsPanel::None;
                }
                _ => {}
            }
        } else {
            self.settings_panel = SettingsPanel::None;
        }
        self.pending.redraw = true;
        *handled = true;
        true
    }

    /// The option under `p` in the settings sheet.
    pub(super) fn setting_chip_at(&self, p: Point) -> Option<SettingKind> {
        self.settings_sheet
            .as_ref()?
            .options
            .iter()
            .find(|option| option.rect.contains(p))
            .map(|option| option.kind)
    }

    // --- the top layer ---------------------------------------------------------

    /// Paint everything that floats, in the window's top layer.
    pub(super) fn paint_overlays(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let outer = presence.push_viewport(self.float_bounds());
        self.toolbar
            .draw_overlay(scene, transform, tr, theme, presence);
        presence.pop_viewport(outer);
        self.tab_menu
            .draw_overlay(scene, transform, tr, theme, presence);
        if self.tab_colour.is_open() {
            self.sync_tab_colour_picker(theme);
            self.tab_colour
                .draw_overlay(scene, transform, tr, theme, presence);
        }
        self.paint_settings_sheet(scene, transform, tr, theme);
        // The command palette floats above everything else, over the scrim.
        if self.palette.is_open() {
            let (pw, ph) = self.palette.desired_size();
            let gutter = f64::from(theme.tokens.spacing.s6);
            let pw = pw.min((self.rect.width() - 2.0 * gutter).max(0.0));
            let ph = ph.min((self.rect.height() - 2.0 * gutter).max(0.0));
            let x0 = self.rect.center().x - pw / 2.0;
            let y0 = (self.rect.y0 + (self.rect.height() * 0.12).max(gutter))
                .min(self.rect.y1 - ph - gutter)
                .max(self.rect.y0 + gutter);
            let card = Rect::new(x0, y0, x0 + pw, y0 + ph);
            ui::overlay::scrim::behind_panel(
                scene,
                transform,
                self.rect,
                vello::kurbo::RoundedRect::from_rect(
                    card,
                    ui::widget::floating_surface::radius(theme),
                ),
                theme,
            );
            self.palette.set_rect(card);
            self.palette.draw(scene, transform, tr, theme, presence);
        }
    }
}
