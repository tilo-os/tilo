//! A document read a page at a time, as a described component: its text set in a reading face
//! and measured into pages, shown a page (or a spread, or scrolled) at a time, with the passages
//! a reader marked drawn under their words.

use vello::kurbo::{Affine, Rect};
use vello::peniko::Color;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat as _;
use ui::component::{Component, Described, Event, Value};
use ui::motion::{Drill, Pose, Presence};
use ui::text_render::{FontChoice, TextRenderer};
use ui::tokens::MotionSource;
use ui::widget::{TaffyTreeExt as _, Widget};

use super::from_rich;
use super::layout::{ChapterLayout, ChapterLayoutRequest, LaidBlock, LayoutKind, layout_chapter};
use super::model::{CompositionStyle, ReadingStyle};
use super::page::{PagePaint, draw_blocks};

/// How a reading page shows its text: a page at a time, scrolled as one column, or two pages
/// side by side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ReadingMode {
    #[default]
    Paged,
    Continuous,
    TwoUp,
}

impl ReadingMode {
    /// The mode a case name reads as (`"paged"`, `"continuous"`, `"two_up"`).
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name {
            "continuous" => Self::Continuous,
            "two_up" => Self::TwoUp,
            _ => Self::Paged,
        }
    }
}

/// A document read a page at a time: its text set in a reading face, broken into pages the box
/// it is given fits, one page (or a spread, or a column) shown, and the page turned by the
/// reader. The app keeps where the reader is: it says which page shows here, and hears `turned`
/// when the reader moves and `selected` when they mark a passage.
#[derive(Component, Default)]
#[component(name = "Reading", role = "Document")]
pub struct ReadingProps {
    /// The text read: its paragraphs, headings, quotes, lists and section breaks.
    #[component(positional)]
    pub value: Option<tilo_ui_graph::rich::RichText>,
    /// The text's size as a multiple of the reading base (1.0 unless said), between 0.8 and 2.
    pub scale: Option<f64>,
    /// The line spacing as a multiple of the face's own (1.2 unless said).
    pub spacing: Option<f64>,
    /// Whether paragraphs are justified like a book's or set ragged; ragged unless said.
    pub justified: bool,
    /// The face the body is set in: `serif`, `sans`, `mono`, or a reading face the toolkit ships
    /// (`libertinus`, `literata`, `garamond`, `source_serif`). Serif unless said.
    pub face: Option<String>,
    /// The book's language, for hyphenation in its words.
    pub language: Option<String>,
    /// How the text is shown: `paged` a page at a time, `continuous` one column scrolled, or
    /// `two_up` two pages side by side.
    pub mode: Option<String>,
    /// The page shown, counted from 0 (a spread, in two pages side by side); a page past the end
    /// shows the last.
    pub page: Option<i64>,
    /// How far down the column stands in the continuous mode, `0` its top to `1` its foot.
    pub place: Option<f64>,
    /// The block to show, counted from 0: the page it stands on is the page shown. Asking for
    /// the same block again after another moved the page shows it again, so a link to a passage
    /// lands on it every time.
    pub reveal: Option<i64>,
    /// The paper behind the words, as a colour (`#rrggbb`); the theme's content colour unless
    /// said.
    pub paper: Option<String>,
    /// The words' colour, as a colour (`#rrggbb`); the theme's text colour unless said.
    pub ink: Option<String>,
    /// The passages the reader marked, drawn under their words: each `block:start:end:colour`,
    /// its characters in that block's words and the colour of its band (`#rrggbb`).
    pub highlights: Vec<String>,
    /// How many pages (or spreads) the text has, each time it changes.
    #[component(event(value = i64))]
    pub pages: Event,
    /// The page the reader turned to, by a key or a press: carries the page, counted from 0,
    /// and the block the page begins at.
    #[component(event(value = i64, at = i64))]
    pub turned: Event,
    /// How far down the column the reader scrolled in the continuous mode: carries `0`..`1`.
    #[component(event(value = f64))]
    pub moved: Event,
    /// The passage the reader selected: carries the block, its first and last character.
    #[component(event(value = Vec<i64>))]
    pub selected: Event,
    /// The first block the page shows, and the character it starts at, each time the page
    /// changes: where a bookmark made now is kept.
    #[component(event(value = Vec<i64>))]
    pub showing: Event,
}

/// The passages a reader selected here, in the text's coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Band {
    block: usize,
    start: usize,
    end: usize,
}

pub struct Reading {
    text: Option<tilo_ui_graph::rich::RichText>,
    style: ReadingStyle,
    face: String,
    language: String,
    mode: ReadingMode,
    page: usize,
    place: f64,
    paper: Option<Color>,
    ink: Option<Color>,
    highlights: Vec<String>,
    pages: Event,
    turned: Event,
    moved: Event,
    selected: Event,
    showing: Event,
    rect: Rect,
    laid: Option<Laid>,
    reported: Option<usize>,
    reveal: Option<i64>,
    shown_reveal: Option<i64>,
    focused: Option<(usize, usize)>,
    pressed: Option<Band>,
    /// The passage selected now, a band for each block it crosses.
    drawn: Vec<Band>,
    /// The direction a page turn waits to start at, `0` for none: a motion starts where the theme
    /// that draws it is known.
    pending_turn: f64,
    turn: Option<Turn>,
    cursor: vello::kurbo::Point,
    dirty: bool,
}

