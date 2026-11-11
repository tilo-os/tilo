//! `ListRow`: one row of a content list (a message, a contact, a request, a file, a device), as a
//! retained widget and as the [`ListRowFace`] immediate painter with identical geometry.
//!
//! A row has up to four columns, each optional except the title:
//!
//! | Column | Holds | Set in |
//! |---|---|---|
//! | leading | an [`Avatar`](crate::widget::Avatar), a person's or agent's face as presence draws it beside a glyph, an [`Icon`], a short badge (`GET`), a thumbnail, a checkbox, a swatch, in a fixed-width column so a list's titles line up | the leading slot's own geometry |
//! | text | the title (with tags after it), an optional subtitle, then an optional secondary line of one or two lines | `Body` (`.strong()` when emphasised), `Body`, `Caption` |
//! | facts | what the item is, in the same order on every row: when (a day and the time of day it is due), who (an avatar), how far (a progress count), how important (a mark); right-aligned on one edge | `Caption.numeric()`, `Caption`, icons |
//! | trailing | one persistent control, a status dot, a check, a disclosure chevron | — |
//!
//! Nothing stands inline after the title: a fact reads in the facts column, whose right edge is the
//! same on every row of a list, so the facts of a list read as one column down its edge. What a face
//! paints lives in `paint` beside this file; the rect of each of its columns in `geometry`.
//!
//! Hover (or keyboard focus) swaps the facts column for the row's actions. The row height comes from
//! the measured line boxes of its text, never a constant, so it grows with the text scale. Selection
//! is the lift (HIG §9.3), and rows sit edge to edge, with no rule between them.
//!
//! **One content edge.** The row's text starts on its column's content edge; the hover and
//! selection lift bleed [`row_bleed`] past it on both sides. A face paints into the row's *outer*
//! rect (the content edge grown by the bleed); a retained row grows its own layout box by the bleed
//! when it paints, so its title lines up with the section title above it.
//!
//! [`row_bleed`]: crate::chrome::row_bleed

use std::borrow::Cow;
use std::collections::HashMap;

use crate::a11y::{AccessNode, AccessState, Role};
use crate::chrome::{RowJoin, SelectionState};
use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{FontChoice, TextRenderer};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::tokens::colors::PresenceHue;
use crate::widget::activation::{Activation, Interaction};
use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, ImageData};
use winit::event::{ElementState, MouseButton, WindowEvent};

/// The operation type of a row's leading checkbox turned.
const CHECK_ROW: &str = "tilo.ui.row.check";
use crate::widget::avatar::AvatarFace;
use crate::widget::{
    ActionEmitter, ChipStyle, DrawChild, Element, Fact, MediaArt, MediaAspect, MediaSource,
    TaffyTreeExt, Tone, Widget,
};

/// What sits in a row's leading column.
#[derive(Clone, Copy)]
pub enum RowLeading<'a> {
    /// A person or an account (`AvatarFace::square()` for an account or a
    /// site).
    Avatar(AvatarFace<'a>),
    /// A kind of thing (a folder, a device, a setting).
    Icon(Icon),
    /// A short code that classifies the row: an HTTP method, a file
    /// extension, a log level. Set in `Caption.strong()` on a quiet pill.
    Badge(&'a str),
    /// A decoded picture, cover-fitted in a square tile (a document's first
    /// page, a photo).
    Thumbnail(&'a ImageData),
    /// Media art at its aspect (a video's frame, an album cover); missing
    /// art shows the generated placeholder seeded with the title's initials.
    Media(MediaArt<'a>, MediaAspect),
    /// A selection checkbox (a multi-select list). Hit-test it with
    /// [`ListRowFace::leading_rect`]. `icon` stands after the box, for a row that is checked off
    /// and still shows what kind of thing it is (a task and its priority).
    Checkbox { checked: bool, icon: Option<Icon> },
    /// A colour that is the row's content (a calendar's colour, a label).
    Swatch(Color),
    /// How strong a radio signal is (a Wi-Fi network): the
    /// [`SignalMeterFace`](crate::widget::SignalMeterFace) at icon size.
    Signal(crate::widget::SignalLevel),
    /// A person's or an agent's face as presence draws it (`presence::Face`): their colour, and
    /// for software the rounded square and the mark. It takes the icon's slot, so a row led by a
    /// face keeps the text column a row led by a glyph does.
    Face(crate::presence::Face<'a>),
    /// A painter for a leading mark no other kind covers (a font sample, a
    /// waveform), given the leading slot.
    Paint(&'a dyn Fn(&mut PaintCtx<'_>, Rect)),
    /// The room an avatar takes, left empty: a row of a list whose rows show
    /// avatars that has none (a conversation with no messages yet) keeps its
    /// text in line with theirs.
    AvatarSpace,
}

impl std::fmt::Debug for RowLeading<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Avatar(face) => f.debug_tuple("Avatar").field(face).finish(),
            Self::Icon(icon) => f.debug_tuple("Icon").field(icon).finish(),
            Self::Badge(badge) => f.debug_tuple("Badge").field(badge).finish(),
            Self::Thumbnail(_) => f.write_str("Thumbnail"),
            Self::Media(_, aspect) => f.debug_tuple("Media").field(aspect).finish(),
            Self::Checkbox { checked, icon } => f
                .debug_struct("Checkbox")
                .field("checked", checked)
                .field("icon", icon)
                .finish(),
            Self::Swatch(colour) => f.debug_tuple("Swatch").field(colour).finish(),
            Self::Signal(level) => f.debug_tuple("Signal").field(level).finish(),
            Self::Face(face) => f.debug_tuple("Face").field(face).finish(),
            Self::Paint(_) => f.write_str("Paint"),
            Self::AvatarSpace => f.write_str("AvatarSpace"),
        }
    }
}

/// A status dot on the row's trailing edge, on the title's line; beside a
/// check or a disclosure chevron it centres with them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowStatus<'a> {
    /// Running, connected, healthy.
    Active,
    /// Needs attention soon.
    Warning,
    /// Failed or offline unexpectedly.
    Error,
    /// Present but inactive.
    Idle,
    /// A person's presence, in their colour.
    Presence(&'a str),
}

impl RowStatus<'_> {
    pub(crate) fn colour(self, theme: &Theme) -> Color {
        let colors = &theme.tokens.colors;
        match self {
            Self::Active => colors.semantic.success,
            Self::Warning => colors.semantic.warning,
            Self::Error => colors.semantic.danger,
            Self::Idle => theme.text_tertiary,
            Self::Presence(id) => colors.presence.hue(PresenceHue::of_identity(id)),
        }
    }

    pub(crate) fn spoken(self) -> Option<&'static str> {
        match self {
            Self::Active => Some("Active"),
            Self::Warning => Some("Needs attention"),
            Self::Error => Some("Error"),
            Self::Idle => Some("Idle"),
            Self::Presence(_) => None,
        }
    }
}

/// How prominent a row's trailing value is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RowValue {
    /// A figure beside the text (a balance, a temperature in a list).
    #[default]
    Body,
    /// The row's main figure (a clock's time, a one-time code).
    Title,
}

/// Measure the leading column that fits every badge in `badges`, so a list
/// of rows with badges of different lengths keeps one title edge. Pass the
/// result to every row's [`ListRowFace::leading_column`]. A [`Section`]
/// does this for the retained rows it holds.
///
/// [`Section`]: crate::widget::Section
pub fn badge_column_width(text: &mut TextRenderer, theme: &Theme, badges: &[&str]) -> f64 {
    let metrics = RowMetrics::new(theme);
    let pad = f64::from(theme.tokens.spacing.s1) * 2.0;
    badges
        .iter()
        .map(|badge| f64::from(text.measure_style(badge, None, &metrics.badge).0) + pad)
        .fold(0.0, f64::max)
        .ceil()
}

/// The fill a row's lift shows: the selection lift of the content ground
/// when selected (or pressed), half of it under the pointer, nothing at rest.
fn row_fill(theme: &Theme, selected: bool, hovered: bool) -> Option<Color> {
    let state = SelectionState::new(selected, hovered, false);
    (selected || hovered).then(|| {
        crate::chrome::selection_tier_on(theme, state, theme.tokens.colors.surfaces.content).fill
    })
}

/// The laid-out columns of one row.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct RowGeometry {
    /// Where the leading column sits.
    leading: Rect,
    /// Where the title starts (the separator starts here too).
    text_x: f64,
    text_right: f64,
    /// Where the title's line ends: the facts column narrows it.
    title_right: f64,
    /// Where the line under the title ends.
    second_right: f64,
    /// The persistent trailing control's slot.
    trailing: Option<Rect>,
    /// The avatar in the facts column, where the row shows one.
    avatar: Option<Rect>,
    /// The trailing value's left edge in the facts column.
    value_right: f64,
    /// The status dot's centre x.
    dot_x: Option<f64>,
    /// The face of whoever made a change the person has not seen.
    change: Option<Rect>,
    check: Option<Rect>,
    /// The disclosure chevron's slot.
    disclosure: Option<Rect>,
    /// Where the marks in the facts column end (the column's own right edge).
    marks_right: Option<f64>,
    /// Where a count's pill ends in the facts column.
    count_right: Option<f64>,
    /// Where the meta's right edge sits in the facts column.
    meta_right: f64,
    /// Where the meta's room starts: what lies left of it is the text's.
    meta_left: f64,
    /// Where the facts' line stands in a card, which puts them under the text; a row stands
    /// them beside it, on the title's line.
    facts_cy: f64,
}

/// How many marks [`ListRowFace::mark`] keeps on a row's edge.
pub const MAX_MARKS: usize = 3;

/// How many lines a card's title may take before it is cut with an ellipsis: a card is a small
/// document, and its title says what it is without being opened.
pub const CARD_TITLE_LINES: u8 = 3;

/// A widget that can be shown as a card: what a card component (a board's
/// [`BoardCard`](super::BoardCard)) asks of the one widget it was given, so the card is that
/// widget's own surface and what it says is written once. Asked as a capability, so a widget
/// wrapped in a host's pane, a context menu or any other decorator still answers.
pub trait CardBody {
    /// Show as a card: its own surface, its title wrapped over up to [`CARD_TITLE_LINES`]
    /// lines, its facts on a line under the title, and a press reporting `activate` — and
    /// selection, where it is the selected one.
    fn show_as_card(&mut self, selected: bool, activate: crate::component::Event);
}

/// One mark on a row's edge: an icon in tertiary ink or the row's tone, or a swatch in a
/// colour that belongs to the item (a task's project, a calendar).
#[derive(Clone, Copy, Debug, PartialEq)]
enum RowMark {
    Icon(Icon),
    Swatch(Color),
}

/// Immediate-mode list row. Build one per row per frame, `measure` it for
/// its height, then `paint` it into the rect you laid out: the row's outer
/// rect, the list's content edge grown by [`row_bleed`] on both sides.
///
/// [`row_bleed`]: crate::chrome::row_bleed
#[derive(Clone, Debug)]
pub struct ListRowFace<'a> {
    title: &'a str,
    /// The item of its app's collection the row shows
    /// ([`ListRow::item`](crate::widget::ListRow::item)).
    item: Option<&'a str>,
    /// A quiet word before the title, on its line.
    prefix: Option<&'a str>,
    title_lines: u8,
    title_family: Option<&'a FontChoice>,
    /// The words a search found the row by, marked where the title has them.
    found: &'a [&'a str],
    subtitle: Option<&'a str>,
    secondary: Option<&'a str>,
    /// The line under the title as separate facts, painted where a secondary line would stand:
    /// facts are their own components, never one string joined by a separator glyph.
    facts: &'a [Fact],
    secondary_lines: u8,
    /// A line under the text of the row's own: what waits on the item.
    note: Option<&'a str>,
    meta: Option<&'a str>,
    meta_spans: &'a [(&'a str, Tone)],
    value: Option<(&'a str, RowValue)>,
    /// Who the item is, as a fact: their face in the facts column.
    avatar: Option<AvatarFace<'a>>,
    marks: [Option<RowMark>; MAX_MARKS],
    /// The tone the row's own mark reads in: an item's priority, a status.
    mark_tone: Tone,
    tags: &'a [(&'a str, ChipStyle)],
    count: Option<usize>,
    leading: Option<RowLeading<'a>>,
    leading_column: Option<f64>,
    /// Indented one leading column under the row above it.
    nested: bool,
    /// How many rows over it the row stands: its content indents one
    /// disclosure gutter per depth, while its lift fills the row.
    depth: u32,
    status: Option<RowStatus<'a>>,
    change: Option<RowChange<'a>>,
    struck: bool,
    /// The title reads in the weaker ink, as a subordinate row of a tree does: not struck
    /// through, just quiet.
    quiet: bool,
    /// The title reads in the quietest ink: a row a window shows for where another stands, as
    /// a tree narrowed to a search shows the branches to what it found.
    faint: bool,
    disclosure: bool,
    /// How far the chevron has turned towards pointing down, 0 to 1: a row
    /// that opens in place rather than onto a page.
    disclosure_turn: f64,
    checked: Option<bool>,
    emphasized: bool,
    selected: bool,
    join: RowJoin,
    next_selected: bool,
    separator: bool,
    actions: &'a [RowAction<'a>],
    hovered_action: Option<usize>,
    reveal_actions: bool,
    swipe: Option<(SwipeAction<'a>, f64)>,
    trailing_width: Option<f64>,
    /// The room the list keeps after every row's trailing control, at
    /// least: its rows' controls stand in one column.
    trailing_after: f64,
    interaction: Interaction,
    /// The rect the lift, separator and focus ring take when it differs from
    /// the rect the text is laid out in (a retained row whose trailing
    /// control wrapped under its text).
    frame: Option<Rect>,
    /// The least a row a person presses is tall: the input's minimum for adjacent collection
    /// rows (48 for a finger, as a button's face grows to its target), so a finger never lands
    /// on the row beside it.
    min_target: f64,
    /// The smallest the press on the leading box may be: the target the person's pointer needs
    /// (`interaction::InteractionEnvironment::minimum_target`), so a finger on a box is never a
    /// press on the row. Zero where nothing leads with a box.
    check_target: f64,
    /// Where the leading checkbox is on its way at this frame, from the row's
    /// [`crate::widget::checkbox::CheckMotion`]; at rest on its value when unset.
    check_pose: Option<crate::widget::checkbox::CheckPose>,
    /// The row as a card: its own surface, its title wrapped over up to
    /// [`CARD_TITLE_LINES`] lines, and its facts on a line under the text instead of beside it.
    card: bool,
    /// The row as a dense row: one quiet line at the caption's size, for a reference that
    /// stands among content or a list that shows a lot at once.
    dense: bool,
    /// The row's own height, where it is not the list's row height: an outline's row token.
    /// The row stands at it, taller only when what it holds needs more.
    row_height: Option<f64>,
}

