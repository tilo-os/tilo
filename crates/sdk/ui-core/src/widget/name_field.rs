//! A one-line, in-place text field for naming something.
//!
//! Both history surfaces need the same small thing: a field that appears where
//! the name will live, takes the keyboard while it is up, commits on Enter and
//! abandons on Escape. The activity pane grows it inside a band header; the
//! word processor's strip grows it in the button row. Writing it twice would
//! mean two sets of key handling, two truncation rules, and two carets that
//! drift apart — so it is one primitive.
//!
//! It is **not** a general-purpose text input: no selection, no clipboard, no
//! caret movement. Those belong to the shared CRDT-backed editor core
//! (`tilo_ui::widget::block_editor` and friends), which is far too much machinery for
//! a label a user types once. What this does have is the behaviour a name field
//! must not get wrong:
//!
//! * It keeps the **tail** visible when the text outruns the box, because what
//!   the user is typing is at the end.
//! * Enter and Escape are consumed, so a host cannot accidentally also act on
//!   them.
//! * An empty commit is reported as an empty string rather than swallowed: over
//!   an existing name that means "remove it", and only the host knows whether
//!   there is one.

use std::cell::RefCell;
use std::sync::Arc;

use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, Ime, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::richtext::RichTextLayout;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;

/// Composes `base` with a translation for text placement. Affine
/// composition is floating-point matrix multiply — it cannot overflow or
/// panic the way integer arithmetic can.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine `*` composes floating-point transform matrices, not integer arithmetic"
)]
fn translated(base: Affine, dx: f64, dy: f64) -> Affine {
    base * Affine::translate((dx, dy))
}

/// What a key did to the field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameFieldOutcome {
    /// Not for us — the host should keep looking.
    Ignored,
    /// The buffer changed; redraw.
    Edited,
    /// Enter: the trimmed text the user settled on. May be empty, which over an
    /// existing name means "remove it".
    Commit(String),
    /// Escape: abandon without writing.
    Cancel,
}

/// A one-line name field. Hosts own the placement and the lifetime; this owns
/// the text and the keys.
pub struct NameField {
    buffer: String,
    placeholder: String,
    /// Display-only IME composition, appended at this end-only field's caret.
    preedit: String,
    preedit_cursor: Option<(usize, usize)>,
    /// `draw` is intentionally `&self` because both history surfaces paint
    /// through immutable row helpers. Interior mutability retains the shaped
    /// paragraph and publishes exact candidate-window geometry.
    visual: RefCell<NameFieldVisual>,
}

struct NameFieldVisual {
    layout: Option<RichTextLayout>,
    signature: u64,
    layout_epoch: Option<Arc<()>>,
    caret: Option<Rect>,
}

impl Default for NameFieldVisual {
    fn default() -> Self {
        Self {
            layout: None,
            signature: u64::MAX,
            layout_epoch: None,
            caret: None,
        }
    }
}

impl Clone for NameField {
    fn clone(&self) -> Self {
        Self {
            buffer: self.buffer.clone(),
            placeholder: self.placeholder.clone(),
            preedit: self.preedit.clone(),
            preedit_cursor: self.preedit_cursor,
            visual: RefCell::new(NameFieldVisual::default()),
        }
    }
}

impl std::fmt::Debug for NameField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NameField")
            .field("buffer", &self.buffer)
            .field("placeholder", &self.placeholder)
            .field("preedit", &self.preedit)
            .finish_non_exhaustive()
    }
}

