//! The reader's quick display-settings surface.
//!
//! This component owns the popover's open state, numeric editors, geometry,
//! drawing, and modal hit stack. It emits typed effects; `BooksApp` remains
//! responsible for persisting those effects in the books document.

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::{Modifiers, WindowEvent};

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::input_controls::{NumberControls, NumberField};
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

/// Event-routing services shared by the quick settings' numeric editors.
/// The host constructs this once at its widget boundary so the component API
/// exposes one coherent interaction contract instead of mirroring every
/// argument required by [`NumberControls`].
pub(crate) struct ReadingSettingsEventCtx<'a> {
    pub event: &'a WindowEvent,
    pub scale_factor: f64,
    pub modifiers: Modifiers,
    pub state: &'a mut AutoCommit,
    pub root: &'a ObjId,
    pub node_id: NodeId,
    pub taffy: &'a TaffyTree<()>,
    pub handled: &'a mut bool,
}

/// Paint services supplied by the reader host for one settings frame.
pub(crate) struct ReadingSettingsPaintCtx<'a> {
    pub scene: &'a mut Scene,
    pub transform: Affine,
    pub text: &'a mut TextRenderer,
    pub theme: &'a Theme,
}

/// Reader state and available geometry consumed by the settings popover.
pub(crate) struct ReadingSettingsView<'a> {
    pub viewport: Rect,
    pub content_x0: f64,
    pub content_x1: f64,
    pub settings: &'a Settings,
    pub fonts: &'a [FontEntry],
    pub default_font_id: &'a str,
    pub schemes: &'a [ColorScheme],
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

#[derive(Clone, Copy)]
struct ControlRow {
    x0: f64,
    x1: f64,
    y: f64,
    height: f64,
    gap: f64,
}

impl ControlRow {
    fn at_y(self, y: f64) -> Self {
        Self { y, ..self }
    }
}

