//! The reader: one presentation of a [`Document`] for every app that shows
//! long-form text.
//!
//! [`ReaderView`] is what an app's reading pane holds. It sets the document
//! in the person's [`ReadingPreferences`]: a centred measure of 60 to 72
//! characters with margins that grow with the window, on the system's page
//! or a warm paper. It scrolls at the display's rate and pages smoothly by
//! one screen less a line with Space and Shift+Space, shows its progress
//! quietly (a thin line and the minutes left at this person's reading
//! speed), opens footnotes in place and pictures full-window, shows where
//! links go, and finds in place for the window's search. It reports where
//! the person is ([`ReadingPlace`]) for the app to keep and restore, and
//! what they asked of a passage they selected ([`ReaderEvent`]).
//!
//! It is a presentation only: the app owns the document and what is kept
//! about it (the reading position, highlights and notes), and routes its
//! pane's painting, input and accessibility here.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::document::{Block, Document, spans_text};
use super::flow::{FlowKey, FlowLayout, FlowRequest, Typesetting, layout_flow};
use super::paint::{FlowHits, FlowPaint, FlowRange, char_at, paint_flow};
use super::palette::ReaderPalette;
use super::preferences::{Measure, ReadingFont, ReadingPreferences};
use crate::a11y::{AccessNode, AccessState, Role};
use crate::color_compat::ColorCompat;
use crate::image_store::ImageStore;
use crate::motion::{Direction, MotionValue, Snap};
use crate::paint::PaintCtx;
use crate::text_render::{FontChoice, TextRenderer};
use crate::theme::Theme;
use crate::time::{Duration, Instant};
use crate::tokens::{MotionSource, TextRole};
use crate::widget::ScrollRegion;
use crate::widget::find_bar::{FindMatch, FindOptions, Findable};

/// Where a person is in a document: the body block at the top of the
/// screen and how far into it, with the share of the whole as a fallback
/// when the text changed under it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ReadingPlace {
    /// The index of the block in [`Document::blocks`].
    pub block: u32,
    /// How far down that block, 0 to 1.
    pub fraction: f32,
    /// How far through the whole document, 0 to 1.
    pub progress: f32,
}

/// A passage the person selected, by the body block it is in and the
/// characters of that block's text it covers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Passage {
    /// The index of the block in [`Document::blocks`].
    pub block: u32,
    pub start: u32,
    pub end: u32,
    /// What it says.
    pub text: String,
}

/// A highlight to paint over a passage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    /// The index of the block in [`Document::blocks`].
    pub block: u32,
    pub start: u32,
    pub end: u32,
    /// An index into [`HIGHLIGHT_COLORS`].
    pub color: usize,
}

/// The colours a passage can be highlighted in, and what they are called.
pub const HIGHLIGHT_COLORS: [(&str, [u8; 3]); 4] = [
    ("Yellow", [0xF5, 0xD0, 0x4C]),
    ("Green", [0x7E, 0xD3, 0x8B]),
    ("Blue", [0x7F, 0xB6, 0xF0]),
    ("Pink", [0xF0, 0x8F, 0xB6]),
];

/// What the person asked of the reader that the app carries out.
#[derive(Clone, Debug, PartialEq)]
pub enum ReaderEvent {
    /// Open a link's address.
    OpenLink(String),
    /// Highlight a passage in a colour of [`HIGHLIGHT_COLORS`].
    Highlight { passage: Passage, color: usize },
    /// Write a note on a passage.
    Note { passage: Passage },
    /// Share a passage.
    Share { passage: Passage },
}

/// Where the measure sits in a reader `area_width` wide.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReaderGeometry {
    /// The measure's left edge, from the area's.
    pub left: f64,
    /// The measure's width.
    pub measure: f32,
    /// How wide a large picture may bleed, centred on the measure.
    pub bleed: f32,
}

/// The least margin either side of the text.
pub fn min_margin(theme: &Theme, area_width: f64) -> f64 {
    f64::from(theme.tokens.spacing.s4).max(area_width * 0.04)
}

/// The measure for `measure` characters of `char_width` average advance in
/// an area `area_width` wide: never longer than the person chose, as long
/// as the window allows with `margin` either side, and the margins take the
/// rest.
#[must_use]
pub fn reader_geometry(
    area_width: f64,
    char_width: f32,
    measure: Measure,
    margin: f64,
) -> ReaderGeometry {
    let available = crate::num::f32_of_f64((area_width - margin * 2.0).max(0.0));
    let wanted = char_width * measure.chars();
    let width = wanted.min(available).max(0.0);
    let left = (area_width - f64::from(width)) / 2.0;
    ReaderGeometry {
        left,
        measure: width,
        bleed: available.min(width * 1.5).max(width),
    }
}

/// The average advance of a character of running text in `style`, spaces
/// included: what a measure in characters is counted in.
#[must_use]
pub fn average_char_width(text: &mut TextRenderer, style: &crate::text_render::TextStyle) -> f32 {
    const SAMPLE: &str = "The quick brown fox jumps over the lazy dog, and reads on.";
    let width = text.measure_style(SAMPLE, None, style).0;
    width / crate::num::f32_of_f64(crate::num::f64_of_usize(SAMPLE.chars().count()))
}

/// A point in the flow: a laid block and a character of its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FlowPoint {
    block: usize,
    char: usize,
}

/// A footnote open in place, under its reference.
#[derive(Clone, Copy, Debug, PartialEq)]
struct OpenNote {
    note: u16,
    anchor: Rect,
}

/// How fast this person reads, measured while they read.
#[derive(Clone, Copy, Debug)]
struct Speed {
    words_per_minute: f32,
    sample: Option<(Instant, usize)>,
    /// Whether a stretch of reading was measured yet.
    measured: bool,
}

impl Speed {
    /// The shortest stretch of reading a speed is measured over.
    const MIN_SAMPLE: Duration = Duration::from_secs(20);
    /// The longest; past it the person was away, not reading.
    const MAX_SAMPLE: Duration = Duration::from_secs(600);

