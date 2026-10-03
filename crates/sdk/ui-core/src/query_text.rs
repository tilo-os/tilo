//! Shared visual/IME state for transient search and command queries.
//!
//! Query owners keep their committed `String` and filtering semantics. This
//! helper makes the editable presentation behave like every other toolkit text
//! surface: one shaped layout for ink, selection, caret and candidate geometry,
//! with IME preedit excluded from application state until commit.

use std::ops::Range;
use std::sync::Arc;

use vello::kurbo::{Point, Rect};
use vello::peniko::Color;
use winit::event::{ElementState, Ime, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::paint::PaintCtx;
use crate::richtext::RichTextLayout;
use crate::text_render::TextStyle;
use crate::text_selection::{
    Selection, clipboard_copy, clipboard_paste, next_char_boundary, next_word_boundary,
    prev_char_boundary, prev_word_boundary,
};

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
    /// Caret in `accent`, selection bands in `accent` at 30% alpha.
    pub fn accented(text: Color, placeholder: Color, accent: Color) -> Self {
        Self {
            text,
            placeholder,
            selection: accent.with_alpha_factor(0.3),
            caret: accent,
        }
    }
}

/// Retained shaping and IME state for an inline query editor.
pub struct QueryComposition {
    preedit: String,
    preedit_cursor: Option<(usize, usize)>,
    layout: Option<RichTextLayout>,
    layout_text: String,
    signature: u64,
    layout_epoch: Option<Arc<()>>,
    caret: Option<Rect>,
}

impl Default for QueryComposition {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryComposition {
    pub fn new() -> Self {
        Self {
            preedit: String::new(),
            preedit_cursor: None,
            layout: None,
            layout_text: String::new(),
            signature: u64::MAX,
            layout_epoch: None,
            caret: None,
        }
    }

    pub fn is_composing(&self) -> bool {
        !self.preedit.is_empty()
    }

    pub fn clear(&mut self) {
        self.preedit.clear();
        self.preedit_cursor = None;
        self.layout = None;
        self.layout_text.clear();
        self.caret = None;
        self.signature = u64::MAX;
        self.layout_epoch = None;
    }

    /// Apply one platform IME event. Preedit changes only this helper; commit
    /// replaces the owner's current selection as one atomic string edit.
    pub fn handle_ime(
        &mut self,
        ime: &Ime,
        text: &mut String,
        selection: &mut Selection,
    ) -> QueryImeChange {
        match ime {
            Ime::Preedit(preedit, cursor) => {
                self.preedit.clone_from(preedit);
                self.preedit_cursor = *cursor;
                QueryImeChange::Display
            }
            Ime::Commit(committed) => {
                let committed: String = committed
                    .chars()
                    .filter(|character| !character.is_control())
                    .collect();
                let range = selection.range();
                text.replace_range(range.clone(), &committed);
                *selection = Selection::collapsed(range.start.saturating_add(committed.len()));
                self.preedit.clear();
                self.preedit_cursor = None;
                QueryImeChange::Committed
            }
            Ime::Disabled => {
                self.preedit.clear();
                self.preedit_cursor = None;
                QueryImeChange::Display
            }
            Ime::Enabled => QueryImeChange::Display,
        }
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

        match logical_key {
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight) => {
                let right = matches!(logical_key, Key::Named(NamedKey::ArrowRight));
                self.handle_arrow_key(text, selection, shift, command, right)
            }
            Key::Named(NamedKey::Home | NamedKey::End) => {
                let next = if matches!(logical_key, Key::Named(NamedKey::Home)) {
                    0
                } else {
                    text.len()
                };
                if shift {
                    selection.extend_to(next);
                } else {
                    selection.move_to(next);
                }
                QueryEventChange::Display
            }
            Key::Named(NamedKey::Backspace) => handle_backspace(text, selection, command),
            Key::Named(NamedKey::Delete) => handle_delete_key(text, selection, command),
            Key::Named(NamedKey::Space) if !command => {
                if replace_selection(text, selection, " ") {
                    QueryEventChange::Edited
                } else {
                    QueryEventChange::Display
                }
            }
            Key::Character(shortcut) if command => {
                handle_command_shortcut(shortcut.as_str(), text, selection)
            }
            Key::Character(character) if !command => {
                // `KeyEvent::text` captures composed multi-scalar input when
                // available. Synthetic events and some platform backends only
                // populate the logical character, which is still a valid
                // single-key insertion fallback.
                let inserted = event_text.as_deref().unwrap_or(character.as_str());
                if replace_selection(text, selection, inserted) {
                    QueryEventChange::Edited
                } else {
                    QueryEventChange::Ignored
                }
            }
            _ => QueryEventChange::Ignored,
        }
    }