impl<'a> ListRowFace<'a> {
    /// Who else is at the item, when the row shows them: the identity its presence dot stands
    /// for ([`ListRow::presence`]).
    #[cfg(test)]
    pub(crate) fn presence_identity(&self) -> Option<&'a str> {
        match self.status {
            Some(RowStatus::Presence(identity)) => Some(identity),
            _ => None,
        }
    }

    /// Whether the row stands in for an item that was removed ([`ListRow::struck`]): its title
    /// struck through.
    #[cfg(test)]
    pub(crate) fn title_struck(&self) -> bool {
        self.struck
    }

    /// Say which item of its collection the row shows
    /// ([`AccessNode::item`](crate::a11y::AccessNode::item)).
    #[must_use]
    pub fn item(mut self, item: Option<&'a str>) -> Self {
        self.item = item;
        self
    }

    #[must_use]
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            item: None,
            prefix: None,
            title_lines: 1,
            title_family: None,
            found: &[],
            subtitle: None,
            secondary: None,
            facts: &[],
            note: None,
            secondary_lines: 1,
            meta: None,
            meta_spans: &[],
            value: None,
            avatar: None,
            marks: [None; MAX_MARKS],
            mark_tone: Tone::Neutral,
            tags: &[],
            count: None,
            leading: None,
            leading_column: None,
            nested: false,
            depth: 0,
            status: None,
            change: None,
            struck: false,
            quiet: false,
            faint: false,
            disclosure: false,
            disclosure_turn: 0.0,
            checked: None,
            emphasized: false,
            selected: false,
            join: RowJoin::NONE,
            next_selected: false,
            separator: false,
            actions: &[],
            hovered_action: None,
            reveal_actions: false,
            swipe: None,
            trailing_width: None,
            trailing_after: 0.0,
            interaction: Interaction::default(),
            frame: None,
            min_target: 0.0,
            check_target: 0.0,
            check_pose: None,
            card: false,
            dense: false,
            row_height: None,
        }
    }

    /// The row as a card: a surface of its own, its title wrapped over up to
    /// [`CARD_TITLE_LINES`] lines with the facts on a line under the text, in the order they
    /// read in a row.
    #[must_use]
    pub fn card(mut self) -> Self {
        self.card = true;
        self
    }

    /// The row as a dense row: one line at the caption's size, the small glyph, as tall as that
    /// line and its inset: a quiet reference under content, or a list that shows a lot at once.
    #[must_use]
    pub fn dense(mut self) -> Self {
        self.dense = true;
        self
    }

    /// Stand the row `px` tall, rather than the list's row height for what it holds: a tree's
    /// own row token, shorter than a list's. What the row holds still fits inside it.
    #[must_use]
    pub fn row_height(mut self, px: f32) -> Self {
        self.row_height = Some(f64::from(px));
        self
    }

    /// How many lines the title may take: [`Self::two_line_title`] sets two, a card sets what
    /// its title wraps to.
    #[must_use]
    pub fn title_lines(mut self, lines: u8) -> Self {
        self.title_lines = lines.clamp(1, CARD_TITLE_LINES);
        self
    }

    /// The metrics this face lays out with: a card's, or a dense row's, where it is one.
    fn metrics(&self, theme: &Theme) -> RowMetrics {
        if self.card {
            RowMetrics::card(theme)
        } else if self.dense {
            RowMetrics::dense(theme)
        } else {
            RowMetrics::new(theme)
        }
    }

    /// Draw the leading checkbox where its motion is this frame rather than at rest on its
    /// value ([`crate::widget::checkbox::CheckMotion::pose`]).
    #[must_use]
    pub fn check_pose(mut self, pose: crate::widget::checkbox::CheckPose) -> Self {
        self.check_pose = Some(pose);
        self
    }

    /// Mark `words`, what a search found the row by, where the title has
    /// them: each at the start of a word, whatever its case, under the find
    /// bar's match wash ([`paint_find_match`](crate::widget::paint_find_match)).
    #[must_use]
    pub fn found(mut self, words: &'a [&'a str]) -> Self {
        self.found = words;
        self
    }

    /// A quiet word before the title, on its line and in the weaker ink: where the item came
    /// from. The title takes the room left after it.
    #[must_use]
    pub fn prefix(mut self, word: &'a str) -> Self {
        self.prefix = Some(word);
        self
    }

    /// Let the title wrap to two lines before it is elided (a long name in a
    /// narrow list).
    #[must_use]
    pub fn two_line_title(mut self) -> Self {
        self.title_lines = 2;
        self
    }

    /// Set the title in `family`: only for a list of fonts, where the title
    /// is its own sample.
    #[must_use]
    pub fn title_family(mut self, family: &'a FontChoice) -> Self {
        self.title_family = Some(family);
        self
    }

    /// A `Body` line between the title and the secondary text (a message's
    /// subject under its sender).
    #[must_use]
    pub fn subtitle(mut self, text: &'a str) -> Self {
        self.subtitle = Some(text);
        self
    }

    /// The secondary line under the title (a preview, an address, a path),
    /// elided to one line.
    #[must_use]
    pub fn secondary(mut self, text: &'a str) -> Self {
        self.secondary = Some(text);
        self
    }

    /// The line under the title as separate facts ([`Fact`]), in place of a secondary line: a
    /// host and what it used, a plan and its usage. They stand apart by a sibling gap and each
    /// elides on its own, so nothing must join them into one string with a separator.
    #[must_use]
    pub fn facts(mut self, facts: &'a [Fact]) -> Self {
        self.facts = facts;
        self
    }

    /// A line under the text of the row's own: what waits on the item, which the app says and
    /// the row only carries.
    #[must_use]
    pub fn note(mut self, text: &'a str) -> Self {
        self.note = Some(text);
        self
    }

    /// Let the secondary text wrap to two lines before it is elided (a
    /// message preview).
    #[must_use]
    pub fn two_line_secondary(mut self) -> Self {
        self.secondary_lines = 2;
        self
    }

    /// Let the secondary text take up to `lines` lines (one to four): a
    /// thing whose facts each stand on a line of their own.
    #[must_use]
    pub fn secondary_lines(mut self, lines: u8) -> Self {
        self.secondary_lines = lines.clamp(1, 4);
        self
    }

    /// When it happened or is due (a day, with the time of day it is due where the view pairs
    /// one), in tabular figures: the first fact of the facts column, on the title's line.
    #[must_use]
    pub fn meta(mut self, text: &'a str) -> Self {
        self.meta = Some(text);
        self
    }

    /// A meta made of toned figures (`+2.4%` in success, `−0.8%` in danger),
    /// each made legible on the row's ground and on its lift.
    #[must_use]
    pub fn meta_spans(mut self, spans: &'a [(&'a str, Tone)]) -> Self {
        self.meta_spans = spans;
        self
    }

    /// A figure among the facts (how far a branch got, a temperature), between who it is and
    /// how important it is.
    #[must_use]
    pub fn value(mut self, text: &'a str) -> Self {
        self.value = Some((text, RowValue::Body));
        self
    }

    /// [`Self::value`] as the row's main figure, in `Title`.
    #[must_use]
    pub fn title_value(mut self, text: &'a str) -> Self {
        self.value = Some((text, RowValue::Title));
        self
    }

    /// Who the item is, as a fact in the facts column: their face, between when it is due and
    /// how far it has got. A row led by an avatar ([`RowLeading::Avatar`]) is a row that _is_
    /// the person (a contact); a row that is something a person is on shows them here.
    #[must_use]
    pub fn avatar(mut self, avatar: AvatarFace<'a>) -> Self {
        self.avatar = Some(avatar);
        self
    }

    /// A mark on the row's edge, after every other fact (starred, pinned, shared, how much it
    /// matters), in tertiary ink or the row's own tone. Each call adds one more, in order, up
    /// to [`MAX_MARKS`]; later ones are ignored. Star, pin and bookmark are filled.
    #[must_use]
    pub fn mark(mut self, icon: Icon) -> Self {
        self.push_mark(RowMark::Icon(icon));
        self
    }

    /// The tone the row's mark reads in (an item's priority): a theme role,
    /// never a colour.
    #[must_use]
    pub fn mark_tone(mut self, tone: Tone) -> Self {
        self.mark_tone = tone;
        self
    }

    /// A colour the item carries (its project, its calendar), as a small
    /// swatch among the marks after the title. It counts as one of the
    /// [`MAX_MARKS`].
    #[must_use]
    pub fn swatch_mark(mut self, color: Color) -> Self {
        self.push_mark(RowMark::Swatch(color));
        self
    }

    fn push_mark(&mut self, mark: RowMark) {
        if let Some(slot) = self.marks.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(mark);
        }
    }

    /// Tags after the title, each a small pill in its style.
    #[must_use]
    pub fn tags(mut self, tags: &'a [(&'a str, ChipStyle)]) -> Self {
        self.tags = tags;
        self
    }

    /// How many items the row stands for (the messages in a thread, a branch's open steps), in
    /// a quiet pill among the facts, after the figure.
    #[must_use]
    pub fn count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }

    #[must_use]
    pub fn leading(mut self, leading: RowLeading<'a>) -> Self {
        self.leading = Some(leading);
        self
    }

    /// The width every row of this list reserves for its leading column,
    /// from [`badge_column_width`]. Any kind of leading content centres in a
    /// column at least this wide.
    #[must_use]
    pub fn leading_column(mut self, width: f64) -> Self {
        self.leading_column = Some(width);
        self
    }

    /// Indent the row one leading column, under the row above it: an
    /// option that belongs to the one above and shows only while it applies.
    #[must_use]
    pub fn nested(mut self, nested: bool) -> Self {
        self.nested = nested;
        self
    }

    /// Indent the row's content `depth` steps in, one disclosure gutter each, as a
    /// subtree's rows stand under their parents.
    #[must_use]
    pub fn depth(mut self, depth: u32) -> Self {
        self.depth = depth;
        self
    }

    #[must_use]
    pub fn change(mut self, change: RowChange<'a>) -> Self {
        self.change = Some(change);
        self
    }

    /// The row stands for an item that was removed: its title struck through, in the weaker
    /// ink, at the row's own height.
    #[must_use]
    pub fn struck(mut self, struck: bool) -> Self {
        self.struck = struck;
        self
    }

    /// The row reads as subordinate to the rows around it: its title in the weaker ink, as an
    /// outline's quieter branch does.
    #[must_use]
    pub fn quiet(mut self, quiet: bool) -> Self {
        self.quiet = quiet;
        self
    }

    /// The row reads as one a window shows for where another stands: its title in the
    /// quietest ink, under a match a search keeps in its place.
    #[must_use]
    pub fn faint(mut self, faint: bool) -> Self {
        self.faint = faint;
        self
    }

    /// Where the face of an unseen change sits in `rect`, when the row shows one.
    #[must_use]
    pub fn change_rect(&self, rect: Rect, text: &mut TextRenderer, theme: &Theme) -> Option<Rect> {
        self.geometry(rect, &self.metrics(theme), text, theme)
            .change
    }

    #[must_use]
    pub fn status(mut self, status: RowStatus<'a>) -> Self {
        self.status = Some(status);
        self
    }

    /// A trailing chevron: activating the row opens a page.
    #[must_use]
    pub fn disclosure(mut self) -> Self {
        self.disclosure = true;
        self
    }

    /// A trailing chevron for a row that opens in place, turned `turn` of
    /// the way (0 closed, 1 open) from pointing at the row's end to
    /// pointing down — [`crate::motion::Disclose`]'s `turn`.
    #[must_use]
    pub fn disclosure_turned(mut self, turn: f64) -> Self {
        self.disclosure = true;
        self.disclosure_turn = turn.clamp(0.0, 1.0);
        self
    }

    /// One choice of a single-choice list: a trailing check on the chosen
    /// row. Every row of the list reserves the check's slot.
    #[must_use]
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    /// Unread or otherwise new: the title takes the strong weight.
    #[must_use]
    pub fn emphasized(mut self, emphasized: bool) -> Self {
        self.emphasized = emphasized;
        self
    }

    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Which neighbours are selected with this row: a run of selected rows
    /// is one lift with no notch.
    #[must_use]
    pub fn joined(mut self, join: RowJoin) -> Self {
        self.join = join;
        self
    }

    /// Draw the inset separator along the bottom edge. It hides when this
    /// row or `next_selected` (the row below) is selected.
    #[must_use]
    pub fn separator(mut self, next_selected: bool) -> Self {
        self.separator = true;
        self.next_selected = next_selected;
        self
    }

    /// Actions shown in place of the meta while the row is hovered or
    /// focused. Hit-test them with [`Self::action_rects`].
    #[must_use]
    pub fn actions(mut self, actions: &'a [RowAction<'a>]) -> Self {
        self.actions = actions;
        self
    }

    /// Which action the pointer (or the keyboard) is on.
    #[must_use]
    pub fn hovered_action(mut self, index: Option<usize>) -> Self {
        self.hovered_action = index;
        self
    }

    /// Show the actions without hover: a selected row under a touch pointer,
    /// which has no hover.
    #[must_use]
    pub fn reveal_actions(mut self, reveal: bool) -> Self {
        self.reveal_actions = reveal;
        self
    }

    /// The row slid `offset` logical pixels toward its leading edge,
    /// revealing `action` behind its trailing edge.
    #[must_use]
    pub fn swiped(mut self, action: SwipeAction<'a>, offset: f64) -> Self {
        self.swipe = Some((action, offset.max(0.0)));
        self
    }

    /// Reserve `width` at the trailing edge for one persistent control (a
    /// switch, a button, a small progress ring) the host lays out itself in
    /// [`Self::trailing_rect`]. Separators keep their full width.
    #[must_use]
    pub fn trailing_width(mut self, width: f64) -> Self {
        self.trailing_width = Some(width.max(0.0));
        self
    }

    /// Keep at least `width` after the trailing slot, where a row of the
    /// same list holds a mark this one does not: the list's controls stand
    /// in one column.
    #[must_use]
    pub(crate) fn trailing_after(mut self, width: f64) -> Self {
        self.trailing_after = width.max(0.0);
        self
    }

    #[must_use]
    pub fn interaction(mut self, interaction: Interaction) -> Self {
        self.interaction = interaction;
        self
    }

    /// A disabled row: dimmed, no hover, no actions.
    #[must_use]
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.interaction.disabled = !enabled;
        self
    }

    #[must_use]
    pub(crate) fn frame(mut self, frame: Rect) -> Self {
        self.frame = Some(frame);
        self
    }

    /// The number of lines under the title.
    fn untitled(&self) -> bool {
        self.title.trim().is_empty()
    }

    /// The title as the row shows and speaks it: an item nobody has named yet shows a muted
    /// placeholder, so its row stays visible and pressable until it has one.
    fn shown_title(&self) -> Cow<'_, str> {
        if self.untitled() {
            Cow::Owned(crate::i18n!("list-row-untitled"))
        } else {
            Cow::Borrowed(self.title)
        }
    }

    /// The name a row is announced by: its badge, then its title.
    fn spoken_name(&self) -> String {
        let title = self.shown_title();
        // The row says its quiet word and its badge before its title, as it draws them there.
        let mut name = String::new();
        if let Some(RowLeading::Badge(badge)) = &self.leading {
            name.push_str(badge);
            name.push(' ');
        }
        if let Some(prefix) = &self.prefix {
            name.push_str(prefix);
            name.push(' ');
        }
        name.push_str(&title);
        name
    }

    /// The row's accessibility node: a list item named by its badge and
    /// title, with the subtitle, secondary line, meta, value, count, tags
    /// and status as its value, and one button child per action.
    pub fn accessibility(&self, rect: Rect, action_target: impl Into<String>) -> AccessNode {
        let spans: String = self
            .meta_spans
            .iter()
            .map(|(span, _)| *span)
            .collect::<Vec<_>>()
            .join(" ");
        let count = self.count.map(|count| count.to_string());
        // Separate facts read as their own words on the line under the title.
        let facts: String = self
            .facts
            .iter()
            .map(|fact| fact.text.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        // A toned meta draws as spans of its own words: the row says it once.
        let meta = self.meta.filter(|_| self.meta_spans.is_empty());
        let mut value: Vec<&str> = self.tags.iter().map(|(label, _)| *label).collect();
        value.extend(
            [
                self.subtitle,
                self.secondary,
                (!facts.is_empty()).then_some(facts.as_str()),
                self.note,
                meta,
                (!spans.is_empty()).then_some(spans.as_str()),
                self.value.map(|(value, _)| value),
                count.as_deref(),
                self.status.and_then(RowStatus::spoken),
                self.change.map(|change| change.spoken),
            ]
            .into_iter()
            .flatten(),
        );
        let checked = match (self.checked, self.leading) {
            (Some(checked), _) | (None, Some(RowLeading::Checkbox { checked, .. })) => {
                Some(checked)
            }
            _ => None,
        };
        let mut state = AccessState::new()
            .focusable(true)
            .focused(self.interaction.focused)
            .selected(self.selected)
            .disabled(self.interaction.disabled);
        if let Some(checked) = checked {
            state = state.checked(checked);
        }
        let target: String = action_target.into();
        let mut node = AccessNode::new(Role::ListItem)
            .with_name(self.spoken_name())
            .with_action_target(target.clone())
            .with_bounds(rect)
            .with_state(state);
        if let Some(item) = self.item {
            node = node.with_item(item);
        }
        if !value.is_empty() {
            node = node.with_value(value.join(", "));
        }
        if !self.actions.is_empty() {
            // Each action is a button of its own, addressed as
            // `<row>/action/<index>`; the keyboard reaches them through the
            // row (the arrow keys), so they are not tab stops.
            node = node.with_children(
                self.actions
                    .iter()
                    .enumerate()
                    .map(|(index, action)| {
                        AccessNode::new(Role::Button)
                            .with_name(action.label)
                            .with_action_target(format!("{target}/action/{index}"))
                            .with_state(AccessState::new().disabled(self.interaction.disabled))
                    })
                    .collect(),
            );
        }
        node
    }
}