/// A laid-out text, keyed by what it was set from.
struct Laid {
    key: u64,
    width: f32,
    height: f32,
    layout: ChapterLayout,
}

/// A spread's colours and its bands, as one call carries them.
struct Spread<'a> {
    face: &'a FontChoice,
    ink: Color,
    ink_dim: Color,
    bands: &'a [(usize, usize, usize, Color)],
}

/// A page turning: the page that left and the page that arrives, on the shared drill motion.
struct Turn {
    anim: Presence,
    /// `1` a forward turn, `-1` back: the side the incoming page arrives from.
    dir: f64,
    /// The first page of the spread that left.
    from_page: usize,
}

const PAGE_TURN_KEYS: [NamedKey; 8] = [
    NamedKey::ArrowLeft,
    NamedKey::ArrowRight,
    NamedKey::ArrowUp,
    NamedKey::ArrowDown,
    NamedKey::PageUp,
    NamedKey::PageDown,
    NamedKey::Space,
    NamedKey::Enter,
];

impl Reading {
    /// The face the page is shaped in.
    fn face_choice(&self) -> FontChoice {
        match self.face.as_str() {
            "sans" => FontChoice::SansSerif,
            "mono" => FontChoice::Monospace,
            "libertinus" => FontChoice::Named(ui::ReadingFace::Libertinus.family().to_owned()),
            "literata" => FontChoice::Named(ui::ReadingFace::Literata.family().to_owned()),
            "garamond" => FontChoice::Named(ui::ReadingFace::Garamond.family().to_owned()),
            "source_serif" => FontChoice::Named(ui::ReadingFace::SourceSerif.family().to_owned()),
            _ => FontChoice::Serif,
        }
    }

    /// The size the text is laid out at, for a page box `width` × `height`.
    fn layout_size(&self, width: f32, height: f32) -> (f32, f32) {
        match self.mode {
            ReadingMode::TwoUp => (width / 2.0 - 24.0, height),
            ReadingMode::Continuous => (width, f32::MAX / 4.0),
            ReadingMode::Paged => (width, height),
        }
    }

    fn page_count(&self) -> usize {
        let Some(laid) = &self.laid else {
            return 1;
        };
        let pages = laid.layout.page_count();
        if self.mode == ReadingMode::TwoUp {
            pages.div_ceil(2).max(1)
        } else {
            pages
        }
    }

    /// The first page the spread shows.
    fn first_page(&self) -> usize {
        match self.mode {
            ReadingMode::TwoUp => self.page.saturating_mul(2),
            _ => self.page,
        }
    }

    /// Lay the text out for `width` × `height` when it is not laid out for it already. The
    /// continuous mode sets the whole text as one column: its pages are never turned.
    fn lay_out(&mut self, width: f32, height: f32, text: &mut TextRenderer) {
        if width <= 1.0 || height <= 1.0 {
            return;
        }
        let key = self.key();
        let (w, h) = self.layout_size(width, height);
        if let Some(laid) = &self.laid
            && laid.key == key
            && (laid.width - w).abs() < 0.5
            && (laid.height - h).abs() < 0.5
        {
            return;
        }
        let blocks = self
            .text
            .as_ref()
            .map(from_rich::blocks_of)
            .unwrap_or_default();
        let face = self.face_choice();
        let layout = layout_chapter(
            text,
            &ChapterLayoutRequest {
                blocks: &blocks,
                content_width: w,
                content_height: h,
                settings: &self.style,
                chapter: 0,
                language: &self.language,
                base_family: &face,
                font_multiplier: 1.0,
                font_signature: 0,
            },
        );
        self.laid = Some(Laid {
            key,
            width: w,
            height: h,
            layout,
        });
        if self.page >= self.page_count() {
            self.page = self.page_count().saturating_sub(1);
        }
        self.reveal_page();
        self.note_page_start();
        self.dirty = true;
    }

    /// Show the page the block `reveal` names stands on, where the view asked for one it has
    /// not shown since.
    fn reveal_page(&mut self) {
        let Some(block) = self.reveal.and_then(|b| usize::try_from(b).ok()) else {
            return;
        };
        if self.shown_reveal == self.reveal {
            return;
        }
        let Some(laid) = &self.laid else {
            return;
        };
        let Some(laid_block) = laid.layout.blocks.iter().find(|b| b.index == block) else {
            return;
        };
        let page = laid
            .layout
            .pages
            .iter()
            .rposition(|start| *start <= laid_block.top)
            .unwrap_or(0);
        self.shown_reveal = self.reveal;
        let page = match self.mode {
            ReadingMode::TwoUp => page / 2,
            _ => page,
        };
        let leaving = self.first_page();
        self.page = page.min(self.page_count().saturating_sub(1));
        self.pending_turn = if self.page >= leaving { 1.0 } else { -1.0 };
        self.turn = Some(Turn {
            anim: Presence::closed(),
            dir: self.pending_turn,
            from_page: leaving,
        });
        if !self.turned.is_none() {
            let block = self.block_of(self.first_page());
            self.turned.fire(Some(Value::List(vec![
                Value::Int(i64::try_from(self.page).unwrap_or(0)),
                Value::Int(i64::try_from(block).unwrap_or(0)),
            ])));
        }
    }

    /// The block the page beginning at `page` starts at.
    fn block_of(&self, page: usize) -> usize {
        let Some(laid) = &self.laid else {
            return 0;
        };
        let start = laid.layout.pages.get(page).copied().unwrap_or(0.0);
        laid.layout
            .blocks
            .iter()
            .find(|block| block.top + (block.bottom - block.top) > start)
            .map_or(0, |block| block.index)
    }

