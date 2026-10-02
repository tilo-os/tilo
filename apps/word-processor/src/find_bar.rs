//! Find & Replace chrome for the word processor.
//!
//! This component owns the query/mode state, match index, rendering, and the
//! hit map produced by rendering.  It deliberately emits document-edit intents
//! instead of editing qdoc itself: the parent editor remains the sole owner of
//! undoable document mutations.

use tilo_doc::snapshot::BlockView;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{EventCtx, TextInput, TextInputKind, Widget};

use crate::i18n::{arg, t, t_args};
use crate::num;
use crate::paint_context::PaintContext;
use crate::text_offsets::byte_to_char;

/// Classic 1× bar height, the floor of the measured [`FindMetrics::bar_h`].
pub(crate) const HEIGHT: f64 = 42.0;
/// Classic 1× field height / width, toggle and action widths and the
/// match-count slot: the floors the measured metrics grow from.
const FIELD_H: f64 = 26.0;
const FIELD_W: f64 = 188.0;
/// A field never shrinks below this, whatever the canvas width.
const FIELD_MIN_W: f64 = 72.0;
const TOGGLE_W: [f64; 3] = [30.0, 26.0, 30.0];
const ACTION_W: [f64; 5] = [28.0, 28.0, 72.0, 40.0, 28.0];
const COUNT_W: f64 = 78.0;
/// Seams on the base grid (HIG §7b): the three mode toggles read as one
/// segmented group (`s1`), the action buttons are siblings (`s2`); the 5 px
/// field radius.
const TOGGLE_SEAM: f64 = ui::tokens::Spacing::BASE.s1 as f64;
const ACTION_SEAM: f64 = ui::tokens::Spacing::BASE.s2 as f64;
const FIELD_RADIUS: f64 = 5.0;
/// Type roles (renderer input; scaled once): chip labels, field/action text.
const LABEL_FONT: f32 = 12.5;
const ACTION_FONT: f32 = 13.0;

/// Geometry of the bar for the renderer that paints it, refreshed by
/// [`FindBar::measure`] before the host reserves the band, so the reserved
/// height and the painted controls come from one measurement. Every slot is
/// the measured text plus a spacing token, floored at its classic 1× value.
#[derive(Clone, Copy)]
struct FindMetrics {
    bar_h: f64,
    /// 1 (classic) or 2: when the controls no longer fit one row of the bar
    /// (a 2× text scale, a narrow canvas) the replace field and the actions
    /// move to a second row rather than running off the window.
    rows: usize,
    field_h: f64,
    field_w: f64,
    lh_label: f64,
    toggle_w: [f64; 3],
    action_w: [f64; 5],
    count_w: f64,
    s2: f64,
    s3: f64,
}

impl Default for FindMetrics {
    fn default() -> Self {
        Self {
            bar_h: HEIGHT,
            rows: 1,
            field_h: FIELD_H,
            field_w: FIELD_W,
            lh_label: f64::from(LABEL_FONT) * 1.25,
            toggle_w: TOGGLE_W,
            action_w: ACTION_W,
            count_w: COUNT_W,
            s2: 8.0,
            s3: 12.0,
        }
    }
}

const TOGGLE_LABELS: [&str; 3] = ["Aa", "W", ".*"];

pub(crate) type FindMatch = (String, usize, usize);