/// The colour a row's leading avatar is drawn in where it has one of its own,
/// instead of the one its identity derives.
#[derive(Clone, Copy, Debug, PartialEq)]
enum AvatarTint {
    /// A colour chosen outright: an agent's keeper picked it.
    Colour(Color),
    /// One of the presence palette's hues, named by the caller: an agent's
    /// client, warm for one and cool for another.
    Hue(PresenceHue),
}

/// Owned form of [`RowLeading`] for the retained row.
enum Leading {
    Avatar {
        name: String,
        identity: String,
        image: Option<ImageData>,
        square: bool,
        glyph: Option<Icon>,
        tint: Option<AvatarTint>,
        /// Software: the agent's own face ([`AvatarFace::agent`]).
        agent: bool,
    },
    /// The room an avatar takes, left empty ([`RowLeading::AvatarSpace`]).
    AvatarSpace,
    Icon(Icon),
    Badge(String),
    Thumbnail(ImageData),
    Media {
        source: MediaSource,
        aspect: MediaAspect,
        loaded: Option<ImageData>,
    },
    Checkbox {
        checked: bool,
        on_toggle: Box<dyn FnMut(bool)>,
        /// A glyph after the box: what kind of thing the row is.
        icon: Option<Icon>,
    },
    Swatch(Color),
    Signal(crate::widget::SignalLevel),
}

/// Who a row is about, as a fact in its facts column ([`ListRow::avatar`]): their face.
struct FactAvatar {
    name: String,
    identity: String,
    image: Option<ImageData>,
    square: bool,
    glyph: Option<Icon>,
}

impl FactAvatar {
    fn face(&self) -> AvatarFace<'_> {
        let face = AvatarFace::new(&self.name, &self.identity).image(self.image.as_ref());
        let face = if self.square { face.square() } else { face };
        match self.glyph {
            Some(glyph) => face.glyph(glyph),
            None => face,
        }
    }
}

/// What a retained hover action runs.
enum ActionHandler {
    Callback(Box<dyn FnMut()>),
    Bound(ActionEmitter<()>),
}

struct OwnedAction {
    icon: Icon,
    label: String,
    handler: ActionHandler,
}

impl OwnedAction {
    fn run(&mut self, control: &str) {
        match &mut self.handler {
            ActionHandler::Callback(f) => f(),
            ActionHandler::Bound(emitter) => {
                if let Err(error) = emitter.emit(()) {
                    tracing::warn!(%error, %control, "row action refused its arguments");
                }
            }
        }
    }

    fn is_available(&self) -> bool {
        match &self.handler {
            ActionHandler::Callback(_) => true,
            ActionHandler::Bound(emitter) => emitter.is_available(),
        }
    }
}

struct OwnedSwipe {
    icon: Icon,
    label: String,
    destructive: bool,
    on_activate: Option<Box<dyn FnMut()>>,
    /// What releasing the swipe past the threshold presses, when the view bound it: the row
    /// answers for it as it does for its other actions.
    press: crate::widget::Press,
}

impl OwnedSwipe {
    /// Run it: the action the view bound, or the row's own callback.
    fn run(&mut self) {
        if !self.press.is_none() {
            self.press.run();
        } else if let Some(callback) = &mut self.on_activate {
            callback();
        }
    }
}

/// Distance past which a released swipe completes, as a share of the row.
const SWIPE_COMMIT: f64 = 0.4;

/// The narrowest a row's text may get, in line boxes of its title, before
/// its trailing control wraps under it.
const MIN_TEXT_LINES: u8 = 8;

/// An unseen change a retained row shows ([`ListRow::change`]).
struct Changed {
    identity: String,
    name: String,
    spoken: String,
    said: String,
    show: Option<Box<dyn FnMut()>>,
}

/// The change a retained row shows, as [`ListRow::shown_change`] reads it: who made it, what
/// they are called, what they did (the sentence under the title), and what assistive
/// technology hears of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShownChange<'a> {
    /// Who made the change.
    pub identity: &'a str,
    /// What they are called.
    pub name: &'a str,
    /// What they did, in words.
    pub said: &'a str,
    /// Who changed it and in which run, as the row's tooltip and assistive technology say it.
    pub spoken: &'a str,
}

