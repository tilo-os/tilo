//! Shared visual/IME state for transient search and command queries.
//!
//! Query owners keep their committed `String` and filtering semantics. This
//! helper makes the editable presentation behave like every other toolkit text
//! surface: one shaped layout for ink, selection, caret and candidate geometry,
//! with IME preedit excluded from application state until commit.

use std::sync::Arc;

use vello::kurbo::{Point, Rect};
use vello::peniko::Color;
use winit::event::{ElementState, Ime, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::paint::PaintCtx;
use crate::richtext::RichTextLayout;
use crate::text_edit::{Edit, EditCore, Motion};
use crate::text_render::TextStyle;
use crate::text_selection::{Selection, TextPosition, clipboard_copy, clipboard_paste};

/// Whether an IME event changed only the display composition or committed a
/// new query value. Owners should re-run filtering only for `Committed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryImeChange {
    Display,
    Committed,
}

/// Result of routing an event through the shared single-line query editor.
///
/// `Display` covers caret/selection movement, clipboard copy, and IME preedit;
/// owners should repaint but must not recompute their result set. `Edited`
/// means committed text changed and filtering should be refreshed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryEventChange {
    Ignored,
    Display,
    Edited,
}

/// The query a [`QueryComposition`] paints: the committed text and its
/// selection, the placeholder shown while it is empty, and whether the caret
/// is visible.
#[derive(Clone, Copy, Debug)]
pub struct QueryView<'a> {
    pub text: &'a str,
    pub selection: &'a Selection,
    pub placeholder: &'a str,
    pub show_caret: bool,
}

/// The colours a [`QueryComposition`] paints with.
#[derive(Clone, Copy, Debug)]
pub struct QueryInk {
    pub text: Color,
    pub placeholder: Color,
    pub selection: Color,
    pub caret: Color,
}

impl QueryInk {
    /// Primary ink, the secondary tier for the placeholder, and the
    /// person's own colour for the caret and selection
    /// ([`crate::tokens::Intents::you`]).
    pub fn of(colors: &crate::tokens::Colors) -> Self {
        Self {
            text: colors.text_primary,
            placeholder: colors.text_dim,
            selection: colors.intent.selection_tint,
            caret: colors.intent.you,
        }
    }
}

/// Retained shaping and IME state for an inline query editor. Editing goes
/// through the shared [`EditCore`]: the owner's [`Selection`] is loaded into
/// it for every event and written back afterwards, so a query keeps the same
/// grapheme, word, clipboard and composition rules as every other field.
pub struct QueryComposition {
    core: EditCore,
    layout: Option<RichTextLayout>,
    layout_text: String,
    signature: u64,
    layout_epoch: Option<Arc<()>>,
    caret: Option<Rect>,
    origin: Point,
}

impl Default for QueryComposition {
    fn default() -> Self {
        Self::new()
    }
}

/// A query is one line: control characters (newlines, tabs) never enter it.
fn query_char(character: char) -> bool {
    !character.is_control()
}

impl QueryComposition {
    pub fn new() -> Self {
        let mut core = EditCore::new();
        core.set_filter(Some(query_char));
        Self {
            core,
            layout: None,
            layout_text: String::new(),
            signature: u64::MAX,
            layout_epoch: None,
            caret: None,
            origin: Point::ZERO,
        }
    }

    pub fn is_composing(&self) -> bool {
        self.core.has_preedit()
    }

    pub fn clear(&mut self) {
        self.core.clear_preedit();
        self.layout = None;
        self.layout_text.clear();
        self.caret = None;
        self.signature = u64::MAX;
        self.layout_epoch = None;
    }