#[derive(Clone, Copy)]
pub(crate) struct FindPalette {
    pub bar: Color,
    pub chip: Color,
    pub border: Color,
    pub text: Color,
    pub text_dim: Color,
    pub accent: Color,
    pub on_accent: Color,
    pub danger: Color,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FindEffect {
    None,
    SelectActive,
    ReplaceCurrent,
    ReplaceAll,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    FocusFind,
    FocusReplace,
    Prev,
    Next,
    Replace,
    ReplaceAll,
    Close,
    ToggleCase,
    ToggleWord,
    ToggleRegex,
}

/// The three search-mode toggles, grouped so `FindBar` itself doesn't carry
/// more than a handful of independent bools.
#[derive(Default, Clone, Copy)]
struct SearchOptions {
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
}

pub(crate) struct FindBar {
    open: bool,
    query: TextInput,
    replacement: TextInput,
    on_replace: bool,
    matches: Vec<FindMatch>,
    index: usize,
    buttons: Vec<(Action, Rect)>,
    options: SearchOptions,
    regex_error: bool,
    pointer: Point,
    metrics: FindMetrics,
}

impl Default for FindBar {
    fn default() -> Self {
        Self {
            open: false,
            query: TextInput::local()
                .with_kind(TextInputKind::Search)
                .with_id("word_processor_find")
                .with_font_size(13.0),
            replacement: TextInput::local()
                .with_kind(TextInputKind::Search)
                .with_id("word_processor_replace")
                .with_font_size(13.0),
            on_replace: false,
            matches: Vec::new(),
            index: 0,
            buttons: Vec::new(),
            options: SearchOptions::default(),
            regex_error: false,
            pointer: Point::ZERO,
            metrics: FindMetrics::default(),
        }
    }
}

/// The render arguments [`FindBar::draw`] threads through its per-section
/// helpers, so each one carries a handful of positions rather than the whole
/// argument list.
struct DrawCtx<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    tr: &'a mut TextRenderer,
    palette: FindPalette,
}

impl FindBar {
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn height(&self) -> f64 {
        if self.open { self.metrics.bar_h } else { 0.0 }
    }

    /// Resolve the bar's geometry against the renderer about to paint it and
    /// the `bar_w` it will span. The host calls this before asking for
    /// [`Self::height`], so the band it reserves and the controls painted
    /// into it agree at every text scale and width: when one row cannot hold
    /// every control, the fields shrink towards their classic width and, if
    /// that is still not enough, the bar takes a second row.
    pub(crate) fn measure(&mut self, tr: &mut TextRenderer, theme: &Theme, bar_w: f64) {
        let sp = &theme.tokens.spacing;
        let (s2, s3) = (f64::from(sp.s2), f64::from(sp.s3));
        let scale = f64::from(tr.text_scale());
        let lh_label = f64::from(tr.measure("Ag", LABEL_FONT, None).1);
        let lh_action = f64::from(tr.measure("Ag", ACTION_FONT, None).1);
        // The fields hold 13 px `TextInput`s: their line box plus `s2`.
        let field_h = (lh_action + s2).max(FIELD_H);
        let mut toggle_w = TOGGLE_W;
        for (w, label) in toggle_w.iter_mut().zip(TOGGLE_LABELS) {
            *w = (f64::from(tr.measure(label, LABEL_FONT, None).0) + 2.0 * s2).max(*w);
        }
        let mut action_w = ACTION_W;
        let actions = [
            "↑".to_string(),
            "↓".to_string(),
            t("find-replace"),
            t("find-replace-all"),
            "✕".to_string(),
        ];
        for (w, label) in action_w.iter_mut().zip(&actions) {
            *w = (f64::from(tr.measure(label, ACTION_FONT, None).0) + 2.0 * s2).max(*w);
        }
        // The count slot holds the widest readout it can show.
        let widest_count = [
            t("find-no-results"),
            t("find-bad-pattern"),
            t_args("find-count", &[arg("cur", 999_i64), arg("total", 999_i64)]),
        ]
        .iter()
        .map(|text| f64::from(tr.measure(text, LABEL_FONT, None).0))
        .fold(0.0, f64::max);
        let count_w = (widest_count + s2).max(COUNT_W);
        let toggle_row_w = toggle_w.iter().sum::<f64>() + 2.0 * TOGGLE_SEAM;
        let action_row_w = action_w.iter().sum::<f64>() + 4.0 * ACTION_SEAM;
        // One row: find field, toggles, replace field, count, actions.
        let field_w = FIELD_W * scale;
        let one_row =
            2.0 * s3 + field_w + s2 + toggle_row_w + s2 + field_w + s2 + count_w + action_row_w;
        let (rows, field_w) = if one_row <= bar_w {
            (1, field_w)
        } else {
            // Two rows: find field + toggles + count above, replace field +
            // actions below. Each field yields down to the classic width
            // (and beyond, on a very narrow canvas) so its row fits.
            let room_top = bar_w - 2.0 * s3 - s2 - toggle_row_w - s2 - count_w;
            let room_bottom = bar_w - 2.0 * s3 - s2 - action_row_w;
            (2, field_w.min(room_top).min(room_bottom).max(FIELD_MIN_W))
        };
        let bar_h = (rows as f64 * (field_h + s2) + s2).max(HEIGHT);
        self.metrics = FindMetrics {
            bar_h,
            rows,
            field_h,
            field_w,
            lh_label,
            toggle_w,
            action_w,
            count_w,
            s2,
            s3,
        };
    }

