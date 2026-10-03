//! The reader's quick display-settings surface.
//!
//! This component owns the popover's open state, numeric editors, geometry,
//! drawing, and modal hit stack. It emits typed effects; `BooksApp` remains
//! responsible for persisting those effects in the books document.

use automerge::{AutoCommit, ObjId};
use vello::kurbo::{Point, Rect};

use ui::TextRole;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    Button, ButtonFace, ButtonRole, ChipFace, ChipLead, ControlSize, SegmentedFace, ToggleGroup,
};

use crate::input_controls::{InputEventCtx, NumberControls, NumberField};
use crate::model::{ColorScheme, CompositionStyle, FontEntry, ReadMode, ReadTheme, Settings};

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
    segment_h: f64,
    chip_h: f64,
    chip_gap: f64,
    /// Between the popover's groups.
    gap: f64,
    panel: Rect,
    ix0: f64,
    ix1: f64,
    font_pills: Vec<FontPill>,
    font_widths: Vec<f64>,
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

/// The reader's current display settings and the fonts and colour schemes
/// the panel offers to switch between.
pub(crate) struct Appearance<'a> {
    pub(crate) settings: &'a Settings,
    pub(crate) fonts: &'a [FontEntry],
    pub(crate) default_font_id: &'a str,
    pub(crate) schemes: &'a [ColorScheme],
}

/// One choice pill: its label, whether it is the current choice, and the
/// action it registers. The current choice swallows its click.
struct Choice<'a> {
    label: &'a str,
    active: bool,
    action: ReadingSettingsAction,
}