    /// Place an inline query caret using its last shaped, painted layout.
    /// Stale text or active IME composition keeps the existing selection.
    pub fn pointer_selection(&self, text: &str, position: Point) -> Option<Selection> {
        if self.is_composing()
            || self.layout_text != text
            || !position.x.is_finite()
            || !position.y.is_finite()
        {
            return None;
        }
        let relative = position - self.origin;
        let layout = self.layout.as_ref()?;
        if relative.x.abs() > f64::from(f32::MAX) || relative.y.abs() > f64::from(f32::MAX) {
            return None;
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "finite local text-layout pixel coordinates fit the f32 shaping API"
        )]
        let position = layout.hit_position(relative.x as f32, relative.y as f32);
        Some(Selection::collapsed(position.byte.min(text.len())))
    }

    /// Load the owner's selection into the core, clamped onto `text`.
    fn load(&mut self, text: &str, selection: &Selection) {
        self.core.set_selection(selection.anchor, selection.cursor);
        self.core.clamp_to(text);
    }

    /// Apply a core edit to the owner's text: `Edited` when it changed the
    /// text, otherwise `unchanged`.
    fn apply(text: &mut String, edit: &Edit, unchanged: QueryEventChange) -> QueryEventChange {
        if !edit.is_noop() && edit.apply(text) {
            QueryEventChange::Edited
        } else {
            unchanged
        }
    }

    /// Apply one platform IME event. Preedit changes only this helper; commit
    /// replaces the owner's current selection as one atomic string edit.
    pub fn handle_ime(
        &mut self,
        ime: &Ime,
        text: &mut String,
        selection: &mut Selection,
    ) -> QueryImeChange {
        self.load(text, selection);
        let change = match ime {
            Ime::Preedit(preedit, cursor) => {
                self.core.set_preedit(preedit, *cursor);
                QueryImeChange::Display
            }
            Ime::Commit(committed) => {
                let edit = self.core.commit(committed);
                Self::apply(text, &edit, QueryEventChange::Display);
                QueryImeChange::Committed
            }
            Ime::Disabled => {
                self.core.clear_preedit();
                QueryImeChange::Display
            }
            Ime::Enabled => QueryImeChange::Display,
        };
        *selection = self.core.selection();
        change
    }

    /// Apply the ordinary editing subset of a window event to a transient
    /// query. Navigation/activation keys such as Up, Down, Enter, Escape and
    /// Tab are intentionally left to the owning popup.
    ///
    /// The owner supplies modifier state because winit reports it separately
    /// from `KeyEvent`. `command` should be true for Ctrl on Linux/Windows or
    /// Command on macOS.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        text: &mut String,
        selection: &mut Selection,
        shift: bool,
        command: bool,
    ) -> QueryEventChange {
        if let WindowEvent::Ime(ime) = event {
            return match self.handle_ime(ime, text, selection) {
                QueryImeChange::Display => QueryEventChange::Display,
                QueryImeChange::Committed => QueryEventChange::Edited,
            };
        }
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    text: event_text,
                    ..
                },
            ..
        } = event
        else {
            return QueryEventChange::Ignored;
        };

        self.load(text, selection);
        let change = match logical_key {
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight) => {
                let right = matches!(logical_key, Key::Named(NamedKey::ArrowRight));
                self.arrow(text, shift, command, right);
                QueryEventChange::Display
            }
            Key::Named(NamedKey::Home) => {
                self.core.motion(text, Motion::DocStart, shift);
                QueryEventChange::Display
            }
            Key::Named(NamedKey::End) => {
                self.core.motion(text, Motion::DocEnd, shift);
                QueryEventChange::Display
            }
            Key::Named(NamedKey::Backspace) => {
                let edit = self.core.delete_backward(text, command);
                Self::apply(text, &edit, QueryEventChange::Display)
            }
            Key::Named(NamedKey::Delete) => {
                let edit = self.core.delete_forward(text, command);
                Self::apply(text, &edit, QueryEventChange::Display)
            }
            Key::Named(NamedKey::Space) if !command => {
                let edit = self.core.insert(" ");
                Self::apply(text, &edit, QueryEventChange::Display)
            }
            Key::Character(shortcut) if command => self.command_shortcut(shortcut.as_str(), text),
            Key::Character(character) if !command => {
                // `KeyEvent::text` captures composed multi-scalar input when
                // available. Synthetic events and some platform backends only
                // populate the logical character, which is still a valid
                // single-key insertion fallback.
                let inserted = event_text.as_deref().unwrap_or(character.as_str());
                let edit = self.core.insert(inserted);
                Self::apply(text, &edit, QueryEventChange::Ignored)
            }
            _ => QueryEventChange::Ignored,
        };
        *selection = self.core.selection();
        change
    }

    /// `ArrowLeft`/`ArrowRight`: by word with `command`, otherwise to the
    /// adjacent shaped (visual) caret stop, falling back to the logical
    /// grapheme step before the first layout. An unshifted arrow over a
    /// selection collapses it to its edge.
    fn arrow(&mut self, text: &str, shift: bool, command: bool, right: bool) {
        let motion = match (command, right) {
            (true, true) => Motion::WordRight,
            (true, false) => Motion::WordLeft,
            (false, true) => Motion::Right,
            (false, false) => Motion::Left,
        };
        let visual = (!command)
            .then(|| self.visual_motion(text, self.core.caret(), right))
            .flatten();
        match visual {
            Some(target) => self
                .core
                .motion_to(motion, TextPosition::new(target), shift),
            None => self.core.motion(text, motion, shift),
        }
    }

    /// Ctrl/Cmd chords: select-all, copy, cut, paste.
    fn command_shortcut(&mut self, shortcut: &str, text: &mut String) -> QueryEventChange {
        match shortcut {
            "a" | "A" => {
                self.core.select_all(text);
                QueryEventChange::Display
            }
            "c" | "C" => {
                if let Some(slice) = self.core.selected(text) {
                    clipboard_copy(slice);
                }
                QueryEventChange::Display
            }
            "x" | "X" => {
                let Some(slice) = self.core.selected(text) else {
                    return QueryEventChange::Display;
                };
                clipboard_copy(slice);
                let edit = self.core.delete_selection();
                Self::apply(text, &edit, QueryEventChange::Display)
            }
            "v" | "V" => match clipboard_paste() {
                Some(pasted) => {
                    let edit = self.core.insert(&pasted);
                    Self::apply(text, &edit, QueryEventChange::Display)
                }
                None => QueryEventChange::Display,
            },
            _ => QueryEventChange::Ignored,
        }
    }

    /// Display text including the composition, shown at the caret. The
    /// committed `text` is untouched until the IME commits.
    pub fn display_text(&mut self, text: &str, selection: &Selection) -> String {
        self.load(text, selection);
        self.core.display_text(text).into_owned()
    }

    fn signature(&self, display: &str, style: &TextStyle, text: &str) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hash = DefaultHasher::new();
        display.hash(&mut hash);
        style.size.to_bits().hash(&mut hash);
        style.line_height.to_bits().hash(&mut hash);
        style.weight.hash(&mut hash);
        style.italic.hash(&mut hash);
        style.family.hash(&mut hash);
        // The same visible string can be produced by committed text or by a
        // composition shown at the caret. Keep those layouts distinct
        // so an IME underline can neither linger nor move to a stale range.
        self.is_composing().hash(&mut hash);
        if let Some(range) = self.core.preedit_display_range(text) {
            range.hash(&mut hash);
        }
        hash.finish()
    }

    /// Paint an unwrapped query into a clipped viewport. The shaped caret is
    /// kept visible by horizontal scrolling; placeholder, selection and
    /// preedit use the same layout and baseline.
    pub fn draw(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        query: QueryView<'_>,
        rect: Rect,
        style: &TextStyle,
        ink: QueryInk,
    ) {
        let QueryView {
            text,
            selection,
            placeholder,
            show_caret,
        } = query;
        let transform = ctx.transform;
        let scene = &mut *ctx.scene;
        let renderer = &mut *ctx.text;
        self.load(text, selection);
        let composing = self.is_composing();
        let shows_placeholder = text.is_empty() && !composing;
        let display = if shows_placeholder {
            placeholder.to_string()
        } else {
            self.core.display_text(text).into_owned()
        };
        let signature = self.signature(&display, style, text);
        let epoch_stale = self
            .layout_epoch
            .as_ref()
            .is_none_or(|epoch| !Arc::ptr_eq(epoch, renderer.layout_epoch()));
        if self.layout.is_none() || self.signature != signature || epoch_stale {
            let browser_emoji = renderer.browser_color_emoji_installed();
            let mut layout = RichTextLayout::shape_plain_browser(
                &mut renderer.font_system,
                &display,
                style,
                None,
                browser_emoji,
            );
            renderer.note_browser_rich_layout(&layout);
            if let Some(range) = self.core.preedit_display_range(text) {
                layout.add_underline(range);
            }
            self.layout = Some(layout);
            self.layout_text.clone_from(&display);
            self.signature = signature;
            self.layout_epoch = Some(Arc::clone(renderer.layout_epoch()));
        }

        let caret_byte = if shows_placeholder {
            0
        } else {
            self.core.display_caret(text)
        };
        let Some(layout) = self.layout.as_ref() else {
            // Unreachable: the block above always leaves `self.layout`
            // populated before this point runs.
            return;
        };
        let (caret_x, caret_top, caret_height) = layout.caret_geometry(caret_byte);
        let scroll_x = if shows_placeholder {
            0.0
        } else {
            (f64::from(caret_x) - rect.width().max(1.0)).max(0.0)
        };
        let origin = Point::new(
            rect.x0 - scroll_x,
            rect.center().y - f64::from(layout.height()) / 2.0,
        );
        self.origin = origin;
        let caret = Rect::new(
            origin.x + f64::from(caret_x),
            origin.y + f64::from(caret_top),
            origin.x + f64::from(caret_x) + 2.0,
            origin.y + f64::from(caret_top + caret_height),
        );

        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &rect,
        );
        if !composing && !shows_placeholder {
            let range = selection.range();
            if !range.is_empty() {
                for (x0, y0, x1, y1) in layout.selection_rects(range.start, range.end) {
                    let band = Rect::new(
                        origin.x + f64::from(x0),
                        origin.y + f64::from(y0),
                        origin.x + f64::from(x1),
                        origin.y + f64::from(y1),
                    );
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        ink.selection,
                        None,
                        &band,
                    );
                }
            }
        }
        renderer.draw_rich(
            scene,
            layout,
            transform.pre_translate(origin.to_vec2()),
            if shows_placeholder {
                ink.placeholder
            } else {
                ink.text
            },
        );
        if show_caret {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                ink.caret,
                None,
                &caret,
            );
        }
        scene.pop_layer();
        self.caret = show_caret.then_some(caret);
    }

    /// Private text/selection equality without retaining a query snapshot.
    pub fn ime_edit_fingerprint(
        &self,
        text: &str,
        selection: &Selection,
        key: &[u8; 32],
    ) -> Option<[u8; 32]> {
        (!self.is_composing())
            .then(|| crate::ime::edit_fingerprint(text, selection.anchor, selection.cursor, key))
    }

    pub fn ime_cursor_rect(&self) -> Option<Rect> {
        self.caret
    }

    /// Move to the adjacent shaped caret stop when the retained layout still
    /// represents the committed query. Callers can fall back to logical
    /// grapheme movement before the first draw after an edit.
    pub fn visual_motion(&self, text: &str, byte: usize, right: bool) -> Option<usize> {
        if self.is_composing() || self.layout_text != text {
            return None;
        }
        self.layout
            .as_ref()
            .map(|layout| layout.visual_motion(byte, right))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn deferred_input_query_refuses_composition_and_pins_selection() {
        let mut composition = super::QueryComposition::new();
        let mut selection = crate::text_selection::Selection::collapsed(5);
        let stamp = composition
            .ime_edit_fingerprint("hello", &selection, &[4; 32])
            .unwrap();
        selection.anchor = 0;
        assert_ne!(
            composition
                .ime_edit_fingerprint("hello", &selection, &[4; 32])
                .unwrap(),
            stamp
        );
        composition.core.set_preedit("draft", Some((5, 5)));
        assert!(
            composition
                .ime_edit_fingerprint("hello", &selection, &[4; 32])
                .is_none()
        );
    }

    use super::*;
    use crate::text_render::{FontChoice, TextRenderer};
    use vello::Scene;
    use vello::kurbo::Affine;

    fn key(logical_key: Key, text: Option<&str>) -> WindowEvent {
        let event = crate::synthetic_input::key_event(
            logical_key,
            text.map(Into::into),
            ElementState::Pressed,
        );
        crate::synthetic_input::keyboard_input(event)
    }

    #[test]
    fn shared_query_editor_edits_selections_graphemes_and_words() {
        // Case `shared_editor_replaces_selection_and_deletes_extended_graphemes`.
        {
            let mut composition = QueryComposition::new();
            let mut text = "a👨‍👩‍👧‍👦z".to_string();
            let family_start = 1;
            let family_end = text.len() - 1;
            let mut selection = Selection {
                anchor: family_start,
                cursor: family_end,
            };
            assert_eq!(
                composition.handle_event(
                    &key(Key::Character("x".into()), Some("x")),
                    &mut text,
                    &mut selection,
                    false,
                    false,
                ),
                QueryEventChange::Edited
            );
            assert_eq!(text, "axz");
            assert_eq!(selection, Selection::collapsed(2));

            selection.move_to(2);
            assert_eq!(
                composition.handle_event(
                    &key(Key::Named(NamedKey::Backspace), None),
                    &mut text,
                    &mut selection,
                    false,
                    false,
                ),
                QueryEventChange::Edited
            );
            assert_eq!(text, "az");
        }

        // Case `shared_editor_supports_selection_collapse_shift_and_word_motion`.
        {
            let mut composition = QueryComposition::new();
            let mut text = "alpha beta".to_string();
            let mut selection = Selection {
                anchor: 2,
                cursor: 8,
            };
            composition.handle_event(
                &key(Key::Named(NamedKey::ArrowLeft), None),
                &mut text,
                &mut selection,
                false,
                false,
            );
            assert_eq!(selection, Selection::collapsed(2));

            composition.handle_event(
                &key(Key::Named(NamedKey::ArrowRight), None),
                &mut text,
                &mut selection,
                true,
                true,
            );
            assert_eq!(selection.anchor, 2);
            assert_eq!(selection.cursor, 6);
        }
    }

    #[test]
    fn query_preedit_is_display_only_until_commit_with_its_own_caret_and_layout() {
        // Case `preedit_shows_at_the_caret_until_commit_replaces_the_selection`.
        {
            let mut composition = QueryComposition::new();
            let mut text = "search old value".to_string();
            let mut selection = Selection {
                anchor: 7,
                cursor: 10,
            };
            assert_eq!(
                composition.handle_ime(
                    &Ime::Preedit("新".into(), Some(("新".len(), "新".len()))),
                    &mut text,
                    &mut selection,
                ),
                QueryImeChange::Display
            );
            assert_eq!(text, "search old value");
            assert_eq!(
                composition.display_text(&text, &selection),
                "search old新 value"
            );
            assert_eq!(
                composition.handle_ime(&Ime::Commit("新".into()), &mut text, &mut selection),
                QueryImeChange::Committed
            );
            assert_eq!(text, "search 新 value");
            assert_eq!(selection, Selection::collapsed("search 新".len()));
        }

        // Case `draw_publishes_an_internal_preedit_caret`.
        {
            let mut composition = QueryComposition::new();
            let mut text = "ab".to_string();
            let mut selection = Selection::collapsed(1);
            composition.handle_ime(
                &Ime::Preedit("漢字".into(), Some(("漢".len(), "漢".len()))),
                &mut text,
                &mut selection,
            );
            let mut renderer = TextRenderer::new();
            let mut scene = Scene::new();
            let rect = Rect::new(10.0, 10.0, 210.0, 42.0);
            composition.draw(
                &mut PaintCtx::new(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut renderer,
                    &crate::theme::Theme::dark(),
                ),
                QueryView {
                    text: &text,
                    selection: &selection,
                    placeholder: "Search…",
                    show_caret: true,
                },
                rect,
                &TextStyle::new(16.0).with_family(FontChoice::SansSerif),
                QueryInk {
                    text: Color::WHITE,
                    placeholder: Color::from_rgb8(128, 128, 128),
                    selection: Color::from_rgba8(80, 100, 255, 100),
                    caret: Color::WHITE,
                },
            );
            assert!(rect.contains(composition.ime_cursor_rect().unwrap().center()));
            assert_eq!(text, "ab");
        }

        // Case `equal_preedit_and_committed_text_do_not_share_an_underlined_layout`.
        {
            let mut composition = QueryComposition::new();
            let mut text = "a".to_string();
            let mut selection = Selection {
                anchor: 0,
                cursor: 1,
            };
            let mut renderer = TextRenderer::new();
            let mut scene = Scene::new();
            let rect = Rect::new(0.0, 0.0, 200.0, 32.0);
            let style = TextStyle::new(16.0).with_family(FontChoice::SansSerif);
            let draw = |composition: &mut QueryComposition,
                        scene: &mut Scene,
                        renderer: &mut TextRenderer,
                        text: &str,
                        selection: &Selection| {
                composition.draw(
                    &mut PaintCtx::new(
                        scene,
                        Affine::IDENTITY,
                        renderer,
                        &crate::theme::Theme::dark(),
                    ),
                    QueryView {
                        text,
                        selection,
                        placeholder: "Search…",
                        show_caret: true,
                    },
                    rect,
                    &style,
                    QueryInk {
                        text: Color::WHITE,
                        placeholder: Color::from_rgb8(128, 128, 128),
                        selection: Color::from_rgba8(80, 100, 255, 100),
                        caret: Color::WHITE,
                    },
                );
            };

            composition.handle_ime(
                &Ime::Preedit("a".into(), Some((1, 1))),
                &mut text,
                &mut selection,
            );
            draw(
                &mut composition,
                &mut scene,
                &mut renderer,
                &text,
                &selection,
            );
            let composing_signature = composition.signature;

            composition.handle_ime(&Ime::Disabled, &mut text, &mut selection);
            draw(
                &mut composition,
                &mut scene,
                &mut renderer,
                &text,
                &selection,
            );
            assert_ne!(composition.signature, composing_signature);
        }
    }

    #[test]
    fn typing_composing_and_backspacing_clusters_edits_the_exact_query() {
        let mut composition = QueryComposition::new();
        let mut text = String::new();
        let mut selection = Selection::collapsed(0);
        let press = |composition: &mut QueryComposition,
                     text: &mut String,
                     selection: &mut Selection,
                     event: WindowEvent,
                     shift: bool,
                     command: bool| {
            composition.handle_event(&event, text, selection, shift, command)
        };
        let typed = |s: &str| key(Key::Character(s.into()), Some(s));
        let named = |k: NamedKey| key(Key::Named(k), None);

        // A decomposed "é" arrives as two keystrokes; emoji and a flag as one.
        for s in ["e", "\u{301}", "👍🏽", "🇩🇪"] {
            assert_eq!(
                press(
                    &mut composition,
                    &mut text,
                    &mut selection,
                    typed(s),
                    false,
                    false
                ),
                QueryEventChange::Edited
            );
        }
        assert_eq!(text, "e\u{301}👍🏽🇩🇪");
        // A tab is a control character and never enters a one-line query.
        assert_eq!(
            press(
                &mut composition,
                &mut text,
                &mut selection,
                typed("\t"),
                false,
                false
            ),
            QueryEventChange::Ignored
        );
        assert_eq!(text, "e\u{301}👍🏽🇩🇪");

        // Backspace removes whole clusters: the flag, then the toned thumb.
        press(
            &mut composition,
            &mut text,
            &mut selection,
            named(NamedKey::Backspace),
            false,
            false,
        );
        assert_eq!(text, "e\u{301}👍🏽");
        press(
            &mut composition,
            &mut text,
            &mut selection,
            named(NamedKey::Backspace),
            false,
            false,
        );
        assert_eq!(text, "e\u{301}");
        // Left steps over the whole "é"; Delete removes it as one.
        press(
            &mut composition,
            &mut text,
            &mut selection,
            named(NamedKey::ArrowLeft),
            false,
            false,
        );
        assert_eq!(selection, Selection::collapsed(0));
        press(
            &mut composition,
            &mut text,
            &mut selection,
            named(NamedKey::Delete),
            false,
            false,
        );
        assert_eq!(text, "");

        // IME: the composition is display-only, the commit lands at the caret.
        for s in ["a", "b"] {
            press(
                &mut composition,
                &mut text,
                &mut selection,
                typed(s),
                false,
                false,
            );
        }
        press(
            &mut composition,
            &mut text,
            &mut selection,
            named(NamedKey::ArrowLeft),
            false,
            false,
        );
        assert_eq!(
            press(
                &mut composition,
                &mut text,
                &mut selection,
                WindowEvent::Ime(Ime::Preedit("か".into(), Some((3, 3)))),
                false,
                false,
            ),
            QueryEventChange::Display
        );
        assert_eq!(text, "ab");
        assert_eq!(composition.display_text(&text, &selection), "aかb");
        assert_eq!(
            press(
                &mut composition,
                &mut text,
                &mut selection,
                WindowEvent::Ime(Ime::Commit("火".into())),
                false,
                false,
            ),
            QueryEventChange::Edited
        );
        assert_eq!(text, "a火b");
        assert!(!composition.is_composing());
        assert_eq!(selection, Selection::collapsed("a火".len()));

        // Shift+Home selects back to the start; typing replaces the selection.
        press(
            &mut composition,
            &mut text,
            &mut selection,
            named(NamedKey::Home),
            true,
            false,
        );
        assert_eq!(selection.range(), 0.."a火".len());
        press(
            &mut composition,
            &mut text,
            &mut selection,
            typed("z"),
            false,
            false,
        );
        assert_eq!(text, "zb");
        assert_eq!(selection, Selection::collapsed(1));

        // Ctrl+A then Backspace clears; Ctrl+Backspace removes a word.
        text = "find these words".to_owned();
        selection = Selection::collapsed(text.len());
        press(
            &mut composition,
            &mut text,
            &mut selection,
            named(NamedKey::Backspace),
            false,
            true,
        );
        assert_eq!(text, "find these ");
        press(
            &mut composition,
            &mut text,
            &mut selection,
            typed("a"),
            false,
            true,
        );
        assert_eq!(selection.range(), 0..text.len());
        press(
            &mut composition,
            &mut text,
            &mut selection,
            named(NamedKey::Backspace),
            false,
            false,
        );
        assert_eq!(text, "");
        assert_eq!(
            press(
                &mut composition,
                &mut text,
                &mut selection,
                named(NamedKey::Backspace),
                false,
                false
            ),
            QueryEventChange::Display
        );
    }

    #[test]
    fn unchanged_browser_query_reshapes_after_font_failure_and_install() {
        let root = crate::font_catalog::checkout_root();
        let fonts = crate::font_catalog::FontCatalog::product("", "en-US")
            .load_browser_startup_bytes(|name| std::fs::read(root.join(name)).ok())
            .unwrap();
        let mut renderer = TextRenderer::with_browser_font_system(fonts);
        let mut composition = QueryComposition::new();
        let text = "مَرْحَبًا".to_owned();
        let selection = Selection::collapsed(text.len());
        let mut scene = Scene::new();
        let style = TextStyle::new(16.0).with_family(FontChoice::SansSerif);
        let mut draw = |composition: &mut QueryComposition, renderer: &mut TextRenderer| {
            composition.draw(
                &mut PaintCtx::new(
                    &mut scene,
                    Affine::IDENTITY,
                    renderer,
                    &crate::theme::Theme::dark(),
                ),
                QueryView {
                    text: &text,
                    selection: &selection,
                    placeholder: "",
                    show_caret: true,
                },
                Rect::new(0.0, 0.0, 220.0, 36.0),
                &style,
                QueryInk {
                    text: Color::WHITE,
                    placeholder: Color::WHITE,
                    selection: Color::WHITE,
                    caret: Color::WHITE,
                },
            );
        };
        draw(&mut composition, &mut renderer);
        assert_eq!(
            renderer.take_browser_font_demands(),
            ["NotoSansArabic[wght].ttf"]
        );
        renderer.browser_font_failed();
        draw(&mut composition, &mut renderer);
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
        draw(&mut composition, &mut renderer);
        assert!(
            composition
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