    /// The clickable rects of this frame's controls, in draw order.
    #[cfg(test)]
    pub(crate) fn button_rects(&self) -> Vec<Rect> {
        self.buttons.iter().map(|(_, rect)| *rect).collect()
    }

    pub(crate) fn matches(&self) -> &[FindMatch] {
        &self.matches
    }

    pub(crate) fn active_match(&self) -> Option<&FindMatch> {
        self.matches.get(self.index)
    }

    pub(crate) fn replacement(&self) -> &str {
        self.replacement.value()
    }

    /// The Find query as typed; with [`Self::matches`] it is the window's
    /// Find state in its participant view.
    pub(crate) fn query(&self) -> &str {
        self.query.value()
    }

    #[cfg(test)]
    pub(crate) fn set_queries_for_tests(&mut self, query: &str, replacement: &str) {
        self.query.set_text(query);
        self.replacement.set_text(replacement);
    }

    pub(crate) fn open(&mut self, seed: Option<&str>, blocks: &[BlockView]) -> FindEffect {
        self.open = true;
        self.on_replace = false;
        if let Some(seed) = seed.filter(|s| !s.is_empty() && !s.contains('\n')) {
            self.query.set_text(seed);
        }
        Widget::set_focus(&mut self.query, true);
        Widget::set_focus(&mut self.replacement, false);
        self.recompute(blocks)
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
        self.matches.clear();
        self.buttons.clear();
        Widget::set_focus(&mut self.query, false);
        Widget::set_focus(&mut self.replacement, false);
    }

    /// Re-run the current query after the document changes.
    pub(crate) fn refresh(&mut self, blocks: &[BlockView]) -> FindEffect {
        if self.open {
            self.recompute(blocks)
        } else {
            FindEffect::None
        }
    }