    /// They are at word `word` at `now`.
    fn note(&mut self, now: Instant, word: usize) {
        let Some((since, from)) = self.sample else {
            self.sample = Some((now, word));
            return;
        };
        let elapsed = now.saturating_duration_since(since);
        if elapsed < Self::MIN_SAMPLE {
            return;
        }
        self.sample = Some((now, word));
        if elapsed > Self::MAX_SAMPLE || word <= from {
            return;
        }
        let words = crate::num::f32_of_f64(crate::num::f64_of_usize(word - from));
        let minutes = crate::num::f32_of_f64(elapsed.as_secs_f64() / 60.0);
        let rate = words / minutes.max(1e-3);
        if words >= 50.0 && ReadingPreferences::WPM_RANGE.contains(&rate) {
            self.words_per_minute = self.words_per_minute * 0.8 + rate * 0.2;
            self.measured = true;
        }
    }
}

/// See the [module docs](self).
pub struct ReaderView {
    key: Option<String>,
    document: Option<Rc<Document>>,
    /// The blocks laid out: the header, then the body.
    blocks: Vec<Block>,
    /// How many of `blocks` are the header.
    header_len: usize,
    content_sig: u64,
    revision: u64,
    prefs: ReadingPreferences,
    publisher_face: Option<FontChoice>,
    images: Option<ImageStore>,
    layout: Option<FlowLayout>,
    /// Words before each laid block, and in all.
    word_prefix: Vec<usize>,
    words: usize,
    geometry: ReaderGeometry,
    base_family: FontChoice,
    area: Rect,
    top_pad: f64,
    bottom_pad: f64,
    line_height: f64,
    /// The renderer's text scale at the last paint, for measuring between
    /// paints as the paint did.
    text_scale: f32,
    scroller: ScrollRegion,
    glide: Option<MotionValue<f64>>,
    hscroll: HashMap<usize, f32>,
    /// A place to show once the document is laid out.
    pending_place: Option<ReadingPlace>,
    /// What the screen showed before a new copy of the same document
    /// arrived: the text of the block at the top and how far into it.
    refill_anchor: Option<(String, ReadingPlace)>,
    palette: Option<ReaderPalette>,
    page_theme: Theme,
    hits: FlowHits,
    pointer: Point,
    hovered_link: Option<Arc<str>>,
    find: Vec<FlowRange>,
    current_find: Option<usize>,
    note: Option<OpenNote>,
    lightbox: Option<String>,
    selection: Option<(FlowPoint, FlowPoint)>,
    selecting: bool,
    hud: crate::reader_hud::SelectionHud,
    marks: Vec<Mark>,
    speed: Speed,
    events: Vec<ReaderEvent>,
}

impl Default for ReaderView {
    fn default() -> Self {
        Self::new()
    }
}

impl ReaderView {
    #[must_use]
    pub fn new() -> Self {
        let prefs = ReadingPreferences::default();
        Self {
            key: None,
            document: None,
            blocks: Vec::new(),
            header_len: 0,
            content_sig: 0,
            revision: 0,
            prefs,
            publisher_face: None,
            images: None,
            layout: None,
            word_prefix: Vec::new(),
            words: 0,
            geometry: ReaderGeometry {
                left: 0.0,
                measure: 0.0,
                bleed: 0.0,
            },
            base_family: FontChoice::Serif,
            area: Rect::ZERO,
            top_pad: 0.0,
            bottom_pad: 0.0,
            line_height: 0.0,
            text_scale: 1.0,
            scroller: ScrollRegion::vertical(),
            glide: None,
            hscroll: HashMap::new(),
            pending_place: None,
            refill_anchor: None,
            palette: None,
            page_theme: Theme::light(),
            hits: FlowHits::default(),
            pointer: Point::new(f64::NAN, f64::NAN),
            hovered_link: None,
            find: Vec::new(),
            current_find: None,
            note: None,
            lightbox: None,
            selection: None,
            selecting: false,
            hud: crate::reader_hud::SelectionHud::default(),
            marks: Vec::new(),
            speed: Speed {
                words_per_minute: prefs.words_per_minute,
                sample: None,
                measured: false,
            },
            events: Vec::new(),
        }
    }

    // --- what is shown ------------------------------------------------------

    /// Show `document`, known by `key` (an article's id, a book's chapter).
    ///
    /// A new key opens at `place` (where the person left it) or the top. The
    /// same key with new content (the full article arriving after the feed's
    /// excerpt) keeps the passage on screen where it is, so the text never
    /// jumps under the reader's eyes.
    pub fn set_document(&mut self, key: &str, document: Rc<Document>, place: Option<ReadingPlace>) {
        let same_key = self.key.as_deref() == Some(key);
        if same_key
            && self
                .document
                .as_ref()
                .is_some_and(|shown| Rc::ptr_eq(shown, &document) || **shown == *document)
        {
            return;
        }
        if same_key {
            // The block on top now, by what it says.
            self.refill_anchor = self.place().and_then(|place| {
                self.document
                    .as_ref()
                    .and_then(|doc| doc.blocks.get(usize_of(place.block)))
                    .map(|block| (block.plain_text(), place))
            });
        } else {
            self.refill_anchor = None;
            self.pending_place = place;
            self.scroller.set_offset(Vec2::ZERO);
            self.glide = None;
            self.hscroll.clear();
            self.selection = None;
            self.note = None;
            self.lightbox = None;
            self.marks.clear();
            self.speed.sample = None;
        }
        let date = document
            .published
            .filter(|at| *at > 0)
            .map(|at| crate::format::date_of(&crate::format::UnixSecs(at)));
        self.blocks = document.presented_blocks(date);
        self.header_len = self
            .blocks
            .iter()
            .take_while(|block| {
                matches!(block, Block::Heading { level: 0, .. } | Block::Meta { .. })
            })
            .count();
        self.content_sig = signature(&self.blocks);
        self.key = Some(key.to_owned());
        self.document = Some(document);
        self.layout = None;
        self.find.clear();
        self.current_find = None;
        self.revision = self.revision.wrapping_add(1);
    }