/// A retained list row. Construct with a stable id and a title; press
/// activates it (pointer, Enter/Space, or assistive technology).
pub struct ListRow {
    title: String,
    /// The item of its app's collection the row shows, when it shows one: the
    /// identity an action naming the item is found by, unlike its title.
    item: Option<String>,
    /// A quiet word before the title, on its line: where an item came from.
    prefix: Option<String>,
    /// Whether a layout pass has built the row.
    laid_out: bool,
    /// Whether the last paint cut the title short; the tooltip then shows it
    /// in full.
    title_elided: bool,
    two_line_title: bool,
    title_family: Option<FontChoice>,
    subtitle: Option<String>,
    secondary: Option<String>,
    /// How many lines the secondary text may take.
    secondary_line_count: u8,
    /// A line under the text of the row's own: what waits on the item.
    note: Option<String>,
    meta: Option<String>,
    meta_spans: Vec<(String, Tone)>,
    value: Option<(String, RowValue)>,
    mark: Option<Icon>,
    /// The tone the row's mark and its checkbox ring read in: an item's priority.
    mark_tone: Tone,
    tags: Vec<(String, ChipStyle)>,
    count: Option<usize>,
    /// Who the row is about, as a fact in the facts column.
    avatar: Option<FactAvatar>,
    leading: Option<Leading>,
    leading_column: Option<f64>,
    nested: bool,
    /// How many rows over it the row stands, indenting its content one
    /// disclosure gutter each ([`ListRowFace::depth`]).
    depth: u32,
    /// Every badge of the row's list, measured into `leading_column` at
    /// layout.
    column_badges: Option<std::rc::Rc<[String]>>,
    pub(crate) shared_leading: Option<f64>,
    /// Set by the list: the room every row of it keeps after its trailing
    /// control.
    pub(crate) shared_after_trailing: Option<f64>,
    leading_tooltip: Option<String>,
    tooltip: Option<String>,
    /// What a reader hears of the row beyond its name and value; it is not
    /// shown to the pointer.
    description: Option<String>,
    status: Option<(StatusKind, String)>,
    /// An unseen change of what the row shows ([`Self::change`]).
    change: Option<Changed>,
    /// It stands for a removed item ([`Self::struck`]).
    struck: bool,
    /// Its title reads in the weaker ink ([`Self::quiet`]).
    quiet: bool,
    /// Its title reads in the quietest ink ([`Self::faint`]).
    faint: bool,
    disclosure: bool,
    checked: Option<bool>,
    emphasized: bool,
    selected: bool,
    join: RowJoin,
    separator: bool,
    next_selected: bool,
    actions: Vec<OwnedAction>,
    hovered_action: Option<usize>,
    armed_action: Option<usize>,
    /// The list this row stands in has something painted over it: its tooltip stays away.
    tooltips_suppressed: bool,
    /// A press on one of the row's own controls (a hover action, the
    /// leading checkbox); a press on the row itself is the activation's.
    pressed_part: crate::pointer_replay::HeldPress<RowPart>,
    /// The commands the row's menu offers with a declared shortcut
    /// (`key: "Ctrl+Enter"`), and the keys each answers: pressed where the
    /// row has the keyboard, they run it as choosing it in the menu does.
    shortcuts: Vec<(crate::platform::Shortcut, std::rc::Rc<dyn Fn()>)>,
    swipe: Option<OwnedSwipe>,
    swipe_origin: Option<(u64, f64)>,
    swipe_offset: f64,
    trailing: Option<Trailing>,
    activation: Activation,
    /// The leading checkbox's own state, published as the row's view action
    /// `check` ([`ListRow::checkbox_view`]) and answered by the row.
    check_verb: Option<super::control_verb::ControlVerb<bool>>,
    /// The value a described row's leading checkbox is bound to, followed so
    /// a check made elsewhere shows in the row.
    check_binding: Option<crate::widget::binding::BoxedBinding<bool>>,
    /// The action a described row's `check` runs when it is an action the
    /// app publishes: a press on the box is the action's own route, which an
    /// agent's invocation of it lands on too.
    check_action: Option<crate::widget::ActionEmitter<()>>,
    environment: crate::interaction::InteractionEnvironment,
    metrics: Option<RowMetrics>,
    theme: Option<Theme>,
    rect: Rect,
    dirty: bool,
    /// The pace the row's own motion plays at (its mark, its coming and going in a list).
    feel: crate::motion::Feel,
    /// How far the leading checkbox's fill and mark have come: a row's checkbox is painted in
    /// the row, so the row keeps the motion ([`crate::widget::checkbox::CheckMotion`]).
    check: crate::widget::checkbox::CheckMotion,
    /// Where that motion is this frame, for the face it draws.
    check_pose: Option<crate::widget::checkbox::CheckPose>,
    /// A host that builds the row anew for what it shows now: the one built
    /// in its place goes on from its mark (`motion::keep`).
    hand_on: crate::motion::HandOn<KeptRow>,
    /// The row is a card ([`ListRowFace::card`]): its own surface, its facts on a line under
    /// its text.
    card: bool,
    /// How many lines the title wraps to in a card at the width it was last measured in: a
    /// card is as tall as its title needs, so its height is measured, not fixed.
    card_lines: u8,
    /// The row is a dense row ([`ListRowFace::dense`]): one quiet line at the caption's size.
    dense: bool,
    /// The row's own height ([`ListRowFace::row_height`]), where it is not the list's row
    /// height.
    row_height: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusKind {
    Active,
    Warning,
    Error,
    Idle,
    Presence,
}

impl ListRow {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self::new_owned(id.into(), title.into())
    }

    /// [`Self::new`] once its text is owned: one copy in the toolkit
    /// rather than one in every app that calls the generic constructor.
    fn new_owned(id: String, title: String) -> Self {
        Self {
            title,
            item: None,
            prefix: None,
            laid_out: false,
            title_elided: false,
            two_line_title: false,
            title_family: None,
            subtitle: None,
            secondary: None,
            note: None,
            secondary_line_count: 1,
            meta: None,
            meta_spans: Vec::new(),
            value: None,
            mark: None,
            mark_tone: Tone::Neutral,
            tags: Vec::new(),
            count: None,
            avatar: None,
            leading: None,
            leading_column: None,
            nested: false,
            depth: 0,
            column_badges: None,
            shared_leading: None,
            shared_after_trailing: None,
            leading_tooltip: None,
            tooltip: None,
            description: None,
            status: None,
            change: None,
            struck: false,
            quiet: false,
            faint: false,
            disclosure: false,
            checked: None,
            emphasized: false,
            selected: false,
            join: RowJoin::NONE,
            separator: false,
            next_selected: false,
            actions: Vec::new(),
            hovered_action: None,
            armed_action: None,
            tooltips_suppressed: false,
            pressed_part: crate::pointer_replay::HeldPress::default(),
            shortcuts: Vec::new(),
            swipe: None,
            swipe_origin: None,
            swipe_offset: 0.0,
            trailing: None,
            activation: Activation::new(id),
            check_verb: None,
            check_binding: None,
            check_action: None,
            environment: crate::interaction::InteractionEnvironment::default(),
            metrics: None,
            theme: None,
            rect: Rect::ZERO,
            dirty: true,
            feel: crate::motion::Feel::Standard,
            check: crate::widget::checkbox::CheckMotion::default(),
            check_pose: None,
            hand_on: crate::motion::HandOn::default(),
            card: false,
            card_lines: 1,
            dense: false,
            row_height: None,
        }
    }

    /// The metrics the row lays out with: a card's, or a dense row's, where it is one.
    fn metrics(&self, theme: &Theme) -> RowMetrics {
        if self.card {
            RowMetrics::card(theme)
        } else if self.dense {
            RowMetrics::dense(theme)
        } else {
            RowMetrics::new(theme)
        }
    }

    /// Show the row as a dense row: one quiet line at the caption's size, as tall as that line
    /// and its inset (a reference under content).
    #[must_use]
    pub fn dense(mut self) -> Self {
        self.set_dense();
        self
    }

    /// Stand the row `px` tall, rather than the list's row height for what it holds
    /// ([`ListRowFace::row_height`]).
    #[must_use]
    pub fn row_height(mut self, px: f32) -> Self {
        self.row_height = Some(f64::from(px));
        self.dirty = true;
        self
    }

    /// [`Self::dense`], for a row a host has already built.
    pub fn set_dense(&mut self) {
        self.dense = true;
        self.dirty = true;
    }

    /// Whether the row stands dense ([`ListRowFace::dense`]).
    #[must_use]
    pub const fn is_dense(&self) -> bool {
        self.dense
    }

    /// Show the row as a card: a surface of its own, its title wrapped over up to
    /// [`CARD_TITLE_LINES`] lines, and its facts on a line under the text instead of beside it
    /// (a board's card).
    #[must_use]
    pub fn card(mut self) -> Self {
        self.set_card();
        self
    }

    /// [`Self::card`], for a row a host has already built.
    pub fn set_card(&mut self) {
        self.card = true;
        self.dirty = true;
    }

    /// Whether the row stands as a card: its own surface, as a card component showed it
    /// ([`CardBody::show_as_card`]).
    #[must_use]
    pub const fn is_card(&self) -> bool {
        self.card
    }

    /// Have a press on the row (or Enter or Space while it has the keyboard) report `event`:
    /// what a component's card does when it opens what it stands for.
    pub fn set_activate(&mut self, event: crate::component::Event) {
        let label = self.title.clone();
        self.activation.set_event(event, &label);
        self.dirty = true;
    }

    /// The commands the row's menu offers with the keys they declare: pressing one while the
    /// row has the keyboard runs it, as choosing it in the menu does.
    #[must_use]
    pub fn shortcuts(
        mut self,
        shortcuts: Vec<(crate::platform::Shortcut, std::rc::Rc<dyn Fn()>)>,
    ) -> Self {
        self.shortcuts = shortcuts;
        self
    }

    /// See [`ListRowFace::two_line_title`].
    #[must_use]
    pub fn two_line_title(mut self) -> Self {
        self.two_line_title = true;
        self
    }

    /// See [`ListRowFace::title_family`]: only for a list of fonts.
    #[must_use]
    pub fn title_family(mut self, family: FontChoice) -> Self {
        self.title_family = Some(family);
        self
    }

    /// See [`ListRowFace::subtitle`].
    #[must_use]
    pub fn subtitle(mut self, text: impl Into<String>) -> Self {
        self.subtitle = Some(text.into());
        self
    }

    /// See [`ListRowFace::secondary`].
    #[must_use]
    pub fn secondary(mut self, text: impl Into<String>) -> Self {
        self.secondary = Some(text.into());
        self
    }

    /// See [`ListRowFace::two_line_secondary`].
    #[must_use]
    pub fn two_line_secondary(mut self) -> Self {
        self.secondary_line_count = 2;
        self
    }

    /// See [`ListRowFace::secondary_lines`].
    #[must_use]
    pub fn secondary_lines(mut self, lines: u8) -> Self {
        self.secondary_line_count = lines.clamp(1, 4);
        self
    }

    /// See [`ListRowFace::note`].
    #[must_use]
    pub fn note(mut self, text: impl Into<String>) -> Self {
        self.note = Some(text.into());
        self
    }

    /// See [`ListRowFace::meta`].
    #[must_use]
    pub fn meta(mut self, text: impl Into<String>) -> Self {
        self.meta = Some(text.into());
        self
    }

    /// See [`ListRowFace::meta_spans`].
    #[must_use]
    pub fn meta_spans(mut self, spans: &[(&str, Tone)]) -> Self {
        self.meta_spans = spans
            .iter()
            .map(|(text, tone)| ((*text).to_owned(), *tone))
            .collect();
        self
    }

    /// Say which item of its collection the row shows
    /// ([`AccessNode::item`](crate::a11y::AccessNode::item)): a task's issue,
    /// a note's block. An action naming that item is drawn on this row.
    #[must_use]
    pub fn item(mut self, item: impl Into<String>) -> Self {
        self.item = Some(item.into()).filter(|item| !item.is_empty());
        self.dirty = true;
        self
    }

    /// See [`ListRowFace::value`].
    #[must_use]
    pub fn value(mut self, text: impl Into<String>) -> Self {
        self.value = Some((text.into(), RowValue::Body));
        self
    }

    /// See [`ListRowFace::title_value`].
    #[must_use]
    pub fn title_value(mut self, text: impl Into<String>) -> Self {
        self.value = Some((text.into(), RowValue::Title));
        self
    }

    /// Replace the trailing value (a ticking clock).
    pub fn set_value(&mut self, text: impl Into<String>) {
        let text = text.into();
        if let Some((value, _)) = &mut self.value {
            if *value != text {
                *value = text;
                self.dirty = true;
            }
        } else {
            self.value = Some((text, RowValue::Body));
            self.dirty = true;
        }
    }

    /// See [`ListRowFace::mark`].
    #[must_use]
    pub fn mark(mut self, icon: Icon) -> Self {
        self.mark = Some(icon);
        self
    }

    /// The tone the row's mark and its checkbox ring read in
    /// ([`ListRowFace::mark_tone`]).
    #[must_use]
    pub fn mark_tone(mut self, tone: Tone) -> Self {
        self.mark_tone = tone;
        self
    }

    /// See [`ListRowFace::tags`].
    #[must_use]
    pub fn tags(mut self, tags: &[(&str, ChipStyle)]) -> Self {
        self.tags = tags
            .iter()
            .map(|(label, style)| ((*label).to_owned(), *style))
            .collect();
        self
    }