    /// `ArrowLeft`/`ArrowRight` handling for [`handle_event`](Self::handle_event):
    /// collapse an existing selection to its edge, or otherwise move by shaped
    /// visual caret stop, word or character boundary.
    fn handle_arrow_key(
        &self,
        text: &str,
        selection: &mut Selection,
        shift: bool,
        command: bool,
        right: bool,
    ) -> QueryEventChange {
        if !shift && !selection.is_collapsed() {
            let edge = if right {
                selection.max()
            } else {
                selection.min()
            };
            selection.move_to(edge);
            return QueryEventChange::Display;
        }
        let cursor = selection.cursor;
        let next = if command {
            if right {
                next_word_boundary(text, cursor)
            } else {
                prev_word_boundary(text, cursor)
            }
        } else {
            self.visual_motion(text, cursor, right).unwrap_or_else(|| {
                if right {
                    next_char_boundary(text, cursor)
                } else {
                    prev_char_boundary(text, cursor)
                }
            })
        };
        if shift {
            selection.extend_to(next);
        } else {
            selection.move_to(next);
        }
        QueryEventChange::Display
    }

    /// Display text including composition. While composing, the committed
    /// selection is visually replaced but remains untouched in `text`.
    pub fn display_text(&self, text: &str, selection: &Selection) -> String {
        if self.preedit.is_empty() {
            return text.to_string();
        }
        let range = selection.range();
        let mut display = String::with_capacity(
            text.len()
                .saturating_sub(range.len())
                .saturating_add(self.preedit.len()),
        );
        display.push_str(text.get(..range.start).unwrap_or(""));
        display.push_str(&self.preedit);
        display.push_str(text.get(range.end..).unwrap_or(""));
        display
    }

    fn preedit_range(&self, selection: &Selection) -> Range<usize> {
        let start = selection.range().start;
        start..start.saturating_add(self.preedit.len())
    }

    fn display_caret(&self, selection: &Selection) -> usize {
        if self.preedit.is_empty() {
            return selection.cursor;
        }
        let start = selection.range().start;
        let offset = self
            .preedit_cursor
            .map_or(self.preedit.len(), |(_, end)| end)
            .min(self.preedit.len());
        start.saturating_add(self.preedit.floor_char_boundary(offset))
    }