    /// Show nothing.
    pub fn clear(&mut self) {
        *self = Self {
            prefs: self.prefs,
            images: self.images.take(),
            speed: self.speed,
            ..Self::new()
        };
    }

    /// The key of the document shown.
    #[must_use]
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    /// The document shown.
    #[must_use]
    pub fn document(&self) -> Option<&Rc<Document>> {
        self.document.as_ref()
    }

    /// Set text in `prefs` from now on; the passage on screen stays there.
    pub fn set_preferences(&mut self, prefs: ReadingPreferences) {
        let prefs = prefs.clamped();
        if prefs == self.prefs {
            return;
        }
        // Only what sets the text moves it; a new paper or speed does not.
        let relayout = (prefs.size - self.prefs.size).abs() > f32::EPSILON
            || prefs.font != self.prefs.font
            || prefs.measure != self.prefs.measure
            || prefs.justify != self.prefs.justify;
        if relayout && self.layout.is_some() && self.pending_place.is_none() {
            self.pending_place = self.place();
        }
        if !self.speed.measured {
            self.speed.words_per_minute = prefs.words_per_minute;
        }
        self.prefs = prefs;
    }

    /// The preferences text is set in.
    #[must_use]
    pub fn preferences(&self) -> ReadingPreferences {
        self.prefs
    }

    /// The document's own face, for [`ReadingFont::Publisher`].
    pub fn set_publisher_face(&mut self, face: Option<FontChoice>) {
        self.publisher_face = face;
    }

    /// Where the document's pictures come from.
    pub fn set_images(&mut self, images: ImageStore) {
        self.images = Some(images);
    }

    /// The person's highlights in this document.
    pub fn set_marks(&mut self, marks: Vec<Mark>) {
        self.marks = marks;
    }

    /// What the person asked for since the last call.
    pub fn take_events(&mut self) -> Vec<ReaderEvent> {
        std::mem::take(&mut self.events)
    }

    /// How far the reader is scrolled, in logical pixels.
    #[must_use]
    pub fn offset(&self) -> f64 {
        self.scroller.offset().y
    }

    /// Where the measure sat at the last paint.
    #[must_use]
    pub fn geometry(&self) -> ReaderGeometry {
        self.geometry
    }

    /// The flow as last laid out.
    #[must_use]
    pub fn layout(&self) -> Option<&FlowLayout> {
        self.layout.as_ref()
    }

    /// Whether the document's title has scrolled out of sight, so the
    /// window's bar may name it.
    #[must_use]
    pub fn title_scrolled_away(&self) -> bool {
        let Some(layout) = &self.layout else {
            return false;
        };
        let header_bottom = self
            .header_len
            .checked_sub(1)
            .and_then(|last| layout.blocks.get(last))
            .map_or(0.0, |block| f64::from(block.bottom));
        header_bottom > 0.0 && self.scroller.offset().y > self.top_pad + header_bottom
    }

    /// The height of one line of body text at the last paint.
    #[must_use]
    pub fn line_height(&self) -> f64 {
        self.line_height
    }

    /// How fast the person reads, once a stretch of reading was measured.
    #[must_use]
    pub fn measured_words_per_minute(&self) -> Option<f32> {
        self.speed.measured.then_some(self.speed.words_per_minute)
    }

    // --- where the person is -------------------------------------------------

    /// Where the person is, once the document is laid out.
    #[must_use]
    pub fn place(&self) -> Option<ReadingPlace> {
        let layout = self.layout.as_ref()?;
        let top = crate::num::f32_of_f64((self.scroller.offset().y - self.top_pad).max(0.0));
        let at = layout
            .blocks
            .iter()
            .enumerate()
            .skip(self.header_len)
            .find(|(_, block)| block.bottom > top)
            .or_else(|| layout.blocks.iter().enumerate().last())?;
        let (index, block) = at;
        let height = (block.bottom - block.top).max(1.0);
        let fraction = ((top - block.top) / height).clamp(0.0, 1.0);
        Some(ReadingPlace {
            block: u32::try_from(index.saturating_sub(self.header_len)).unwrap_or(u32::MAX),
            fraction,
            progress: crate::num::f32_of_f64(self.progress()),
        })
    }

    /// How far through the document the person is, 0 to 1.
    #[must_use]
    pub fn progress(&self) -> f64 {
        let content = self.content_height();
        let visible = self.area.height();
        if content <= visible || content <= 0.0 {
            return if self.layout.is_some() { 1.0 } else { 0.0 };
        }
        (self.scroller.offset().y / (content - visible)).clamp(0.0, 1.0)
    }

    /// Whole minutes left at this person's speed; `None` before layout and
    /// at the end.
    #[must_use]
    pub fn minutes_left(&self) -> Option<u32> {
        let layout = self.layout.as_ref()?;
        if self.progress() >= 0.999 {
            return None;
        }
        let top = crate::num::f32_of_f64((self.scroller.offset().y - self.top_pad).max(0.0));
        let index = layout.blocks.partition_point(|block| block.bottom <= top);
        let read = self.word_prefix.get(index).copied().unwrap_or(self.words);
        let left = self.words.saturating_sub(read);
        let minutes = crate::num::f32_of_f64(crate::num::f64_of_usize(left))
            / self.speed.words_per_minute.max(1.0);
        Some(crate::num::u32_of_f64(f64::from(minutes.ceil())))
    }

    fn content_height(&self) -> f64 {
        self.layout.as_ref().map_or(0.0, |layout| {
            self.top_pad + f64::from(layout.total_height) + self.bottom_pad
        })
    }

    /// The scroll offset that puts `place` at the top of the screen.
    fn offset_for(&self, place: ReadingPlace) -> Option<f64> {
        let layout = self.layout.as_ref()?;
        let index = self.header_len.saturating_add(usize_of(place.block));
        let y = match layout.blocks.get(index) {
            Some(block) if place.block > 0 || place.fraction > 0.0 => {
                block.top + (block.bottom - block.top) * place.fraction.clamp(0.0, 1.0)
            }
            Some(_) => 0.0,
            None => layout.total_height * place.progress.clamp(0.0, 1.0),
        };
        if y <= 0.0 {
            return Some(0.0);
        }
        Some(self.top_pad + f64::from(y))
    }

