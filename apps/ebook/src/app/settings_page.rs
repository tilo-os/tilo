//! The settings page: its tabs, rows, the font and colour-scheme managers
//! and their editors.

use super::{
    Action, BlendMode, BooksApp, ButtonFace, ButtonRole, Color, ColorScheme, ColorSwatchFace,
    ControlSize, EmptyState, EmptyStateContent, Fill, FontChoice, FontEntry, FontKind,
    HostedDropdown, Icon, InputHint, ListRowFace, ModalAction, ModalSpec, NumberField, PAGE_MAX_W,
    PaintCtx, PathBuf, Point, ReadTheme, ReaderPalette, ReaderShortcut, Rect, RenameTarget,
    ScrollMetrics, SegmentedFace, SegmentedStyle, SelectionState, SettingsTab, TextRole, Theme,
    ToggleGroup, Widget, button_width, content, key_legend, modal_dialog, paint_scroll_thumb,
    switch_size,
};

/// The settings page's content column: its leading edge and width, bounded
/// to a readable measure.
#[derive(Clone, Copy)]
pub(super) struct SettingsColumn {
    pub(super) x: f64,
    pub(super) w: f64,
}

impl SettingsColumn {
    pub(super) fn x1(self) -> f64 {
        self.x + self.w
    }

    /// The span of a row whose text, separators and controls line up with
    /// the column: the row's own inset hangs outside it, where only a
    /// row's lift reaches.
    pub(super) fn row_span(self, theme: &Theme) -> (f64, f64) {
        let inset = ui::chrome::row_bleed(theme);
        (self.x - inset, self.x1() + inset)
    }
}

/// The sizes a scheme row's actions take, measured once per row.
pub(super) struct SchemeControls {
    pub(super) duplicate_w: f64,
    pub(super) edit_w: f64,
    pub(super) switch_w: f64,
    pub(super) small_h: f64,
    pub(super) gap: f64,
}

impl SchemeControls {
    pub(super) fn measure(ctx: &mut PaintCtx<'_>) -> Self {
        let small = ControlSize::Small;
        Self {
            duplicate_w: button_width(ctx, "Duplicate", None, small),
            edit_w: button_width(ctx, "Edit", None, small),
            switch_w: switch_size(ctx.theme).0,
            small_h: small.height(ctx.theme),
            gap: f64::from(ctx.theme.tokens.spacing.rhythm.related),
        }
    }

    /// The whole cluster's width, with a custom scheme's Edit and delete:
    /// every row reserves it, so every row's name has the same room.
    pub(super) fn width(&self) -> f64 {
        self.edit_w
            + self.gap
            + self.small_h
            + self.gap
            + self.duplicate_w
            + self.gap
            + self.switch_w
    }
}

/// The scheme editor's measured rows.
#[derive(Clone, Copy)]
pub(super) struct SchemeEditorMetrics {
    pub(super) sub: f64,
    pub(super) caption: f64,
    pub(super) field_h: f64,
    pub(super) swatch_h: f64,
    pub(super) preview_h: f64,
}

/// One settings row: what it says and how it stands.
pub(super) struct SettingsRow<'a> {
    pub(super) title: &'a str,
    pub(super) secondary: Option<&'a str>,
    /// Room the row's controls take at its trailing end.
    pub(super) trailing: f64,
    /// Another row follows: a separator under this one.
    pub(super) separator: bool,
    pub(super) selected: bool,
    /// The rename field takes the row's text: paint none.
    pub(super) renaming: bool,
    /// A font row sets its name in the font it names.
    pub(super) title_family: Option<&'a FontChoice>,
}

/// The settings tabs, in order.
pub(super) const SETTINGS_TABS: [(SettingsTab, &str); 4] = [
    (SettingsTab::Library, "Library"),
    (SettingsTab::Fonts, "Fonts"),
    (SettingsTab::Colours, "Colour schemes"),
    (SettingsTab::Shortcuts, "Shortcuts"),
];

// --- Settings screen: the font + colour-scheme managers ---------------------

impl BooksApp {
    pub(super) fn draw_settings(&mut self, ctx: &mut PaintCtx<'_>) {
        let theme = ctx.theme;
        let r = self.rect;
        let band = self.draw_page_header(ctx);
        let size = ControlSize::Regular;
        let height = size.height(theme);
        let width = button_width(ctx, "Library", Some(Icon::ChevronLeft), size);
        let x0 = band.x0 + f64::from(theme.tokens.spacing.s3);
        let back = Rect::new(
            x0,
            band.center().y - height / 2.0,
            x0 + width,
            band.center().y + height / 2.0,
        );
        self.button(
            ctx,
            ButtonFace::new(back, "Library")
                .icon(Icon::ChevronLeft)
                .accessible_name("Back to library")
                .role(ButtonRole::Plain),
            Action::CloseSettings,
        );

        let rhythm = &theme.tokens.spacing.rhythm;
        let gutter = f64::from(rhythm.group);
        let body = Rect::new(r.x0, band.y1, r.x1, r.y1);
        let col = SettingsColumn {
            x: body.x0 + gutter,
            w: (body.width() - 2.0 * gutter).clamp(1.0, PAGE_MAX_W),
        };
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &body,
        );
        let top = body.y0 + gutter - self.settings_scroll;
        let mut y = Self::draw_page_title(ctx, Point::new(col.x, top), "Settings", None);
        y += f64::from(rhythm.section);
        y = self.draw_settings_tabs(ctx, col, y);
        y += f64::from(rhythm.group);
        let end_y = match self.settings_tab {
            SettingsTab::Library => self.draw_library_settings(ctx, col, y),
            SettingsTab::Fonts => self.draw_fonts_manager(ctx, col, y),
            SettingsTab::Colours => self.draw_schemes_manager(ctx, col, y),
            SettingsTab::Shortcuts => self.draw_shortcuts_settings(ctx, col, y),
        };
        ctx.scene.pop_layer();
        let content_h = end_y - top + 2.0 * gutter;
        self.settings_max_scroll = (content_h - body.height()).max(0.0);
        self.settings_scroll = self.settings_scroll.clamp(0.0, self.settings_max_scroll);
        paint_scroll_thumb(
            ctx,
            body,
            ScrollMetrics::new(content_h, body.height(), self.settings_scroll),
        );