    /// See [`ListRowFace::count`].
    #[must_use]
    pub fn count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }

    /// A person's avatar in the leading column: the row _is_ the person (a contact, an
    /// account). Something a person is on shows them as a fact instead
    /// ([`ListRowFace::avatar`]), on its facts column.
    #[must_use]
    pub fn avatar(mut self, name: impl Into<String>, identity: impl Into<String>) -> Self {
        self.leading = Some(Leading::Avatar {
            name: name.into(),
            identity: identity.into(),
            image: None,
            square: false,
            glyph: None,
            tint: None,
            agent: false,
        });
        self
    }

    /// The leading avatar is an agent's own face ([`AvatarFace::agent`]), in its tint.
    #[must_use]
    pub fn agent_face(mut self) -> Self {
        if let Some(Leading::Avatar { agent, square, .. }) = &mut self.leading {
            *agent = true;
            *square = true;
        }
        self
    }

    /// The room an avatar takes, left empty ([`RowLeading::AvatarSpace`]): for
    /// a row without one in a list whose rows show avatars.
    #[must_use]
    pub fn avatar_space(mut self) -> Self {
        self.leading = Some(Leading::AvatarSpace);
        self
    }

    /// An app without artwork of its own in the leading avatar: its glyph on
    /// the neutral square, the mark the shell shows for it
    /// ([`AvatarFace::glyph`]).
    #[must_use]
    pub fn avatar_glyph(mut self, icon: Icon) -> Self {
        if let Some(Leading::Avatar { glyph, .. }) = &mut self.leading {
            *glyph = Some(icon);
        }
        self
    }

    /// The person's picture for the leading avatar.
    #[must_use]
    pub fn avatar_image(mut self, image: ImageData) -> Self {
        if let Some(Leading::Avatar { image: slot, .. }) = &mut self.leading {
            *slot = Some(image);
        }
        self
    }

    /// Use canonical normalized public identity pixels; no files are opened.
    #[cfg(feature = "local-daemons")]
    #[must_use]
    pub fn with_profile_photo(self, photo: &runtime_identity::ProfilePhoto) -> Self {
        self.avatar_image(crate::widget::avatar::profile_image(photo))
    }

    /// Make the leading avatar square: an account or a site, not a person.
    #[must_use]
    pub fn square(mut self) -> Self {
        if let Some(Leading::Avatar { square, .. }) = &mut self.leading {
            *square = true;
        }
        self
    }

    /// Tint the leading avatar in `hue`, one of the presence palette's
    /// colours, instead of the hue its identity derives: what an agent's
    /// client looks like (warm for one, cool for another), in every theme.
    #[must_use]
    pub fn avatar_hue(mut self, hue: PresenceHue) -> Self {
        if let Some(Leading::Avatar { tint, .. }) = &mut self.leading {
            *tint = Some(AvatarTint::Hue(hue));
        }
        self
    }

    /// Play the row's own motion — its mark, its coming and going in a list — at `feel` rather
    /// than the theme's pace.
    #[must_use]
    pub fn with_feel(mut self, feel: crate::motion::Feel) -> Self {
        self.feel = feel;
        self
    }

    /// [`Self::with_feel`] on a row already built.
    pub fn set_feel(&mut self, feel: crate::motion::Feel) {
        if self.feel != feel {
            self.feel = feel;
            self.dirty = true;
        }
    }

    /// [`Self::checkbox`] published as the row's view action `check`: a
    /// press on the box, Space on the focused row, a screen reader's toggle,
    /// a script's and an agent's all reach `on_toggle` through the same
    /// action. The row's control is `id` ([`Self::new`]).
    #[must_use]
    pub fn checkbox_view(mut self, checked: bool, on_toggle: impl FnMut(bool) + 'static) -> Self {
        let control = self.activation.id().to_owned();
        self.check_verb = Some(super::control_verb::ControlVerb::new(
            &control,
            "Check",
            "check",
            CHECK_ROW,
            super::control_verb::VerbArgument::Flag,
        ));
        self.leading = Some(Leading::Checkbox {
            checked,
            on_toggle: Box::new(on_toggle),
            icon: None,
        });
        self
    }

    /// See [`ListRowFace::leading_column`]. A [`crate::widget::Section`]
    /// equalises the badge column of consecutive rows on its own.
    #[must_use]
    pub fn leading_column(mut self, width: f64) -> Self {
        self.leading_column = Some(width);
        self
    }

    /// See [`ListRowFace::nested`].
    #[must_use]
    pub fn nested(mut self) -> Self {
        self.nested = true;
        self
    }

    /// See [`ListRowFace::depth`].
    #[must_use]
    pub fn depth(mut self, depth: u32) -> Self {
        self.depth = depth;
        self
    }

    /// The leading column fits every badge in `badges`, measured when the
    /// row is laid out: the rows of a
    /// [`VirtualList`](crate::widget::virtual_list::VirtualList), which
    /// mounts only those on screen, keep one title edge with the rest.
    #[must_use]
    pub fn badge_column(mut self, badges: std::rc::Rc<[String]>) -> Self {
        self.column_badges = Some(badges);
        self
    }

    /// What the pointer resting on the row shows, where no part of it (the
    /// leading badge, an action, a title cut short) says more.
    #[must_use]
    pub fn tooltip(mut self, tooltip: impl Into<String>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    /// What a reader hears of the row beyond its name and value — a task's
    /// status beside its reference, say. It stays out of the pointer's way:
    /// the row explains itself to a reader, not on hover.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    #[must_use]
    pub fn active(mut self) -> Self {
        self.status = Some((StatusKind::Active, String::new()));
        self
    }

    #[must_use]
    pub fn warning(mut self) -> Self {
        self.status = Some((StatusKind::Warning, String::new()));
        self
    }

    #[must_use]
    pub fn error(mut self) -> Self {
        self.status = Some((StatusKind::Error, String::new()));
        self
    }

    #[must_use]
    pub fn idle(mut self) -> Self {
        self.status = Some((StatusKind::Idle, String::new()));
        self
    }

    /// The row stands for a removed item: its title struck through ([`ListRowFace::struck`]).
    #[must_use]
    pub fn struck(mut self) -> Self {
        self.struck = true;
        self
    }

    /// The row reads as subordinate to the rows around it: its title in the weaker ink
    /// ([`ListRowFace::quiet`]).
    #[must_use]
    pub fn quiet(mut self) -> Self {
        self.quiet = true;
        self
    }

    /// The row reads as one a window shows for where another stands ([`ListRowFace::faint`]).
    #[must_use]
    pub fn faint(mut self) -> Self {
        self.faint = true;
        self
    }

    /// A change `identity` (called `name`) made to what the row shows and the person has not
    /// seen: their face on the trailing edge and their colour faintly under the row, at its own
    /// height, and `said` as a line under the row's title; `spoken` (who, in which run) on hover
    /// and to assistive technology; pressing the face runs `show`.
    #[must_use]
    pub fn change(
        mut self,
        identity: impl Into<String>,
        name: impl Into<String>,
        spoken: impl Into<String>,
        said: impl Into<String>,
        show: Option<Box<dyn FnMut()>>,
    ) -> Self {
        let spoken = spoken.into();
        if self.tooltip.is_none() {
            self.tooltip = Some(spoken.clone());
        }
        self.change = Some(Changed {
            identity: identity.into(),
            name: name.into(),
            spoken,
            said: said.into(),
            show,
        });
        self
    }

    /// The change the row shows, while the person has not seen it: who made it, what they are
    /// called and what they did, as the row draws them.
    #[must_use]
    pub fn shown_change(&self) -> Option<ShownChange<'_>> {
        let change = self.change.as_ref()?;
        Some(ShownChange {
            identity: &change.identity,
            name: &change.name,
            said: &change.said,
            spoken: &change.spoken,
        })
    }

    /// Hide this row's tooltip while something the host paints covers the list it stands in:
    /// the list keeps the flag over every row it mounts ([`crate::widget::VirtualList`]).
    pub(crate) fn suppress_tooltips(&mut self, suppressed: bool) {
        self.tooltips_suppressed = suppressed;
    }

    /// The presence dot of the person `identity`.
    #[must_use]
    pub fn presence(mut self, identity: impl Into<String>) -> Self {
        self.status = Some((StatusKind::Presence, identity.into()));
        self
    }

    #[must_use]
    pub fn disclosure(mut self) -> Self {
        self.disclosure = true;
        self
    }

    /// See [`ListRowFace::checked`].
    #[must_use]
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    /// Move the check of a single-choice list.
    pub fn set_checked(&mut self, checked: bool) {
        if self.checked != Some(checked) {
            self.checked = Some(checked);
            self.dirty = true;
        }
    }

    #[must_use]
    pub fn emphasized(mut self, emphasized: bool) -> Self {
        self.emphasized = emphasized;
        self
    }

    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// A disabled row is dimmed, skipped by focus and inert.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.activation.state.disabled = !enabled;
        self
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.activation.state.disabled == enabled {
            self.activation.state.disabled = !enabled;
            self.dirty = true;
        }
    }

    /// One persistent control at the trailing edge: a [`Toggle`], a
    /// [`Button`], a [`ToggleGroup`], a small [`ProgressRing`] or a
    /// [`Badge`]. The row reserves its width, keeps it reachable by keyboard
    /// and assistive technology, and moves it under the text when the row is
    /// too narrow for both.
    ///
    /// [`Toggle`]: crate::widget::Toggle
    /// [`Button`]: crate::widget::Button
    /// [`ToggleGroup`]: crate::widget::ToggleGroup
    /// [`ProgressRing`]: crate::widget::ProgressRing
    /// [`Badge`]: crate::widget::Badge
    #[must_use]
    pub fn trailing(mut self, control: Element) -> Self {
        self.trailing = Some(Trailing {
            band_slot: crate::widget::Absent::new(),
            element: control,
            band: None,
            node: None,
            band_height: 0.0,
            wrapped: false,
            natural: 0.0,
            margins: 0.0,
            fill: None,
            own_min: taffy::Dimension::auto(),
            on_disclosure_column: false,
        });
        self
    }

    /// [`Self::trailing`] for an icon button that opens what the row is
    /// about while the row itself does something else (a network row that
    /// connects, its chevron opening the network's page). The button is
    /// centred on the column a [`Self::disclosure`] chevron stands in, so it
    /// lines up with the chevrons of the rows around it; its target keeps
    /// its own size, reaching into the row's bleed.
    #[must_use]
    pub fn trailing_disclosure(self, button: Element) -> Self {
        let mut row = self.trailing(button);
        if let Some(trailing) = &mut row.trailing {
            trailing.on_disclosure_column = true;
        }
        row
    }

    /// The row's marks after its title (a status, a role), in order.
    #[must_use]
    pub fn tag_labels(&self) -> Vec<&str> {
        self.tags.iter().map(|(label, _)| label.as_str()).collect()
    }

    /// Whether the trailing control sits under the text (the row is narrow).
    pub fn trailing_wrapped(&self) -> bool {
        self.trailing.as_ref().is_some_and(|t| t.wrapped)
    }

    /// Set by the list the row sits in ([`crate::widget::Section`]): whether
    /// a separator runs under the row and whether the row below is
    /// selected, which hides it.
    #[cfg(test)]
    pub(crate) fn separator_state(&self) -> (bool, bool) {
        (self.separator, self.next_selected)
    }

    /// Set by the list: which selected neighbours this row's lift joins.
    pub(crate) fn set_join(&mut self, join: RowJoin) {
        if self.join != join {
            self.join = join;
            self.dirty = true;
        }
    }

    #[cfg(test)]
    pub(crate) fn join(&self) -> RowJoin {
        self.join
    }

    /// Set by the list: the badge column its consecutive rows share.
    pub(crate) fn set_shared_leading(&mut self, width: Option<f64>) {
        if self.shared_leading != width {
            self.shared_leading = width;
            self.dirty = true;
        }
    }

    /// Set by the list: the room its consecutive rows keep after their
    /// trailing controls, so the controls stand in one column.
    pub(crate) fn set_shared_after_trailing(&mut self, width: Option<f64>) {
        if self.shared_after_trailing != width {
            self.shared_after_trailing = width;
            self.dirty = true;
        }
    }

    /// The room this row's own marks take after its trailing control, for
    /// the list to share across its rows; none without a control.
    pub(crate) fn marks_after_trailing(&self, theme: &Theme) -> f64 {
        if self.trailing.is_none() {
            return 0.0;
        }
        let m = self.metrics(theme);
        self.with_face(|face| face.marks_after_trailing(&m))
    }

    pub(crate) fn badge_label(&self) -> Option<&str> {
        match &self.leading {
            Some(Leading::Badge(badge)) => Some(badge),
            _ => None,
        }
    }

    /// Add an action revealed while the row is hovered or focused.
    #[must_use]
    pub fn action(
        mut self,
        icon: Icon,
        label: impl Into<String>,
        on_activate: impl FnMut() + 'static,
    ) -> Self {
        self.actions.push(OwnedAction {
            icon,
            label: label.into(),
            handler: ActionHandler::Callback(Box::new(on_activate)),
        });
        self
    }

    /// [`Self::action`] through an exported resource action: the host
    /// resolves its policy (and asks for confirmation where it must) before
    /// it runs, and the action shows only while it is available.
    #[must_use]
    pub fn action_binding(
        mut self,
        icon: Icon,
        label: impl Into<String>,
        binding: tilo_actions::ActionBinding<()>,
    ) -> Self {
        self.actions.push(OwnedAction {
            icon,
            label: label.into(),
            handler: ActionHandler::Bound(ActionEmitter::new(binding)),
        });
        self
    }

    /// Swiping the row toward its leading edge reveals `icon`; releasing
    /// past the threshold runs `on_activate`.
    #[must_use]
    pub fn swipe_action(
        mut self,
        icon: Icon,
        label: impl Into<String>,
        destructive: bool,
        on_activate: impl FnMut() + 'static,
    ) -> Self {
        self.swipe = Some(OwnedSwipe {
            icon,
            label: label.into(),
            destructive,
            on_activate: Some(Box::new(on_activate)),
            press: crate::widget::Press::default(),
        });
        self
    }

    /// Swiping the row toward its leading edge reveals `icon`; releasing past the threshold runs
    /// `binding` as its action, as [`Self::action_binding`] runs a trailing action.
    #[must_use]
    pub fn swipe_binding(
        mut self,
        icon: Icon,
        label: impl Into<String>,
        destructive: bool,
        binding: tilo_actions::ActionBinding<()>,
    ) -> Self {
        self.swipe = Some(OwnedSwipe {
            icon,
            label: label.into(),
            destructive,
            on_activate: None,
            press: crate::widget::Press::of(crate::component::Event::Action(binding)),
        });
        self
    }

    /// Run `f` when the row is activated.
    #[must_use]
    pub fn on_activate(mut self, f: impl FnMut() + 'static) -> Self {
        self.activation.set_callback(Box::new(f));
        self
    }

    /// Route activation through an exported resource action.
    #[must_use]
    pub fn on_action(mut self, binding: tilo_actions::ActionBinding<()>) -> Self {
        self.activation.set_action(binding);
        self
    }

    /// Publish activation as a zero-argument effect, as
    /// [`Button::on_effect`](crate::widget::Button::on_effect) does: the
    /// closure keeps working and the row is discoverable and agent-driveable.
    #[must_use]
    pub fn on_effect(mut self, f: impl FnMut() + 'static) -> Self {
        let label = self.title.clone();
        self.activation
            .set_native(tilo_actions::OperationKind::Effect, &label, Box::new(f));
        self
    }

    /// Publish activation as a transient view action (opening, selecting), as
    /// [`Button::on_view`](crate::widget::Button::on_view) does.
    #[must_use]
    pub fn on_view(mut self, f: impl FnMut() + 'static) -> Self {
        let label = self.title.clone();
        self.activation
            .set_native(tilo_actions::OperationKind::View, &label, Box::new(f));
        self
    }

    pub fn set_selected(&mut self, selected: bool) {
        if self.selected != selected {
            self.selected = selected;
            self.dirty = true;
        }
    }

    pub fn is_selected(&self) -> bool {
        self.selected
    }

    fn reveal_actions(&self) -> bool {
        (self.selected && !self.environment.supports_hover()) || self.armed_action.is_some()
    }

    /// The rect the row paints in: its layout box grown by the bleed.
    fn outer(&self) -> Rect {
        if self.card {
            // A card is its own surface: nothing of it paints past its box.
            return self.rect;
        }
        let bleed = self.metrics.as_ref().map_or(0.0, |m| m.bleed);
        self.rect.inflate(bleed, 0.0)
    }

    /// The band the text is laid out in, as an outer rect.
    fn text_band(&self) -> Rect {
        let outer = self.outer();
        match &self.trailing {
            Some(trailing) if trailing.wrapped => Rect::new(
                outer.x0,
                outer.y0,
                outer.x1,
                outer.y0 + f64::from(trailing.band_height),
            ),
            _ => outer,
        }
    }

    fn trailing_reserve(&self) -> Option<f64> {
        let trailing = self.trailing.as_ref()?;
        (!trailing.wrapped).then(|| trailing.element.get_rect().width())
    }

    /// The grid the row laid itself out on, for an audit comparing rows with each other. Read
    /// from the geometry the row paints from, so the two never disagree.
    fn grid(&self) -> Option<crate::widget::RowLayout> {
        let metrics = self.metrics.as_ref()?;
        let theme = self.theme.as_ref()?;
        let band = self.text_band();
        Some(crate::text_render::with_layout_renderer(
            theme.text_scale,
            |text| {
                self.with_face(|face| {
                    let geo = face.geometry(band, metrics, text, theme);
                    let content_right = band.x1 - metrics.bleed;
                    // What stands in the facts column, from the edge inwards: any of them, the
                    // leftmost is where the column's content begins.
                    let mut facts_left = content_right;
                    if let Some(right) = geo.marks_right {
                        facts_left = facts_left.min(right - face.marks_width(metrics));
                    }
                    if let Some(count_right) = geo.count_right {
                        facts_left = facts_left.min(count_right);
                    }
                    if let Some(avatar) = geo.avatar {
                        facts_left = facts_left.min(avatar.x0);
                    }
                    if self.value.is_some() {
                        facts_left = facts_left.min(geo.value_right);
                    }
                    if let Some(trailing) = geo.trailing {
                        facts_left = facts_left.min(trailing.x0);
                    }
                    facts_left = facts_left.min(geo.meta_left).max(geo.text_right);
                    crate::widget::RowLayout {
                        depth: self.depth,
                        content_left: crate::num::f32_of_f64(geo.leading.x0),
                        text_x: crate::num::f32_of_f64(geo.text_x),
                        text_right: crate::num::f32_of_f64(geo.text_right),
                        facts_left: crate::num::f32_of_f64(facts_left),
                        content_right: crate::num::f32_of_f64(content_right),
                        marks_left: geo
                            .marks_right
                            .map(|right| crate::num::f32_of_f64(right - face.marks_width(metrics))),
                    }
                })
            },
        ))
    }

    /// The row as the face it draws, read by the list that builds it and by its tests.
    pub(crate) fn with_face<R>(&self, f: impl FnOnce(&ListRowFace<'_>) -> R) -> R {
        let actions: Vec<RowAction<'_>> = self
            .actions
            .iter()
            .filter(|a| a.is_available())
            .map(|a| RowAction {
                icon: a.icon,
                label: &a.label,
            })
            .collect();
        let tags: Vec<(&str, ChipStyle)> = self
            .tags
            .iter()
            .map(|(label, style)| (label.as_str(), *style))
            .collect();
        let spans: Vec<(&str, Tone)> = self
            .meta_spans
            .iter()
            .map(|(text, tone)| (text.as_str(), *tone))
            .collect();
        let mut interaction = self.activation.state;
        // A row that does nothing when pressed (a setting's label beside its
        // switch, a reading) does not lift under the pointer: only what
        // acts answers hover.
        interaction.hovered &= self.environment.supports_hover() && self.answers_pointer();
        let mut face = ListRowFace::new(&self.title)
            .item(self.item.as_deref())
            .emphasized(self.emphasized)
            .selected(self.selected)
            .joined(self.join)
            .actions(&actions)
            .hovered_action(self.armed_action.or(self.hovered_action))
            .reveal_actions(self.reveal_actions())
            .tags(&tags)
            .meta_spans(&spans)
            .interaction(interaction);
        if self.answers_pointer() {
            face.min_target = f64::from(self.environment.minimum_collection_row());
        }
        // The box a row leads with is a control of its own: its press reaches the target the
        // person's pointer needs, as a button's does, while the row keeps its height and the
        // box its size.
        if matches!(self.leading, Some(Leading::Checkbox { .. })) {
            face.check_target = f64::from(self.environment.minimum_target());
        }
        if let Some(prefix) = &self.prefix {
            face = face.prefix(prefix);
        }
        if let Some(pose) = self.check_pose {
            face = face.check_pose(pose);
        }
        if self.card {
            face = face.card().title_lines(self.card_lines);
        } else if self.two_line_title {
            face = face.two_line_title();
        }
        if self.dense {
            face = face.dense();
        }
        if let Some(height) = self.row_height {
            face.row_height = Some(height);
        }
        if let Some(family) = &self.title_family {
            face = face.title_family(family);
        }
        if let Some(subtitle) = &self.subtitle {
            face = face.subtitle(subtitle);
        }
        if let Some(secondary) = &self.secondary {
            face = face
                .secondary(secondary)
                .secondary_lines(self.secondary_line_count);
        }
        if let Some(note) = &self.note {
            face = face.note(note);
        }
        if let Some(meta) = &self.meta {
            face = face.meta(meta);
        }
        if let Some((value, size)) = &self.value {
            face = match size {
                RowValue::Body => face.value(value),
                RowValue::Title => face.title_value(value),
            };
        }
        if let Some(mark) = self.mark {
            face = face.mark(mark).mark_tone(self.mark_tone);
        }
        if let Some(count) = self.count {
            face = face.count(count);
        }
        if let Some(checked) = self.checked {
            face = face.checked(checked);
        }
        if let Some(width) = self.shared_after_trailing {
            face = face.trailing_after(width);
        }
        if let Some(width) = self.leading_column.or(self.shared_leading) {
            face = face.leading_column(width);
        }
        face = face.nested(self.nested);
        face = face.depth(self.depth);
        if let Some(avatar) = &self.avatar {
            face = face.avatar(avatar.face());
        }
        if let Some(leading) = &self.leading {
            face = face.leading(match leading {
                Leading::Avatar {
                    name,
                    identity,
                    image,
                    square,
                    glyph,
                    tint,
                    agent,
                } => {
                    let avatar = AvatarFace::new(name, identity).image(image.as_ref());
                    let avatar = if *square { avatar.square() } else { avatar };
                    let avatar = match glyph {
                        Some(glyph) => avatar.glyph(*glyph),
                        None => avatar,
                    };
                    // Its own tint, after the glyph, which asks for the
                    // neutral square.
                    let avatar = match tint {
                        Some(AvatarTint::Colour(colour)) => avatar.presence(*colour),
                        Some(AvatarTint::Hue(hue)) => avatar.hue(*hue),
                        None => avatar,
                    };
                    RowLeading::Avatar(if *agent { avatar.agent() } else { avatar })
                }
                Leading::AvatarSpace => RowLeading::AvatarSpace,
                Leading::Icon(icon) => RowLeading::Icon(*icon),
                Leading::Badge(badge) => RowLeading::Badge(badge),
                Leading::Thumbnail(image) => RowLeading::Thumbnail(image),
                Leading::Media {
                    source,
                    aspect,
                    loaded,
                } => RowLeading::Media(
                    match (loaded, source) {
                        (Some(image), _) | (None, MediaSource::Image(image)) => {
                            MediaArt::Image(image)
                        }
                        (None, MediaSource::AppIcon(icon)) => MediaArt::AppIcon(icon),
                        (None, MediaSource::Kind(icon)) => MediaArt::Kind(*icon),
                        (None, MediaSource::Mosaic(images)) => MediaArt::Mosaic(images),
                        _ => MediaArt::Placeholder,
                    },
                    *aspect,
                ),
                Leading::Checkbox { checked, icon, .. } => RowLeading::Checkbox {
                    checked: *checked,
                    icon: *icon,
                },
                Leading::Swatch(colour) => RowLeading::Swatch(*colour),
                Leading::Signal(level) => RowLeading::Signal(*level),
            });
        }
        // A row checked off is done with: its title strikes through and mutes, as a removed
        // item's does. The box follows the value it is bound to, so a check shows in the row
        // itself, whoever made it and wherever its row stands.
        face = face.struck(
            self.struck || matches!(&self.leading, Some(Leading::Checkbox { checked: true, .. })),
        );
        face = face.quiet(self.quiet).faint(self.faint);
        if let Some(change) = &self.change {
            face = face.change(RowChange {
                identity: &change.identity,
                name: &change.name,
                spoken: &change.spoken,
                said: &change.said,
            });
        }
        if let Some((kind, id)) = &self.status {
            face = face.status(match kind {
                StatusKind::Active => RowStatus::Active,
                StatusKind::Warning => RowStatus::Warning,
                StatusKind::Error => RowStatus::Error,
                StatusKind::Idle => RowStatus::Idle,
                StatusKind::Presence => RowStatus::Presence(id),
            });
        }
        if self.disclosure {
            face = face.disclosure();
        }
        if self.separator {
            face = face.separator(self.next_selected);
        }
        if let Some(width) = self.trailing_reserve() {
            face = face.trailing_width(width);
        }
        if self.trailing_wrapped() {
            face = face.frame(self.outer());
        }
        if let Some(swipe) = &self.swipe
            && self.swipe_offset > 0.0
        {
            face = face.swiped(
                SwipeAction {
                    icon: swipe.icon,
                    label: &swipe.label,
                    destructive: swipe.destructive,
                },
                self.swipe_offset,
            );
        }
        f(&face)
    }

    /// Whether the row itself acts: it activates, offers hover actions or
    /// carries a selection checkbox.
    fn answers_pointer(&self) -> bool {
        self.activation.is_bound()
            || !self.actions.is_empty()
            || matches!(self.leading, Some(Leading::Checkbox { .. }))
    }

    /// The indices into `self.actions` of the actions shown, in order.
    fn shown_actions(&self) -> Vec<usize> {
        self.actions
            .iter()
            .enumerate()
            .filter(|(_, a)| a.is_available())
            .map(|(i, _)| i)
            .collect()
    }

    /// Where the row's hover actions sit, in the order it shows them: what a host presses to
    /// reach them from a script or a test.
    pub(crate) fn action_rects(&self) -> Vec<Rect> {
        let (Some(metrics), Some(theme)) = (&self.metrics, &self.theme) else {
            return Vec::new();
        };
        let band = self.text_band();
        crate::text_render::with_layout_renderer(theme.text_scale, |text| {
            self.with_face(|face| face.action_rects_with(band, metrics, text, theme))
        })
    }

    /// Where the row's actions stand, in the order it holds them, whether or not it shows them:
    /// assistive technology reaches one without hovering the row.
    fn action_slots(&self) -> Vec<Rect> {
        let (Some(metrics), Some(theme)) = (&self.metrics, &self.theme) else {
            return Vec::new();
        };
        let band = self.text_band();
        crate::text_render::with_layout_renderer(theme.text_scale, |text| {
            self.with_face(|face| face.action_slots_with(band, metrics, text, theme))
        })
    }

    /// Whether a press at `point` is one of the row's own controls': a hover action, its check,
    /// a trailing control or a change. Those keep the press; on any other part of it the list
    /// takes the press, to drag the row and, let go without a drag, to press it.
    pub(crate) fn claims_pointer(&self, point: Point) -> bool {
        if self.activation.state.disabled {
            return false;
        }
        self.action_rects().iter().any(|rect| rect.contains(point))
            || matches!(self.leading, Some(Leading::Checkbox { .. }))
                && self.leading_hit().is_some_and(|rect| rect.contains(point))
            || self
                .trailing
                .as_ref()
                .is_some_and(|trailing| trailing.element.get_rect().contains(point))
            || self.change_hit().is_some_and(|rect| rect.contains(point))
    }

    /// Where the face of an unseen change sits, when the row shows one that does something.
    fn change_hit(&self) -> Option<Rect> {
        self.change.as_ref()?.show.as_ref()?;
        let theme = self.theme.as_ref()?;
        let band = self.text_band();
        crate::text_render::with_layout_renderer(theme.text_scale, |text| {
            self.with_face(|face| face.change_rect(band, text, theme))
        })
    }

    fn leading_hit(&self) -> Option<Rect> {
        let theme = self.theme.as_ref()?;
        if !matches!(
            self.leading,
            Some(
                Leading::Checkbox { .. }
                    | Leading::Badge(_)
                    | Leading::Icon(_)
                    | Leading::Signal(_)
            )
        ) {
            return None;
        }
        let band = self.text_band();
        Some(crate::text_render::with_layout_renderer(
            theme.text_scale,
            |text| self.with_face(|face| face.leading_rect(band, text, theme)),
        ))
    }

    fn toggle_checkbox(&mut self) -> bool {
        if let Some(Leading::Checkbox {
            checked, on_toggle, ..
        }) = &mut self.leading
        {
            *checked = !*checked;
            let now = *checked;
            self.dirty = true;
            if let Some(verb) = &mut self.check_verb {
                verb.emit(now);
                return true;
            }
            if let Some(action) = &mut self.check_action {
                if let Err(error) = action.emit(()) {
                    tracing::warn!(%error, "a row's check refused the action's arguments");
                }
                return true;
            }
            on_toggle(now);
            return true;
        }
        false
    }

    fn run_action(&mut self, shown_index: usize) -> bool {
        let shown = self.shown_actions();
        let Some(&index) = shown.get(shown_index) else {
            return false;
        };
        let control = self.activation.id().to_owned();
        if let Some(action) = self.actions.get_mut(index) {
            action.run(&control);
            return true;
        }
        false
    }

    fn handle_swipe(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        use winit::event::TouchPhase;
        let WindowEvent::Touch(touch) = event else {
            return;
        };
        let x = touch.location.x / ctx.scale_factor;
        let y = touch.location.y / ctx.scale_factor;
        match touch.phase {
            TouchPhase::Started if self.outer().contains(Point::new(x, y)) => {
                self.swipe_origin = Some((touch.id, x));
            }
            TouchPhase::Moved => {
                if let Some((id, start)) = self.swipe_origin
                    && id == touch.id
                {
                    let offset = (start - x).clamp(0.0, self.outer().width());
                    if (offset - self.swipe_offset).abs() > f64::EPSILON {
                        self.swipe_offset = offset;
                        self.dirty = true;
                        *ctx.handled = true;
                    }
                }
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                if let Some((id, _)) = self.swipe_origin
                    && id == touch.id
                {
                    let committed = touch.phase == TouchPhase::Ended
                        && self.swipe_offset >= self.outer().width() * SWIPE_COMMIT;
                    if committed && let Some(swipe) = &mut self.swipe {
                        swipe.run();
                    }
                    if self.swipe_offset > 0.0 {
                        *ctx.handled = true;
                    }
                    self.swipe_origin = None;
                    self.swipe_offset = 0.0;
                    self.dirty = true;
                }
            }
            TouchPhase::Started => {}
        }
    }
}