    /// Scroll to `place` now.
    pub fn show_place(&mut self, place: ReadingPlace) {
        if self.layout.is_none() {
            self.pending_place = Some(place);
            return;
        }
        if let Some(offset) = self.offset_for(place) {
            self.glide = None;
            self.scroller.set_offset(Vec2::new(0.0, offset));
        }
    }

    // --- paging --------------------------------------------------------------

    /// How far one page moves: the screen, less one line, so the line that
    /// was last is first.
    #[must_use]
    pub fn page_step(&self) -> f64 {
        (self.area.height() - self.line_height).max(self.line_height)
    }

    /// Page down (`forward`) or up by one screen less a line, gliding.
    pub fn page(&mut self, forward: bool, motion: crate::tokens::EffectiveMotion) {
        let now = crate::motion::frame_now();
        let from = self
            .glide
            .as_ref()
            .map_or(self.scroller.offset().y, |glide| glide.target());
        let step = self.page_step();
        let max = (self.content_height() - self.area.height()).max(0.0);
        let to = (from + if forward { step } else { -step }).clamp(0.0, max);
        let mut glide = self
            .glide
            .take()
            .unwrap_or_else(|| MotionValue::new(self.scroller.offset().y).with_tolerance(0.5));
        glide.set(
            to,
            Snap::WHEEL.timing(Direction::Enter, motion, MotionSource::Keyboard),
            now,
        );
        if glide.is_moving(now) {
            self.glide = Some(glide);
        } else {
            self.scroller.set_offset(Vec2::new(0.0, to));
        }
    }

    /// The reader's keys: Space and Shift+Space page, Page Up/Down too, Home
    /// and End go to either end, Escape closes a note or a picture, Ctrl+C
    /// copies the selection. Returns whether the key was the reader's.
    pub fn key(&mut self, key: &Key, modifiers: ModifiersState) -> bool {
        let motion = self.page_theme.motion();
        let ctrl = modifiers.control_key() || modifiers.super_key();
        match key {
            Key::Named(NamedKey::Escape) if self.lightbox.is_some() || self.note.is_some() => {
                self.lightbox = None;
                self.note = None;
            }
            Key::Named(NamedKey::Escape) if self.selection.is_some() => self.selection = None,
            Key::Named(NamedKey::Space) if !ctrl => self.page(!modifiers.shift_key(), motion),
            Key::Named(NamedKey::PageDown) => self.page(true, motion),
            Key::Named(NamedKey::PageUp) => self.page(false, motion),
            Key::Named(NamedKey::Home) if !ctrl => {
                self.glide = None;
                self.scroller.set_offset(Vec2::ZERO);
            }
            Key::Named(NamedKey::End) if !ctrl => {
                self.glide = None;
                let max = (self.content_height() - self.area.height()).max(0.0);
                self.scroller.set_offset(Vec2::new(0.0, max));
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("c") => {
                let Some(text) = self.selection_text() else {
                    return false;
                };
                crate::text_selection::clipboard_copy(&text);
            }
            _ => return false,
        }
        true
    }

    // --- painting --------------------------------------------------------------

    /// Paint the reader into `area`.
    pub fn paint(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        self.area = area;
        if area.width() <= 0.0 || area.height() <= 0.0 {
            return;
        }
        let palette = ReaderPalette::resolve(self.prefs.paper, theme);
        if let Some(page) = palette.page {
            scene.fill(Fill::NonZero, t, page, None, &area);
        }
        let page_theme = palette.page_theme(theme);
        self.scroller.set_viewport(area, &page_theme);
        if self.document.is_none() {
            self.palette = Some(palette);
            self.page_theme = page_theme;
            self.scroller.set_content_size(area.width(), 0.0);
            return;
        }
        self.ensure_layout(text, theme);
        self.settle_glide();
        let content = self.content_height();
        self.scroller.set_content_size(area.width(), content);

        let flow_x = area.x0 + self.geometry.left;
        let flow_y = area.y0 + self.top_pad - self.scroller.offset().y;
        let find = self.find.clone();
        let selection = self.selection_range();
        let marks = self.mark_ranges(palette.is_dark());
        let hscroll = self.hscroll.clone();
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &area);
        if let Some(layout) = &self.layout {
            let mut ctx = PaintCtx::new(scene, t, text, &page_theme);
            self.hits = paint_flow(
                &mut ctx,
                &FlowPaint {
                    layout,
                    palette: &palette,
                    page_theme: &page_theme,
                    base_family: &self.base_family,
                    x: flow_x,
                    y: flow_y,
                    measure: self.geometry.measure,
                    clip: area,
                    hscroll: &|index| hscroll.get(&index).copied().unwrap_or(0.0),
                    images: self.images.as_ref(),
                    find: &find,
                    current_find: self.current_find,
                    selection,
                    marks: &marks,
                    hovered_link: self.hovered_link.as_deref(),
                },
            );
        }
        scene.pop_layer();
        let offset = self.scroller.offset().y;
        if offset > 0.0 {
            crate::surface::content_edge_fade(
                scene,
                t,
                area,
                crate::surface::ContentEdge::Top,
                palette.ground,
            );
        }
        if offset + area.height() < content {
            crate::surface::content_edge_fade(
                scene,
                t,
                area,
                crate::surface::ContentEdge::Bottom,
                palette.ground,
            );
        }
        self.paint_progress(scene, t, text, &palette, &page_theme);
        self.scroller.paint_bars(scene, t, &page_theme);
        self.paint_selection_hud(scene, t, text, theme);
        self.paint_note(scene, t, text, theme);
        self.paint_lightbox(scene, t, &page_theme);
        if let Some(word) = self.word_at_top() {
            self.speed.note(crate::time::now(), word);
        }
        self.palette = Some(palette);
        self.page_theme = page_theme;
    }

