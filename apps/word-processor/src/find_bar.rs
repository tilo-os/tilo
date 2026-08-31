//! Find & Replace chrome for the word processor.
//!
//! This component owns the query/mode state, match index, rendering, and the
//! hit map produced by rendering.  It deliberately emits document-edit intents
//! instead of editing qdoc itself: the parent editor remains the sole owner of
//! undoable document mutations.

use qdoc::snapshot::BlockView;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};
use winit::keyboard::{Key, NamedKey};

use ui::text_render::TextRenderer;

use crate::i18n::{arg, t, t_args};
use crate::num;

pub(crate) const HEIGHT: f64 = 42.0;

pub(crate) type FindMatch = (String, usize, usize);

#[derive(Clone, Copy)]
pub(crate) struct FindPalette {
    pub bar: Color,
    pub field: Color,
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

#[derive(Default)]
pub(crate) struct FindBar {
    open: bool,
    query: String,
    replacement: String,
    on_replace: bool,
    matches: Vec<FindMatch>,
    index: usize,
    buttons: Vec<(Action, Rect)>,
    options: SearchOptions,
    regex_error: bool,
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
        if self.open { HEIGHT } else { 0.0 }
    }

    pub(crate) fn matches(&self) -> &[FindMatch] {
        &self.matches
    }

    pub(crate) fn active_match(&self) -> Option<&FindMatch> {
        self.matches.get(self.index)
    }

    pub(crate) fn replacement(&self) -> &str {
        &self.replacement
    }

    #[cfg(test)]
    pub(crate) fn set_queries_for_tests(&mut self, query: &str, replacement: &str) {
        self.query = query.to_string();
        self.replacement = replacement.to_string();
    }