    pub(crate) fn click(&mut self, point: Point, blocks: &[BlockView]) -> FindEffect {
        let action = self
            .buttons
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(action, _)| *action);
        action.map_or(FindEffect::None, |action| self.apply(action, blocks))
    }

    /// Route raw input through the shared text fields. Search query state stays
    /// local, while the returned effect lets the editor keep document changes
    /// on its existing collaborative/undoable splice path.
    pub(crate) fn handle_event(
        &mut self,
        event: &WindowEvent,
        context: &mut EventCtx<'_>,
        blocks: &[BlockView],
    ) -> FindEffect {
        if !self.open {
            return FindEffect::None;
        }

        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Point::new(
                position.x / context.scale_factor,
                position.y / context.scale_factor,
            );
            // Each field must see cursor movement so hover/drag state stays
            // correct even when focus is about to move between them.
            for input in [&mut self.query, &mut self.replacement] {
                Widget::handle_event(input, event, context);
            }
            return FindEffect::None;
        }

        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            match logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.close();
                    *context.handled = true;
                    return FindEffect::None;
                }
                Key::Named(NamedKey::Enter) => {
                    *context.handled = true;
                    return self.jump(if context.modifiers.state().shift_key() {
                        -1
                    } else {
                        1
                    });
                }
                Key::Named(NamedKey::Tab) => {
                    self.focus_replace(!self.on_replace);
                    *context.handled = true;
                    return FindEffect::None;
                }
                // Ctrl+F remains the editor-level open/close shortcut. Other
                // Ctrl combinations (A/C/X/V and word navigation) belong to
                // the shared TextInput and are forwarded below.
                Key::Character(s)
                    if context.modifiers.state().control_key() && s.eq_ignore_ascii_case("f") =>
                {
                    return FindEffect::None;
                }
                _ => {}
            }
        }

        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
        {
            let point = self.pointer;
            if self.query.get_rect().contains(point) {
                self.focus_replace(false);
            } else if self.replacement.get_rect().contains(point) {
                self.focus_replace(true);
            }
        }

        let input = if self.on_replace {
            &mut self.replacement
        } else {
            &mut self.query
        };
        Widget::handle_event(input, event, context);

        let query_changed = self.query.take_dirty().is_some();
        let _ = self.replacement.take_dirty();
        if query_changed {
            self.recompute(blocks)
        } else {
            FindEffect::None
        }
    }

    fn focus_replace(&mut self, on_replace: bool) {
        self.on_replace = on_replace;
        Widget::set_focus(&mut self.query, !on_replace);
        Widget::set_focus(&mut self.replacement, on_replace);
    }

    pub(crate) fn set_host_focused(&mut self, focused: bool) {
        if focused && self.open {
            self.focus_replace(self.on_replace);
        } else {
            Widget::set_focus(&mut self.query, false);
            Widget::set_focus(&mut self.replacement, false);
        }
    }

    pub(crate) fn ime_cursor_rect(&self) -> Option<Rect> {
        if self.on_replace {
            Widget::ime_cursor_rect(&self.replacement)
        } else {
            Widget::ime_cursor_rect(&self.query)
        }
    }

    pub(crate) fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        if self.on_replace {
            Widget::ime_purpose(&self.replacement)
        } else {
            Widget::ime_purpose(&self.query)
        }
    }

    fn apply(&mut self, action: Action, blocks: &[BlockView]) -> FindEffect {
        match action {
            Action::FocusFind => {
                self.focus_replace(false);
                FindEffect::None
            }
            Action::FocusReplace => {
                self.focus_replace(true);
                FindEffect::None
            }
            Action::Prev => self.jump(-1),
            Action::Next => self.jump(1),
            Action::Replace => FindEffect::ReplaceCurrent,
            Action::ReplaceAll => FindEffect::ReplaceAll,
            Action::Close => {
                self.close();
                FindEffect::None
            }
            Action::ToggleCase => {
                self.options.case_sensitive = !self.options.case_sensitive;
                self.recompute(blocks)
            }
            Action::ToggleWord => {
                self.options.whole_word = !self.options.whole_word;
                self.recompute(blocks)
            }
            Action::ToggleRegex => {
                self.options.regex = !self.options.regex;
                self.recompute(blocks)
            }
        }
    }

    fn jump(&mut self, delta: i32) -> FindEffect {
        if self.matches.is_empty() {
            return FindEffect::None;
        }
        let count = num::i32_of_usize(self.matches.len());
        self.index =
            num::usize_of_i32(((num::i32_of_usize(self.index) + delta) % count + count) % count);
        FindEffect::SelectActive
    }

    fn recompute(&mut self, blocks: &[BlockView]) -> FindEffect {
        self.matches.clear();
        self.regex_error = false;
        if self.query.value().is_empty() {
            self.index = 0;
            return FindEffect::None;
        }
        if self.options.regex {
            self.recompute_regex(blocks);
        } else {
            self.recompute_literal(blocks);
        }
        if self.index >= self.matches.len() {
            self.index = 0;
        }
        if self.matches.is_empty() {
            FindEffect::None
        } else {
            FindEffect::SelectActive
        }
    }

    fn recompute_literal(&mut self, blocks: &[BlockView]) {
        let needle: Vec<char> = if self.options.case_sensitive {
            self.query.value().chars().collect()
        } else {
            self.query.value().to_lowercase().chars().collect()
        };
        if needle.is_empty() {
            return;
        }
        for block in blocks {
            let raw: Vec<char> = block.text.chars().collect();
            let haystack: Vec<char> = if self.options.case_sensitive {
                raw.clone()
            } else {
                block.text.to_lowercase().chars().collect()
            };
            if needle.len() > haystack.len() {
                continue;
            }
            let mut start = 0;
            while start + needle.len() <= haystack.len() {
                let end = start + needle.len();
                if haystack.get(start..end) == Some(&needle[..])
                    && (!self.options.whole_word || is_word_match(&raw, start, end))
                {
                    self.matches.push((block.id.clone(), start, end));
                    start = end;
                } else {
                    start += 1;
                }
            }
        }
    }

    fn recompute_regex(&mut self, blocks: &[BlockView]) {
        let pattern = if self.options.whole_word {
            format!(r"\b(?:{})\b", self.query.value())
        } else {
            self.query.value().to_string()
        };
        let Ok(regex) = regex::RegexBuilder::new(&pattern)
            .case_insensitive(!self.options.case_sensitive)
            .build()
        else {
            self.regex_error = true;
            return;
        };
        for block in blocks {
            for found in regex.find_iter(&block.text) {
                if found.start() != found.end() {
                    self.matches.push((
                        block.id.clone(),
                        byte_to_char(&block.text, found.start()),
                        byte_to_char(&block.text, found.end()),
                    ));
                }
            }
        }
    }

    pub(crate) fn draw(
        &mut self,
        context: PaintContext<'_>,
        theme: &Theme,
        presence: &PresenceCtx,
        palette: FindPalette,
        bar: Rect,
    ) {
        let PaintContext {
            scene,
            transform,
            text: tr,
        } = context;
        self.buttons.clear();
        scene.fill(Fill::NonZero, transform, palette.bar, None, &bar);
        scene.fill(
            Fill::NonZero,
            transform,
            palette.border,
            None,
            &Rect::new(bar.x0, bar.y1 - 1.0, bar.x1, bar.y1),
        );
        let m = self.metrics;
        // Rows are `field_h` bands `s2` apart, `s2` in from the bar's top.
        let row_cy = |row: f64| bar.y0 + m.s2 + m.field_h / 2.0 + row * (m.field_h + m.s2);
        let cy = row_cy(0.0);
        let field_y = cy - m.field_h / 2.0;
        let mut ctx = DrawCtx {
            scene,
            transform,
            tr,
            palette,
        };
        let mut x = bar.x0 + m.s3;
        x = self.draw_find_field(&mut ctx, theme, presence, field_y, x);
        x = self.draw_mode_toggles(&mut ctx, field_y, cy, x) + m.s2;
        if m.rows > 1 {
            // The count closes the first row; the replace field and the
            // actions take the second, from the bar's left inset.
            self.draw_match_count(&mut ctx, cy, x);
            let cy2 = row_cy(1.0);
            let field_y2 = cy2 - m.field_h / 2.0;
            let x2 = self.draw_replace_field(&mut ctx, theme, presence, field_y2, bar.x0 + m.s3);
            self.draw_action_buttons(&mut ctx, field_y2, cy2, x2);
        } else {
            x = self.draw_replace_field(&mut ctx, theme, presence, field_y, x);
            x = self.draw_match_count(&mut ctx, cy, x) + m.count_w;
            self.draw_action_buttons(&mut ctx, field_y, cy, x);
        }
    }

    /// The find field itself, plus its bad-pattern outline. Returns the `x`
    /// past it.
    fn draw_find_field(
        &mut self,
        ctx: &mut DrawCtx,
        theme: &Theme,
        presence: &PresenceCtx,
        field_y: f64,
        x: f64,
    ) -> f64 {
        let m = self.metrics;
        let find_rect = Rect::new(x, field_y, x + m.field_w, field_y + m.field_h);
        self.query.set_placeholder(&t("find-find"));
        self.query.apply_layout_external(find_rect);
        Widget::draw(
            &mut self.query,
            ctx.scene,
            ctx.transform,
            ctx.tr,
            theme,
            presence,
        );
        if self.options.regex && self.regex_error {
            ctx.scene.stroke(
                &vello::kurbo::Stroke::new(1.5),
                ctx.transform,
                ctx.palette.danger,
                None,
                &vello::kurbo::RoundedRect::from_rect(find_rect, FIELD_RADIUS),
            );
        }
        self.buttons.push((Action::FocusFind, find_rect));
        x + m.field_w + m.s2
    }

    /// The case/word/regex toggle chips. Returns the `x` past the last one.
    fn draw_mode_toggles(&mut self, ctx: &mut DrawCtx, field_y: f64, cy: f64, mut x: f64) -> f64 {
        let m = self.metrics;
        let [case_w, word_w, regex_w] = m.toggle_w;
        let [case_l, word_l, regex_l] = TOGGLE_LABELS;
        for (action, label, width, active) in [
            (
                Action::ToggleCase,
                case_l,
                case_w,
                self.options.case_sensitive,
            ),
            (Action::ToggleWord, word_l, word_w, self.options.whole_word),
            (Action::ToggleRegex, regex_l, regex_w, self.options.regex),
        ] {
            let rect = Rect::new(x, field_y, x + width, field_y + m.field_h);
            // Rest: the chip face with its derived edge; active: the accent
            // fill alone (a chosen accent never carries a hairline).
            let foreground = if active {
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    ctx.palette.accent,
                    None,
                    &vello::kurbo::RoundedRect::from_rect(rect, FIELD_RADIUS),
                );
                ctx.palette.on_accent
            } else {
                ui::surface::card(
                    ctx.scene,
                    ctx.transform,
                    rect,
                    FIELD_RADIUS,
                    ctx.palette.chip,
                );
                ctx.palette.text_dim
            };
            // Centred on the measured label box, not a per-glyph estimate.
            let (label_w, label_h) = ctx.tr.measure(label, LABEL_FONT, None);
            let text_x = rect.x0 + (width - f64::from(label_w)).max(0.0) / 2.0;
            ctx.tr.draw_text(
                ctx.scene,
                label,
                LABEL_FONT,
                foreground,
                ctx.transform * Affine::translate((text_x, cy - f64::from(label_h) / 2.0)),
                None,
            );
            self.buttons.push((action, rect));
            x += width + TOGGLE_SEAM;
        }
        x
    }

    /// The replace field. Returns the `x` past it.
    fn draw_replace_field(
        &mut self,
        ctx: &mut DrawCtx,
        theme: &Theme,
        presence: &PresenceCtx,
        field_y: f64,
        x: f64,
    ) -> f64 {
        let m = self.metrics;
        let replace_rect = Rect::new(x, field_y, x + m.field_w, field_y + m.field_h);
        self.replacement.set_placeholder(&t("find-replace-with"));
        self.replacement.apply_layout_external(replace_rect);
        Widget::draw(
            &mut self.replacement,
            ctx.scene,
            ctx.transform,
            ctx.tr,
            theme,
            presence,
        );
        self.buttons.push((Action::FocusReplace, replace_rect));
        x + m.field_w + m.s2
    }

    /// The "N of M" (or error/empty) readout. Returns the `x` past it.
    fn draw_match_count(&self, ctx: &mut DrawCtx, cy: f64, x: f64) -> f64 {
        let (count, count_color) = if self.query.value().is_empty() {
            (String::new(), ctx.palette.text_dim)
        } else if self.options.regex && self.regex_error {
            (t("find-bad-pattern"), ctx.palette.danger)
        } else if self.matches.is_empty() {
            (t("find-no-results"), ctx.palette.text_dim)
        } else {
            (
                t_args(
                    "find-count",
                    &[
                        arg("cur", num::i64_of_usize(self.index + 1)),
                        arg("total", num::i64_of_usize(self.matches.len())),
                    ],
                ),
                ctx.palette.text_dim,
            )
        };
        ctx.tr.draw_text(
            ctx.scene,
            &count,
            LABEL_FONT,
            count_color,
            ctx.transform * Affine::translate((x, cy - self.metrics.lh_label / 2.0)),
            None,
        );
        x
    }

    /// Prev/Next/Replace/Replace All/Close.
    fn draw_action_buttons(&mut self, ctx: &mut DrawCtx, field_y: f64, cy: f64, mut x: f64) {
        let m = self.metrics;
        let [prev_w, next_w, replace_w, all_w, close_w] = m.action_w;
        for (action, label, width) in [
            (Action::Prev, "↑".to_string(), prev_w),
            (Action::Next, "↓".to_string(), next_w),
            (Action::Replace, t("find-replace"), replace_w),
            (Action::ReplaceAll, t("find-replace-all"), all_w),
            (Action::Close, "✕".to_string(), close_w),
        ] {
            let rect = Rect::new(x, field_y, x + width, field_y + m.field_h);
            ui::surface::card(
                ctx.scene,
                ctx.transform,
                rect,
                FIELD_RADIUS,
                ctx.palette.chip,
            );
            let (label_w, label_h) = ctx.tr.measure(&label, ACTION_FONT, None);
            let text_x = rect.x0 + (width - f64::from(label_w)).max(0.0) / 2.0;
            ctx.tr.draw_text(
                ctx.scene,
                &label,
                ACTION_FONT,
                ctx.palette.text,
                ctx.transform * Affine::translate((text_x, cy - f64::from(label_h) / 2.0)),
                None,
            );
            self.buttons.push((action, rect));
            x += width + ACTION_SEAM;
        }
    }

    pub(crate) fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        Widget::draw_overlay(&mut self.query, scene, transform, tr, theme, presence);
        Widget::draw_overlay(&mut self.replacement, scene, transform, tr, theme, presence);
    }
}