    /// Lay the document out again if anything it depends on changed.
    fn ensure_layout(&mut self, text: &mut TextRenderer, theme: &Theme) {
        let area = self.area;
        let typesetting = Typesetting {
            body_size: self.prefs.body_px(),
            line_spacing: 1.2,
            justify: self.prefs.justify,
            reading_scale: self.prefs.size,
        };
        let family = available_family(text, self.prefs.font, self.publisher_face.as_ref());
        let body_style =
            super::flow::run_style(typesetting.body_size, false, false, false, &family);
        let char_width = average_char_width(text, &body_style);
        let margin = min_margin(theme, area.width());
        let geometry = reader_geometry(area.width(), char_width, self.prefs.measure, margin);
        let spacing = &theme.tokens.spacing;
        self.top_pad = f64::from(spacing.s8);
        self.bottom_pad = f64::from(spacing.s16);
        let scale = text.text_scale();
        self.text_scale = scale;
        self.line_height =
            f64::from(typesetting.body_size * 1.25 * typesetting.line_spacing * scale);
        let language = self
            .document
            .as_ref()
            .map(|doc| doc.language.clone())
            .unwrap_or_default();
        let key = FlowKey::new(
            0,
            geometry.measure,
            0.0,
            &typesetting,
            self.content_sig ^ family_signature(&family),
            scale,
        );
        if self.layout.as_ref().is_some_and(|layout| layout.key == key) {
            return;
        }
        // Keep the passage on screen through a relayout of the same text.
        if self.layout.is_some() && self.pending_place.is_none() && self.refill_anchor.is_none() {
            self.pending_place = self.place();
        }
        let layout = layout_flow(
            text,
            &FlowRequest {
                blocks: &self.blocks,
                content_width: geometry.measure,
                bleed_width: geometry.bleed,
                content_height: 0.0,
                typesetting: &typesetting,
                section: 0,
                language: &language,
                base_family: &family,
                font_signature: key.font_sig,
            },
        );
        self.word_prefix = Vec::with_capacity(layout.blocks.len());
        let mut words = 0;
        for block in &layout.blocks {
            self.word_prefix.push(words);
            words += block.text.split_whitespace().count();
        }
        self.words = words;
        self.layout = Some(layout);
        self.geometry = geometry;
        self.base_family = family;
        self.scroller
            .set_content_size(area.width(), self.content_height());
        let refill = self.refill_anchor.take();
        let pending = self.pending_place.take();
        let place = refill
            .and_then(|(said, place)| self.relocate(&said, place))
            .or(pending);
        if let Some(place) = place
            && let Some(offset) = self.offset_for(place)
        {
            self.scroller.set_offset(Vec2::new(0.0, offset));
        }
    }

    /// The place in the new copy of the document that holds the block that
    /// said `text`, at the same depth; else the same share of the whole.
    fn relocate(&self, text: &str, place: ReadingPlace) -> Option<ReadingPlace> {
        if place.block == 0 && place.fraction <= 0.0 {
            return None;
        }
        let document = self.document.as_ref()?;
        let found = document
            .blocks
            .iter()
            .position(|block| block.plain_text() == text)
            .and_then(|index| u32::try_from(index).ok());
        Some(match found {
            Some(block) => ReadingPlace { block, ..place },
            None => ReadingPlace {
                block: u32::MAX,
                ..place
            },
        })
    }

    /// Move the scroll offset along its glide.
    fn settle_glide(&mut self) {
        let Some(glide) = &self.glide else {
            return;
        };
        let now = crate::motion::frame_now();
        let at = glide.get(now);
        let moving = glide.is_moving(now);
        self.scroller.set_offset(Vec2::new(0.0, at));
        if !moving {
            self.glide = None;
        }
    }

    /// The thin progress line along the top, and the minutes left.
    fn paint_progress(
        &self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        palette: &ReaderPalette,
        theme: &Theme,
    ) {
        let area = self.area;
        if self.content_height() <= area.height() {
            return;
        }
        let progress = self.progress();
        let line = Rect::new(
            area.x0,
            area.y0,
            area.x0 + area.width() * progress,
            area.y0 + 2.0,
        );
        scene.fill(
            Fill::NonZero,
            t,
            palette.ink_dim.with_alpha_factor(0.55),
            None,
            &line,
        );
        let Some(minutes) = self.minutes_left().filter(|m| *m > 0) else {
            return;
        };
        let label = if minutes == 1 {
            "1 min left".to_owned()
        } else {
            format!("{minutes} min left")
        };
        let style = theme.text(TextRole::Caption).numeric();
        let (w, h) = text.measure_style(&label, None, &style);
        let pad = f64::from(theme.tokens.spacing.s3);
        let x = area.x1 - pad - f64::from(w) - f64::from(theme.tokens.metrics.scrollbar_gutter);
        let y = area.y1 - pad - f64::from(h);
        text.draw_style(
            scene,
            &label,
            palette.ink_dim,
            t * Affine::translate((x, y)),
            None,
            &style,
        );
    }

    /// The footnote open in place: a panel under its reference.
    fn paint_note(&self, scene: &mut Scene, t: Affine, text: &mut TextRenderer, theme: &Theme) {
        let Some(open) = self.note else {
            return;
        };
        let Some(note) = self
            .document
            .as_ref()
            .and_then(|doc| doc.footnotes.get(usize::from(open.note)))
        else {
            return;
        };
        let body = spans_text(&note.spans);
        let pad = f64::from(theme.tokens.spacing.s4);
        let width = f64::from(self.geometry.measure).clamp(160.0, 420.0);
        let style = theme.text(TextRole::Body);
        let (_, h) = text.measure_style(
            &body,
            Some(crate::num::f32_of_f64(width - pad * 2.0)),
            &style,
        );
        let height = f64::from(h) + pad * 2.0;
        let x = (open.anchor.center().x - width / 2.0).clamp(
            self.area.x0 + pad,
            (self.area.x1 - pad - width).max(self.area.x0 + pad),
        );
        let below = open.anchor.y1 + 6.0;
        let y = if below + height > self.area.y1 - pad {
            (open.anchor.y0 - 6.0 - height).max(self.area.y0 + pad)
        } else {
            below
        };
        let rect = Rect::new(x, y, x + width, y + height);
        crate::widget::floating_surface::paint(scene, t, rect, theme);
        let ink = theme.on_surface(crate::chrome::panel_ground(theme)).ink;
        text.draw_style(
            scene,
            &body,
            ink,
            t * Affine::translate((x + pad, y + pad)),
            Some(crate::num::f32_of_f64(width - pad * 2.0)),
            &style,
        );
    }