    /// Tell the app which block the page starts at, when the page changed.
    fn note_page_start(&mut self) {
        let Some(laid) = &self.laid else {
            return;
        };
        let page = self.first_page();
        let start = laid.layout.pages.get(page).copied().unwrap_or(0.0);
        let block = laid
            .layout
            .blocks
            .iter()
            .find(|block| block.top + (block.bottom - block.top) > start)
            .map(|block| block.index)
            .unwrap_or(0);
        let here = Some((page, block));
        if self.focused == here {
            return;
        }
        self.focused = here;
        if !self.showing.is_none() {
            self.showing.fire(Some(Value::List(vec![
                Value::Int(i64::try_from(block).unwrap_or(0)),
                Value::Int(0),
            ])));
        }
    }

    fn key(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        if let Some(text) = &self.text {
            text.blocks.len().hash(&mut hasher);
            for block in &text.blocks {
                block.kind.hash(&mut hasher);
                block.text.hash(&mut hasher);
                block.marks.len().hash(&mut hasher);
                block.attrs.len().hash(&mut hasher);
            }
        }
        self.face.hash(&mut hasher);
        self.language.hash(&mut hasher);
        self.mode.hash(&mut hasher);
        self.style.font_scale.to_bits().hash(&mut hasher);
        self.style.line_spacing.to_bits().hash(&mut hasher);
        hasher.finish()
    }

    fn turn_to(&mut self, page: usize, report: bool) {
        let page = page.min(self.page_count().saturating_sub(1));
        if page == self.page {
            return;
        }
        let leaving = self.first_page();
        self.page = page;
        self.pending_turn = if page >= leaving { 1.0 } else { -1.0 };
        self.turn = Some(Turn {
            anim: Presence::closed(),
            dir: self.pending_turn,
            from_page: leaving,
        });
        self.dirty = true;
        self.note_page_start();
        if report && !self.turned.is_none() {
            let block = self.block_of(self.first_page());
            self.turned.fire(Some(Value::List(vec![
                Value::Int(i64::try_from(page).unwrap_or(i64::MAX)),
                Value::Int(i64::try_from(block).unwrap_or(i64::MAX)),
            ])));
        }
    }

    fn scroll_to(&mut self, place: f64, report: bool) {
        let place = place.clamp(0.0, 1.0);
        if (place - self.place).abs() < 1e-6 {
            return;
        }
        self.place = place;
        self.dirty = true;
        if report && !self.moved.is_none() {
            self.moved.fire(Some(Value::Float(place)));
        }
    }

    fn report_pages(&mut self) -> bool {
        let count = self.page_count();
        if self.reported == Some(count) {
            return false;
        }
        self.reported = Some(count);
        if !self.pages.is_none() {
            self.pages
                .fire(Some(Value::Int(i64::try_from(count).unwrap_or(i64::MAX))));
        }
        true
    }

    /// The word under `point`, as a band: presses and drags select whole words.
    fn word_at(&self, point: vello::kurbo::Point) -> Option<Band> {
        let laid = self.laid.as_ref()?;
        let mut block = None;
        for b in &laid.layout.blocks {
            let first = b.lines.first()?;
            let last = b.lines.last()?;
            let top = f64::from(first.y - self.offset(laid));
            let bottom = f64::from(last.y + b.line_height - self.offset(laid));
            if point.y >= self.rect.y0 + top && point.y <= self.rect.y0 + bottom {
                block = Some((b, top));
                break;
            }
        }
        let (b, _) = block?;
        let line = b
            .lines
            .iter()
            .find(|l| {
                let ly = self.rect.y0 + f64::from(l.y - self.offset(laid));
                point.y >= ly && point.y <= ly + f64::from(b.line_height)
            })
            .or_else(|| b.lines.first())?;
        let x = point.x - self.rect.x0 - f64::from(b.indent);
        let mut chars = 0usize;
        for run in &line.runs {
            let start = run.char_start;
            let next = line.runs.iter().find(|r| r.char_start > start).map_or(
                f64::from(run.x) + f64::from(b.size) * run.char_len as f64,
                |r| f64::from(r.x),
            );
            if x >= f64::from(run.x) && x < next {
                chars = run.char_start;
                break;
            }
            chars = start;
        }
        let text = &b.text;
        let mut from = chars.min(text.chars().count());
        let mut to = from;
        let chars_of: Vec<char> = text.chars().collect();
        while from > 0 && chars_of.get(from - 1).is_some_and(|c| !c.is_whitespace()) {
            from -= 1;
        }
        while to < chars_of.len() && chars_of.get(to).is_some_and(|c| !c.is_whitespace()) {
            to += 1;
        }
        (from < to).then_some(Band {
            block: b.index,
            start: from,
            end: to,
        })
    }

    /// The passage from `from` to `to`, a band for each block it crosses: the words after `from`
    /// in its block to its end, every block between, and the words before `to` in its block.
    fn passage(&self, from: Band, to: Band) -> Vec<Band> {
        let Some(laid) = &self.laid else {
            return Vec::new();
        };
        let (first, last) = if from.block <= to.block {
            (from, to)
        } else {
            (to, from)
        };
        if first.block == last.block {
            return vec![Band {
                block: first.block,
                start: first.start.min(last.start),
                end: first.end.max(last.end),
            }];
        }
        let mut passage = Vec::new();
        let chars = |block: usize| {
            laid.layout
                .blocks
                .iter()
                .find(|b| b.index == block)
                .map_or(0, |b| b.char_len)
        };
        passage.push(Band {
            block: first.block,
            start: first.start,
            end: chars(first.block),
        });
        for block in first.block + 1..last.block {
            passage.push(Band {
                block,
                start: 0,
                end: chars(block),
            });
        }
        passage.push(Band {
            block: last.block,
            start: 0,
            end: last.end,
        });
        passage
    }