    pub(crate) fn open(&mut self, seed: Option<&str>, blocks: &[BlockView]) -> FindEffect {
        self.open = true;
        self.on_replace = false;
        if let Some(seed) = seed.filter(|s| !s.is_empty() && !s.contains('\n')) {
            self.query.clear();
            self.query.push_str(seed);
        }
        self.recompute(blocks)
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
        self.matches.clear();
        self.buttons.clear();
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

    /// Consume a key while open. The parent only needs to enact the returned
    /// document intent; query editing and match navigation stay private here.
    pub(crate) fn key(
        &mut self,
        key: &Key,
        typed: Option<&str>,
        shift: bool,
        blocks: &[BlockView],
    ) -> FindEffect {
        match key {
            Key::Named(NamedKey::Escape) => {
                self.close();
                FindEffect::None
            }
            Key::Named(NamedKey::Enter) => self.jump(if shift { -1 } else { 1 }),
            Key::Named(NamedKey::Tab) => {
                self.on_replace = !self.on_replace;
                FindEffect::None
            }
            Key::Named(NamedKey::Backspace) => {
                if self.on_replace {
                    self.replacement.pop();
                    FindEffect::None
                } else {
                    self.query.pop();
                    self.recompute(blocks)
                }
            }
            _ => {
                if let Some(text) =
                    typed.filter(|s| !s.is_empty() && !s.chars().any(char::is_control))
                {
                    if self.on_replace {
                        self.replacement.push_str(text);
                        return FindEffect::None;
                    }
                    self.query.push_str(text);
                    return self.recompute(blocks);
                }
                FindEffect::None
            }
        }
    }

    fn apply(&mut self, action: Action, blocks: &[BlockView]) -> FindEffect {
        match action {
            Action::FocusFind => {
                self.on_replace = false;
                FindEffect::None
            }
            Action::FocusReplace => {
                self.on_replace = true;
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
        if self.query.is_empty() {
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
            self.query.chars().collect()
        } else {
            self.query.to_lowercase().chars().collect()
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
                if haystack[start..end] == needle[..]
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
            format!(r"\b(?:{})\b", self.query)
        } else {
            self.query.clone()
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
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        palette: FindPalette,
        bar: Rect,
    ) {
        self.buttons.clear();
        scene.fill(Fill::NonZero, transform, palette.bar, None, &bar);
        scene.fill(
            Fill::NonZero,
            transform,
            palette.border,
            None,
            &Rect::new(bar.x0, bar.y1 - 1.0, bar.x1, bar.y1),
        );
        let cy = f64::midpoint(bar.y0, bar.y1);
        let field_y = cy - 13.0;
        let mut ctx = DrawCtx {
            scene,
            transform,
            tr,
            palette,
        };
        let mut x = bar.x0 + 12.0;
        x = self.draw_find_field(&mut ctx, field_y, x);
        x = self.draw_mode_toggles(&mut ctx, field_y, cy, x) + 8.0;
        x = self.draw_replace_field(&mut ctx, field_y, x);
        x = self.draw_match_count(&mut ctx, cy, x) + 78.0;
        self.draw_action_buttons(&mut ctx, field_y, cy, x);
    }

    /// The find field itself, plus its bad-pattern outline. Returns the `x`
    /// past it.
    fn draw_find_field(&mut self, ctx: &mut DrawCtx, field_y: f64, x: f64) -> f64 {
        let find_rect = Rect::new(x, field_y, x + 188.0, field_y + 26.0);
        draw_field(
            ctx.scene,
            ctx.transform,
            ctx.tr,
            ctx.palette,
            find_rect,
            &self.query,
            &t("find-find"),
            !self.on_replace,
        );
        if self.options.regex && self.regex_error {
            ctx.scene.stroke(
                &vello::kurbo::Stroke::new(1.5),
                ctx.transform,
                ctx.palette.danger,
                None,
                &vello::kurbo::RoundedRect::from_rect(find_rect, 5.0),
            );
        }
        self.buttons.push((Action::FocusFind, find_rect));
        x + 196.0
    }

    /// The case/word/regex toggle chips. Returns the `x` past the last one.
    fn draw_mode_toggles(&mut self, ctx: &mut DrawCtx, field_y: f64, cy: f64, mut x: f64) -> f64 {
        for (action, label, width, active) in [
            (Action::ToggleCase, "Aa", 30.0, self.options.case_sensitive),
            (Action::ToggleWord, "W", 26.0, self.options.whole_word),
            (Action::ToggleRegex, ".*", 30.0, self.options.regex),
        ] {
            let rect = Rect::new(x, field_y, x + width, field_y + 26.0);
            let (background, foreground) = if active {
                (ctx.palette.accent, ctx.palette.on_accent)
            } else {
                (ctx.palette.chip, ctx.palette.text_dim)
            };
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                background,
                None,
                &vello::kurbo::RoundedRect::from_rect(rect, 5.0),
            );
            let text_x =
                rect.x0 + (width - num::f64_of_usize(label.chars().count()) * 7.2).max(4.0) / 2.0;
            ctx.tr.draw_text(
                ctx.scene,
                label,
                12.5,
                foreground,
                ctx.transform * Affine::translate((text_x, cy - 8.0)),
                None,
            );
            self.buttons.push((action, rect));
            x += width + 5.0;
        }
        x
    }

    /// The replace field. Returns the `x` past it.
    fn draw_replace_field(&mut self, ctx: &mut DrawCtx, field_y: f64, x: f64) -> f64 {
        let replace_rect = Rect::new(x, field_y, x + 188.0, field_y + 26.0);
        draw_field(
            ctx.scene,
            ctx.transform,
            ctx.tr,
            ctx.palette,
            replace_rect,
            &self.replacement,
            &t("find-replace-with"),
            self.on_replace,
        );
        self.buttons.push((Action::FocusReplace, replace_rect));
        x + 196.0
    }

    /// The "N of M" (or error/empty) readout. Returns the `x` past it.
    fn draw_match_count(&self, ctx: &mut DrawCtx, cy: f64, x: f64) -> f64 {
        let (count, count_color) = if self.query.is_empty() {
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
            12.5,
            count_color,
            ctx.transform * Affine::translate((x, cy - 7.0)),
            None,
        );
        x
    }

    /// Prev/Next/Replace/Replace All/Close.
    fn draw_action_buttons(&mut self, ctx: &mut DrawCtx, field_y: f64, cy: f64, mut x: f64) {
        for (action, label, width) in [
            (Action::Prev, "↑".to_string(), 28.0),
            (Action::Next, "↓".to_string(), 28.0),
            (Action::Replace, t("find-replace"), 72.0),
            (Action::ReplaceAll, t("find-replace-all"), 40.0),
            (Action::Close, "✕".to_string(), 28.0),
        ] {
            let rect = Rect::new(x, field_y, x + width, field_y + 26.0);
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                ctx.palette.chip,
                None,
                &vello::kurbo::RoundedRect::from_rect(rect, 5.0),
            );
            let text_x =
                rect.x0 + (width - num::f64_of_usize(label.chars().count()) * 7.2).max(6.0) / 2.0;
            ctx.tr.draw_text(
                ctx.scene,
                &label,
                13.0,
                ctx.palette.text,
                ctx.transform * Affine::translate((text_x, cy - 8.0)),
                None,
            );
            self.buttons.push((action, rect));
            x += width + 6.0;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_field(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    palette: FindPalette,
    rect: Rect,
    text: &str,
    placeholder: &str,
    focused: bool,
) {
    let border = if focused {
        palette.accent
    } else {
        palette.border
    };
    scene.fill(
        Fill::NonZero,
        transform,
        palette.field,
        None,
        &vello::kurbo::RoundedRect::from_rect(rect, 5.0),
    );
    scene.stroke(
        &vello::kurbo::Stroke::new(if focused { 1.5 } else { 1.0 }),
        transform,
        border,
        None,
        &vello::kurbo::RoundedRect::from_rect(rect, 5.0),
    );
    let (shown, color) = if text.is_empty() && !focused {
        (placeholder.to_string(), palette.text_dim)
    } else if focused {
        (format!("{text}_"), palette.text)
    } else {
        (text.to_string(), palette.text)
    };
    tr.draw_text(
        scene,
        &shown,
        13.0,
        color,
        transform * Affine::translate((rect.x0 + 8.0, f64::midpoint(rect.y0, rect.y1) - 8.0)),
        None,
    );
}

fn byte_to_char(text: &str, byte_offset: usize) -> usize {
    text.get(..byte_offset)
        .map_or(0, |prefix| prefix.chars().count())
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

    #[test]
    fn literal_modes_and_regex_are_owned_by_the_component() {
        let blocks = [block("Hello hello HELLO category cat a1 b22")];
        let mut bar = FindBar {
            query: "hello".to_string(),
            ..Default::default()
        };
        bar.recompute(&blocks);
        assert_eq!(bar.matches.len(), 3);
        bar.apply(Action::ToggleCase, &blocks);
        assert_eq!(bar.matches.len(), 1);

        bar.query = "cat".to_string();
        bar.options.case_sensitive = false;
        bar.recompute(&blocks);
        assert_eq!(bar.matches.len(), 2);
        bar.apply(Action::ToggleWord, &blocks);
        assert_eq!(bar.matches.len(), 1);

        bar.query = r"\d+".to_string();
        bar.options.whole_word = false;
        bar.apply(Action::ToggleRegex, &blocks);
        assert_eq!(bar.matches.len(), 2);
        bar.query = "(".to_string();
        bar.recompute(&blocks);
        assert!(bar.regex_error);
        assert!(bar.matches.is_empty());
    }

    #[test]
    fn navigation_wraps_and_close_drops_stale_hits() {
        let blocks = [block("one one")];
        let mut bar = FindBar {
            query: "one".to_string(),
            ..Default::default()
        };
        bar.open(None, &blocks);
        assert_eq!(bar.jump(-1), FindEffect::SelectActive);
        assert_eq!(bar.index, 1);
        bar.buttons
            .push((Action::Next, Rect::new(0.0, 0.0, 10.0, 10.0)));
        bar.close();
        assert!(!bar.is_open());
        assert!(bar.matches.is_empty());
        assert!(bar.buttons.is_empty());
    }

    #[test]
    fn drawn_button_geometry_drives_click_navigation() {
        let blocks = [block("one one")];
        let mut bar = FindBar {
            query: "one".to_string(),
            ..Default::default()
        };
        bar.open(None, &blocks);
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let palette = FindPalette {
            bar: Color::BLACK,
            field: Color::BLACK,
            chip: Color::BLACK,
            border: Color::WHITE,
            text: Color::WHITE,
            text_dim: Color::WHITE,
            accent: Color::WHITE,
            on_accent: Color::BLACK,
            danger: Color::WHITE,
        };
        bar.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
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
}