    /// A picture opened full-window, over a scrim.
    fn paint_lightbox(&self, scene: &mut Scene, t: Affine, theme: &Theme) {
        let Some(src) = &self.lightbox else {
            return;
        };
        let area = self.area;
        scene.fill(
            Fill::NonZero,
            t,
            Color::from_rgba8(0, 0, 0, 0xE0),
            None,
            &area,
        );
        let pad = f64::from(theme.tokens.spacing.s8);
        if let Some(image) = self.images.as_ref().and_then(|images| images.get(src)) {
            crate::image_store::paint_image_contain(scene, t, area.inset(-pad), &image, 0.0);
        }
    }

    /// The bar over a selected passage: highlight colours, Note, Copy, Share.
    fn paint_selection_hud(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.hud = crate::reader_hud::SelectionHud::default();
        if self.selecting {
            return;
        }
        let Some(range) = self.selection_range().filter(|r| r.end > r.start) else {
            return;
        };
        let Some(anchor) = self.range_top(range) else {
            return;
        };
        let swatches: Vec<crate::reader_hud::HudSwatch> = HIGHLIGHT_COLORS
            .iter()
            .map(|(name, rgb)| crate::reader_hud::HudSwatch::new(*rgb, false).named(*name))
            .collect();
        self.hud = crate::reader_hud::SelectionHud::draw(
            scene,
            t,
            text,
            theme,
            &crate::reader_hud::HudControls {
                swatches: &swatches,
                show_note: true,
                show_delete: false,
                show_select_all: true,
                shortcuts: false,
                show_share: true,
            },
            crate::reader_hud::HudPlacement::new(anchor, self.area),
            self.pointer,
        );
    }

    /// The screen point above the first line of `range`, centred on it.
    fn range_top(&self, range: FlowRange) -> Option<Point> {
        let layout = self.layout.as_ref()?;
        let block = layout.blocks.get(range.block)?;
        let line = block.lines.iter().find(|line| {
            line.runs
                .iter()
                .any(|run| run.char_start + run.char_len > range.start)
        })?;
        let y = self.area.y0 + self.top_pad - self.scroller.offset().y + f64::from(line.y);
        let x = self.area.x0 + self.geometry.left + f64::from(self.geometry.measure) / 2.0;
        Some(Point::new(x, y))
    }

    // --- input -------------------------------------------------------------------