    /// The flow's vertical offset in the page box for the mode and place shown.
    fn offset(&self, laid: &Laid) -> f32 {
        match self.mode {
            ReadingMode::Continuous => {
                let head = laid.layout.total_height - ui::num::f32_of_f64(self.rect.height());
                head.max(0.0) * ui::num::f32_of_f64(self.place).clamp(0.0, 1.0)
            }
            _ => 0.0,
        }
    }

    fn paint(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &ui::theme::Theme,
    ) {
        let now = ui::motion::frame_now();
        let motion = theme.motion();
        if self.pending_turn != 0.0 {
            if let Some(turn) = &mut self.turn {
                turn.anim
                    .show(true, Drill::SPEC, motion, MotionSource::Pointer, now);
            }
            self.pending_turn = 0.0;
            self.dirty = true;
        }
        let colors = &theme.tokens.colors;
        let ink = self.ink.unwrap_or(colors.text_primary);
        let ink_dim = self.ink.map_or(colors.text_dim, |ink| ink.with_alpha(0.6));
        if let Some(paper) = self.paper {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                paper,
                None,
                &self.rect,
            );
        }
        let bands = self.bands(ink);
        let face = self.face_choice();
        let width = self.rect.width();
        let turning = self.turn.as_ref().map(|turn| {
            (
                turn.anim.progress(now),
                turn.anim.is_moving(now),
                turn.dir,
                turn.from_page,
            )
        });
        if matches!(turning, Some((_, false, _, _))) {
            self.turn = None;
            self.dirty = true;
        }
        let turning = if self.turn.is_some() { turning } else { None };
        let Some(laid) = &self.laid else {
            return;
        };
        match turning {
            Some((progress, _moving, dir, from)) => {
                let leaving = Drill::pose(-dir * progress, width, motion);
                let arriving = Drill::pose(dir * (1.0 - progress), width, motion);
                let spread = Spread {
                    face: &face,
                    ink,
                    ink_dim,
                    bands: &bands,
                };
                self.draw_spread(scene, transform, text, &spread, from, leaving, laid);
                self.draw_spread(
                    scene,
                    transform,
                    text,
                    &spread,
                    self.first_page(),
                    arriving,
                    laid,
                );
            }
            None => {
                let spread = Spread {
                    face: &face,
                    ink,
                    ink_dim,
                    bands: &bands,
                };
                self.draw_spread(
                    scene,
                    transform,
                    text,
                    &spread,
                    self.first_page(),
                    Pose::REST,
                    laid,
                );
            }
        }
    }

    /// The passages marked, as bands: the reader's marks and the live selection.
    fn bands(&self, selection_ink: Color) -> Vec<(usize, usize, usize, Color)> {
        self.highlights
            .iter()
            .filter_map(|mark| mark_range(mark))
            .chain(self.drawn.iter().map(|band| {
                (
                    band.block,
                    band.start,
                    band.end,
                    selection_ink.with_alpha(0.25),
                )
            }))
            .collect()
    }

    /// Draw the spread beginning at `start`, posed by `pose`, in `paint`'s colours.
    fn draw_spread(
        &self,
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut TextRenderer,
        paint: &Spread<'_>,
        start: usize,
        pose: Pose,
        laid: &Laid,
    ) {
        if !pose.is_visible() {
            return;
        }
        let posed = transform * pose.transform;
        let layered = pose.opacity < 0.999;
        let clip = self.rect.inflate(64.0, 0.0);
        if layered {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                pose.opacity,
                transform,
                &clip,
            );
        }
        match self.mode {
            ReadingMode::TwoUp => {
                let gap = 24.0;
                let half = self.rect.width() / 2.0;
                for (side, page) in [(0.0, start), (half + gap, start + 1)] {
                    let rect = Rect::new(
                        self.rect.x0 + side,
                        self.rect.y0,
                        self.rect.x0 + side + half - gap,
                        self.rect.y1,
                    );
                    draw_blocks(
                        scene,
                        posed,
                        text,
                        &laid.layout,
                        page,
                        &PagePaint {
                            rect,
                            paper: None,
                            ink: paint.ink,
                            ink_dim: paint.ink_dim,
                            base_family: paint.face,
                            marks: paint.bands,
                            offset: 0.0,
                        },
                    );
                }
            }
            ReadingMode::Paged => draw_blocks(
                scene,
                posed,
                text,
                &laid.layout,
                start,
                &PagePaint {
                    rect: self.rect,
                    paper: None,
                    ink: paint.ink,
                    ink_dim: paint.ink_dim,
                    base_family: paint.face,
                    marks: paint.bands,
                    offset: 0.0,
                },
            ),
            ReadingMode::Continuous => draw_blocks(
                scene,
                posed,
                text,
                &laid.layout,
                usize::MAX,
                &PagePaint {
                    rect: self.rect,
                    paper: None,
                    ink: paint.ink,
                    ink_dim: paint.ink_dim,
                    base_family: paint.face,
                    marks: paint.bands,
                    offset: self.offset(laid),
                },
            ),
        }
        if layered {
            scene.pop_layer();
        }
    }

    fn bound(&self) -> usize {
        self.page_count().saturating_sub(1)
    }
}