    fn signature(&self, display: &str, style: &TextStyle, selection: &Selection) -> u64 {
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
        // preedit replacing an equal selection. Keep those layouts distinct
        // so an IME underline can neither linger nor move to a stale range.
        self.is_composing().hash(&mut hash);
        if self.is_composing() {
            self.preedit_range(selection).hash(&mut hash);
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
        let composing = !self.preedit.is_empty();
        let shows_placeholder = text.is_empty() && !composing;
        let display = if shows_placeholder {
            placeholder.to_string()
        } else {
            self.display_text(text, selection)
        };
        let signature = self.signature(&display, style, selection);
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
            if composing {
                layout.add_underline(self.preedit_range(selection));
            }
            self.layout = Some(layout);
            self.layout_text.clone_from(&display);
            self.signature = signature;
            self.layout_epoch = Some(Arc::clone(renderer.layout_epoch()));
        }

        let caret_byte = if shows_placeholder {
            0
        } else {
            self.display_caret(selection)
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

/// Replace the current selection with `inserted` (control characters
/// filtered) and collapse the selection to the end of the inserted text.
/// Returns `false` (no-op) when there is nothing left to insert.
fn replace_selection(text: &mut String, selection: &mut Selection, inserted: &str) -> bool {
    let inserted: String = inserted
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    if inserted.is_empty() {
        return false;
    }
    let range = selection.range();
    text.replace_range(range.clone(), &inserted);
    *selection = Selection::collapsed(range.start.saturating_add(inserted.len()));
    true
}

/// Delete the current selection in place. Returns `false` (no-op) when the
/// selection is already collapsed.
fn delete_selection(text: &mut String, selection: &mut Selection) -> bool {
    if selection.is_collapsed() {
        return false;
    }
    let range = selection.range();
    text.replace_range(range.clone(), "");
    *selection = Selection::collapsed(range.start);
    true
}

/// `Backspace` handling for [`QueryComposition::handle_event`].
fn handle_backspace(
    text: &mut String,
    selection: &mut Selection,
    command: bool,
) -> QueryEventChange {
    if delete_selection(text, selection) {
        return QueryEventChange::Edited;
    }
    let cursor = selection.cursor;
    if cursor == 0 {
        return QueryEventChange::Display;
    }
    let previous = if command {
        prev_word_boundary(text, cursor)
    } else {
        prev_char_boundary(text, cursor)
    };
    text.replace_range(previous..cursor, "");
    selection.move_to(previous);
    QueryEventChange::Edited
}

/// `Delete` handling for [`QueryComposition::handle_event`].
fn handle_delete_key(
    text: &mut String,
    selection: &mut Selection,
    command: bool,
) -> QueryEventChange {
    if delete_selection(text, selection) {
        return QueryEventChange::Edited;
    }
    let cursor = selection.cursor;
    if cursor == text.len() {
        return QueryEventChange::Display;
    }
    let next = if command {
        next_word_boundary(text, cursor)
    } else {
        next_char_boundary(text, cursor)
    };
    text.replace_range(cursor..next, "");
    QueryEventChange::Edited
}

/// Ctrl/Cmd-chord handling (select-all, copy, cut, paste) for
/// [`QueryComposition::handle_event`].
fn handle_command_shortcut(
    shortcut: &str,
    text: &mut String,
    selection: &mut Selection,
) -> QueryEventChange {
    match shortcut {
        "a" | "A" => {
            selection.anchor = 0;
            selection.cursor = text.len();
            QueryEventChange::Display
        }
        "c" | "C" => {
            let range = selection.range();
            if let Some(slice) = (!range.is_empty()).then(|| text.get(range)).flatten() {
                clipboard_copy(slice);
            }
            QueryEventChange::Display
        }
        "x" | "X" => {
            let range = selection.range();
            if range.is_empty() {
                QueryEventChange::Display
            } else {
                if let Some(slice) = text.get(range) {
                    clipboard_copy(slice);
                }
                delete_selection(text, selection);
                QueryEventChange::Edited
            }
        }
        "v" | "V" => match clipboard_paste() {
            Some(pasted) if replace_selection(text, selection, &pasted) => QueryEventChange::Edited,
            _ => QueryEventChange::Display,
        },
        _ => QueryEventChange::Ignored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_render::{FontChoice, TextRenderer};
    use vello::Scene;
    use vello::kurbo::Affine;

    fn key(logical_key: Key, text: Option<&str>) -> WindowEvent {
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        event.logical_key = logical_key;
        event.text = text.map(Into::into);
        event.location = winit::keyboard::KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.repeat = false;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event,
            is_synthetic: false,
        }
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
        // Case `preedit_replaces_selection_only_in_the_display_until_commit`.
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
                "search 新 value"
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
    fn unchanged_browser_query_reshapes_after_font_failure_and_install() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts");
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
