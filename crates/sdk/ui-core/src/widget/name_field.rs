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
//! caret movement — those belong to [`TextInput`](crate::widget::TextInput),
//! which is far too much machinery for a label a user types once. Its typing,
//! grapheme-wise Backspace and IME composition still go through the shared
//! [`EditCore`], so it erases and composes exactly like every other field.
//! What this does have is the behaviour a name field must not get wrong:
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
use crate::text_edit::{Edit, EditCore};
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;

/// A name is one line: control characters never enter it.
fn name_char(character: char) -> bool {
    !character.is_control()
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
    /// Caret and display-only IME composition. The caret stays at the end of
    /// the buffer: this field types and erases at its tail only.
    core: EditCore,
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
            core: self.core.clone(),
            visual: RefCell::new(NameFieldVisual::default()),
        }
    }
}

impl std::fmt::Debug for NameField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NameField")
            .field("buffer", &self.buffer)
            .field("placeholder", &self.placeholder)
            .field("preedit", &self.core.preedit())
            .finish_non_exhaustive()
    }
}

impl NameField {
    /// A field seeded with `text` — the current name when renaming, empty when
    /// naming something for the first time.
    pub fn new(text: impl Into<String>) -> Self {
        let buffer = text.into();
        let mut core = EditCore::at(buffer.len());
        core.set_filter(Some(name_char));
        Self {
            buffer,
            placeholder: "Name this version\u{2026}".to_string(),
            core,
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
        self.core.clear_preedit();
        self.core.set_caret(self.buffer.len());
    }

    /// Apply a core edit to the buffer, keeping the caret at its end.
    fn apply(&mut self, edit: &Edit) {
        edit.apply(&mut self.buffer);
        self.core.set_caret(self.buffer.len());
    }

    /// Exact shaped caret rectangle from the most recent draw, suitable for
    /// positioning the platform IME candidate window.
    /// Private equality for a deferred input into this shared name control.
    pub fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        (!self.core.has_preedit()).then(|| {
            crate::ime::edit_fingerprint(&self.buffer, self.core.anchor(), self.core.caret(), key)
        })
    }

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
            match ime {
                Ime::Preedit(text, cursor) => self.core.set_preedit(text, *cursor),
                Ime::Commit(text) => {
                    let edit = self.core.commit(text);
                    self.apply(&edit);
                }
                Ime::Disabled => self.core.clear_preedit(),
                Ime::Enabled => {}
            }
            return NameFieldOutcome::Edited;
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
                let edit = self.core.delete_backward(&self.buffer, false);
                self.apply(&edit);
                NameFieldOutcome::Edited
            }
            Key::Named(NamedKey::Space) => {
                let edit = self.core.insert(" ");
                self.apply(&edit);
                NameFieldOutcome::Edited
            }
            Key::Character(c) => {
                // A control-only keystroke admits nothing; it is still ours
                // so it cannot reach a host's typeahead.
                let edit = self.core.insert(c);
                self.apply(&edit);
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

        let shows_placeholder = self.buffer.is_empty() && !self.core.has_preedit();
        let (display, color) = if shows_placeholder {
            (self.placeholder.clone(), colors.text_dim)
        } else {
            (
                self.core.display_text(&self.buffer).into_owned(),
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

        let caret_byte = if shows_placeholder {
            0
        } else {
            self.core.display_caret(&self.buffer)
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
            transform.pre_translate((origin.x, origin.y).into()),
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
            self.core.preedit_cursor().hash(&mut hash);
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
            if let Some(range) = self.core.preedit_display_range(&self.buffer) {
                layout.add_underline(range);
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
        crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
            k,
            None,
            ElementState::Pressed,
        ))
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
            assert!(!field.core.has_preedit());
        }
    }

    #[test]
    fn typing_composing_and_erasing_clusters_leaves_the_exact_name() {
        let m = Modifiers::default();
        let typed = |s: &str| key(Key::Character(s.into()));
        let backspace = key(Key::Named(NamedKey::Backspace));
        let mut field = NameField::new("");
        // A decomposed "é" arrives as two keystrokes; emoji and a flag as one.
        for s in ["e", "\u{301}", "👍🏽", "🇩🇪"] {
            assert_eq!(field.handle_event(&typed(s), &m), NameFieldOutcome::Edited);
        }
        assert_eq!(field.text(), "e\u{301}👍🏽🇩🇪");
        // A tab is consumed but never enters a one-line name.
        assert_eq!(
            field.handle_event(&typed("\t"), &m),
            NameFieldOutcome::Edited
        );
        assert_eq!(field.text(), "e\u{301}👍🏽🇩🇪");

        // The composition shows after the text and stays out of the value; a
        // filtered control character never reaches it either.
        field.handle_event(
            &WindowEvent::Ime(Ime::Preedit("か\n".into(), Some((3, 3)))),
            &m,
        );
        assert_eq!(field.text(), "e\u{301}👍🏽🇩🇪");
        assert_eq!(field.core.preedit(), "か");
        assert_eq!(field.core.display_text(field.text()), "e\u{301}👍🏽🇩🇪か");
        field.handle_event(&WindowEvent::Ime(Ime::Commit("火".into())), &m);
        assert_eq!(field.text(), "e\u{301}👍🏽🇩🇪火");
        assert!(!field.core.has_preedit());

        // Backspace erases one whole cluster per press.
        for want in ["e\u{301}👍🏽🇩🇪", "e\u{301}👍🏽", "e\u{301}", ""] {
            field.handle_event(&backspace, &m);
            assert_eq!(field.text(), want);
        }
        field.handle_event(&backspace, &m);
        assert_eq!(field.text(), "", "Backspace on an empty name is harmless");

        // A rename seeds the caret at the end of the existing name.
        field.set_text("Draft 👍🏽");
        field.handle_event(&backspace, &m);
        field.handle_event(&typed("2"), &m);
        assert_eq!(
            field.handle_event(&key(Key::Named(NamedKey::Enter)), &m),
            NameFieldOutcome::Commit("Draft 2".to_owned())
        );
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
    }

    #[test]
    fn unchanged_browser_name_reshapes_after_font_generation_changes() {
        let root = crate::font_catalog::checkout_root();
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