impl Described for Reading {
    type Props = ReadingProps;

    fn from_props(_key: &str, props: ReadingProps) -> Self {
        let mut reading = Self {
            text: None,
            style: ReadingStyle::default(),
            face: String::new(),
            language: "en".to_owned(),
            mode: ReadingMode::Paged,
            page: 0,
            place: 0.0,
            paper: None,
            ink: None,
            highlights: Vec::new(),
            pages: Event::None,
            turned: Event::None,
            moved: Event::None,
            selected: Event::None,
            showing: Event::None,
            rect: Rect::ZERO,
            laid: None,
            reported: None,
            reveal: None,
            shown_reveal: None,
            focused: None,
            pressed: None,
            drawn: Vec::new(),
            pending_turn: 0.0,
            turn: None,
            cursor: vello::kurbo::Point::ZERO,
            dirty: true,
        };
        reading.set_props(props);
        reading
    }

    fn set_props(&mut self, props: ReadingProps) -> bool {
        let ReadingProps {
            value,
            scale,
            spacing,
            justified,
            face,
            language,
            mode,
            page,
            place,
            reveal,
            paper,
            ink,
            highlights,
            pages,
            turned,
            moved,
            selected,
            showing,
        } = props;
        let mut style = ReadingStyle::with_scale(scale.map_or(1.0, ui::num::f32_of_f64));
        if let Some(spacing) = spacing {
            style.line_spacing = ui::num::f32_of_f64(spacing)
                .clamp(ReadingStyle::SPACING_MIN, ReadingStyle::SPACING_MAX);
        }
        style.composition = if justified {
            CompositionStyle::Book
        } else {
            CompositionStyle::Ragged
        };
        if style != self.style
            || face.as_deref().is_some_and(|face| face != self.face)
            || mode
                .as_deref()
                .is_some_and(|mode| ReadingMode::from_name(mode) != self.mode)
            || language
                .as_deref()
                .is_some_and(|l| !l.is_empty() && l != self.language)
            || paper
                .as_deref()
                .and_then(ui::color::parse_hex)
                .map(|c| Color::from_rgb8(c.r(), c.g(), c.b()))
                != self.paper
            || ink
                .as_deref()
                .and_then(ui::color::parse_hex)
                .map(|c| Color::from_rgb8(c.r(), c.g(), c.b()))
                != self.ink
            || highlights != self.highlights
        {
            self.dirty = true;
        }
        self.style = style;
        if let Some(face) = face {
            self.face = face;
        }
        if let Some(mode) = mode {
            self.mode = ReadingMode::from_name(&mode);
        }
        if let Some(language) = language.filter(|l| !l.is_empty()) {
            self.language = language;
        }
        self.paper = paper
            .as_deref()
            .and_then(ui::color::parse_hex)
            .map(|c| Color::from_rgb8(c.r(), c.g(), c.b()));
        self.ink = ink
            .as_deref()
            .and_then(ui::color::parse_hex)
            .map(|c| Color::from_rgb8(c.r(), c.g(), c.b()));
        self.highlights = highlights;
        self.pages = pages;
        self.turned = turned;
        self.moved = moved;
        self.selected = selected;
        self.showing = showing;
        if reveal != self.reveal {
            self.shown_reveal = None;
        }
        self.reveal = reveal;
        let page = page.and_then(|p| usize::try_from(p).ok()).unwrap_or(0);
        let place = place.unwrap_or(0.0).clamp(0.0, 1.0);
        if page != self.page {
            let leaving = self.first_page();
            self.pending_turn = if page >= self.page { 1.0 } else { -1.0 };
            self.turn = Some(Turn {
                anim: Presence::closed(),
                dir: self.pending_turn,
                from_page: leaving,
            });
            self.dirty = true;
        }
        if (place - self.place).abs() > 1e-6 {
            self.dirty = true;
        }
        self.page = page;
        self.place = place;
        self.text = value;
        true
    }
}