struct PillSpec<'a> {
    label: &'a str,
    active: bool,
    action: ReadingSettingsAction,
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
        ctx: &mut ReadingSettingsEventCtx<'_>,
    ) -> Vec<ReadingSettingsAction> {
        self.numbers
            .handle_event(
                ctx.event,
                ctx.scale_factor,
                ctx.modifiers,
                ctx.state,
                ctx.root,
                ctx.node_id,
                ctx.taffy,
                ctx.handled,
            )
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

    pub(crate) fn draw(
        &mut self,
        ctx: &mut ReadingSettingsPaintCtx<'_>,
        view: &ReadingSettingsView<'_>,
    ) {
        if !self.open {
            return;
        }

        // The page remains an undimmed live preview. The viewport hit below
        // is only a modal outside-click target; later panel hits win.
        self.push_hit(view.viewport, ReadingSettingsAction::Dismiss);
        let layout = Self::layout(ctx.text, view);
        self.draw_panel_background(ctx, layout.panel);

        let mut y = layout.panel.y0 + layout.pad;
        let header = ControlRow {
            x0: layout.ix0,
            x1: layout.ix1,
            y,
            height: layout.header_h,
            gap: 0.0,
        };
        y = self.draw_header(ctx, header);
        let stepper_row = ControlRow {
            y,
            height: layout.row_h,
            ..header
        };
        y = self.draw_steppers(ctx, stepper_row, view.settings);
        let pill_row = ControlRow {
            y,
            height: layout.pill_h,
            gap: layout.pill_gap,
            ..header
        };
        y = self.draw_layout_row(ctx, pill_row, view.settings);
        y = self.draw_composition_row(ctx, pill_row.at_y(y), view.settings);
        y = self.draw_font_row(ctx, pill_row.at_y(y), &layout.font_pills);
        self.draw_page_row(
            ctx,
            pill_row.at_y(y),
            &layout.scheme_pills,
            &layout.scheme_widths,
        );
    }

    /// Paint the popover's rounded panel background + border and register it
    /// as a modal "swallow" hit so a click inside the panel (but on no more
    /// specific control) doesn't fall through to the outside-dismiss hit.
    fn draw_panel_background(&mut self, ctx: &mut ReadingSettingsPaintCtx<'_>, panel: Rect) {
        let colors = &ctx.theme.tokens.colors;
        paint::fill_round(
            ctx.scene,
            ctx.transform,
            panel,
            f64::from(ctx.theme.tokens.radii.lg),
            colors.surfaces.raised,
        );
        paint::stroke_round(
            ctx.scene,
            ctx.transform,
            panel,
            f64::from(ctx.theme.tokens.radii.lg),
            colors.border_standard,
            1.0,
        );
        self.push_hit(panel, ReadingSettingsAction::Swallow);
    }

    fn layout(text: &mut TextRenderer, view: &ReadingSettingsView<'_>) -> Layout {
        let pad = 16.0;
        let row_h = 34.0;
        let pill_h = row_h - 4.0;
        let pill_gap = 8.0;
        let (px0, px1) = Self::panel_x_bounds(view.viewport, view.content_x0, view.content_x1);
        let inner_w = px1 - px0 - pad * 2.0;

        let (font_pills, font_widths) =
            Self::font_pills(text, view.fonts, view.settings, view.default_font_id);
        let (scheme_pills, scheme_widths) = Self::scheme_pills(text, view.schemes, view.settings);

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
        let py0 = view.viewport.y0 + TOP_BAR + 8.0;
        let py1 = (py0 + panel_h).min(view.viewport.y1 - 12.0);

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

    fn draw_header(&mut self, ctx: &mut ReadingSettingsPaintCtx<'_>, row: ControlRow) -> f64 {
        ctx.text.draw_text_weighted(
            ctx.scene,
            "Display",
            13.5,
            ctx.theme.tokens.colors.text_primary,
            ctx.transform * Affine::translate((row.x0, row.y + 4.0)),
            None,
            TextWeight::Semibold,
        );
        let size = 26.0;
        let rect = Rect::new(row.x1 - size, row.y - 2.0, row.x1, row.y - 2.0 + size);
        if rect.contains(self.cursor) {
            paint::fill_round(
                ctx.scene,
                ctx.transform,
                rect,
                f64::from(ctx.theme.tokens.radii.md),
                ctx.theme.bg_hover,
            );
        }
        paint::dots(
            ctx.scene,
            ctx.transform,
            rect,
            ctx.theme.tokens.colors.text_dim,
        );
        self.push_hit(rect, ReadingSettingsAction::OpenManager);
        row.y + row.height
    }

    fn draw_steppers(
        &mut self,
        ctx: &mut ReadingSettingsPaintCtx<'_>,
        row: ControlRow,
        settings: &Settings,
    ) -> f64 {
        self.stepper(
            ctx,
            row,
            "Text size",
            f64::from(settings.font_scale) * 100.0,
            &NumberField::FontScale,
        );
        let y = row.y + row.height + 10.0;
        self.stepper(
            ctx,
            row.at_y(y),
            "Line spacing",
            f64::from(settings.line_spacing),
            &NumberField::LineSpacing,
        );
        y + row.height + 22.0
    }

    fn stepper(
        &mut self,
        ctx: &mut ReadingSettingsPaintCtx<'_>,
        row: ControlRow,
        label: &str,
        value: f64,
        field: &NumberField,
    ) {
        ctx.text.draw_text_weighted(
            ctx.scene,
            label,
            13.0,
            ctx.theme.tokens.colors.text_primary,
            ctx.transform * Affine::translate((row.x0, row.y + (row.height - 13.0) / 2.0)),
            None,
            TextWeight::Medium,
        );
        let width = (row.height * 2.0 + 62.0).min((row.x1 - row.x0) * 0.58);
        self.numbers.draw(
            field,
            value,
            Rect::new(row.x1 - width, row.y, row.x1, row.y + row.height),
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
        );
    }

    fn draw_layout_row(
        &mut self,
        ctx: &mut ReadingSettingsPaintCtx<'_>,
        row: ControlRow,
        settings: &Settings,
    ) -> f64 {
        Self::section_label(ctx, Point::new(row.x0, row.y), "LAYOUT");
        let mut y = row.y + 16.0;
        let mut x = row.x0;
        for mode in ReadMode::ALL {
            let width = Self::pill_width(ctx.text, mode.label(), 12.5);
            if x + width > row.x1 && x > row.x0 {
                x = row.x0;
                y += row.height + row.gap;
            }
            let active = settings.mode == mode;
            self.pill(
                ctx,
                x,
                y,
                row.height,
                PillSpec {
                    label: mode.label(),
                    active,
                    action: if active {
                        ReadingSettingsAction::Swallow
                    } else {
                        ReadingSettingsAction::SetMode(mode)
                    },
                },
            );
            x += width + row.gap;
        }
        y + row.height + 22.0
    }

    fn draw_composition_row(
        &mut self,
        ctx: &mut ReadingSettingsPaintCtx<'_>,
        row: ControlRow,
        settings: &Settings,
    ) -> f64 {
        Self::section_label(ctx, Point::new(row.x0, row.y), "COMPOSITION");
        let y = row.y + 16.0;
        let mut x = row.x0;
        for composition in CompositionStyle::ALL {
            let active = settings.composition == composition;
            let width = self.pill(
                ctx,
                x,
                y,
                row.height,
                PillSpec {
                    label: composition.label(),
                    active,
                    action: if active {
                        ReadingSettingsAction::Swallow
                    } else {
                        ReadingSettingsAction::SetComposition(composition)
                    },
                },
            );
            x += width + row.gap;
        }
        y + row.height + 22.0
    }

    fn draw_font_row(
        &mut self,
        ctx: &mut ReadingSettingsPaintCtx<'_>,
        row: ControlRow,
        pills: &[(String, String, f64, bool)],
    ) -> f64 {
        Self::section_label(ctx, Point::new(row.x0, row.y), "TYPEFACE");
        let mut y = row.y + 16.0;
        let mut x = row.x0;
        for (id, label, width, active) in pills {
            if x + width > row.x1 && x > row.x0 {
                x = row.x0;
                y += row.height + row.gap;
            }
            self.pill(
                ctx,
                x,
                y,
                row.height,
                PillSpec {
                    label,
                    active: *active,
                    action: if *active {
                        ReadingSettingsAction::Swallow
                    } else {
                        ReadingSettingsAction::SetFont(id.clone())
                    },
                },
            );
            x += width + row.gap;
        }
        y + row.height + 22.0
    }

    fn draw_page_row(
        &mut self,
        ctx: &mut ReadingSettingsPaintCtx<'_>,
        row: ControlRow,
        pills: &[SchemePill],
        widths: &[f64],
    ) {
        Self::section_label(ctx, Point::new(row.x0, row.y), "PAGE");
        let mut y = row.y + 16.0;
        let mut x = row.x0;
        for (pill, width) in pills.iter().zip(widths) {
            if x + width > row.x1 && x > row.x0 {
                x = row.x0;
                y += row.height + row.gap;
            }
            self.pill(
                ctx,
                x,
                y,
                row.height,
                PillSpec {
                    label: &pill.label,
                    active: pill.active,
                    action: if pill.active {
                        ReadingSettingsAction::Swallow
                    } else {
                        pill.action.clone()
                    },
                },
            );
            x += width + row.gap;
        }
    }

    fn section_label(ctx: &mut ReadingSettingsPaintCtx<'_>, origin: Point, label: &str) {
        ctx.text.draw_text_weighted(
            ctx.scene,
            label,
            10.0,
            ctx.theme.tokens.colors.text_dim,
            ctx.transform * Affine::translate((origin.x, origin.y)),
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

    fn pill(
        &mut self,
        ctx: &mut ReadingSettingsPaintCtx<'_>,
        x: f64,
        y: f64,
        height: f64,
        spec: PillSpec<'_>,
    ) -> f64 {
        let width = Self::pill_width(ctx.text, spec.label, 12.5);
        let rect = Rect::new(x, y, x + width, y + height);
        let colors = &ctx.theme.tokens.colors;
        let background = if spec.active {
            ctx.theme.accent_primary
        } else if rect.contains(self.cursor) {
            ctx.theme.bg_hover
        } else {
            colors.surfaces.overlay
        };
        paint::fill_round(ctx.scene, ctx.transform, rect, height / 2.0, background);
        ctx.text.draw_text_weighted(
            ctx.scene,
            spec.label,
            12.5,
            if spec.active {
                Color::from_rgb8(248, 248, 252)
            } else {
                colors.text_primary
            },
            ctx.transform * Affine::translate((x + 12.0, y + (height - 12.5) / 2.0)),
            None,
            TextWeight::Medium,
        );
        self.push_hit(rect, spec.action);
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