impl NameField {
    /// A field seeded with `text` — the current name when renaming, empty when
    /// naming something for the first time.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            buffer: text.into(),
            placeholder: "Name this version\u{2026}".to_string(),
            preedit: String::new(),
            preedit_cursor: None,
            visual: RefCell::new(NameFieldVisual::default()),
        }
    }

    /// Builder: replace the dim prompt shown while the field is empty.
    #[must_use]
    pub fn with_placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// The text as typed, untrimmed.
    pub fn text(&self) -> &str {
        &self.buffer
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.buffer = text.into();
        self.preedit.clear();
        self.preedit_cursor = None;
    }

    /// Exact shaped caret rectangle from the most recent draw, suitable for
    /// positioning the platform IME candidate window.
    pub fn ime_cursor_rect(&self) -> Option<Rect> {
        self.visual.borrow().caret
    }

    /// Feed a window event in. Chorded keys (Ctrl / Super) are left alone so the
    /// host's own shortcuts still work while the field has the keyboard.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        modifiers: &winit::event::Modifiers,
    ) -> NameFieldOutcome {
        if let WindowEvent::Ime(ime) = event {
            return match ime {
                Ime::Preedit(text, cursor) => {
                    self.preedit.clone_from(text);
                    self.preedit_cursor = *cursor;
                    NameFieldOutcome::Edited
                }
                Ime::Commit(text) => {
                    self.preedit.clear();
                    self.preedit_cursor = None;
                    self.buffer
                        .extend(text.chars().filter(|ch| !ch.is_control()));
                    NameFieldOutcome::Edited
                }
                Ime::Disabled => {
                    self.preedit.clear();
                    self.preedit_cursor = None;
                    NameFieldOutcome::Edited
                }
                Ime::Enabled => NameFieldOutcome::Edited,
            };
        }
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return NameFieldOutcome::Ignored;
        };
        if modifiers.state().control_key() || modifiers.state().super_key() {
            return NameFieldOutcome::Ignored;
        }
        match logical_key {
            Key::Named(NamedKey::Enter) => NameFieldOutcome::Commit(self.buffer.trim().to_string()),
            Key::Named(NamedKey::Escape) => NameFieldOutcome::Cancel,
            Key::Named(NamedKey::Backspace) => {
                let end = self.buffer.len();
                self.buffer
                    .truncate(crate::text_selection::prev_char_boundary(&self.buffer, end));
                NameFieldOutcome::Edited
            }
            Key::Named(NamedKey::Space) => {
                self.buffer.push(' ');
                NameFieldOutcome::Edited
            }
            Key::Character(c) => {
                let text: String = c.chars().filter(|ch| !ch.is_control()).collect();
                if text.is_empty() {
                    // A control-only keystroke is not a character; treat it as
                    // ours anyway so it cannot reach a host's typeahead.
                    return NameFieldOutcome::Edited;
                }
                self.buffer.push_str(&text);
                NameFieldOutcome::Edited
            }
            // Everything else is still consumed: while a name field is up, the
            // keyboard belongs to it.
            _ => NameFieldOutcome::Edited,
        }
    }

    /// Paint the field into `rect` in `style` (a resolved role), with a caret
    /// at the end of the text.
    pub fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        style: &TextStyle,
    ) {
        if rect.width() < 24.0 || rect.height() < 12.0 {
            return;
        }
        let colors = &theme.tokens.colors;
        let shape = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.detail));
        Self::draw_chrome(scene, transform, colors, &shape);

        let shows_placeholder = self.buffer.is_empty() && self.preedit.is_empty();
        let (display, color) = if shows_placeholder {
            (self.placeholder.clone(), colors.text_dim)
        } else {
            (
                format!("{}{}", self.buffer, self.preedit),
                colors.text_primary,
            )
        };
        self.reshape_if_stale(tr, &display, style);

        let mut visual = self.visual.borrow_mut();
        // Just-populated above (or already current) — but degrade to
        // "paint the chrome only" instead of panicking if that invariant
        // is ever wrong.
        let Some(layout) = visual.layout.as_ref() else {
            return;
        };

        let preedit_at = self
            .preedit_cursor
            .map_or(self.preedit.len(), |(_, end)| end)
            .min(self.preedit.len());
        let preedit_at = self.preedit.floor_char_boundary(preedit_at);
        let caret_byte = if shows_placeholder {
            0
        } else {
            self.buffer.len().saturating_add(preedit_at)
        };
        let (caret_local_x, caret_top, caret_height) = layout.caret_geometry(caret_byte);
        let viewport_width = (rect.width() - 14.0).max(1.0);
        let scroll_x = if shows_placeholder {
            0.0
        } else {
            (f64::from(caret_local_x) - viewport_width).max(0.0)
        };
        let origin = Point::new(
            rect.x0 + 7.0 - scroll_x,
            rect.center().y - f64::from(layout.height()) / 2.0,
        );
        let caret = Rect::new(
            origin.x + f64::from(caret_local_x),
            origin.y + f64::from(caret_top),
            origin.x + f64::from(caret_local_x) + 1.5,
            origin.y + f64::from(caret_top + caret_height),
        );
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &shape,
        );
        tr.draw_rich(
            scene,
            layout,
            translated(transform, origin.x, origin.y),
            color,
        );
        visual.caret = Some(caret);
        Self::draw_caret_line(scene, transform, colors, caret);
        scene.pop_layer();
    }

    /// The field's background fill and focus-ring stroke.
    fn draw_chrome(
        scene: &mut Scene,
        transform: Affine,
        colors: &crate::tokens::colors::Colors,
        shape: &RoundedRect,
    ) {
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colors.surfaces.overlay,
            None,
            shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.border_focus,
            None,
            shape,
        );
    }

    /// Re-shapes `display` into `self.visual` if the cached layout is
    /// missing or stale (text, style, buffer length or preedit cursor
    /// changed since the last shape).
    fn reshape_if_stale(&self, tr: &mut TextRenderer, display: &str, style: &TextStyle) {
        let signature = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut hash = DefaultHasher::new();
            display.hash(&mut hash);
            style.size.to_bits().hash(&mut hash);
            style.weight.hash(&mut hash);
            style.family.hash(&mut hash);
            self.buffer.len().hash(&mut hash);
            self.preedit_cursor.hash(&mut hash);
            hash.finish()
        };
        let mut visual = self.visual.borrow_mut();
        let epoch_stale = visual
            .layout_epoch
            .as_ref()
            .is_none_or(|epoch| !Arc::ptr_eq(epoch, tr.layout_epoch()));
        if visual.layout.is_none() || visual.signature != signature || epoch_stale {
            let browser_emoji = tr.browser_color_emoji_installed();
            let mut layout = RichTextLayout::shape_plain_browser(
                &mut tr.font_system,
                display,
                style,
                None,
                browser_emoji,
            );
            tr.note_browser_rich_layout(&layout);
            if !self.preedit.is_empty() {
                layout.add_underline(self.buffer.len()..display.len());
            }
            visual.layout = Some(layout);
            visual.signature = signature;
            visual.layout_epoch = Some(Arc::clone(tr.layout_epoch()));
        }
    }

    /// The blinking-caret line, drawn over the shaped text.
    fn draw_caret_line(
        scene: &mut Scene,
        transform: Affine,
        colors: &crate::tokens::colors::Colors,
        caret: Rect,
    ) {
        scene.stroke(
            &Stroke::new(1.5),
            transform,
            colors.intent.inverted_hover,
            None,
            &Line::new(
                Point::new(caret.x0, caret.y0),
                Point::new(caret.x0, caret.y1),
            ),
        );
    }
}