impl Widget for Reading {
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &ui::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let style = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            min_size: taffy::Size {
                width: taffy::Dimension::length(0.0),
                height: taffy::Dimension::length(0.0),
            },
            flex_grow: 1.0,
            flex_shrink: 1.0,
            ..taffy::Style::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let width = known.width.unwrap_or_else(|| match available.width {
            taffy::AvailableSpace::Definite(width) => width,
            taffy::AvailableSpace::MinContent | taffy::AvailableSpace::MaxContent => 640.0,
        });
        let height = known.height.unwrap_or_else(|| match available.height {
            taffy::AvailableSpace::Definite(height) => height,
            taffy::AvailableSpace::MinContent | taffy::AvailableSpace::MaxContent => 900.0,
        });
        self.lay_out(width, height, text);
        taffy::Size { width, height }
    }

    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        let rect = ui::widget::layout_rect(taffy, node, origin);
        if (rect.width() - self.rect.width()).abs() > 0.5
            || (rect.height() - self.rect.height()).abs() > 0.5
        {
            self.dirty = true;
        }
        self.rect = rect;
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &ui::theme::Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
        let rect = self.rect;
        self.lay_out(
            ui::num::f32_of_f64(rect.width()),
            ui::num::f32_of_f64(rect.height()),
            text,
        );
        self.paint(scene, transform, text, theme);
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn needs_animation_frame(&self) -> bool {
        self.pending_turn != 0.0
            || self
                .turn
                .as_ref()
                .is_some_and(|turn| turn.anim.is_moving(ui::motion::frame_now()))
    }

    fn mark_drawn(&mut self) {
        self.dirty = self.needs_animation_frame();
    }

    fn sync(&mut self, _state: &ui::widget::AppModel, _path: &ui::widget::ModelPath) {
        self.report_pages();
    }

    fn update(&mut self, _state: &ui::widget::AppModel, _path: &ui::widget::ModelPath) -> bool {
        self.report_pages()
    }

    fn is_layout_dirty(&self) -> bool {
        let (width, height) = self.layout_size(
            ui::num::f32_of_f64(self.rect.width()),
            ui::num::f32_of_f64(self.rect.height()),
        );
        self.laid.as_ref().is_none_or(|laid| {
            (laid.width - width).abs() > 0.5 || (laid.height - height).abs() > 0.5
        })
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if self.mode == ReadingMode::Continuous {
                    let step = 0.2;
                    let Key::Named(key) = logical_key else {
                        return;
                    };
                    let target = match key {
                        NamedKey::ArrowRight | NamedKey::ArrowDown | NamedKey::PageDown => {
                            self.place + step
                        }
                        NamedKey::ArrowLeft | NamedKey::ArrowUp | NamedKey::PageUp => {
                            self.place - step
                        }
                        _ => return,
                    };
                    self.scroll_to(target, true);
                    *ctx.handled = true;
                    return;
                }
                let Key::Named(key) = logical_key else {
                    return;
                };
                if !PAGE_TURN_KEYS.contains(key) {
                    return;
                }
                let target = match key {
                    NamedKey::ArrowLeft | NamedKey::ArrowUp | NamedKey::PageUp => {
                        self.page.saturating_sub(1)
                    }
                    _ => (self.page + 1).min(self.bound()),
                };
                self.turn_to(target, true);
                *ctx.handled = true;
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = ctx.logical(position);
                if let Some(anchor) = self.pressed
                    && let Some(to) = self.word_at(self.cursor)
                {
                    let passage = self.passage(anchor, to);
                    if self.drawn != passage {
                        self.drawn = passage;
                        self.dirty = true;
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let point = self.cursor;
                if !self.rect.contains(point) {
                    return;
                }
                self.pressed = self.word_at(point);
                self.drawn = self.pressed.into_iter().collect();
                self.dirty = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(anchor) = self.pressed.take() {
                    let mut passage = self.drawn.clone();
                    if passage.is_empty() {
                        passage.push(anchor);
                    }
                    if !self.selected.is_none() {
                        let flat: Vec<Value> = passage
                            .iter()
                            .flat_map(|band| {
                                [
                                    Value::Int(i64::try_from(band.block).unwrap_or(0)),
                                    Value::Int(i64::try_from(band.start).unwrap_or(0)),
                                    Value::Int(i64::try_from(band.end).unwrap_or(0)),
                                ]
                            })
                            .collect();
                        self.selected.fire(Some(Value::List(flat)));
                    }
                    *ctx.handled = true;
                    return;
                }
                // A press that marked nothing turns the page: its left third goes back, its
                // right two thirds on.
                let point = self.cursor;
                if !self.rect.contains(point) {
                    return;
                }
                let third = self.rect.x0 + self.rect.width() / 3.0;
                if self.mode == ReadingMode::Continuous {
                    return;
                }
                let target = if point.x < third {
                    self.page.saturating_sub(1)
                } else {
                    (self.page + 1).min(self.bound())
                };
                self.turn_to(target, true);
                *ctx.handled = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let down = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => *y < 0.0,
                    winit::event::MouseScrollDelta::PixelDelta(position) => position.y < 0.0,
                };
                if self.mode == ReadingMode::Continuous {
                    let target = if down {
                        self.place + 0.1
                    } else {
                        self.place - 0.1
                    };
                    self.scroll_to(target, true);
                    return;
                }
                let target = if down {
                    (self.page + 1).min(self.bound())
                } else {
                    self.page.saturating_sub(1)
                };
                self.turn_to(target, true);
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "Reading"
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let laid = self.laid.as_ref()?;
        let start = laid
            .layout
            .pages
            .get(self.first_page())
            .copied()
            .unwrap_or(0.0);
        let end = laid
            .layout
            .pages
            .get(self.first_page() + 1)
            .copied()
            .unwrap_or(laid.layout.total_height);
        let shown: Vec<&LaidBlock> = laid
            .layout
            .blocks
            .iter()
            .filter(|block: &&LaidBlock| {
                block.top + (block.bottom - block.top) > start && block.top < end
            })
            .collect();
        let words: Vec<&str> = shown.iter().map(|block| block.text.as_str()).collect();
        // The page reads as a document of its paragraphs, each a line a reader meets in order,
        // as the page sets them.
        let mut node =
            ui::a11y::AccessNode::new(ui::a11y::Role::Document).with_name(words.join("\n"));
        node.children = shown
            .iter()
            .filter(|block| !block.text.trim().is_empty())
            .map(|block| {
                let role = match block.kind {
                    LayoutKind::Heading => ui::a11y::Role::Heading { level: block.level },
                    LayoutKind::Separator => ui::a11y::Role::StaticText,
                    _ => ui::a11y::Role::Paragraph,
                };
                ui::a11y::AccessNode::new(role).with_name(block.text.clone())
            })
            .collect();
        Some(node)
    }
}

