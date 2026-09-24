//! The reader's quick display-settings surface.
//!
//! This component owns the popover's open state, numeric editors, geometry,
//! drawing, and modal hit stack. It emits typed effects; `BooksApp` remains
//! responsible for persisting those effects in the books document.

use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::input_controls::{InputEventCtx, NumberControls, NumberField};
use crate::model::{ColorScheme, CompositionStyle, FontEntry, ReadMode, ReadTheme, Settings};
use crate::paint;

const TOP_BAR: f64 = 54.0;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ReadingSettingsAction {
    Swallow,
    Dismiss,
    OpenManager,
    SetFontScale(f32),
    SetLineSpacing(f32),
    SetMode(ReadMode),
    SetComposition(CompositionStyle),
    SetFont(String),
    SetReadTheme(ReadTheme),
    SetScheme(String),
}

#[derive(Clone)]
struct Hit {
    rect: Rect,
    action: ReadingSettingsAction,
}

struct SchemePill {
    label: String,
    action: ReadingSettingsAction,
    active: bool,
}

struct Layout {
    pad: f64,
    row_h: f64,
    pill_h: f64,
    pill_gap: f64,
    header_h: f64,
    panel: Rect,
    ix0: f64,
    ix1: f64,
    font_pills: Vec<(String, String, f64, bool)>,
    scheme_pills: Vec<SchemePill>,
    scheme_widths: Vec<f64>,
}

#[derive(Default)]
pub(crate) struct ReadingSettings {
    open: bool,
    cursor: Point,
    hits: Vec<Hit>,
    numbers: NumberControls,
}

/// One font pill as laid out: `(id, label, width, selected)`. Named because
/// the four-tuple appears in both the layout's return type and the draw pass.
type FontPill = (String, String, f64, bool);