/// Shorten `s` until it fits `budget` pixels in `style`, appending an
/// ellipsis when anything was dropped.
///
/// Binary chop on the char index; `measure` is cheap and the common case (a short
/// label in a wide slot) exits on the first measurement. Returns an empty string
/// when not even one character fits, which is the empty result for a slot that
/// has been squeezed to nothing.
///
/// Shared by every history surface: a version name that ellipsises one way in the
/// activity pane and another on the version strip reads as two different names.
pub fn fit(tr: &mut TextRenderer, s: &str, style: &TextStyle, budget: f64) -> String {
    const ELLIPSIS: &str = "\u{2026}";
    if budget <= 0.0 {
        return String::new();
    }
    if f64::from(tr.measure_style(s, None, style).0) <= budget {
        return s.to_string();
    }
    let ell_w = f64::from(tr.measure_style(ELLIPSIS, None, style).0);
    if ell_w > budget {
        return String::new();
    }
    let inner = budget - ell_w;
    let mut lo = 0usize;
    let mut hi = s.chars().count();
    while lo < hi {
        // Never overflows: `hi - lo` can't underflow (loop invariant
        // `lo < hi`), and `lo + (hi - lo)` is exactly `hi`, already a
        // valid `usize`. `saturating_*` states that to the lint rather
        // than relying on the invariant holding at every call site.
        let mid = lo.saturating_add(hi.saturating_sub(lo).div_ceil(2));
        let candidate: String = s.chars().take(mid).collect();
        if f64::from(tr.measure_style(&candidate, None, style).0) <= inner {
            lo = mid;
        } else {
            // `mid >= lo + 1 >= 1` whenever `lo < hi`, so this never
            // underflows; `saturating_sub` keeps that true by construction
            // rather than by the argument above.
            hi = mid.saturating_sub(1);
        }
    }
    if lo == 0 {
        return ELLIPSIS.to_string();
    }
    let mut out: String = s.chars().take(lo).collect();
    out.push_str(ELLIPSIS);
    out
}