/// One highlight a reading app reports, read: `block:start:end:colour`. A range that marks no
/// characters, a number that is not one, or a colour that is not, marks nothing.
fn mark_range(entry: &str) -> Option<(usize, usize, usize, Color)> {
    let mut parts = entry.split(':');
    let block = parts.next()?.parse().ok()?;
    let start: usize = parts.next()?.parse().ok()?;
    let end: usize = parts.next()?.parse().ok()?;
    let colour = parts.next()?;
    if start >= end {
        return None;
    }
    let colour = ui::color::parse_hex(colour)?;
    Some((
        block,
        start,
        end,
        Color::from_rgb8(colour.r(), colour.g(), colour.b()),
    ))
}

#[cfg(test)]
mod tests {
    use tilo_ui_graph::rich::{Block as WireBlock, RichText};
    use ui::component::{Fired, Intent};
    use ui::testing::{dispatch, lay_out};
    use winit::keyboard::NamedKey;

    use super::*;

    fn reader(text: &str, scale: Option<f64>) -> (Reading, ui::Intents<Fired>) {
        let queue = ui::Intents::default();
        let props = ReadingProps {
            value: Some(RichText {
                blocks: vec![WireBlock {
                    kind: "paragraph".to_owned(),
                    text: text.to_owned(),
                    ..WireBlock::default()
                }],
                embeds: Vec::new(),
            }),
            scale,
            pages: Event::View(Intent::new(&queue, "r", "pages")),
            turned: Event::View(Intent::new(&queue, "r", "turned")),
            ..ReadingProps::default()
        };
        (Reading::from_props("r", props), queue)
    }

    fn long_text() -> String {
        "A reading page breaks its words into pages as the column it is given holds them. "
            .repeat(80)
    }

    #[test]
    fn a_long_text_is_more_than_one_page() {
        let (mut reading, _) = reader(&long_text(), None);
        lay_out(&mut reading, 480.0, 320.0).expect("the page lays out");
        assert!(reading.page_count() > 1, "the words take more than a page");
    }

    #[test]
    fn the_page_asked_for_past_the_end_shows_the_last() {
        let (mut reading, _) = reader(&long_text(), None);
        reading.set_props(ReadingProps {
            value: reading.text.clone(),
            page: Some(99),
            ..ReadingProps::default()
        });
        lay_out(&mut reading, 480.0, 320.0).expect("the page lays out");
        assert_eq!(reading.page, reading.page_count() - 1);
    }

    #[test]
    fn a_bigger_text_makes_more_pages() {
        let (mut small, _) = reader(&long_text(), Some(1.0));
        lay_out(&mut small, 480.0, 320.0).expect("the page lays out");
        let (mut large, _) = reader(&long_text(), Some(2.0));
        lay_out(&mut large, 480.0, 320.0).expect("the page lays out");
        assert!(
            large.page_count() > small.page_count(),
            "twice the text takes more pages: {} against {}",
            large.page_count(),
            small.page_count()
        );
    }

    #[test]
    fn the_next_key_turns_the_page_and_says_where_it_went() {
        let (mut reading, queue) = reader(&long_text(), None);
        lay_out(&mut reading, 480.0, 320.0).expect("the page lays out");
        assert!(reading.page_count() > 1);
        let _ = queue.take();
        assert!(dispatch(
            &mut reading,
            &ui::synthetic_input::named_key(NamedKey::ArrowRight)
        ));
        let fired = queue.take();
        assert_eq!(fired.len(), 1, "one turn: {fired:?}");
        assert_eq!(fired[0].event, "turned");
        match &fired[0].value {
            Some(Value::List(items)) => {
                assert_eq!(items.first(), Some(&Value::Int(1)), "the page it went to");
                assert!(
                    matches!(items.get(1), Some(Value::Int(_))),
                    "and the block the page begins at: {items:?}"
                );
            }
            other => panic!("a turn says its page and block, not {other:?}"),
        }
        assert_eq!(reading.page, 1);
    }

    #[test]
    fn the_page_counts_the_pages_it_has() {
        let (mut reading, queue) = reader(&long_text(), None);
        lay_out(&mut reading, 480.0, 320.0).expect("the page lays out");
        let count = reading.page_count();
        assert!(reading.report_pages());
        let fired = queue.take();
        assert_eq!(fired.len(), 1, "the count once: {fired:?}");
        assert_eq!(
            fired[0].value,
            Some(Value::Int(i64::try_from(count).unwrap()))
        );
        assert!(!reading.report_pages(), "the same count is not told twice");
        assert!(queue.take().is_empty());
    }

    #[test]
    fn two_up_counts_spreads_and_shows_two_pages() {
        let (mut reading, _) = reader(&long_text(), None);
        lay_out(&mut reading, 480.0, 320.0).expect("the page lays out");
        let paged = reading.page_count();
        let (mut spread, _) = reader(&long_text(), None);
        spread.set_props(ReadingProps {
            value: spread.text.clone(),
            mode: Some("two_up".to_owned()),
            ..ReadingProps::default()
        });
        lay_out(&mut spread, 480.0, 320.0).expect("the spread lays out");
        // A spread is two pages of half the width: its count is the pages it sets, two to a
        // spread, so a spread of a full-width page holds about as many as the page did.
        let laid_pages = spread
            .laid
            .as_ref()
            .expect("the spread laid out")
            .layout
            .page_count();
        assert_eq!(
            spread.page_count(),
            laid_pages.div_ceil(2),
            "a spread is two pages"
        );
        assert!(
            spread.page_count() * 2 > paged,
            "a spread shows two half-width pages: {laid_pages} against the page's {paged}"
        );
        assert_eq!(spread.first_page(), 0);
    }