impl ReadingSettings {
    pub(crate) fn accessibility_nodes(
        &self,
        settings: &Settings,
        focused: Option<&str>,
    ) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let mut nodes = self
            .hits
            .iter()
            .filter_map(|hit| {
                let (target, name, role, checked) = match &hit.action {
                    ReadingSettingsAction::OpenManager => (
                        "books.reader.settings.manage".into(),
                        "Manage fonts and colour themes".into(),
                        Role::Button,
                        None,
                    ),
                    ReadingSettingsAction::Dismiss => (
                        "books.reader.settings.close".into(),
                        "Close reading appearance".into(),
                        Role::Button,
                        None,
                    ),
                    ReadingSettingsAction::SetMode(mode) => (
                        format!("books.reader.settings.mode:{}", mode.as_str()),
                        format!("{} reading mode", mode.label()),
                        Role::RadioButton,
                        Some(settings.mode == *mode),
                    ),
                    ReadingSettingsAction::SetComposition(style) => (
                        format!("books.reader.settings.composition:{}", style.as_str()),
                        format!("{} text composition", style.label()),
                        Role::RadioButton,
                        Some(settings.composition == *style),
                    ),
                    ReadingSettingsAction::SetFont(id) => (
                        format!("books.reader.settings.font:{id}"),
                        format!("Use {id} font"),
                        Role::RadioButton,
                        Some(settings.font_id == *id),
                    ),
                    ReadingSettingsAction::SetReadTheme(theme) => (
                        format!("books.reader.settings.theme:{}", theme.as_str()),
                        format!("{} reading theme", theme.label()),
                        Role::RadioButton,
                        Some(settings.theme == *theme && settings.scheme_id.is_empty()),
                    ),
                    ReadingSettingsAction::SetScheme(id) => (
                        format!("books.reader.settings.scheme:{id}"),
                        format!("Use {id} colour theme"),
                        Role::RadioButton,
                        Some(settings.scheme_id == *id),
                    ),
                    ReadingSettingsAction::Swallow
                    | ReadingSettingsAction::SetFontScale(_)
                    | ReadingSettingsAction::SetLineSpacing(_) => return None,
                };
                let mut state = AccessState::new()
                    .focusable(true)
                    .focused(focused == Some(target.as_str()));
                if let Some(checked) = checked {
                    state = state.checked(checked);
                }
                Some(
                    AccessNode::new(role)
                        .with_name(name)
                        .with_state(state)
                        .with_bounds(hit.rect)
                        .with_action_target(target),
                )
            })
            .collect::<Vec<_>>();
        // One node per action target: the outside-click dismiss is registered
        // as several bands around the panel, and the tree reports the last
        // (largest) one rather than four "Close" buttons.
        let mut seen = std::collections::HashSet::new();
        nodes.reverse();
        nodes.retain(|node| {
            node.action_target
                .clone()
                .is_none_or(|target| seen.insert(target))
        });
        nodes.reverse();
        nodes.extend(self.numbers.accessibility_nodes());
        nodes
    }

    pub(crate) fn perform_number_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AutoCommit,
        root: &ObjId,
    ) -> Option<ReadingSettingsAction> {
        let (field, value) = self
            .numbers
            .perform_semantic_action(target, action, state, root)?;
        let value = value?;
        match field {
            NumberField::FontScale => {
                Some(ReadingSettingsAction::SetFontScale((value / 100.0) as f32))
            }
            NumberField::LineSpacing => Some(ReadingSettingsAction::SetLineSpacing(value as f32)),
            NumberField::FontMult(_) => None,
        }
    }

    pub(crate) fn semantic_action(&self, target: &str) -> Option<ReadingSettingsAction> {
        self.hits.iter().rev().find_map(|hit| {
            let candidate = match &hit.action {
                ReadingSettingsAction::OpenManager => "books.reader.settings.manage".into(),
                ReadingSettingsAction::Dismiss => "books.reader.settings.close".into(),
                ReadingSettingsAction::SetMode(mode) => {
                    format!("books.reader.settings.mode:{}", mode.as_str())
                }
                ReadingSettingsAction::SetComposition(style) => {
                    format!("books.reader.settings.composition:{}", style.as_str())
                }
                ReadingSettingsAction::SetFont(id) => {
                    format!("books.reader.settings.font:{id}")
                }
                ReadingSettingsAction::SetReadTheme(theme) => {
                    format!("books.reader.settings.theme:{}", theme.as_str())
                }
                ReadingSettingsAction::SetScheme(id) => {
                    format!("books.reader.settings.scheme:{id}")
                }
                _ => return None,
            };
            (candidate == target).then(|| hit.action.clone())
        })
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn open(&mut self) {
        self.open = true;
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
        self.hits.clear();
        self.numbers.begin_frame();
    }

    pub(crate) fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open();
        }
    }

    pub(crate) fn set_cursor(&mut self, cursor: Point) {
        self.cursor = cursor;
    }

    pub(crate) fn begin_frame(&mut self) {
        self.hits.clear();
        self.numbers.begin_frame();
    }

    pub(crate) fn hit_test(&self, point: Point) -> Option<ReadingSettingsAction> {
        self.hits
            .iter()
            .rev()
            .find(|hit| hit.rect.contains(point))
            .map(|hit| hit.action.clone())
    }

    pub(crate) fn handle_event(
        &mut self,
        ctx: &mut InputEventCtx<'_>,
    ) -> Vec<ReadingSettingsAction> {
        self.numbers
            .handle_event(ctx)
            .into_iter()
            .filter_map(|(field, value)| match field {
                NumberField::FontScale => {
                    Some(ReadingSettingsAction::SetFontScale((value / 100.0) as f32))
                }
                NumberField::LineSpacing => {
                    Some(ReadingSettingsAction::SetLineSpacing(value as f32))
                }
                NumberField::FontMult(_) => None,
            })
            .collect()
    }

    /// Horizontal panel bounds inside the reader area. The host passes the
    /// already-resolved left/right edges of any docked rails.
    pub(crate) fn panel_x_bounds(viewport: Rect, content_x0: f64, content_x1: f64) -> (f64, f64) {
        let panel_w = 320.0_f64.min(viewport.width() - 24.0);
        let px1 = content_x1;
        ((px1 - panel_w).max(content_x0), px1)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Reading settings paints from viewport and column bounds, current settings, available fonts, default font ID, and color schemes."
    )]
    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
        content_x0: f64,
        content_x1: f64,
        settings: &Settings,
        fonts: &[FontEntry],
        default_font_id: &str,
        schemes: &[ColorScheme],
    ) {
        if !self.open {
            return;
        }

        let layout = Self::layout(
            text,
            viewport,
            content_x0,
            content_x1,
            settings,
            fonts,
            default_font_id,
            schemes,
        );
        // The page remains an undimmed live preview. The outside-click
        // dismiss target is the viewport *around* the panel, registered as
        // the four bands beside it rather than one rect under it, so its
        // semantic bounds never overlap the panel's own controls. The band
        // pushed last is the one the accessibility tree reports.
        let panel = layout.panel;
        for band in [
            Rect::new(viewport.x0, viewport.y0, viewport.x1, panel.y0),
            Rect::new(viewport.x0, panel.y1, viewport.x1, viewport.y1),
            Rect::new(panel.x1, panel.y0, viewport.x1, panel.y1),
            Rect::new(viewport.x0, panel.y0, panel.x0, panel.y1),
        ] {
            if band.width() > 0.0 && band.height() > 0.0 {
                self.push_hit(band, ReadingSettingsAction::Dismiss);
            }
        }
        self.draw_panel_background(scene, transform, theme, layout.panel);

        let mut y = layout.panel.y0 + layout.pad;
        y = self.draw_header(
            scene,
            transform,
            text,
            theme,
            layout.ix0,
            layout.ix1,
            y,
            layout.header_h,
        );
        y = self.draw_steppers(
            scene,
            transform,
            text,
            theme,
            layout.ix0,
            layout.ix1,
            y,
            layout.row_h,
            settings,
        );
        y = self.draw_layout_row(
            scene,
            transform,
            text,
            theme,
            layout.ix0,
            layout.ix1,
            y,
            layout.pill_h,
            layout.pill_gap,
            settings,
        );
        y = self.draw_composition_row(
            scene,
            transform,
            text,
            theme,
            layout.ix0,
            y,
            layout.pill_h,
            layout.pill_gap,
            settings,
        );
        y = self.draw_font_row(
            scene,
            transform,
            text,
            theme,
            layout.ix0,
            layout.ix1,
            y,
            layout.pill_h,
            layout.pill_gap,
            &layout.font_pills,
        );
        self.draw_page_row(
            scene,
            transform,
            text,
            theme,
            layout.ix0,
            layout.ix1,
            y,
            layout.pill_h,
            layout.pill_gap,
            &layout.scheme_pills,
            &layout.scheme_widths,
        );
    }

    /// Paint the popover's rounded panel background + border and register it
    /// as a modal "swallow" hit so a click inside the panel (but on no more
    /// specific control) doesn't fall through to the outside-dismiss hit.
    fn draw_panel_background(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        panel: Rect,
    ) {
        // A floating Panel: elevation, ground and the derived Panel border.
        crate::chrome::panel(scene, transform, theme, panel);
        self.push_hit(panel, ReadingSettingsAction::Swallow);
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Panel layout measures font and scheme choices against the viewport and reading-column bounds using the current settings."
    )]
    fn layout(
        text: &mut TextRenderer,
        viewport: Rect,
        content_x0: f64,
        content_x1: f64,
        settings: &Settings,
        fonts: &[FontEntry],
        default_font_id: &str,
        schemes: &[ColorScheme],
    ) -> Layout {
        let pad = 16.0;
        let row_h = 34.0;
        let pill_h = row_h - 4.0;
        let pill_gap = 8.0;
        let (px0, px1) = Self::panel_x_bounds(viewport, content_x0, content_x1);
        let inner_w = px1 - px0 - pad * 2.0;

        let (font_pills, font_widths) = Self::font_pills(text, fonts, settings, default_font_id);
        let (scheme_pills, scheme_widths) = Self::scheme_pills(text, schemes, settings);

        let font_rows = wrap_row_count(&font_widths, inner_w, pill_gap);
        let scheme_rows = wrap_row_count(&scheme_widths, inner_w, pill_gap);
        let header_h = 30.0;
        let steppers_h = row_h + 10.0 + row_h;
        let layout_h = 16.0 + pill_h;
        let composition_h = 16.0 + pill_h;
        let type_h =
            16.0 + font_rows as f64 * pill_h + font_rows.saturating_sub(1) as f64 * pill_gap;
        let page_h =
            16.0 + scheme_rows as f64 * pill_h + scheme_rows.saturating_sub(1) as f64 * pill_gap;
        let panel_h = pad * 2.0
            + header_h
            + steppers_h
            + 22.0
            + layout_h
            + 22.0
            + composition_h
            + 22.0
            + type_h
            + 22.0
            + page_h;
        let py0 = viewport.y0 + TOP_BAR + 8.0;
        let py1 = (py0 + panel_h).min(viewport.y1 - 12.0);

        Layout {
            pad,
            row_h,
            pill_h,
            pill_gap,
            header_h,
            panel: Rect::new(px0, py0, px1, py1),
            ix0: px0 + pad,
            ix1: px1 - pad,
            font_pills,
            scheme_pills,
            scheme_widths,
        }
    }

    /// The typeface quick-picker pills: one per shown font (falling back to
    /// every registered font if none are marked `shown`), each labelled with
    /// its display name and flagged active against the resolved current font.
    fn font_pills(
        text: &mut TextRenderer,
        fonts: &[FontEntry],
        settings: &Settings,
        default_font_id: &str,
    ) -> (Vec<FontPill>, Vec<f64>) {
        let shown: Vec<&FontEntry> = {
            let enabled: Vec<_> = fonts.iter().filter(|font| font.shown).collect();
            if enabled.is_empty() {
                fonts.iter().collect()
            } else {
                enabled
            }
        };
        let wanted = if settings.font_id.is_empty() {
            default_font_id
        } else {
            settings.font_id.as_str()
        };
        let active_id = fonts
            .iter()
            .find(|font| font.id == wanted)
            .or_else(|| fonts.iter().find(|font| font.id == "serif"))
            .or_else(|| fonts.first())
            .map_or("", |font| font.id.as_str());
        let font_pills: Vec<_> = shown
            .into_iter()
            .map(|font| {
                let width = f64::from(
                    text.measure_weighted(&font.name, 12.5, None, TextWeight::Medium)
                        .0,
                ) + 24.0;
                (
                    font.id.clone(),
                    font.name.clone(),
                    width,
                    font.id == active_id,
                )
            })
            .collect();
        let font_widths: Vec<_> = font_pills.iter().map(|pill| pill.2).collect();
        (font_pills, font_widths)
    }

    /// The colour-scheme quick-picker pills: one per enabled scheme, resolving
    /// built-ins to the matching [`ReadTheme`] action and custom schemes to a
    /// direct scheme-id action, flagged active against the current settings.
    fn scheme_pills(
        text: &mut TextRenderer,
        schemes: &[ColorScheme],
        settings: &Settings,
    ) -> (Vec<SchemePill>, Vec<f64>) {
        let scheme_pills: Vec<_> = schemes
            .iter()
            .filter(|scheme| scheme.enabled)
            .map(|scheme| {
                if scheme.builtin {
                    let read_theme = ReadTheme::from_str(&scheme.id);
                    SchemePill {
                        label: scheme.name.clone(),
                        action: ReadingSettingsAction::SetReadTheme(read_theme),
                        active: settings.scheme_id.is_empty() && settings.theme == read_theme,
                    }
                } else {
                    SchemePill {
                        label: scheme.name.clone(),
                        action: ReadingSettingsAction::SetScheme(scheme.id.clone()),
                        active: settings.scheme_id == scheme.id,
                    }
                }
            })
            .collect();
        let scheme_widths: Vec<_> = scheme_pills
            .iter()
            .map(|pill| {
                f64::from(
                    text.measure_weighted(&pill.label, 12.5, None, TextWeight::Medium)
                        .0,
                ) + 24.0
            })
            .collect();
        (scheme_pills, scheme_widths)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "The settings header painter takes render context and explicit horizontal bounds, vertical origin, and row height."
    )]
    fn draw_header(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        x0: f64,
        x1: f64,
        y: f64,
        height: f64,
    ) -> f64 {
        text.draw_text_weighted(
            scene,
            "Display",
            13.5,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((x0, y + 4.0)),
            None,
            TextWeight::Semibold,
        );
        let size = 26.0;
        let rect = Rect::new(x1 - size, y - 2.0, x1, y - 2.0 + size);
        if rect.contains(self.cursor) {
            paint::fill_round(
                scene,
                transform,
                rect,
                f64::from(theme.tokens.radii.md),
                theme.bg_hover,
            );
        }
        paint::dots(scene, transform, rect, theme.tokens.colors.text_dim);
        self.push_hit(rect, ReadingSettingsAction::OpenManager);
        y + height
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "The text-size and line-height steppers share render context, row bounds, spacing, and the current reading settings."
    )]
    fn draw_steppers(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        x0: f64,
        x1: f64,
        y: f64,
        row_h: f64,
        settings: &Settings,
    ) -> f64 {
        self.stepper(
            scene,
            transform,
            text,
            theme,
            x0,
            x1,
            y,
            row_h,
            "Text size",
            f64::from(settings.font_scale) * 100.0,
            &NumberField::FontScale,
        );
        let y = y + row_h + 10.0;
        self.stepper(
            scene,
            transform,
            text,
            theme,
            x0,
            x1,
            y,
            row_h,
            "Line spacing",
            f64::from(settings.line_spacing),
            &NumberField::LineSpacing,
        );
        y + row_h + 22.0
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Each reading stepper needs render context, row geometry, label, value, and field identity for the shared numeric control."
    )]
    fn stepper(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        x0: f64,
        x1: f64,
        y: f64,
        height: f64,
        label: &str,
        value: f64,
        field: &NumberField,
    ) {
        text.draw_text_weighted(
            scene,
            label,
            13.0,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((x0, y + (height - 13.0) / 2.0)),
            None,
            TextWeight::Medium,
        );
        let width = (height * 2.0 + 62.0).min((x1 - x0) * 0.58);
        self.numbers.draw(
            field,
            value,
            Rect::new(x1 - width, y, x1, y + height),
            scene,
            transform,
            text,
            theme,
        );
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Layout choices wrap within explicit row bounds and spacing while reflecting the current reading mode."
    )]
    fn draw_layout_row(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        x0: f64,
        x1: f64,
        mut y: f64,
        height: f64,
        gap: f64,
        settings: &Settings,
    ) -> f64 {
        Self::section_label(scene, transform, text, theme, x0, y, "LAYOUT");
        y += 16.0;
        let mut x = x0;
        for mode in ReadMode::ALL {
            let width = Self::pill_width(text, mode.label(), 12.5);
            if x + width > x1 && x > x0 {
                x = x0;
                y += height + gap;
            }
            let active = settings.mode == mode;
            self.pill(
                scene,
                transform,
                text,
                theme,
                x,
                y,
                height,
                mode.label(),
                active,
                if active {
                    ReadingSettingsAction::Swallow
                } else {
                    ReadingSettingsAction::SetMode(mode)
                },
            );
            x += width + gap;
        }
        y + height + 22.0
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Composition choices use render context, row origin, height and gap, and the selected composition from reading settings."
    )]
    fn draw_composition_row(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        x0: f64,
        mut y: f64,
        height: f64,
        gap: f64,
        settings: &Settings,
    ) -> f64 {
        Self::section_label(scene, transform, text, theme, x0, y, "COMPOSITION");
        y += 16.0;
        let mut x = x0;
        for composition in CompositionStyle::ALL {
            let active = settings.composition == composition;
            let width = self.pill(
                scene,
                transform,
                text,
                theme,
                x,
                y,
                height,
                composition.label(),
                active,
                if active {
                    ReadingSettingsAction::Swallow
                } else {
                    ReadingSettingsAction::SetComposition(composition)
                },
            );
            x += width + gap;
        }
        y + height + 22.0
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Typeface pills use render context, wrapping bounds, row height and gap, and premeasured labels with selection state."
    )]
    fn draw_font_row(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        x0: f64,
        x1: f64,
        mut y: f64,
        height: f64,
        gap: f64,
        pills: &[(String, String, f64, bool)],
    ) -> f64 {
        Self::section_label(scene, transform, text, theme, x0, y, "TYPEFACE");
        y += 16.0;
        let mut x = x0;
        for (id, label, width, active) in pills {
            if x + width > x1 && x > x0 {
                x = x0;
                y += height + gap;
            }
            self.pill(
                scene,
                transform,
                text,
                theme,
                x,
                y,
                height,
                label,
                *active,
                if *active {
                    ReadingSettingsAction::Swallow
                } else {
                    ReadingSettingsAction::SetFont(id.clone())
                },
            );
            x += width + gap;
        }
        y + height + 22.0
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Page-scheme pills use render context, wrapping bounds and spacing, and separately measured scheme labels and widths."
    )]
    fn draw_page_row(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        x0: f64,
        x1: f64,
        mut y: f64,
        height: f64,
        gap: f64,
        pills: &[SchemePill],
        widths: &[f64],
    ) {
        Self::section_label(scene, transform, text, theme, x0, y, "PAGE");
        y += 16.0;
        let mut x = x0;
        for (pill, width) in pills.iter().zip(widths) {
            if x + width > x1 && x > x0 {
                x = x0;
                y += height + gap;
            }
            self.pill(
                scene,
                transform,
                text,
                theme,
                x,
                y,
                height,
                &pill.label,
                pill.active,
                if pill.active {
                    ReadingSettingsAction::Swallow
                } else {
                    pill.action.clone()
                },
            );
            x += width + gap;
        }
    }

    fn section_label(
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        y: f64,
        label: &str,
    ) {
        text.draw_text_weighted(
            scene,
            label,
            10.0,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((x, y)),
            None,
            TextWeight::Semibold,
        );
    }

    fn pill_width(text: &mut TextRenderer, label: &str, size: f32) -> f64 {
        f64::from(
            text.measure_weighted(label, size, None, TextWeight::Medium)
                .0,
        ) + 24.0
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Each settings pill paints from render context, position and height, label and selection, then records its specific action."
    )]
    fn pill(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        y: f64,
        height: f64,
        label: &str,
        active: bool,
        action: ReadingSettingsAction,
    ) -> f64 {
        let width = Self::pill_width(text, label, 12.5);
        let rect = Rect::new(x, y, x + width, y + height);
        let colors = &theme.tokens.colors;
        // Chip quiet (HIG §9.5): a raised face with its derived edge at rest;
        // the chosen chip is the accent fill with no edge, inked for contrast.
        if active {
            paint::fill_round(scene, transform, rect, height / 2.0, theme.accent_primary);
        } else {
            crate::chrome::quiet_pill(
                scene,
                transform,
                theme,
                rect,
                height / 2.0,
                colors.surfaces.raised,
                rect.contains(self.cursor),
            );
        }
        let (_, label_h) = text.measure_weighted(label, 12.5, None, TextWeight::Medium);
        text.draw_text_weighted(
            scene,
            label,
            12.5,
            if active {
                theme.on_surface(theme.accent_primary).ink
            } else {
                colors.text_primary
            },
            transform * Affine::translate((x + 12.0, y + (height - f64::from(label_h)) / 2.0)),
            None,
            TextWeight::Medium,
        );
        self.push_hit(rect, action);
        width
    }

    fn push_hit(&mut self, rect: Rect, action: ReadingSettingsAction) {
        self.hits.push(Hit { rect, action });
    }

    #[cfg(test)]
    pub(crate) fn number_rect(&self, field: &NumberField) -> Option<Rect> {
        self.numbers.rect(field)
    }

    #[cfg(test)]
    pub(crate) fn action_rect(
        &self,
        predicate: impl Fn(&ReadingSettingsAction) -> bool,
    ) -> Option<Rect> {
        self.hits
            .iter()
            .find(|hit| predicate(&hit.action))
            .map(|hit| hit.rect)
    }
}