    /// Route a window event to the reader. Returns whether it used it.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        let used =
            self.handle_window_event(event, ctx.scale_factor, ctx.modifiers.state(), ctx.handled);
        if used {
            *ctx.handled = true;
        }
        used
    }

    /// [`Self::handle_event`] for a painted host without a widget context.
    pub fn handle_window_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: ModifiersState,
        handled: &mut bool,
    ) -> bool {
        let theme = self.page_theme.clone();
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Point::new(position.x / scale_factor, position.y / scale_factor);
            self.hovered_link = self
                .hits
                .links
                .iter()
                .find(|(rect, _)| rect.contains(self.pointer))
                .map(|(_, link)| Arc::clone(link));
            if self.selecting
                && let Some(head) = self.flow_point_at(self.pointer)
                && let Some((anchor, _)) = self.selection
            {
                self.selection = Some((anchor, head));
            }
        }
        if let WindowEvent::MouseWheel { delta, .. } = event
            && self.area.contains(self.pointer)
            && let Some(&(_, block)) = self
                .hits
                .scrollers
                .iter()
                .find(|(r, _)| r.contains(self.pointer))
        {
            let (dx, dy) = match delta {
                MouseScrollDelta::LineDelta(x, y) => (f64::from(*x) * -40.0, f64::from(*y) * -40.0),
                MouseScrollDelta::PixelDelta(p) => (-p.x / scale_factor, -p.y / scale_factor),
            };
            let sideways = if modifiers.shift_key() && dx.abs() < f64::EPSILON {
                dy
            } else {
                dx
            };
            if sideways.abs() > dy.abs() * 0.5 && sideways.abs() > f64::EPSILON {
                self.scroll_sideways(block, sideways);
                *handled = true;
                return true;
            }
        }
        if self
            .scroller
            .handle_window_event(event, scale_factor, modifiers, handled, &theme)
        {
            self.glide = None;
            return true;
        }
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.area.contains(self.pointer) => {
                self.press();
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.selecting => {
                self.selecting = false;
                if self
                    .selection_range()
                    .is_none_or(|range| range.end <= range.start)
                {
                    self.selection = None;
                }
                true
            }
            _ => false,
        }
    }

    fn scroll_sideways(&mut self, block: usize, delta: f64) {
        let Some(laid) = self
            .layout
            .as_ref()
            .and_then(|layout| layout.blocks.get(block))
        else {
            return;
        };
        let max = (laid.scroll_width - self.geometry.measure).max(0.0);
        let entry = self.hscroll.entry(block).or_insert(0.0);
        *entry = (*entry + crate::num::f32_of_f64(delta)).clamp(0.0, max);
    }

    fn press(&mut self) {
        let point = self.pointer;
        if self.lightbox.take().is_some() {
            return;
        }
        if let Some(action) = self.hud.hit(point) {
            self.hud_action(action);
            return;
        }
        if self.note.take().is_some() {
            return;
        }
        if let Some(&(anchor, note)) = self.hits.notes.iter().find(|(r, _)| r.contains(point)) {
            self.note = Some(OpenNote { note, anchor });
            return;
        }
        if let Some((_, link)) = self.hits.links.iter().find(|(r, _)| r.contains(point)) {
            self.events.push(ReaderEvent::OpenLink(link.to_string()));
            return;
        }
        if let Some((_, src)) = self.hits.images.iter().find(|(r, _)| r.contains(point)) {
            self.lightbox = Some(src.clone());
            return;
        }
        self.selection = self.flow_point_at(point).map(|at| (at, at));
        self.selecting = self.selection.is_some();
    }

    fn hud_action(&mut self, action: crate::reader_hud::HudAction) {
        use crate::reader_hud::HudAction;
        if action == HudAction::SelectAll {
            self.select_all();
            return;
        }
        let Some(passage) = self.selected_passage() else {
            return;
        };
        match action {
            HudAction::Copy => crate::text_selection::clipboard_copy(&passage.text),
            HudAction::Highlight(color) => {
                self.events.push(ReaderEvent::Highlight { passage, color });
            }
            HudAction::Note => self.events.push(ReaderEvent::Note { passage }),
            HudAction::Share => self.events.push(ReaderEvent::Share { passage }),
            HudAction::Delete | HudAction::SelectAll => {}
        }
        self.selection = None;
    }

    /// Select the whole text, from its first block to the end of its last.
    pub fn select_all(&mut self) {
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        let Some(last) = layout.blocks.last() else {
            return;
        };
        self.selection = Some((
            FlowPoint { block: 0, char: 0 },
            FlowPoint {
                block: layout.blocks.len() - 1,
                char: last.char_len,
            },
        ));
        self.selecting = false;
    }

    /// The laid block and character under screen point `point`.
    fn flow_point_at(&self, point: Point) -> Option<FlowPoint> {
        let layout = self.layout.as_ref()?;
        let flow_top = self.area.y0 + self.top_pad - self.scroller.offset().y;
        let y = crate::num::f32_of_f64(point.y - flow_top);
        let index = layout
            .blocks
            .iter()
            .position(|block| y < block.bottom)
            .unwrap_or_else(|| layout.blocks.len().saturating_sub(1));
        let block = layout.blocks.get(index)?;
        let line = block
            .lines
            .iter()
            .position(|line| y < line.y + block.line_height)
            .unwrap_or_else(|| block.lines.len().saturating_sub(1));
        let origin = self.area.x0 + self.geometry.left
            - f64::from(self.hscroll.get(&index).copied().unwrap_or(0.0));
        let char = crate::text_render::with_layout_renderer(self.text_scale, |text| {
            char_at(text, block, line, point.x, origin, &self.base_family)
        });
        Some(FlowPoint { block: index, char })
    }

    fn selection_range(&self) -> Option<FlowRange> {
        let (anchor, head) = self.selection?;
        let (from, to) = if (head.block, head.char) < (anchor.block, anchor.char) {
            (head, anchor)
        } else {
            (anchor, head)
        };
        // A selection stays in the block it began in.
        let end = if to.block == from.block {
            to.char
        } else {
            self.layout
                .as_ref()
                .and_then(|layout| layout.blocks.get(from.block))
                .map_or(from.char, |block| block.char_len)
        };
        Some(FlowRange {
            block: from.block,
            start: from.char,
            end,
        })
    }

    /// The selected passage, when the person selected text.
    #[must_use]
    pub fn selected_passage(&self) -> Option<Passage> {
        let range = self.selection_range().filter(|r| r.end > r.start)?;
        let block = range.block.checked_sub(self.header_len)?;
        let text = self.selection_text()?;
        Some(Passage {
            block: u32::try_from(block).ok()?,
            start: u32::try_from(range.start).ok()?,
            end: u32::try_from(range.end).ok()?,
            text,
        })
    }

    /// The selected text.
    #[must_use]
    pub fn selection_text(&self) -> Option<String> {
        let range = self.selection_range().filter(|r| r.end > r.start)?;
        let block = self.layout.as_ref()?.blocks.get(range.block)?;
        Some(
            block
                .text
                .chars()
                .skip(range.start)
                .take(range.end - range.start)
                .collect(),
        )
    }

    fn mark_ranges(&self, dark: bool) -> Vec<(FlowRange, Color)> {
        self.marks
            .iter()
            .filter_map(|mark| {
                let (_, [r, g, b]) = HIGHLIGHT_COLORS.get(mark.color)?;
                let alpha = if dark { 0x5C } else { 0x80 };
                Some((
                    FlowRange {
                        block: self.header_len.saturating_add(usize_of(mark.block)),
                        start: usize_of(mark.start),
                        end: usize_of(mark.end),
                    },
                    Color::from_rgba8(*r, *g, *b, alpha),
                ))
            })
            .collect()
    }

    fn word_at_top(&self) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        let top = crate::num::f32_of_f64((self.scroller.offset().y - self.top_pad).max(0.0));
        let index = layout.blocks.partition_point(|block| block.bottom <= top);
        self.word_prefix.get(index).copied()
    }

    // --- what the host asks -------------------------------------------------------

    /// Whether a glide or the scroll bar's fade still needs frames.
    #[must_use]
    pub fn needs_animation_frame(&self) -> bool {
        let now = crate::motion::frame_now();
        self.glide
            .as_ref()
            .is_some_and(|glide| glide.is_moving(now))
            || self.scroller.needs_animation_frame(&self.page_theme)
    }

    /// When the reader next wants a frame without input.
    #[must_use]
    pub fn next_wake_deadline(&self) -> Option<Instant> {
        self.glide
            .as_ref()
            .and_then(MotionValue::settles_at)
            .into_iter()
            .chain(self.scroller.next_wake_deadline(&self.page_theme))
            .min()
    }

    /// Where a link under `point` goes, for a tooltip; a picture's
    /// description.
    #[must_use]
    pub fn tooltip_at(&self, point: Point) -> Option<(Rect, String)> {
        if let Some((rect, link)) = self.hits.links.iter().find(|(r, _)| r.contains(point)) {
            return Some((*rect, link.to_string()));
        }
        let document = self.document.as_ref()?;
        self.hits
            .images
            .iter()
            .find(|(r, _)| r.contains(point))
            .and_then(|(rect, src)| {
                document.blocks.iter().find_map(|block| match block {
                    Block::Figure { image, .. } if image.src == *src && !image.alt.is_empty() => {
                        Some((*rect, image.alt.clone()))
                    }
                    _ => None,
                })
            })
    }

    /// The pointer over a link, a note or a picture is a hand.
    #[must_use]
    pub fn cursor_at(&self, point: Point) -> Option<winit::window::CursorIcon> {
        let over = self.hits.links.iter().any(|(r, _)| r.contains(point))
            || self.hits.notes.iter().any(|(r, _)| r.contains(point))
            || self.hits.images.iter().any(|(r, _)| r.contains(point))
            || self.hud.contains(point);
        if over {
            return Some(winit::window::CursorIcon::Pointer);
        }
        self.area
            .contains(point)
            .then_some(winit::window::CursorIcon::Text)
    }

    /// The reader's region for assistive technology and agents.
    #[must_use]
    pub fn primary_vertical_scroll(&self) -> Option<crate::widget::PrimaryScrollRegion> {
        self.scroller.primary_vertical_scroll()
    }

    /// Apply an accessible scroll action.
    pub fn apply_primary_vertical_scroll(
        &mut self,
        action: crate::widget::PrimaryScrollAction,
    ) -> crate::widget::PrimaryScrollResult {
        self.glide = None;
        self.scroller.apply_primary_vertical_scroll(action)
    }

    /// The document as assistive technology reads it: its headings,
    /// paragraphs, links and pictures in order.
    #[must_use]
    pub fn accessibility(&self, empty: &str) -> AccessNode {
        let Some(document) = &self.document else {
            return AccessNode::new(Role::Document)
                .with_name(empty.to_owned())
                .with_bounds(self.area);
        };
        let children = self
            .blocks
            .iter()
            .enumerate()
            .map(|(index, block)| {
                let role = match block {
                    Block::Heading { level, .. } => Role::Heading {
                        level: level.saturating_add(1).min(6),
                    },
                    Block::ListItem { .. } => Role::ListItem,
                    Block::Figure { .. } => Role::Image,
                    Block::Table { .. } => Role::Table,
                    Block::Separator => Role::Separator,
                    Block::Meta { .. } => Role::StaticText,
                    _ => Role::Paragraph,
                };
                let name = match block {
                    Block::Figure { image, caption } if caption.is_empty() => image.alt.clone(),
                    _ => block.plain_text(),
                };
                AccessNode::new(role)
                    .with_name(name)
                    .with_hashed_key((self.content_sig, index))
            })
            .collect();
        AccessNode::new(Role::Document)
            .with_name(document.title.clone())
            .with_state(AccessState::new().read_only(true))
            .with_bounds(self.area)
            .with_children(children)
    }
}