        // The sheets float over the page.
        if self.editing_scheme.is_some() {
            self.draw_scheme_editor(ctx);
        }
        if self.font_picker {
            self.draw_font_picker(ctx);
        }
        if self.color_picker.is_open() {
            self.color_picker
                .draw(ctx.scene, ctx.transform, ctx.text, theme, self.rect);
        }
    }

    /// The tab strip under the page title, aligned with the content column.
    /// When the tabs do not fit on one line they wrap into further strips,
    /// so every tab stays whole and reachable.
    pub(super) fn draw_settings_tabs(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        y: f64,
    ) -> f64 {
        let theme = ctx.theme;
        let size = ControlSize::Regular;
        let height = SegmentedFace::height(size, theme);
        // Each tab as wide as a tab strip makes it: packed from the leading
        // edge, its label's own width.
        let segments: Vec<ui::widget::Segment> = SETTINGS_TABS
            .iter()
            .map(|(_, label)| ui::widget::Segment::new(*label))
            .collect();
        let widths: Vec<f64> = SegmentedFace::partition_segments(
            Rect::new(0.0, 0.0, f64::MAX / 4.0, height),
            &segments,
            SegmentedStyle::Tabs,
            size,
            ctx.text,
            theme,
        )
        .iter()
        .map(Rect::width)
        .collect();
        // Greedy rows of whole tabs.
        let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
        let mut x = 0.0;
        for (i, width) in widths.iter().enumerate() {
            if x > 0.0 && x + width > col.w {
                rows.push(Vec::new());
                x = 0.0;
            }
            if let Some(row) = rows.last_mut() {
                row.push(i);
            }
            x += width;
        }
        // A tab strip's first label starts the strip, so the strip starts on
        // the column: in line with the title and the rows below it.
        let mut row_y = y;
        for row in rows {
            let mut x = col.x;
            let segments: Vec<Rect> = row
                .iter()
                .map(|&i| {
                    let w = widths.get(i).copied().unwrap_or(0.0);
                    let rect = Rect::new(x, row_y, x + w, row_y + height);
                    x += w;
                    rect
                })
                .collect();
            let labels: Vec<&str> = row
                .iter()
                .filter_map(|&i| SETTINGS_TABS.get(i).map(|(_, label)| *label))
                .collect();
            let selected = row
                .iter()
                .position(|&i| {
                    SETTINGS_TABS
                        .get(i)
                        .is_some_and(|(tab, _)| *tab == self.settings_tab)
                })
                .unwrap_or(labels.len());
            let hovered = segments.iter().position(|rect| rect.contains(self.cursor));
            ToggleGroup::paint(
                ctx.scene,
                ctx.transform,
                ctx.text,
                theme,
                &SegmentedFace::new(&segments, &labels, selected)
                    .style(SegmentedStyle::Tabs)
                    .control_size(size)
                    .hovered(hovered),
            );
            for (&i, rect) in row.iter().zip(&segments) {
                if let Some((tab, _)) = SETTINGS_TABS.get(i) {
                    self.push_hit(*rect, Action::SetSettingsTab(*tab));
                }
            }
            row_y += height;
        }
        row_y
    }

    /// A section of the settings page: its title in `Subheading` and, when
    /// the person needs it to act, one sentence about it. Aligned with the
    /// row titles below. Returns the y where the rows start.
    pub(super) fn draw_settings_section(
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        mut y: f64,
        title: &str,
        description: Option<&str>,
    ) -> f64 {
        let theme = ctx.theme;
        let rhythm = &theme.tokens.spacing.rhythm;
        let x = col.x;
        let wrap = Some(ui::num::f32_of_f64((col.x1() - x).max(1.0)));
        ctx.text_role(
            title,
            TextRole::Subheading,
            theme.text_primary,
            Point::new(x, y),
            wrap,
        );
        y += ctx.measure_role(title, TextRole::Subheading, wrap).height;
        if let Some(description) = description {
            y += f64::from(rhythm.tight);
            ctx.text_role(
                description,
                TextRole::Body,
                theme.text_dim,
                Point::new(x, y),
                wrap,
            );
            y += ctx.measure_role(description, TextRole::Body, wrap).height;
        }
        y + f64::from(rhythm.related)
    }

    /// One settings row from `x0` to `x1`: its title and secondary line as a
    /// list row, leaving its trailing room for the row's controls, and the
    /// separator under it when another row follows. Returns the row rect.
    pub(super) fn draw_settings_row(
        ctx: &mut PaintCtx<'_>,
        (x0, x1): (f64, f64),
        y: f64,
        row: &SettingsRow<'_>,
    ) -> Rect {
        let &SettingsRow {
            title,
            secondary,
            trailing,
            separator,
            selected,
            renaming,
            title_family,
        } = row;
        let theme = ctx.theme;
        let mut face = ListRowFace::new(title).selected(selected);
        if let Some(family) = title_family {
            face = face.title_family(family);
        }
        if let Some(secondary) = secondary {
            face = face.secondary(secondary);
        }
        let h = face.height(ctx.text, theme);
        let row = Rect::new(x0, y, x1, y + h);
        if selected {
            ui::chrome::paint_row_selection(
                ctx.scene,
                ctx.transform,
                row,
                SelectionState::active(true),
                theme.tokens.colors.surfaces.content,
                theme,
            );
        }
        if !renaming {
            face.selected(false)
                .paint(ctx, Rect::new(x0, y, (x1 - trailing).max(x0), y + h));
        }
        if separator {
            // Inset like a list row's own separator: from the text to the
            // trailing edge of the controls.
            let inset = ui::chrome::row_bleed(theme);
            ui::surface::separator(
                ctx.scene,
                ctx.transform,
                x0 + inset,
                x1 - inset,
                row.y1,
                0.0,
                theme.tokens.colors.surfaces.content,
            );
        }
        row
    }

    /// Device-local keyboard bindings. Each row records one printable key;
    /// bindings are not part of the shared document because another reader's
    /// keyboard layout is not shared book state. Arrow and Page keys always
    /// navigate.
    pub(super) fn draw_shortcuts_settings(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        y: f64,
    ) -> f64 {
        let theme = ctx.theme;
        let mut y = Self::draw_settings_section(
            ctx,
            col,
            y,
            "Keyboard shortcuts",
            Some("Choose a shortcut, then press a key. Arrow and Page keys always turn pages."),
        );
        if let Some(error) = self.shortcut_capture_error.clone() {
            let x = col.x;
            let wrap = Some(ui::num::f32_of_f64((col.x1() - x).max(1.0)));
            ctx.text_role(
                &error,
                TextRole::Body,
                theme.tokens.colors.semantic.danger,
                Point::new(x, y),
                wrap,
            );
            y += ctx.measure_role(&error, TextRole::Body, wrap).height
                + f64::from(theme.tokens.spacing.rhythm.related);
        }
        let keycap_h = InputHint::keycap_height(theme);
        for (i, shortcut) in ReaderShortcut::ALL.into_iter().enumerate() {
            let capturing = self.shortcut_capture == Some(shortcut);
            let key = key_legend(
                &self
                    .reader_keys
                    .shortcut(shortcut.id(), shortcut.default_key()),
            );
            let labels = [key];
            let trailing = if capturing {
                ctx.measure_role("Press a key", TextRole::Body, None).width
            } else {
                InputHint::measure_keycap_labels(&labels, ctx.text, theme)
            } + 2.0 * f64::from(theme.tokens.spacing.s3);
            let row = Self::draw_settings_row(
                ctx,
                col.row_span(theme),
                y,
                &SettingsRow {
                    title: shortcut.label(),
                    secondary: None,
                    trailing,
                    separator: i + 1 < ReaderShortcut::ALL.len(),
                    selected: capturing,
                    renaming: false,
                    title_family: None,
                },
            );
            let x1 = col.x1();
            if capturing {
                let w = ctx.measure_role("Press a key", TextRole::Body, None).width;
                ctx.text_role_in_band(
                    "Press a key",
                    TextRole::Body,
                    theme.text_dim,
                    x1 - w,
                    row,
                    None,
                );
            } else {
                let w = InputHint::measure_keycap_labels(&labels, ctx.text, theme);
                InputHint::paint_keycap_labels_at(
                    ctx.scene,
                    ctx.transform,
                    Point::new(x1 - w, row.center().y - keycap_h / 2.0),
                    &labels,
                    ctx.text,
                    theme,
                );
            }
            self.push_hit(row, Action::BeginShortcutCapture(shortcut));
            y = row.y1;
        }
        y
    }

    /// The Library pane: the folders watched for new books, the secondary
    /// way in ("Open book" stays primary).
    pub(super) fn draw_library_settings(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        y: f64,
    ) -> f64 {
        let y = Self::draw_settings_section(
            ctx,
            col,
            y,
            "Watched folders",
            Some("Turn a folder on to add its books to your library, now and as new ones arrive."),
        );

        // Candidate folders: watched ones, plus folders books were opened
        // from, so there is always something concrete to turn on.
        let mut folders: Vec<PathBuf> = self.watched_folders.clone();
        for p in self.library_paths.values() {
            if let Some(parent) = p.parent() {
                let parent = parent.to_path_buf();
                if !folders.contains(&parent) {
                    folders.push(parent);
                }
            }
        }
        folders.sort();
        folders.dedup();

        let y = if folders.is_empty() {
            self.draw_library_settings_empty(ctx, col, y)
        } else {
            self.draw_watched_folder_rows(ctx, col, y, &folders)
        };
        self.draw_catalogue_settings(ctx, col, y)
    }

    /// No folder to offer yet: say how one appears, and offer the way in.
    pub(super) fn draw_library_settings_empty(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        y: f64,
    ) -> f64 {
        let theme = ctx.theme;
        let title = "No folders yet";
        let action = "Open a book";
        let area_h =
            EmptyState::minimum_essential_height(ctx.text, theme, col.w, title, Some(action))
                + 2.0 * self.header_h;
        let area = Rect::new(col.x, y, col.x1(), y + area_h);
        let content = EmptyStateContent {
            icon: Some(Icon::Folder),
            message: Some("Open a book from a folder and the folder appears here."),
            action_label: Some(action),
            ..EmptyStateContent::new(title)
        };
        if let Some(rect) = EmptyState::paint(ctx, area, &content, self.cursor) {
            self.push_hit(rect, Action::OpenViaPortal);
        }
        area.y1
    }

    /// One row per candidate folder: its name, where it is, and whether it
    /// is watched.
    pub(super) fn draw_watched_folder_rows(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        mut y: f64,
        folders: &[PathBuf],
    ) -> f64 {
        let theme = ctx.theme;
        let home = tilo_paths::home().map(|home| home.to_string_lossy().into_owned());
        let ws = content::workspace_root();
        // Present folders in human terms, never a raw internal mount (HIG
        // §4): the workspace reads as "Workspace", the reader's home as "~".
        let pretty = |p: &std::path::Path| -> String {
            if let Ok(rest) = p.strip_prefix(&ws) {
                let rest = rest.to_string_lossy();
                return if rest.is_empty() {
                    "Workspace".to_string()
                } else {
                    format!("Workspace/{rest}")
                };
            }
            let s = p.to_string_lossy().to_string();
            match &home {
                Some(h) if !h.is_empty() => match s.strip_prefix(h.as_str()) {
                    Some(rest) => format!("~{rest}"),
                    None => s,
                },
                _ => s,
            }
        };
        let (switch_w, _) = switch_size(theme);
        let trailing = switch_w + 2.0 * f64::from(theme.tokens.spacing.s3);
        for (i, folder) in folders.iter().enumerate() {
            let watched = self.watched_folders.iter().any(|f| f == folder);
            let path = pretty(folder);
            let name = folder
                .file_name()
                .map_or_else(|| path.clone(), |n| n.to_string_lossy().to_string());
            let row = Self::draw_settings_row(
                ctx,
                col.row_span(theme),
                y,
                &SettingsRow {
                    title: &name,
                    secondary: Some(&path),
                    trailing,
                    separator: i + 1 < folders.len(),
                    selected: false,
                    renaming: false,
                    title_family: None,
                },
            );
            let x1 = col.x1();
            self.switch_at(
                ctx,
                ("watch-folder", &folder),
                x1,
                row.center().y,
                watched,
                Action::ToggleWatchFolder(folder.clone()),
            );
            y = row.y1;
        }
        y
    }

    pub(super) fn draw_fonts_manager(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        y: f64,
    ) -> f64 {
        let theme = ctx.theme;
        let fonts = self.fonts.clone();
        let default_id = if self.default_font_id.is_empty() {
            fonts.first().map(|f| f.id.clone()).unwrap_or_default()
        } else {
            self.default_font_id.clone()
        };

        // The default font: a row whose trailing control is the list of
        // fonts.
        let mut y = Self::draw_settings_section(ctx, col, y, "Default font", None);
        let size = ControlSize::Regular;
        let control_w =
            HostedDropdown::width(fonts.iter().map(|f| f.name.as_str()), size, ctx.text, theme);
        let row = Self::draw_settings_row(
            ctx,
            col.row_span(theme),
            y,
            &SettingsRow {
                title: "Font for new books",
                secondary: None,
                trailing: control_w + 2.0 * f64::from(theme.tokens.spacing.s3),
                separator: false,
                selected: false,
                renaming: false,
                title_family: None,
            },
        );
        let x1 = col.x1();
        let height = size.height(theme);
        self.font_dropdown.set(
            fonts
                .iter()
                .map(|f| (f.id.clone(), f.name.clone()))
                .collect(),
            &default_id,
        );
        self.font_dropdown.draw(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            Rect::new(
                x1 - control_w,
                row.center().y - height / 2.0,
                x1,
                row.center().y + height / 2.0,
            ),
        );
        y = row.y1 + f64::from(theme.tokens.spacing.rhythm.group);

        y = Self::draw_settings_section(
            ctx,
            col,
            y,
            "Reading fonts",
            Some(
                "Turn a font off to leave it out of the reading settings. Select a name to rename it.",
            ),
        );
        for (i, f) in fonts.iter().enumerate() {
            y = self.draw_font_row(ctx, col, y, f, i + 1 < fonts.len());
        }
        y += f64::from(theme.tokens.spacing.rhythm.section);
        self.draw_add_font_button(ctx, col, y)
    }

    /// One font row: its name (or the rename field), what kind of face it
    /// is, and its controls: size adjustment, shown in the reading settings,
    /// and remove (fonts the reader added). Returns the y below it.
    pub(super) fn draw_font_row(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        y: f64,
        f: &FontEntry,
        separator: bool,
    ) -> f64 {
        let theme = ctx.theme;
        let spacing = &theme.tokens.spacing;
        let kind = match &f.kind {
            FontKind::Default => "Default face".to_string(),
            FontKind::Serif => "System reading face".to_string(),
            FontKind::Reading(_) => "Reading face".to_string(),
            FontKind::Mono => "Monospace".to_string(),
            FontKind::Named(_) => "System font".to_string(),
        };
        let field = NumberField::FontMult(f.id.clone());
        let stepper_w = self.number_controls.preferred_width(
            &field,
            f64::from(f.size_mult) * 100.0,
            ctx.text,
            theme,
        );
        let (switch_w, _) = switch_size(theme);
        let small = ControlSize::Small.height(theme);
        let gap = f64::from(spacing.rhythm.sibling);
        let controls_w = small + gap + stepper_w + gap + switch_w;
        let renaming_this =
            matches!(&self.renaming, Some(rn) if rn.target == RenameTarget::Font(f.id.clone()));
        let family = crate::paginate::font_choice(&f.kind);
        let row = Self::draw_settings_row(
            ctx,
            col.row_span(theme),
            y,
            &SettingsRow {
                title: &f.name,
                secondary: Some(&kind),
                trailing: controls_w + 2.0 * f64::from(spacing.s3),
                separator,
                selected: false,
                renaming: renaming_this,
                title_family: Some(&family),
            },
        );
        let controls_x0 = col.x1() - controls_w;
        self.draw_row_name_slot(
            ctx,
            Rect::new(row.x0, row.y0, controls_x0 - gap, row.y1),
            renaming_this,
            &f.name,
            Action::BeginRenameFont(f.id.clone()),
        );

        // Trailing, left to right: remove (fonts the reader added), size,
        // shown; the switch keeps the trailing edge of every row.
        let cy = row.center().y;
        let mut x = controls_x0;
        if matches!(f.kind, FontKind::Named(_)) {
            self.icon_button(
                ctx,
                Point::new(x + small / 2.0, cy),
                ControlSize::Small,
                (Icon::Trash2, "Remove font"),
                false,
                Action::RemoveFont(f.id.clone()),
            );
        }
        x += small + gap;
        self.number_controls.draw(
            &field,
            f64::from(f.size_mult) * 100.0,
            Rect::new(x, cy - small / 2.0, x + stepper_w, cy + small / 2.0),
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
        );
        x += stepper_w + gap;
        self.switch_at(
            ctx,
            ("font-shown", &f.id),
            x + switch_w,
            cy,
            f.shown,
            Action::ToggleFontShown(f.id.clone()),
        );
        row.y1
    }

    /// The title line of a settings row that renames in place: the inline
    /// rename field while renaming, otherwise the target that begins it.
    pub(super) fn draw_row_name_slot(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        slot: Rect,
        renaming: bool,
        name: &str,
        begin: Action,
    ) {
        let theme = ctx.theme;
        let pad = ui::chrome::row_bleed(theme);
        if renaming {
            // The field's edge stands on the row text's edge, in line with
            // the separators and the column above it.
            let height = ControlSize::Small.height(theme);
            let cy = slot.center().y;
            let field = Rect::new(slot.x0 + pad, cy - height / 2.0, slot.x1, cy + height / 2.0);
            if let Some(rn) = self.renaming.as_mut() {
                rn.field.apply_layout_external(field);
                rn.field.draw(
                    ctx.scene,
                    ctx.transform,
                    ctx.text,
                    theme,
                    &ui::presence::PresenceCtx::empty(),
                );
            }
        } else {
            let width = ctx.measure_role(name, TextRole::Body, None).width;
            let hit = Rect::new(
                slot.x0 + pad,
                slot.y0,
                (slot.x0 + pad + width).min(slot.x1),
                slot.center().y,
            );
            self.push_hit(hit, begin);
        }
    }

    /// "Add a system font". Returns the y just below it.
    pub(super) fn draw_add_font_button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        y: f64,
    ) -> f64 {
        let theme = ctx.theme;
        let label = "Add a system font";
        let size = ControlSize::Regular;
        let width = button_width(ctx, label, Some(Icon::Plus), size);
        let x = col.x;
        let rect = Rect::new(x, y, x + width, y + size.height(theme));
        self.button(
            ctx,
            ButtonFace::new(rect, label).icon(Icon::Plus),
            Action::OpenFontPicker,
        );
        rect.y1
    }

    pub(super) fn draw_schemes_manager(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        y: f64,
    ) -> f64 {
        let theme = ctx.theme;
        let mut y = Self::draw_settings_section(
            ctx,
            col,
            y,
            "Colour schemes",
            Some("Turn a scheme off to leave it out of the reading settings."),
        );
        let schemes = self.schemes.clone();
        for (i, s) in schemes.iter().enumerate() {
            y = self.draw_scheme_row(ctx, col, y, s, i + 1 < schemes.len());
        }
        y += f64::from(theme.tokens.spacing.rhythm.section);
        let label = "New scheme";
        let size = ControlSize::Regular;
        let width = button_width(ctx, label, Some(Icon::Plus), size);
        let x = col.x;
        let rect = Rect::new(x, y, x + width, y + size.height(theme));
        self.button(
            ctx,
            ButtonFace::new(rect, label).icon(Icon::Plus),
            Action::NewSchemeViaPicker,
        );
        rect.y1
    }

    /// One colour-scheme row: its page, text and accent colours, its name
    /// and whether it is in use (its secondary line says so; a setting is
    /// not a selection), and its actions. Returns the y below it.
    pub(super) fn draw_scheme_row(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        y: f64,
        s: &ColorScheme,
        separator: bool,
    ) -> f64 {
        let theme = ctx.theme;
        let spacing = &theme.tokens.spacing;
        let in_use = if s.builtin {
            self.settings.scheme_id.is_empty() && self.settings.theme == ReadTheme::from_str(&s.id)
        } else {
            self.settings.scheme_id == s.id
        };
        let status = if in_use {
            "In use"
        } else if s.builtin {
            "Built-in"
        } else {
            "Custom"
        };
        let gap = f64::from(spacing.rhythm.related);
        let controls = SchemeControls::measure(ctx);
        let controls_w = controls.width();
        let swatch = f64::from(theme.tokens.metrics.icon_lg);
        let swatches_w = 3.0 * swatch + 2.0 * gap;
        let renaming_this = self.renaming_scheme(&s.id);
        // The row's text follows the swatches; its own inset is the gap.
        let (span_x0, span_x1) = col.row_span(theme);
        let x0 = col.x + swatches_w;
        let title = s.name.as_str();
        let row = Self::draw_settings_row(
            ctx,
            (x0, span_x1),
            y,
            &SettingsRow {
                title,
                secondary: Some(status),
                trailing: controls_w + 2.0 * f64::from(spacing.s3),
                separator: false,
                selected: false,
                renaming: renaming_this,
                title_family: None,
            },
        );
        if separator {
            ui::surface::separator(
                ctx.scene,
                ctx.transform,
                col.x,
                col.x1(),
                row.y1,
                0.0,
                theme.tokens.colors.surfaces.content,
            );
        }
        self.draw_scheme_swatches(ctx, Point::new(col.x, row.center().y), s);

        let controls_x0 = col.x1() - controls_w;
        let name_slot = Rect::new(row.x0, row.y0, controls_x0 - gap, row.y1);
        if s.builtin {
            self.push_hit(
                Rect::new(span_x0, row.y0, controls_x0 - gap, row.y1),
                Self::scheme_use_action(s),
            );
        } else {
            self.draw_row_name_slot(
                ctx,
                name_slot,
                renaming_this,
                &s.name,
                Action::BeginRenameScheme(s.id.clone()),
            );
            self.push_hit(
                Rect::new(row.x0, row.center().y, controls_x0 - gap, row.y1),
                Self::scheme_use_action(s),
            );
        }

        self.draw_scheme_controls(ctx, Point::new(col.x1(), row.center().y), s, &controls);
        row.y1
    }

    /// A scheme row's actions back from `end` (trailing edge, vertical
    /// centre): whether it is shown, Duplicate, then delete and Edit for a
    /// custom scheme. The shared controls stand in the same columns on every
    /// row.
    pub(super) fn draw_scheme_controls(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        end: Point,
        s: &ColorScheme,
        m: &SchemeControls,
    ) {
        let small = ControlSize::Small;
        let cy = end.y;
        let mut x1 = end.x;
        let face_rect =
            |x1: f64, w: f64| Rect::new(x1 - w, cy - m.small_h / 2.0, x1, cy + m.small_h / 2.0);
        self.switch_at(
            ctx,
            ("scheme-enabled", &s.id),
            x1,
            cy,
            s.enabled,
            Action::ToggleSchemeEnabled(s.id.clone()),
        );
        x1 -= m.switch_w + m.gap;
        self.button(
            ctx,
            ButtonFace::new(face_rect(x1, m.duplicate_w), "Duplicate")
                .role(ButtonRole::Plain)
                .control_size(small),
            Action::DuplicateScheme(s.id.clone()),
        );
        x1 -= m.duplicate_w + m.gap;
        if !s.builtin {
            self.icon_button(
                ctx,
                Point::new(x1 - m.small_h / 2.0, cy),
                small,
                (Icon::Trash2, "Delete scheme"),
                false,
                Action::DeleteScheme(s.id.clone()),
            );
            x1 -= m.small_h + m.gap;
            self.button(
                ctx,
                ButtonFace::new(face_rect(x1, m.edit_w), "Edit")
                    .role(ButtonRole::Plain)
                    .control_size(small),
                Action::EditScheme(s.id.clone()),
            );
        }
    }

    /// Whether the inline rename editor is open on scheme `id`.
    pub(super) fn renaming_scheme(&self, id: &str) -> bool {
        matches!(&self.renaming, Some(rn) if rn.target == RenameTarget::Scheme(id.to_string()))
    }

    /// What activating a scheme row does: a built-in sets its reading
    /// theme, a custom scheme is applied by id.
    pub(super) fn scheme_use_action(s: &ColorScheme) -> Action {
        if s.builtin {
            Action::SetReadTheme(ReadTheme::from_str(&s.id))
        } else {
            Action::SetScheme(s.id.clone())
        }
    }

    /// A scheme's page, text and accent colours from `start` (leading
    /// edge, vertical centre). On a custom scheme each opens the colour
    /// picker on its slot.
    pub(super) fn draw_scheme_swatches(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        start: Point,
        s: &ColorScheme,
    ) {
        let theme = ctx.theme;
        let side = f64::from(theme.tokens.metrics.icon_lg);
        let gap = f64::from(theme.tokens.spacing.rhythm.related);
        let mut x = start.x;
        for (slot, (name, rgb)) in s.slots().into_iter().enumerate() {
            let rect = Rect::new(x, start.y - side / 2.0, x + side, start.y + side / 2.0);
            let face = ColorSwatchFace::new(rect, Color::from_rgb8(rgb[0], rgb[1], rgb[2]), name)
                .hovered(!s.builtin && rect.contains(self.cursor));
            face.paint(ctx.scene, ctx.transform, ctx.text, theme);
            if !s.builtin {
                self.push_hit(rect, Action::PickSchemeColor(s.id.clone(), slot));
            }
            x += side + gap;
        }
    }

    /// The scheme editor sheet: the scheme's name (rename in place), its
    /// three colours (each opens the colour picker), and a preview page.
    pub(super) fn draw_scheme_editor(&mut self, ctx: &mut PaintCtx<'_>) {
        let theme = ctx.theme;
        let Some(id) = self.editing_scheme.clone() else {
            return;
        };
        let Some(scheme) = self.schemes.iter().find(|s| s.id == id).cloned() else {
            return;
        };
        let spacing = &theme.tokens.spacing;
        let rhythm = &spacing.rhythm;
        let swatch_h = f64::from(spacing.s12);
        let preview_h = f64::from(spacing.s16) + f64::from(spacing.s4);
        let field_h = ControlSize::Regular.height(theme);
        let sub = ctx.line_box(TextRole::Subheading);
        let caption = ctx.line_box(TextRole::Caption);
        let body_h = sub
            + f64::from(rhythm.related)
            + field_h
            + f64::from(rhythm.group)
            + sub
            + f64::from(rhythm.related)
            + swatch_h
            + f64::from(rhythm.tight)
            + caption
            + f64::from(rhythm.group)
            + sub
            + f64::from(rhythm.related)
            + preview_h;
        let mut done = ModalAction::primary("Done");
        done.hovered = self
            .modal_actions
            .first()
            .is_some_and(|rect| rect.contains(self.cursor));
        let actions = [done];
        let spec = ModalSpec::new("Edit scheme")
            .actions(&actions)
            .body_height(body_h)
            .max_width(f64::from(theme.tokens.metrics.panel_width_list) + f64::from(spacing.s16));
        let layout = modal_dialog::layout(self.rect, &spec, ctx.text, theme);
        modal_dialog::paint_backdrop(ctx.scene, ctx.transform, ctx.text, theme, &layout, &spec);
        modal_dialog::paint_actions(ctx.scene, ctx.transform, ctx.text, theme, &layout, &spec);
        self.push_hit(layout.scrim, Action::CloseSchemeEditor);
        self.push_hit(layout.card, Action::Swallow);
        if let Some(close) = layout.close {
            self.push_hit(close, Action::CloseSchemeEditor);
        }
        self.modal_actions.clone_from(&layout.actions);
        if let Some(done) = layout.actions.first() {
            self.push_hit(*done, Action::CloseSchemeEditor);
        }

        let panel_theme = theme.on_ground(ui::chrome::panel_ground(theme));
        let mut panel = PaintCtx::new(ctx.scene, ctx.transform, ctx.text, &panel_theme);
        self.draw_scheme_editor_body(
            &mut panel,
            layout.body,
            &scheme,
            SchemeEditorMetrics {
                sub,
                caption,
                field_h,
                swatch_h,
                preview_h,
            },
        );
    }

    /// The scheme editor's body: the name, the three colours and the
    /// preview page, in `body`.
    pub(super) fn draw_scheme_editor_body(
        &mut self,
        panel: &mut PaintCtx<'_>,
        body: Rect,
        scheme: &ColorScheme,
        m: SchemeEditorMetrics,
    ) {
        let panel_theme = panel.theme;
        let spacing = &panel_theme.tokens.spacing;
        let rhythm = &spacing.rhythm;
        let SchemeEditorMetrics {
            sub,
            caption,
            field_h,
            swatch_h,
            preview_h,
        } = m;
        let mut y = body.y0;
        panel.text_role(
            "Name",
            TextRole::Subheading,
            panel_theme.text_primary,
            Point::new(body.x0, y),
            None,
        );
        y += sub + f64::from(rhythm.related);
        let field = Rect::new(body.x0, y, body.x1, y + field_h);
        if self.renaming_scheme(&scheme.id) {
            if let Some(rn) = self.renaming.as_mut() {
                rn.field.apply_layout_external(field);
                rn.field.draw(
                    panel.scene,
                    panel.transform,
                    panel.text,
                    panel_theme,
                    &ui::presence::PresenceCtx::empty(),
                );
            }
        } else {
            let width = button_width(
                panel,
                &scheme.name,
                Some(Icon::Pencil),
                ControlSize::Regular,
            );
            let rect = Rect::new(field.x0, field.y0, field.x0 + width, field.y1);
            self.button(
                panel,
                ButtonFace::new(rect, &scheme.name)
                    .icon(Icon::Pencil)
                    .accessible_name("Rename scheme")
                    .role(ButtonRole::Plain),
                Action::BeginRenameScheme(scheme.id.clone()),
            );
        }
        y = field.y1 + f64::from(rhythm.group);

        panel.text_role(
            "Colours",
            TextRole::Subheading,
            panel_theme.text_primary,
            Point::new(body.x0, y),
            None,
        );
        y += sub + f64::from(rhythm.related);
        let gap = f64::from(rhythm.section);
        let slot_w = (body.width() - 2.0 * gap) / 3.0;
        let mut x = body.x0;
        for (i, (label, rgb)) in scheme.slots().into_iter().enumerate() {
            let rect = Rect::new(x, y, x + slot_w, y + swatch_h);
            ColorSwatchFace::new(rect, Color::from_rgb8(rgb[0], rgb[1], rgb[2]), label)
                .hovered(rect.contains(self.cursor))
                .paint(panel.scene, panel.transform, panel.text, panel_theme);
            self.push_hit(rect, Action::PickSchemeColor(scheme.id.clone(), i));
            let w = panel.measure_role(label, TextRole::Caption, None).width;
            panel.text_role(
                label,
                TextRole::Caption,
                panel_theme.text_dim,
                Point::new(x + (slot_w - w) / 2.0, rect.y1 + f64::from(rhythm.tight)),
                None,
            );
            x += slot_w + gap;
        }
        y += swatch_h + f64::from(rhythm.tight) + caption + f64::from(rhythm.group);

        panel.text_role(
            "Preview",
            TextRole::Subheading,
            panel_theme.text_primary,
            Point::new(body.x0, y),
            None,
        );
        y += sub + f64::from(rhythm.related);
        let pal = ReaderPalette::from_scheme(scheme);
        let page = Rect::new(body.x0, y, body.x1, y + preview_h);
        let radius = f64::from(panel_theme.tokens.radii.control);
        panel.fill_round(page, radius, pal.page);
        panel.stroke_round(page, radius, pal.page_edge, 1.0);
        let inset = f64::from(spacing.s4);
        let sample = "The quick brown fox jumps over the lazy dog.";
        let wrap = Some(ui::num::f32_of_f64((page.width() - 2.0 * inset).max(1.0)));
        panel.text_role(
            sample,
            TextRole::Reading,
            pal.ink,
            Point::new(page.x0 + inset, page.y0 + inset),
            wrap,
        );
    }

    /// The "Add a system font" sheet: every installed family, each previewed
    /// in its own face; choosing one adds it.
    pub(super) fn draw_font_picker(&mut self, ctx: &mut PaintCtx<'_>) {
        let theme = ctx.theme;
        // A colour-emoji face cannot set a book's text (nor preview its own
        // name), so it is not offered.
        let fams: Vec<String> = ctx
            .text
            .system_families()
            .into_iter()
            .filter(|family| !family.to_lowercase().contains("emoji"))
            .collect();
        let row_h = ui::chrome::nav_row_height(theme);
        let wanted = fams.len() as f64 * row_h;
        let spec = ModalSpec::new("Add a system font")
            .body_height(wanted)
            .max_width(f64::from(theme.tokens.metrics.panel_width_list))
            .close_hovered(self.modal_close.contains(self.cursor));
        let layout = modal_dialog::layout(self.rect, &spec, ctx.text, theme);
        modal_dialog::paint_backdrop(ctx.scene, ctx.transform, ctx.text, theme, &layout, &spec);
        self.push_hit(layout.scrim, Action::CloseFontPicker);
        self.push_hit(layout.card, Action::Swallow);
        self.modal_close = layout.close.unwrap_or(Rect::ZERO);
        if let Some(close) = layout.close {
            self.push_hit(close, Action::CloseFontPicker);
        }

        let panel_theme = theme.on_ground(ui::chrome::panel_ground(theme));
        let list = layout.body;
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &list,
        );
        let style = panel_theme.text(TextRole::Body);
        let pad = f64::from(theme.tokens.spacing.s3);
        let mut fy = list.y0 - self.picker_scroll;
        for fam in &fams {
            if fy > list.y1 || fy + row_h < list.y0 {
                fy += row_h;
                continue;
            }
            let row = Rect::new(list.x0, fy, list.x1, fy + row_h);
            let hovered = row.contains(self.cursor) && list.contains(self.cursor);
            ui::chrome::paint_row_selection(
                ctx.scene,
                ctx.transform,
                row,
                SelectionState::new(false, hovered, false),
                panel_theme.tokens.colors.surfaces.content,
                &panel_theme,
            );
            // Each family previews itself: its name set in its own face.
            let face = style.clone().with_family(FontChoice::Named(fam.clone()));
            let (_, h) = ctx.text.measure_style(fam, None, &face);
            ctx.text(
                fam,
                &face,
                panel_theme.text_primary,
                Point::new(
                    row.x0 + pad,
                    ui::text_metrics::centered_text_top(row.y0, row_h, f64::from(h)),
                ),
                None,
            );
            self.push_hit(row.intersect(list), Action::AddSystemFont(fam.clone()));
            fy += row_h;
        }
        ctx.scene.pop_layer();
        self.picker_max_scroll = (wanted - list.height()).max(0.0);
        self.picker_scroll = self.picker_scroll.clamp(0.0, self.picker_max_scroll);
        paint_scroll_thumb(
            ctx,
            list,
            ScrollMetrics::new(wanted, list.height(), self.picker_scroll),
        );
    }
}