    #[test]
    fn a_highlight_entry_bands_its_passage_in_its_colour() {
        assert_eq!(
            mark_range("2:4:9:#f5d76e"),
            Some((2, 4, 9, Color::from_rgb8(0xf5, 0xd7, 0x6e)))
        );
        assert_eq!(
            mark_range("2:9:4:#f5d76e"),
            None,
            "an empty range marks nothing"
        );
        assert_eq!(mark_range("two:4:9:#f5d76e"), None, "a block is a number");
        assert_eq!(mark_range("1:0:3"), None, "a colour is named");
    }

    #[test]
    fn a_turn_moves_on_the_shared_drill() {
        let (mut reading, _) = reader(&long_text(), None);
        lay_out(&mut reading, 480.0, 320.0).expect("the page lays out");
        assert!(reading.turn.is_none(), "a page at rest does not move");
        assert!(dispatch(
            &mut reading,
            &ui::synthetic_input::named_key(NamedKey::ArrowRight)
        ));
        assert!(reading.turn.is_some(), "the page it left is in motion");
        assert!(
            reading.needs_animation_frame(),
            "a turn asks for the frames it moves on"
        );
    }

    #[test]
    fn the_page_says_its_words_to_assistive_technology() {
        let (mut reading, _) = reader("A passage read aloud.", None);
        lay_out(&mut reading, 480.0, 320.0).expect("the page lays out");
        let node = reading.accessibility().expect("a page reads as a document");
        assert_eq!(node.role, ui::a11y::Role::Document);
        assert!(
            node.name
                .unwrap_or_default()
                .contains("A passage read aloud."),
            "the page's words are its name"
        );
        assert_eq!(
            node.children.len(),
            1,
            "the page reads a line per paragraph"
        );
        assert_eq!(
            node.children[0].role,
            ui::a11y::Role::Paragraph,
            "each line is a paragraph a reader meets"
        );
    }

    #[test]
    fn a_heading_reads_as_one_to_assistive_technology() {
        let mut heading = WireBlock {
            kind: "heading".to_owned(),
            text: "Chapter Five".to_owned(),
            ..WireBlock::default()
        };
        heading
            .attrs
            .insert("level".to_owned(), tilo_ui_graph::rich::Attr::Int(2));
        let mut reading = Reading::from_props(
            "r",
            ReadingProps {
                value: Some(RichText {
                    blocks: vec![
                        heading,
                        WireBlock {
                            kind: "paragraph".to_owned(),
                            text: "The words after it.".to_owned(),
                            ..WireBlock::default()
                        },
                    ],
                    embeds: Vec::new(),
                }),
                ..ReadingProps::default()
            },
        );
        lay_out(&mut reading, 480.0, 320.0).expect("the page lays out");
        let node = reading.accessibility().expect("a page reads as a document");
        assert_eq!(node.children.len(), 2);
        assert_eq!(
            node.children[0].role,
            ui::a11y::Role::Heading { level: 2 },
            "a heading reads as a heading, at its level"
        );
        assert_eq!(node.children[1].role, ui::a11y::Role::Paragraph);
    }

    #[test]
    fn a_selection_crosses_blocks_as_bands() {
        let mut rich = RichText {
            blocks: vec![
                WireBlock {
                    kind: "paragraph".to_owned(),
                    text: "The first block's words".to_owned(),
                    ..WireBlock::default()
                },
                WireBlock {
                    kind: "paragraph".to_owned(),
                    text: "The middle block".to_owned(),
                    ..WireBlock::default()
                },
                WireBlock {
                    kind: "paragraph".to_owned(),
                    text: "The last block's words".to_owned(),
                    ..WireBlock::default()
                },
            ],
            embeds: Vec::new(),
        };
        rich.blocks[2].kind = "paragraph".to_owned();
        let mut reading = Reading::from_props(
            "r",
            ReadingProps {
                value: Some(rich),
                ..ReadingProps::default()
            },
        );
        lay_out(&mut reading, 480.0, 320.0).expect("the page lays out");
        let passage = reading.passage(
            Band {
                block: 0,
                start: 4,
                end: 9,
            },
            Band {
                block: 2,
                start: 4,
                end: 9,
            },
        );
        assert_eq!(passage.len(), 3, "a band for each block it crosses");
        assert_eq!(
            passage[0],
            Band {
                block: 0,
                start: 4,
                end: "The first block's words".chars().count()
            }
        );
        assert_eq!(
            passage[1],
            Band {
                block: 1,
                start: 0,
                end: "The middle block".chars().count()
            }
        );
        assert_eq!(
            passage[2],
            Band {
                block: 2,
                start: 0,
                end: 9
            }
        );
    }

    #[test]
    fn a_mode_reads_from_its_name() {
        assert_eq!(ReadingMode::from_name("two_up"), ReadingMode::TwoUp);
        assert_eq!(
            ReadingMode::from_name("continuous"),
            ReadingMode::Continuous
        );
        assert_eq!(ReadingMode::from_name("paged"), ReadingMode::Paged);
        assert_eq!(ReadingMode::from_name("elsewhere"), ReadingMode::Paged);
    }
}