impl Findable for ReaderView {
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
        self.find.clear();
        self.current_find = None;
        if query.is_empty() {
            return Vec::new();
        }
        let needle: Vec<char> = if options.match_case {
            query.chars().collect()
        } else {
            query.to_lowercase().chars().collect()
        };
        let mut matches = Vec::new();
        let mut offset = 0;
        for (index, block) in self.blocks.iter().enumerate() {
            let text = block.plain_text();
            let hay: Vec<char> = if options.match_case {
                text.chars().collect()
            } else {
                // Lowercase per character so offsets stay in characters.
                text.chars()
                    .map(|c| c.to_lowercase().next().unwrap_or(c))
                    .collect()
            };
            let mut at = 0;
            while at + needle.len() <= hay.len() && !needle.is_empty() {
                let found = hay.get(at..at + needle.len()) == Some(needle.as_slice());
                let whole = !options.whole_word
                    || (at
                        .checked_sub(1)
                        .and_then(|i| hay.get(i))
                        .is_none_or(|c| !c.is_alphanumeric())
                        && hay
                            .get(at + needle.len())
                            .is_none_or(|c| !c.is_alphanumeric()));
                if found && whole {
                    self.find.push(FlowRange {
                        block: index,
                        start: at,
                        end: at + needle.len(),
                    });
                    matches.push(FindMatch::new(offset + at, offset + at + needle.len()));
                    at += needle.len();
                } else {
                    at += 1;
                }
            }
            offset += hay.len() + 1;
        }
        matches
    }

    fn reveal(&mut self, index: usize) {
        self.current_find = Some(index);
        let Some(range) = self.find.get(index).copied() else {
            return;
        };
        let Some(layout) = &self.layout else {
            return;
        };
        let Some(block) = layout.blocks.get(range.block) else {
            return;
        };
        let line_y = block
            .lines
            .iter()
            .find(|line| {
                line.runs
                    .iter()
                    .any(|run| run.char_start + run.char_len > range.start)
            })
            .map_or(block.top, |line| line.y);
        let y = self.top_pad + f64::from(line_y);
        let offset = self.scroller.offset().y;
        let visible = self.area.height();
        if y < offset + self.top_pad || y + self.line_height > offset + visible - self.bottom_pad {
            self.glide = None;
            self.scroller
                .set_offset(Vec2::new(0.0, (y - visible / 3.0).max(0.0)));
        }
    }

    fn clear_find(&mut self) {
        self.find.clear();
        self.current_find = None;
    }

    fn content_revision(&self) -> u64 {
        self.revision
    }
}

/// The face text is set in, as the system has it: the chosen face where it
/// is installed, else the system's reading serif.
fn available_family(
    text: &TextRenderer,
    font: ReadingFont,
    publisher: Option<&FontChoice>,
) -> FontChoice {
    let family = font.family(publisher);
    match &family {
        FontChoice::Named(name) => {
            let installed = text
                .font_system
                .db()
                .faces()
                .any(|face| face.families.iter().any(|(family, _)| family == name));
            if installed { family } else { FontChoice::Serif }
        }
        _ => family,
    }
}

fn family_signature(family: &FontChoice) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = rustc_hash::FxHasher::default();
    format!("{family:?}").hash(&mut hasher);
    hasher.finish()
}

/// A signature of what `blocks` say, so a change relays out.
fn signature(blocks: &[Block]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = rustc_hash::FxHasher::default();
    blocks.len().hash(&mut hasher);
    for block in blocks {
        format!("{block:?}").hash(&mut hasher);
    }
    hasher.finish()
}

fn usize_of(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}