impl crate::widget::ReservedShape for ListRow {
    fn reserved_shape(&self) -> crate::widget::SkeletonShape {
        crate::widget::SkeletonShape::Row(self.shape())
    }
}

impl crate::widget::RowSelectionSink for ListRow {
    fn take_selection(&mut self, selection: crate::widget::RowSelection) {
        self.set_selected(selection.selected);
        self.set_join(selection.join());
    }
}

impl crate::widget::RowActionRects for ListRow {
    fn row_action_rects(&self) -> Vec<Rect> {
        ListRow::action_rects(self)
    }
}

impl CardBody for ListRow {
    fn show_as_card(&mut self, selected: bool, activate: crate::component::Event) {
        self.set_card();
        self.set_selected(selected);
        self.set_activate(activate);
    }
}

impl Widget for ListRow {
    crate::provides!(
        dyn crate::widget::CardBody,
        dyn crate::widget::controls::ActionControls,
        dyn crate::resources::PublishesResources,
        dyn crate::widget::ResourceOperations,
        dyn crate::widget::ReservedShape,
        dyn crate::widget::RowActionRects,
        dyn crate::widget::RowSelectionSink
    );

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        if self.environment != environment {
            self.environment = environment;
            self.dirty = true;
        }
        if let Some(trailing) = &mut self.trailing {
            trailing.element.set_interaction_environment(environment);
        }
    }

    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        if let Some(binding) = &mut self.check_binding
            && let crate::widget::binding::Pulled::Value(checked) = binding.pull(state, path)
            && let Some(Leading::Checkbox { checked: shown, .. }) = &mut self.leading
            && *shown != checked
        {
            *shown = checked;
            self.dirty = true;
        }
        if let Some(Leading::Media {
            source: MediaSource::Url { store, url },
            loaded,
            ..
        }) = &mut self.leading
            && loaded.is_none()
            && let Some(image) = store.get(url)
        {
            *loaded = Some(image);
            self.dirty = true;
        }
        if let Some(trailing) = &mut self.trailing {
            trailing.element.sync(state, path);
        }
    }

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        self.trailing
            .as_mut()
            .is_some_and(|trailing| trailing.element.update(state, path))
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.laid_out = true;
        if let Some((kept, _)) = self.hand_on.build(self.identity().as_deref()) {
            self.take_up(kept);
        }
        let metrics = self.metrics(theme);
        let column_gap = metrics.column_gap;
        let pad_y = metrics.pad_y;
        let min_text = f64::from(MIN_TEXT_LINES) * f64::from(metrics.title.line_height_px());
        // The control stands in the slot the face reserves for it, inside
        // the columns that follow it, not over them.
        let after_trailing = self.with_face(|face| face.after_trailing(&metrics));
        self.metrics = Some(metrics);
        // Geometry outside a draw (hit-testing actions, measuring) reads the
        // theme the row was last laid out in.
        self.theme = Some(theme.clone());
        if let Some(badges) = &self.column_badges {
            let badges: Vec<&str> = badges.iter().map(String::as_str).collect();
            let width = crate::text_render::with_layout_renderer(theme.text_scale, |text| {
                badge_column_width(text, theme, &badges)
            });
            if self.leading_column != Some(width) {
                self.leading_column = Some(width);
                self.dirty = true;
            }
        }
        let row_style = Style {
            size: taffy::Size {
                width: percent(1.0_f32),
                height: taffy::Dimension::auto(),
            },
            min_size: taffy::Size {
                width: length(0.0_f32),
                height: taffy::Dimension::auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        let Some(trailing) = &mut self.trailing else {
            let node = taffy.leaf_node(row_style);
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            return node;
        };
        if trailing.band_height <= 0.0 {
            trailing.band_height = crate::num::f32_of_f64(
                f64::from(theme.text(TextRole::Body).line_height_px()) + 2.0 * pad_y,
            )
            .max(theme.tokens.metrics.min_pointer_target + theme.tokens.spacing.s2);
        }
        let band = taffy.leaf_node(Style {
            flex_grow: 1.0,
            flex_shrink: 1.0,
            flex_basis: length(0.0_f32),
            size: taffy::Size {
                width: auto(),
                height: length(trailing.band_height),
            },
            min_size: taffy::Size {
                width: length(crate::num::f32_of_f64(min_text)),
                height: auto(),
            },
            ..Default::default()
        });
        let child = trailing.element.build_layout(taffy, theme, node_map);
        if let Ok(style) = taffy.style(child) {
            let mut style = style.clone();
            // A row of controls (a slider and its mute button) is as wide as
            // what it holds, not the whole row: it trails the text.
            if style.size.width == percent(1.0_f32) {
                style.size.width = auto();
            }
            // Never wider than the row: under the text it takes the row's
            // width at most, and a row of controls wraps inside it.
            style.max_size.width = percent(1.0_f32);
            style.flex_shrink = 0.0;
            style.align_self = Some(taffy::AlignSelf::Center);
            let (before, after) = (
                crate::num::f32_of_f64(column_gap),
                crate::num::f32_of_f64(after_trailing),
            );
            style.margin.left = length(before);
            style.margin.right = length(after);
            style.margin.top = length(crate::num::f32_of_f64(pad_y));
            style.margin.bottom = length(crate::num::f32_of_f64(pad_y));
            trailing.own_min = style.min_size.width;
            trailing.margins = before + after;
            trailing.fill = None;
            let _ = taffy.set_style(child, style);
        }
        trailing.band = Some(band);
        trailing.node = Some(child);
        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Row,
            flex_wrap: taffy::FlexWrap::Wrap,
            align_items: Some(taffy::AlignItems::Center),
            ..row_style
        };
        taffy.widget_node(style, &[band, child], node_map, self)
    }

    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        if self.trailing.is_none() {
            return false;
        }
        let filled = self.fill_under_text(taffy, node, text);
        let Some(theme) = &self.theme else {
            return filled;
        };
        let height = self.with_face(|face| face.height(text, theme));
        let Some(trailing) = &mut self.trailing else {
            return filled;
        };
        let height = crate::num::f32_of_f64(height);
        if (height - trailing.band_height).abs() < 0.5 {
            return filled;
        }
        trailing.band_height = height;
        let Some(band) = trailing.band else {
            return filled;
        };
        let Ok(style) = taffy.style(band) else {
            return filled;
        };
        let mut style = style.clone();
        style.size.height = length(height);
        taffy.set_style(band, style).is_ok() || filled
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let Some(theme) = self.theme.clone() else {
            return taffy::Size::ZERO;
        };
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w,
            _ => 0.0,
        });
        // A card's title wraps at the width it is offered, so its height follows the lines it
        // takes here rather than a count fixed when the row was built.
        if self.card {
            let lines = self
                .with_face(|face| face.card_title_lines(f64::from(width), text_renderer, &theme));
            self.card_lines = lines;
        }
        let height = self.with_face(|face| face.height(text_renderer, &theme));
        taffy::Size {
            width,
            height: crate::num::f32_of_f64(height),
        }
    }

    fn layout_info(&self) -> Option<crate::widget::LayoutInfo> {
        Some(crate::widget::LayoutInfo::Row(self.grid()?))
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = crate::widget::layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.dirty = true;
        }
        self.rect = rect;
        if let Some((kept, _)) = self.hand_on.place(rect) {
            self.take_up(kept);
        }
        let Some(trailing) = &mut self.trailing else {
            return;
        };
        let (Some(band), Some(child)) = (trailing.band, trailing.node) else {
            return;
        };
        let band_y = taffy.layout(band).map_or(0.0, |l| l.location.y);
        let child_layout = taffy.layout(child).copied().unwrap_or_default();
        let wrapped = child_layout.location.y >= band_y + trailing.band_height - 0.5;
        if wrapped != trailing.wrapped {
            trailing.wrapped = wrapped;
            self.dirty = true;
        }
        let mut child_origin = rect.origin();
        if wrapped {
            // Under the text, on the text's edge rather than the row's.
            let theme = self.theme.clone();
            // The line's gap before a control beside the text does not
            // apply under it: the control starts on the text's edge and
            // keeps inside the row.
            let shift = theme.map_or(0.0, |theme| {
                let band_rect = self.text_band();
                let m = self.metrics(&theme);
                crate::text_render::with_layout_renderer(theme.text_scale, |text| {
                    self.with_face(|face| face.text_x(band_rect, &m, text, &theme))
                }) - self.rect.x0
                    - f64::from(child_layout.location.x)
            });
            let room = rect.width()
                - f64::from(child_layout.location.x)
                - f64::from(child_layout.size.width);
            // Taffy rounds every layout box onto the display's device pixels; a control placed
            // here by hand takes the same grid, or its edge lands between two device pixels (a
            // text band's fractional inset at a large text scale put a button's left edge at
            // 39.596 at 1.5x).
            let scale = f64::from(crate::widget::device_scale());
            child_origin.x = ((child_origin.x + shift.min(room)) * scale).round() / scale;
        } else if trailing.on_disclosure_column
            && let Some(metrics) = &self.metrics
        {
            // The chevron's column ends on the layout box's edge; the
            // button's centre moves onto it, never past the bleed.
            let width = f64::from(child_layout.size.width);
            child_origin.x += ((width - metrics.icon) / 2.0).clamp(0.0, metrics.bleed);
        }
        if let Some(trailing) = &mut self.trailing {
            trailing.element.apply_layout(taffy, child, child_origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if presence.should_cull(self.outer()) {
            return;
        }
        let band = self.text_band();
        let checked = match &self.leading {
            Some(Leading::Checkbox { checked, .. }) => Some(*checked),
            _ => None,
        };
        if let Some(checked) = checked {
            let now = crate::motion::frame_now();
            self.check.follow_input(
                checked,
                crate::widget::checkbox::CheckGlyph::Check,
                self.feel.over(theme.motion()),
                now,
            );
            self.check_pose = Some(self.check.pose(now));
        }
        self.title_elided = self.with_face(|face| {
            face.paint(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                band,
            );
            face.title_elided(band, text_renderer, theme)
        });
        if let Some(trailing) = &mut self.trailing {
            trailing
                .element
                .draw_child(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let Some(trailing) = &mut self.trailing
            && let Some(node) = trailing.node
        {
            trailing
                .element
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if *ctx.handled {
                self.dirty = true;
                return;
            }
        }
        if self.swipe.is_some() {
            self.handle_swipe(event, ctx);
        }
        if self.handle_key(event, ctx) {
            return;
        }
        let mut exclude = self.action_rects();
        let leading = self.leading_hit();
        if matches!(self.leading, Some(Leading::Checkbox { .. }))
            && let Some(r) = leading
        {
            exclude.push(r);
        }
        if let Some(trailing) = &self.trailing {
            exclude.push(trailing.element.get_rect());
        }
        let change = self.change_hit();
        if let Some(r) = change {
            exclude.push(r);
        }
        let outer = self.outer();
        if let WindowEvent::CursorMoved { .. } = event {
            let changed = self.activation.handle_pointer(event, ctx, outer, &exclude);
            let cursor = self.activation.cursor();
            let hovered = self.action_rects().iter().position(|r| r.contains(cursor));
            if changed || hovered != self.hovered_action {
                self.hovered_action = hovered;
                self.dirty = true;
            }
            if !self.activation.state.hovered && !self.activation.state.focused {
                self.armed_action = None;
            }
            return;
        }
        if let WindowEvent::MouseInput {
            state,
            button: MouseButton::Left,
            ..
        } = event
        {
            use crate::pointer_replay::{PressAt, Release};
            let cursor = self.activation.cursor();
            let rects = self.action_rects();
            // The row's own control under the pointer, named across a
            // rebuild by the row's id.
            let over = rects
                .iter()
                .position(|r| r.contains(cursor))
                .map(|index| (RowPart::Action(index), rects[index]))
                .or_else(|| {
                    (matches!(self.leading, Some(Leading::Checkbox { .. }))
                        && !self.activation.state.disabled)
                        .then_some(leading)
                        .flatten()
                        .filter(|r| r.contains(cursor))
                        .map(|r| (RowPart::Check, r))
                })
                .or_else(|| {
                    change
                        .filter(|r| r.contains(cursor))
                        .map(|r| (RowPart::Change, r))
                });
            let key = over.map(|(part, _)| part.key(self.activation.id()));
            let at = over
                .zip(key.as_deref())
                .map(|((target, rect), key)| PressAt { target, key, rect });
            match state {
                ElementState::Pressed => {
                    if let Some(at) = at {
                        self.pressed_part.press(at);
                        *ctx.handled = true;
                        return;
                    }
                }
                ElementState::Released => match self.pressed_part.release(at) {
                    Release::NotHeld => {}
                    Release::Cancelled => {
                        *ctx.handled = true;
                        return;
                    }
                    Release::Click(part) => {
                        *ctx.handled = true;
                        match part {
                            RowPart::Action(index) => {
                                self.run_action(index);
                            }
                            RowPart::Check => {
                                self.toggle_checkbox();
                            }
                            RowPart::Change => {
                                if let Some(show) =
                                    self.change.as_mut().and_then(|change| change.show.as_mut())
                                {
                                    show();
                                }
                            }
                        }
                        return;
                    }
                },
            }
        }
        if self.activation.handle_pointer(event, ctx, outer, &exclude) {
            self.dirty = true;
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        let pressable = self.activation.is_bound()
            || matches!(self.leading, Some(Leading::Checkbox { .. }))
                && self.leading_hit().is_some_and(|r| r.contains(point))
            || self.action_rects().iter().any(|r| r.contains(point));
        (pressable && self.outer().contains(point)).then_some(winit::window::CursorIcon::Pointer)
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        if self.tooltips_suppressed {
            return None;
        }
        if let Some(tooltip) = &self.leading_tooltip
            && let Some(r) = self.leading_hit()
            && r.contains(point)
        {
            return Some((r, tooltip.clone()));
        }
        let shown = self.shown_actions();
        let action = self
            .action_rects()
            .into_iter()
            .zip(shown)
            .find(|(r, _)| r.contains(point))
            .and_then(|(r, index)| self.actions.get(index).map(|a| (r, a.label.clone())));
        // A title cut short reads in full where it is pointed at.
        action
            .or_else(|| {
                let band = self.text_band();
                (self.title_elided && band.contains(point)).then(|| (band, self.title.clone()))
            })
            .or_else(|| {
                let outer = self.outer();
                outer
                    .contains(point)
                    .then(|| self.tooltip.clone().map(|tooltip| (outer, tooltip)))
                    .flatten()
            })
    }

    fn is_focusable(&self) -> bool {
        self.answers_pointer() && !self.activation.state.disabled
    }

    fn set_focus(&mut self, focused: bool) {
        if self.activation.state.focused != focused {
            self.activation.state.focused = focused;
            if !focused {
                self.armed_action = None;
            }
            self.dirty = true;
        }
    }

    fn is_focused(&self) -> bool {
        self.activation.state.focused
    }

    fn is_enabled(&self) -> bool {
        !self.activation.state.disabled
    }

    /// An action on the row itself, or on one of its action buttons (its
    /// children in the accessibility tree, by position).
    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        match key_path {
            [] => {
                use crate::a11y::Action;
                match action {
                    Action::Activate => {
                        if let Some(armed) = self.armed_action {
                            return self.run_action(armed);
                        }
                        if self.activation.activate() {
                            return true;
                        }
                        self.toggle_checkbox()
                    }
                    Action::ToggleCheck => self.toggle_checkbox(),
                    _ => false,
                }
            }
            [index] if matches!(action, crate::a11y::Action::Activate) => {
                usize::try_from(*index).is_ok_and(|index| self.run_action(index))
            }
            _ => false,
        }
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.check.is_moving(crate::motion::frame_now())
            || self
                .trailing
                .as_ref()
                .is_some_and(|t| t.element.needs_redraw() || t.element.needs_animation_frame())
    }

    /// Frames only while its leading checkbox is on its way.
    fn needs_animation_frame(&self) -> bool {
        self.check.is_moving(crate::motion::frame_now())
            || crate::widget::defaults::needs_animation_frame(&|visit| self.for_each_child(visit))
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        if let Some(trailing) = &mut self.trailing {
            trailing.element.mark_drawn();
        }
    }

    fn is_layout_dirty(&self) -> bool {
        // Never laid out yet; a row inside a collapsed section keeps an
        // empty box without asking to be measured again on every frame.
        !self.laid_out
            || self
                .trailing
                .as_ref()
                .is_some_and(|t| t.element.is_layout_dirty())
    }

    // The control (child 1, after the band's stand-in) is described by the
    // row's subtitle and secondary line, which a reader would otherwise only
    // meet on the row.
    fn accessibility_child_description(&self, child_index: usize) -> Option<String> {
        if child_index != 1 || self.trailing.is_none() {
            return None;
        }
        let words: Vec<&str> = [self.subtitle.as_deref(), self.secondary.as_deref()]
            .into_iter()
            .flatten()
            .filter(|words| !words.is_empty())
            .collect();
        (!words.is_empty()).then(|| words.join(", "))
    }

    // In the order of the row's layout nodes: the text band's stand-in,
    // then the control.
    fn children(&self) -> Vec<&dyn Widget> {
        self.trailing
            .iter()
            .flat_map(|t| [&t.band_slot as &dyn Widget, t.element.as_ref()])
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children: Vec<&mut (dyn Widget + '_)> = Vec::with_capacity(2);
        if let Some(t) = &mut self.trailing {
            children.push(&mut t.band_slot);
            children.push(t.element.as_mut());
        }
        children
    }

    fn wants_pointer_capture(&self) -> bool {
        self.swipe_origin.is_some()
            || self
                .trailing
                .as_ref()
                .is_some_and(|t| t.element.wants_pointer_capture())
    }

    fn name(&self) -> &'static str {
        "ListRow"
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.activation.id())
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let id = self.activation.id().to_owned();
        let outer = self.outer();
        let mut node = self
            .with_face(|face| face.accessibility(outer, id.clone()))
            .with_semantic_key(self.activation.id());
        if let Some(tooltip) = &self.leading_tooltip {
            node.description = Some(tooltip.clone());
        }
        if let Some(description) = &self.description {
            node.description = Some(description.clone());
        }
        // The face numbers the actions it shows; address each by its place
        // among all the row's actions, as its binding is, and say where it
        // shows so a pointer or an agent can reach it.
        let rects = self.action_slots();
        for ((child, index), rect) in node
            .children
            .iter_mut()
            .zip(self.shown_actions())
            .zip(rects)
        {
            child.action_target = Some(format!("{id}/action/{index}"));
            child.bounds = Some(rect.into());
        }
        // The check box is a control of its own at `<row>/check`: a reader, a
        // script and an agent reach it there, and an action on this item rests
        // its mark on the box rather than on the row's middle.
        if let Some(Leading::Checkbox { checked, .. }) = &self.leading
            && let Some(rect) = self.leading_hit()
        {
            node.children.push(
                AccessNode::new(Role::Checkbox)
                    .with_action_target(format!("{id}/check"))
                    .with_bounds(rect.into())
                    .with_state(
                        AccessState::new()
                            .checked(*checked)
                            .disabled(self.activation.state.disabled),
                    ),
            );
        }
        Some(node)
    }

    /// Run the action a button child of the row's node was addressed by.
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &crate::widget::ModelPath,
    ) -> bool {
        let id = self.activation.id().to_owned();
        if target == id {
            return self.perform_action(action, &[], state, path);
        }
        if target.strip_prefix(id.as_str()) == Some("/check") {
            return matches!(action, crate::a11y::Action::Activate) && self.toggle_checkbox();
        }
        let Some(index) = target
            .strip_prefix(id.as_str())
            .and_then(|rest| rest.strip_prefix("/action/"))
            .and_then(|index| index.parse::<usize>().ok())
        else {
            return false;
        };
        if !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        let control = id;
        match self.actions.get_mut(index) {
            Some(row_action) if row_action.is_available() => {
                row_action.run(&control);
                true
            }
            _ => false,
        }
    }
}