impl<'a> Choice<'a> {
    fn new(label: &'a str, active: bool, action: ReadingSettingsAction) -> Self {
        Self {
            label,
            active,
            action: if active {
                ReadingSettingsAction::Swallow
            } else {
                action
            },
        }
    }
}

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

    /// Where the popover sits: under its button, its trailing edge on the
    /// button's (or as far in as the window allows), a utility panel wide.
    pub(crate) fn panel_x_bounds(
        viewport: Rect,
        anchor: Rect,
        width: f64,
        margin: f64,
    ) -> (f64, f64) {
        let width = width.min(viewport.width() - 2.0 * margin).max(0.0);
        let x1 = anchor
            .x1
            .min(viewport.x1 - margin)
            .max(viewport.x0 + margin + width);
        (x1 - width, x1)
    }

    pub(crate) fn draw(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        viewport: Rect,
        anchor: Rect,
        appearance: &Appearance<'_>,
    ) {
        if !self.open {
            return;
        }
        let base = ctx.theme;
        let theme = base.on_ground(ui::chrome::panel_ground(base));
        let layout = Self::layout(ctx.text, &theme, viewport, anchor, appearance);
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
        ui::widget::floating_surface::paint(ctx.scene, ctx.transform, panel, base);
        self.push_hit(panel, ReadingSettingsAction::Swallow);

        let mut panel_ctx = PaintCtx::new(ctx.scene, ctx.transform, ctx.text, &theme);
        let ctx = &mut panel_ctx;
        let settings = appearance.settings;
        let mut y = panel.y0 + layout.pad;
        y = self.draw_steppers(ctx, &layout, y, settings);
        y = self.draw_segments(
            ctx,
            &layout,
            y,
            "Layout",
            &ReadMode::ALL
                .iter()
                .map(|mode| {
                    Choice::new(
                        mode.label(),
                        settings.mode == *mode,
                        ReadingSettingsAction::SetMode(*mode),
                    )
                })
                .collect::<Vec<_>>(),
        );
        y = self.draw_segments(
            ctx,
            &layout,
            y,
            "Text",
            &CompositionStyle::ALL
                .iter()
                .map(|style| {
                    Choice::new(
                        style.label(),
                        settings.composition == *style,
                        ReadingSettingsAction::SetComposition(*style),
                    )
                })
                .collect::<Vec<_>>(),
        );
        let fonts: Vec<Choice<'_>> = layout
            .font_pills
            .iter()
            .map(|(id, label, _, active)| {
                Choice::new(label, *active, ReadingSettingsAction::SetFont(id.clone()))
            })
            .collect();
        y = self.draw_chips(ctx, &layout, y, "Typeface", &fonts, &layout.font_widths);
        let schemes: Vec<Choice<'_>> = layout
            .scheme_pills
            .iter()
            .map(|pill| Choice::new(&pill.label, pill.active, pill.action.clone()))
            .collect();
        y = self.draw_chips(ctx, &layout, y, "Page", &schemes, &layout.scheme_widths);
        self.draw_manage(ctx, &layout, y);
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "row counts of a short popover are far below f64's exact-integer range"
    )]
    fn layout(
        text: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
        anchor: Rect,
        appearance: &Appearance<'_>,
    ) -> Layout {
        let &Appearance {
            settings,
            fonts,
            default_font_id,
            schemes,
        } = appearance;
        let spacing = &theme.tokens.spacing;
        let rhythm = &spacing.rhythm;
        let pad = f64::from(rhythm.section);
        let margin = f64::from(spacing.s3);
        let (px0, px1) = Self::panel_x_bounds(
            viewport,
            anchor,
            f64::from(theme.tokens.metrics.panel_width_utility),
            margin,
        );
        let inner_w = px1 - px0 - pad * 2.0;
        let chip_gap = f64::from(rhythm.related);

        let (font_pills, font_widths) =
            Self::font_pills(text, theme, fonts, settings, default_font_id);
        let (scheme_pills, scheme_widths) = Self::scheme_pills(text, theme, schemes, settings);

        let label_h = f64::from(
            text.measure_style("Ag", None, &theme.text(TextRole::Subheading))
                .1,
        );
        let row_h = ControlSize::Small.height(theme).max(label_h);
        let segment_h = SegmentedFace::height(ControlSize::Small, theme);
        let chip_h = ChipFace::height(theme);
        let section = label_h + f64::from(rhythm.related);
        let gap = f64::from(rhythm.sibling);
        let font_rows = wrap_row_count(&font_widths, inner_w, chip_gap) as f64;
        let scheme_rows = wrap_row_count(&scheme_widths, inner_w, chip_gap) as f64;
        let manage_h = ControlSize::Small.height(theme);
        let panel_h = pad * 2.0
            + 2.0 * row_h
            + f64::from(rhythm.related)
            + gap
            + 2.0 * (section + segment_h + gap)
            + section
            + font_rows * chip_h
            + (font_rows - 1.0).max(0.0) * chip_gap
            + gap
            + section
            + scheme_rows * chip_h
            + (scheme_rows - 1.0).max(0.0) * chip_gap
            + gap
            + manage_h;
        let py0 = anchor.y1 + f64::from(spacing.s1);
        let py1 = (py0 + panel_h).min(viewport.y1 - margin);

        Layout {
            pad,
            row_h,
            segment_h,
            chip_h,
            chip_gap,
            gap,
            panel: Rect::new(px0, py0, px1, py1),
            ix0: px0 + pad,
            ix1: px1 - pad,
            font_pills,
            font_widths,
            scheme_pills,
            scheme_widths,
        }
    }

    /// The typeface choices: one per shown font (every registered font when
    /// none is shown), each labelled with its name and flagged against the
    /// resolved current font. Returns the choices and their chip widths.
    fn font_pills(
        text: &mut TextRenderer,
        theme: &Theme,
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
                let width =
                    ChipFace::intrinsic_width(&font.name, ChipLead::None, false, text, theme);
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

    /// The page choices: one per enabled scheme, resolving built-ins to the
    /// matching [`ReadTheme`] action and custom schemes to a scheme-id
    /// action, flagged against the current settings.
    fn scheme_pills(
        text: &mut TextRenderer,
        theme: &Theme,
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
            .map(|pill| ChipFace::intrinsic_width(&pill.label, ChipLead::None, false, text, theme))
            .collect();
        (scheme_pills, scheme_widths)
    }

    /// The text-size and line-spacing steppers, one labelled row each.
    fn draw_steppers(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        layout: &Layout,
        mut y: f64,
        settings: &Settings,
    ) -> f64 {
        let theme = ctx.theme;
        let (x0, x1, height) = (layout.ix0, layout.ix1, layout.row_h);
        let rows = [
            (
                "Text size",
                f64::from(settings.font_scale) * 100.0,
                NumberField::FontScale,
            ),
            (
                "Line spacing",
                f64::from(settings.line_spacing),
                NumberField::LineSpacing,
            ),
        ];
        let control_h = ControlSize::Small.height(theme);
        for (i, (label, value, field)) in rows.into_iter().enumerate() {
            if i > 0 {
                y += height + f64::from(theme.tokens.spacing.rhythm.related);
            }
            let band = Rect::new(x0, y, x1, y + height);
            ctx.text_role_in_band(label, TextRole::Label, theme.text_primary, x0, band, None);
            let width = self
                .numbers
                .preferred_width(&field, value, ctx.text, theme)
                .min((x1 - x0) * 0.6);
            let cy = band.center().y;
            self.numbers.draw(
                &field,
                value,
                Rect::new(x1 - width, cy - control_h / 2.0, x1, cy + control_h / 2.0),
                ctx.scene,
                ctx.transform,
                ctx.text,
                theme,
            );
        }
        y + height + layout.gap
    }

    /// A labelled segmented choice spanning the panel's width.
    fn draw_segments(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        layout: &Layout,
        mut y: f64,
        title: &str,
        choices: &[Choice<'_>],
    ) -> f64 {
        let theme = ctx.theme;
        y = Self::section_label(ctx, Point::new(layout.ix0, y), title);
        let count = choices.len().max(1);
        let width = (layout.ix1 - layout.ix0) / count as f64;
        let segments: Vec<Rect> = (0..choices.len())
            .map(|i| {
                let x = layout.ix0 + i as f64 * width;
                Rect::new(x, y, x + width, y + layout.segment_h)
            })
            .collect();
        let labels: Vec<&str> = choices.iter().map(|choice| choice.label).collect();
        let selected = choices
            .iter()
            .position(|choice| choice.active)
            .unwrap_or(choices.len());
        let hovered = segments.iter().position(|rect| rect.contains(self.cursor));
        ToggleGroup::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &SegmentedFace::new(&segments, &labels, selected)
                .control_size(ControlSize::Small)
                .hovered(hovered),
        );
        for (choice, rect) in choices.iter().zip(&segments) {
            self.push_hit(*rect, choice.action.clone());
        }
        y + layout.segment_h + layout.gap
    }

    /// A labelled set of choice chips that wrap to the panel's width.
    fn draw_chips(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        layout: &Layout,
        mut y: f64,
        title: &str,
        choices: &[Choice<'_>],
        widths: &[f64],
    ) -> f64 {
        let theme = ctx.theme;
        y = Self::section_label(ctx, Point::new(layout.ix0, y), title);
        let mut x = layout.ix0;
        for (choice, width) in choices.iter().zip(widths) {
            if x + width > layout.ix1 && x > layout.ix0 {
                x = layout.ix0;
                y += layout.chip_h + layout.chip_gap;
            }
            let rect = Rect::new(x, y, (x + width).min(layout.ix1), y + layout.chip_h);
            ChipFace::new(rect, choice.label)
                .selected(choice.active)
                .hovered(rect.contains(self.cursor))
                .paint(ctx.scene, ctx.transform, ctx.text, theme);
            self.push_hit(rect, choice.action.clone());
            x += width + layout.chip_gap;
        }
        y + layout.chip_h + layout.gap
    }

    /// "Fonts and schemes", which opens their managers in Settings.
    fn draw_manage(&mut self, ctx: &mut PaintCtx<'_>, layout: &Layout, y: f64) {
        let theme = ctx.theme;
        let label = "Fonts and schemes";
        let size = ControlSize::Small;
        let width = ButtonFace::new(Rect::ZERO, label)
            .icon(Icon::Settings)
            .control_size(size)
            .intrinsic_width(ctx.text, theme);
        let rect = Rect::new(layout.ix0, y, layout.ix0 + width, y + size.height(theme));
        let face = ButtonFace::new(rect, label)
            .icon(Icon::Settings)
            .control_size(size)
            .role(ButtonRole::Plain)
            .hovered(rect.contains(self.cursor));
        Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face);
        self.push_hit(face.hit_rect(theme), ReadingSettingsAction::OpenManager);
    }

    /// A group label inside the popover. Returns the y below it.
    fn section_label(ctx: &mut PaintCtx<'_>, origin: Point, label: &str) -> f64 {
        let theme = ctx.theme;
        ctx.text_role(
            label,
            TextRole::Subheading,
            theme.text_primary,
            origin,
            None,
        );
        origin.y
            + ctx.line_box(TextRole::Subheading)
            + f64::from(theme.tokens.spacing.rhythm.related)
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

    /// The panel clamps between the rails, pill rows wrap greedily keeping empty sections, and the hit stack prefers controls.
    #[test]
    fn reading_settings_panel_layout_and_hit_stack() {
        // --- phase: panel_hangs_from_its_button_inside_the_window ---
        {
            let viewport = Rect::new(0.0, 0.0, 1280.0, 800.0);
            let button = Rect::new(900.0, 12.0, 936.0, 44.0);
            let (x0, x1) = ReadingSettings::panel_x_bounds(viewport, button, 320.0, 12.0);
            assert_eq!((x0, x1), (616.0, 936.0));
            let near_edge = Rect::new(100.0, 12.0, 136.0, 44.0);
            let (x0, _) = ReadingSettings::panel_x_bounds(viewport, near_edge, 320.0, 12.0);
            assert_eq!(
                x0, 12.0,
                "a button near the leading edge keeps the panel inside"
            );
        }

        // --- phase: pill_rows_wrap_greedily_and_keep_empty_sections_visible ---
        {
            assert_eq!(wrap_row_count(&[], 100.0, 8.0), 1);
            assert_eq!(wrap_row_count(&[40.0, 40.0], 100.0, 8.0), 1);
            assert_eq!(wrap_row_count(&[40.0, 40.0, 40.0], 100.0, 8.0), 2);
        }

        // --- phase: modal_hit_stack_prefers_controls_over_panel_and_outside_dismiss ---
        {
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
}