/// Keep the **tail** of `s`. What a person is typing is at the end, so a field
/// that has run out of room drops the beginning, not the caret.
pub fn fit_tail(tr: &mut TextRenderer, s: &str, style: &TextStyle, budget: f64) -> String {
    const ELLIPSIS: &str = "\u{2026}";
    if budget <= 0.0 {
        return String::new();
    }
    if f64::from(tr.measure_style(s, None, style).0) <= budget {
        return s.to_string();
    }
    let ell_w = f64::from(tr.measure_style(ELLIPSIS, None, style).0);
    if ell_w > budget {
        return String::new();
    }
    let inner = budget - ell_w;
    let chars: Vec<char> = s.chars().collect();
    let mut take = 0usize;
    while take < chars.len() {
        // `take < chars.len()` (loop guard), so `take + 1 <= chars.len()`
        // and this can't saturate in practice; written defensively so it
        // degrades to "keep everything" instead of panicking if that
        // invariant is ever wrong.
        let start = chars.len().saturating_sub(take.saturating_add(1));
        let candidate: String = chars.get(start..).unwrap_or(&[]).iter().collect();
        if f64::from(tr.measure_style(&candidate, None, style).0) > inner {
            break;
        }
        take = take.saturating_add(1);
    }
    let mut out = ELLIPSIS.to_string();
    let start = chars.len().saturating_sub(take);
    out.extend(chars.get(start..).unwrap_or(&[]).iter());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::Modifiers;

    fn draw_field(field: &NameField, rect: Rect) {
        let mut scene = Scene::new();
        let mut renderer = TextRenderer::new();
        field.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut renderer,
            &Theme::dark(),
            rect,
            &Theme::dark().text(crate::tokens::TextRole::Body),
        );
    }

    fn key(k: Key) -> WindowEvent {
        // SAFETY: every pub field of `KeyEvent` tolerates the all-zero bit
        // pattern; `logical_key` is overwritten and `text`'s niche-optimised
        // `None` is all-zero. winit 0.30 keeps `KeyEventExtra` crate-private.
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = k;
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn ch(c: char) -> WindowEvent {
        key(Key::Character(c.to_string().into()))
    }

    #[test]
    fn name_field_commits_trimmed_text_or_cancels() {
        // Case `typing_then_enter_commits_the_trimmed_text`.
        {
            let mut f = NameField::new("");
            let m = Modifiers::default();
            for c in " Intro ".chars() {
                let out = if c == ' ' {
                    f.handle_event(&key(Key::Named(NamedKey::Space)), &m)
                } else {
                    f.handle_event(&ch(c), &m)
                };
                assert_eq!(out, NameFieldOutcome::Edited);
            }
            assert_eq!(f.text(), " Intro ");
            assert_eq!(
                f.handle_event(&key(Key::Named(NamedKey::Enter)), &m),
                NameFieldOutcome::Commit("Intro".to_string())
            );
        }

        // Case `a_rename_starts_from_the_existing_name_and_can_be_emptied`.
        {
            let mut f = NameField::new("Intro rewrite");
            let m = Modifiers::default();
            assert_eq!(f.text(), "Intro rewrite");
            for _ in 0.."Intro rewrite".len() {
                f.handle_event(&key(Key::Named(NamedKey::Backspace)), &m);
            }
            assert_eq!(f.text(), "");
            assert_eq!(
                f.handle_event(&key(Key::Named(NamedKey::Enter)), &m),
                NameFieldOutcome::Commit(String::new()),
                "an empty commit is reported, not swallowed — it means 'remove it'"
            );
        }

        // Case `escape_cancels`.
        {
            let mut f = NameField::new("half typed");
            assert_eq!(
                f.handle_event(&key(Key::Named(NamedKey::Escape)), &Modifiers::default()),
                NameFieldOutcome::Cancel
            );
        }
    }

    #[test]
    fn name_field_owns_editing_keys_but_leaves_chords_to_the_host() {
        // Case `a_chorded_key_is_left_for_the_host`.
        {
            let mut f = NameField::new("");
            let m = Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
            assert_eq!(f.handle_event(&ch('s'), &m), NameFieldOutcome::Ignored);
            assert_eq!(f.text(), "", "Ctrl+S must not type an 's' into the name");
        }

        // Case `a_stray_key_is_consumed_rather_than_reaching_a_typeahead`.
        {
            // The pane behind this field filters by author as you type. An arrow key
            // (or anything else) leaking through while a name field is up would
            // silently re-filter the list under the user.
            let mut f = NameField::new("x");
            let out = f.handle_event(&key(Key::Named(NamedKey::ArrowLeft)), &Modifiers::default());
            assert_eq!(out, NameFieldOutcome::Edited, "consumed");
            assert_eq!(f.text(), "x", "and it changed nothing");
        }

        // Case `backspace_removes_one_extended_grapheme`.
        {
            let family = "A👨\u{200d}👩\u{200d}👧\u{200d}👦";
            let mut field = NameField::new(format!("{family}e\u{301}"));
            let backspace = key(Key::Named(NamedKey::Backspace));
            assert_eq!(
                field.handle_event(&backspace, &Modifiers::default()),
                NameFieldOutcome::Edited
            );
            assert_eq!(field.text(), family, "combining sequence is one deletion");
            field.handle_event(&backspace, &Modifiers::default());
            assert_eq!(field.text(), "A", "ZWJ family emoji is one deletion");
        }

        // Case `ime_preedit_is_shaped_underlined_and_committed_atomically`.
        {
            let mut field = NameField::new("ab");
            let preedit = "漢字";
            assert_eq!(
                field.handle_event(
                    &WindowEvent::Ime(
                        Ime::Preedit(preedit.into(), Some(("漢".len(), "漢".len())),)
                    ),
                    &Modifiers::default(),
                ),
                NameFieldOutcome::Edited
            );
            assert_eq!(field.text(), "ab", "preedit stays out of the value");
            let rect = Rect::new(10.0, 20.0, 210.0, 52.0);
            draw_field(&field, rect);
            let visual = field.visual.borrow();
            assert_eq!(
                visual.layout.as_ref().unwrap().underlines,
                vec![(2, 2 + preedit.len())]
            );
            drop(visual);
            let candidate = field
                .ime_cursor_rect()
                .expect("draw publishes caret geometry");
            assert!(rect.contains(candidate.center()));

            field.handle_event(
                &WindowEvent::Ime(Ime::Commit(preedit.into())),
                &Modifiers::default(),
            );
            assert_eq!(field.text(), "ab漢字");
            assert!(field.preedit.is_empty());
        }
    }

    #[test]
    fn long_or_squeezed_names_keep_the_caret_end_visible_without_overflow() {
        // Case `shaped_end_caret_stays_visible_for_long_ltr_and_rtl_names`.
        {
            let rect = Rect::new(0.0, 0.0, 120.0, 32.0);
            let ltr = NameField::new("a very long version name whose end must stay visible");
            draw_field(&ltr, rect);
            let ltr_caret = ltr.ime_cursor_rect().unwrap();
            assert!(ltr_caret.x0 >= rect.x1 - 9.0 && ltr_caret.x1 < rect.x1);

            let rtl = NameField::new("שלום זהו שם גרסה ארוך מאוד");
            draw_field(&rtl, rect);
            let rtl_caret = rtl.ime_cursor_rect().unwrap();
            assert!(
                rtl_caret.x0 < rect.center().x,
                "logical end of an RTL name is its visual left edge: {rtl_caret:?}"
            );
            assert!(rect.contains(rtl_caret.center()));
        }

        // Case `a_long_name_keeps_the_end_visible`.
        {
            let mut tr = TextRenderer::new();
            let long = "a very long version name that will not fit in the box";
            let style = Theme::dark().text(crate::tokens::TextRole::Label);
            let shown = fit_tail(&mut tr, long, &style, 80.0);
            assert!(shown.starts_with('\u{2026}'), "got {shown:?}");
            assert!(
                long.ends_with(shown.trim_start_matches('\u{2026}')),
                "the tail is what is kept: {shown:?}"
            );
            assert!(shown.chars().count() < long.chars().count());
        }

        // Case `a_box_squeezed_to_nothing_draws_nothing_rather_than_overflowing`.
        {
            let mut tr = TextRenderer::new();
            let style = Theme::dark().text(crate::tokens::TextRole::Label);
            assert_eq!(fit_tail(&mut tr, "anything", &style, 0.0), "");
            assert_eq!(fit_tail(&mut tr, "anything", &style, -5.0), "");
        }
    }

    #[test]
    fn unchanged_browser_name_reshapes_after_font_generation_changes() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts");
        let fonts = crate::font_catalog::FontCatalog::product("", "en-US")
            .load_browser_startup_bytes(|name| std::fs::read(root.join(name)).ok())
            .unwrap();
        let mut renderer = TextRenderer::with_browser_font_system(fonts);
        let field = NameField::new("مَرْحَبًا");
        field.reshape_if_stale(&mut renderer, field.text(), &TextStyle::new(14.0));
        assert_eq!(
            renderer.take_browser_font_demands(),
            ["NotoSansArabic[wght].ttf"]
        );
        renderer.browser_font_failed();
        field.reshape_if_stale(&mut renderer, field.text(), &TextStyle::new(14.0));
        assert_eq!(
            renderer.take_browser_font_demands(),
            ["NotoSansArabic[wght].ttf"]
        );
        renderer
            .install_browser_font(
                "NotoSansArabic[wght].ttf",
                std::fs::read(root.join("NotoSansArabic[wght].ttf")).unwrap(),
            )
            .unwrap();
        field.reshape_if_stale(&mut renderer, field.text(), &TextStyle::new(14.0));
        assert!(
            field
                .visual
                .borrow()
                .layout
                .as_ref()
                .unwrap()
                .buffer
                .layout_runs()
                .flat_map(|run| run.glyphs)
                .all(|glyph| glyph.glyph_id != 0)
        );
    }
}