impl crate::widget::ResourceOperations for ListRow {
    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        let mut handlers = self.activation.native_handlers();
        if self.check_verb.is_some() {
            handlers.insert(CHECK_ROW.to_owned());
        }
        if let Some(trailing) = &self.trailing {
            handlers.extend(crate::widget::resource_operation_handlers(
                &*trailing.element,
            ));
        }
        handlers
    }

    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        if let Some(started) = self.activation.start_native(op)? {
            return Ok(Some(started));
        }
        let checked = self
            .check_verb
            .as_ref()
            .map(|verb| verb.start(op))
            .transpose()?
            .flatten();
        if let Some(checked) = checked {
            if let Some(Leading::Checkbox {
                checked: shown,
                on_toggle,
                ..
            }) = &mut self.leading
            {
                *shown = checked;
                self.dirty = true;
                on_toggle(checked);
            }
            return Ok(Some(crate::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )));
        }
        crate::widget::start_in_children(self, op, doc, path)
    }
}

impl crate::resources::PublishesResources for ListRow {
    fn resources(&self, _doc: &automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        // The row is one resource however many verbs it publishes: a host
        // keeps one spec per id, so a second spec under the same id would
        // shadow the first and the verb it holds would answer nothing.
        let mut resource = self.activation.native_resources().into_iter().next();
        if let Some(verb) = &self.check_verb {
            let mut verb = verb.resource();
            match resource.as_mut() {
                Some(resource) => resource.actions.append(&mut verb.actions),
                None => resource = Some(verb),
            }
        }
        resource.into_iter().collect()
    }
}

#[path = "list_row_keys.rs"]
mod keys;

#[path = "list_row_shape.rs"]
mod shape;
pub use shape::{RowLeadingShape, RowShape};

mod kept;
use kept::KeptRow;

mod geometry;
pub use geometry::mark_text_inset;

mod metrics;
use metrics::RowMetrics;

mod leading;

mod part;
use part::RowPart;

mod edge;
pub use edge::{RowAction, RowChange, SwipeAction};

mod trailing;
use trailing::Trailing;

mod colour;

mod paint;
pub(crate) use paint::paint_code_badge;

mod spans;
use spans::found_spans;

#[cfg(test)]
#[path = "list_row_tests.rs"]
mod tests;

mod component;
#[path = "list_row_controls.rs"]
mod controls;
pub use component::{ListRowProps, SwipeActionProps};

mod control_row;
pub use control_row::ControlRow;

mod control_row_component;
pub use control_row_component::ControlRowProps;