fn is_word_match(chars: &[char], start: usize, end: usize) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let left_ok = start == 0 || chars.get(start.wrapping_sub(1)).is_none_or(|c| !word(*c));
    let right_ok = chars.get(end).is_none_or(|c| !word(*c));
    left_ok && right_ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use taffy::{NodeId, TaffyTree};
    use winit::event::Modifiers;
    use winit::keyboard::{NativeKeyCode, PhysicalKey, SmolStr};

    fn block(text: &str) -> BlockView {
        BlockView {
            id: "p1".to_string(),
            kind: "paragraph".to_string(),
            text: text.to_string(),
            marks: Vec::new(),
            attrs: std::collections::BTreeMap::new(),
            children: Vec::new(),
        }
    }

    fn key_event(key: Key, text: Option<&str>) -> WindowEvent {
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        event.logical_key = key;
        event.text = text.map(SmolStr::new);
        event.location = winit::keyboard::KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.repeat = false;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event,
            is_synthetic: false,
        }
    }

    fn drive(bar: &mut FindBar, event: &WindowEvent, modifiers: Modifiers, blocks: &[BlockView]) {
        let mut state = AutoCommit::new();
        let taffy = TaffyTree::new();
        let mut handled = false;
        let mut focus_target = None;
        let effect = bar.handle_event(
            event,
            &mut EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: &mut state,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: NodeId::new(0),
                taffy: &taffy,
            },
            blocks,
        );
        assert!(handled, "the focused find field consumes {event:?}");
        if !matches!(effect, FindEffect::None) {
            assert_eq!(effect, FindEffect::SelectActive);
        }
    }

    /// Literal modes and regex are owned by the component; navigation wraps and close drops stale hits;
    /// drawn button geometry drives click navigation; the shared input handles IME preedit, commit and match
    /// refresh and preserves select-all and field traversal shortcuts.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn the_find_bar_owns_matching_navigation_and_its_input() {
        // Phase: literal_modes_and_regex_are_owned_by_the_component.
        {
            let blocks = [block("Hello hello HELLO category cat a1 b22")];
            let mut bar = FindBar::default();
            bar.query.set_text("hello");
            bar.recompute(&blocks);
            assert_eq!(bar.matches.len(), 3);
            bar.apply(Action::ToggleCase, &blocks);
            assert_eq!(bar.matches.len(), 1);

            bar.query.set_text("cat");
            bar.options.case_sensitive = false;
            bar.recompute(&blocks);
            assert_eq!(bar.matches.len(), 2);
            bar.apply(Action::ToggleWord, &blocks);
            assert_eq!(bar.matches.len(), 1);

            bar.query.set_text(r"\d+");
            bar.options.whole_word = false;
            bar.apply(Action::ToggleRegex, &blocks);
            assert_eq!(bar.matches.len(), 2);
            bar.query.set_text("(");
            bar.recompute(&blocks);
            assert!(bar.regex_error);
            assert!(bar.matches.is_empty());
        }

        // Phase: navigation_wraps_and_close_drops_stale_hits.
        {
            let blocks = [block("one one")];
            let mut bar = FindBar::default();
            bar.query.set_text("one");
            bar.open(None, &blocks);
            bar.set_host_focused(false);
            assert!(!Widget::is_focused(&bar.query));
            bar.set_host_focused(true);
            assert!(Widget::is_focused(&bar.query));
            assert_eq!(bar.jump(-1), FindEffect::SelectActive);
            assert_eq!(bar.index, 1);
            bar.buttons
                .push((Action::Next, Rect::new(0.0, 0.0, 10.0, 10.0)));
            bar.close();
            assert!(!bar.is_open());
            assert!(bar.matches.is_empty());
            assert!(bar.buttons.is_empty());
        }

        // Phase: drawn_button_geometry_drives_click_navigation.
        {
            let blocks = [block("one one")];
            let mut bar = FindBar::default();
            bar.query.set_text("one");
            bar.open(None, &blocks);
            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            let palette = FindPalette {
                bar: Color::BLACK,
                chip: Color::BLACK,
                border: Color::WHITE,
                text: Color::WHITE,
                text_dim: Color::WHITE,
                accent: Color::WHITE,
                on_accent: Color::BLACK,
                danger: Color::WHITE,
            };
            bar.draw(
                PaintContext::new(&mut scene, Affine::IDENTITY, &mut text),
                &Theme::dark(),
                &PresenceCtx::empty(),
                palette,
                Rect::new(0.0, 0.0, 1200.0, HEIGHT),
            );
            let next = bar
                .buttons
                .iter()
                .find(|(action, _)| *action == Action::Next)
                .map(|(_, rect)| rect.center())
                .expect("draw records a Next target");
            assert_eq!(bar.click(next, &blocks), FindEffect::SelectActive);
            assert_eq!(bar.index, 1);
        }

        // Phase: shared_input_handles_ime_preedit_commit_and_match_refresh.
        {
            let blocks = [block("before 漢 after")];
            let mut bar = FindBar::default();
            bar.open(None, &blocks);

            drive(
                &mut bar,
                &WindowEvent::Ime(winit::event::Ime::Preedit(
                    "漢".into(),
                    Some(("漢".len(), "漢".len())),
                )),
                Modifiers::default(),
                &blocks,
            );
            assert_eq!(bar.query.value(), "", "preedit remains display-only");
            assert!(bar.matches().is_empty());

            drive(
                &mut bar,
                &WindowEvent::Ime(winit::event::Ime::Commit("漢".into())),
                Modifiers::default(),
                &blocks,
            );
            assert_eq!(bar.query.value(), "漢");
            assert_eq!(bar.matches(), &[("p1".to_string(), 7, 8)]);
        }

        // Phase: shared_input_preserves_select_all_and_field_traversal_shortcuts.
        {
            use winit::keyboard::ModifiersState;

            let blocks = [block("one two")];
            let mut bar = FindBar::default();
            bar.open(Some("one"), &blocks);
            let ctrl = Modifiers::from(ModifiersState::CONTROL);
            drive(
                &mut bar,
                &key_event(Key::Character(SmolStr::new("a")), Some("a")),
                ctrl,
                &blocks,
            );
            assert_eq!(bar.query.selection_range_public(), Some((0, 3)));
            drive(
                &mut bar,
                &key_event(Key::Character(SmolStr::new("two")), Some("two")),
                Modifiers::default(),
                &blocks,
            );
            assert_eq!(bar.query.value(), "two");
            assert_eq!(bar.matches(), &[("p1".to_string(), 4, 7)]);

            drive(
                &mut bar,
                &key_event(Key::Named(NamedKey::Tab), None),
                Modifiers::default(),
                &blocks,
            );
            assert!(bar.on_replace);
            assert!(Widget::is_focused(&bar.replacement));
            assert!(!Widget::is_focused(&bar.query));
        }
    }
}