fn wrap_row_count(widths: &[f64], max_width: f64, gap: f64) -> usize {
    if widths.is_empty() {
        return 1;
    }
    let mut rows = 1;
    let mut x = 0.0;
    for (index, width) in widths.iter().copied().enumerate() {
        let advance = if index == 0 { width } else { gap + width };
        if x + advance > max_width && x > 0.0 {
            rows += 1;
            x = width;
        } else {
            x += advance;
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_clamps_between_resolved_reader_rails() {
        let viewport = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let (x0, x1) = ReadingSettings::panel_x_bounds(viewport, 256.0, 936.0);
        assert_eq!((x0, x1), (616.0, 936.0));
    }

    #[test]
    fn pill_rows_wrap_greedily_and_keep_empty_sections_visible() {
        assert_eq!(wrap_row_count(&[], 100.0, 8.0), 1);
        assert_eq!(wrap_row_count(&[40.0, 40.0], 100.0, 8.0), 1);
        assert_eq!(wrap_row_count(&[40.0, 40.0, 40.0], 100.0, 8.0), 2);
    }

    #[test]
    fn modal_hit_stack_prefers_controls_over_panel_and_outside_dismiss() {
        let mut settings = ReadingSettings::default();
        let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
        let panel = Rect::new(400.0, 50.0, 700.0, 500.0);
        let control = Rect::new(500.0, 100.0, 600.0, 140.0);
        settings.push_hit(viewport, ReadingSettingsAction::Dismiss);
        settings.push_hit(panel, ReadingSettingsAction::Swallow);
        settings.push_hit(control, ReadingSettingsAction::OpenManager);
        assert_eq!(
            settings.hit_test(Point::new(550.0, 120.0)),
            Some(ReadingSettingsAction::OpenManager)
        );
        assert_eq!(
            settings.hit_test(Point::new(450.0, 200.0)),
            Some(ReadingSettingsAction::Swallow)
        );
        assert_eq!(
            settings.hit_test(Point::new(100.0, 200.0)),
            Some(ReadingSettingsAction::Dismiss)
        );
        settings.close();
        assert_eq!(
            settings.hit_test(Point::new(550.0, 120.0)),
            None,
            "closing clears stale modal geometry before another frame"
        );
    }
}
