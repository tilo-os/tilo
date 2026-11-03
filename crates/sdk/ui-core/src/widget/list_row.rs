//! `ListRow`: one row of a content list (a message, a contact, a request,
//! a file, a device), as a retained widget and as the [`ListRowFace`]
//! immediate painter with identical geometry.
//!
//! A row has up to four columns, each optional except the title:
//!
//! | Column | Holds | Set in |
//! |---|---|---|
//! | leading | an [`Avatar`](crate::widget::Avatar), an [`Icon`], a short badge (`GET`), a thumbnail, a checkbox, a swatch, in a fixed-width column so a list's titles line up | the leading slot's own geometry |
//! | text | the title (with a mark and tags after it), an optional subtitle, then an optional secondary line of one or two lines | `Body` (`.strong()` when emphasised), `Body`, `Caption` |
//! | meta | a timestamp, a size, a count, on the title's line | `Caption.numeric()`, right-aligned |
//! | trailing | a value, one persistent control, a status dot, a check, a disclosure chevron | — |
//!
//! What a face paints lives in `paint` beside this file; the rect of each of
//! its columns in `geometry`.
//!
//! Hover (or keyboard focus) swaps the meta for the row's actions. The row
//! height comes from the measured line boxes of its text, never a constant,
//! so it grows with the text scale. Selection is the lift (HIG §9.3), and
//! rows sit edge to edge, with no rule between them.
//!
//! **One content edge.** The row's text starts on its column's content
//! edge; the hover and selection lift bleed [`row_bleed`] past it on both
//! sides. A face paints into the row's *outer* rect (the content edge grown
//! by the bleed); a retained row grows its own layout box by the bleed when
//! it paints, so its title lines up with the section title above it.
//!
//! [`row_bleed`]: crate::chrome::row_bleed

use std::borrow::Cow;
use std::collections::HashMap;

use crate::a11y::{AccessNode, AccessState, Role};
use crate::chrome::{RowJoin, SelectionState, row_bleed};
use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{FontChoice, TextRenderer, TextStyle};
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
    ActionEmitter, ChipStyle, ControlBindingMetadata, Element, MediaArt, MediaAspect, MediaSource,
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
    /// [`ListRowFace::leading_rect`].
    Checkbox { checked: bool },
    /// A colour that is the row's content (a calendar's colour, a label).
    Swatch(Color),
    /// How strong a radio signal is (a Wi-Fi network): the
    /// [`SignalMeterFace`](crate::widget::SignalMeterFace) at icon size.
    Signal(crate::widget::SignalLevel),
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
            Self::Checkbox { checked } => f
                .debug_struct("Checkbox")
                .field("checked", checked)
                .finish(),
            Self::Swatch(colour) => f.debug_tuple("Swatch").field(colour).finish(),
            Self::Signal(level) => f.debug_tuple("Signal").field(level).finish(),
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

/// How strongly the colour of whoever made an unseen change washes the row: a hint, quieter
/// than a selection.
const CHANGE_WASH: f32 = 0.07;

/// A change someone else made to what the row shows and the person has not seen yet
/// (task-legibility §3): their face on the row's trailing edge, their colour faintly under it,
/// at the row's own height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowChange<'a> {
    /// Their presence identity, which picks their colour.
    pub identity: &'a str,
    /// What the room calls them: their face's initials.
    pub name: &'a str,
    /// What assistive technology says of it: who, and in which run.
    pub spoken: &'a str,
}

impl RowChange<'_> {
    fn colour(self, theme: &Theme) -> Color {
        theme
            .tokens
            .colors
            .presence
            .hue(PresenceHue::of_identity(self.identity))
    }
}

/// One of a row's hover actions: an icon button that names what it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowAction<'a> {
    pub icon: Icon,
    pub label: &'a str,
}

/// The action a swipe reveals under the row's trailing edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwipeAction<'a> {
    pub icon: Icon,
    pub label: &'a str,
    /// Deleting, archiving away, revoking: the revealed area is the danger
    /// status colour, because the completed swipe is the confirmation.
    pub destructive: bool,
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

/// The resolved geometry of every row in `theme`: what the face paints and
/// the retained row measures, from one place.
#[derive(Clone, Debug)]
struct RowMetrics {
    bleed: f64,
    pad_y: f64,
    min_height: f64,
    column_gap: f64,
    title: TextStyle,
    title_strong: TextStyle,
    subtitle: TextStyle,
    secondary: TextStyle,
    meta: TextStyle,
    badge: TextStyle,
    value_body: TextStyle,
    value_title: TextStyle,
    icon: f64,
    mark: f64,
    action: f64,
    action_gap: f64,
    dot: f64,
    radius: f64,
    tag_gap: f64,
    /// One step of nesting: the gutter a disclosure chevron hangs in, left of its content.
    nest: f64,
}

impl RowMetrics {
    fn new(theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        let metrics = &theme.tokens.metrics;
        Self {
            bleed: row_bleed(theme),
            // The row's height comes from its kind (36 / 52 / 64, compact
            // 28 / 44 / 52); the text keeps a tight inset inside it.
            pad_y: f64::from(spacing.rhythm.tight),
            min_height: f64::from(metrics.row_single),
            column_gap: f64::from(spacing.s3),
            title: theme.text(TextRole::Body),
            title_strong: theme.text(TextRole::Body).strong(),
            subtitle: theme.text(TextRole::Body),
            secondary: theme.text(TextRole::Caption),
            meta: theme.text(TextRole::Caption).numeric(),
            badge: theme.text(TextRole::Caption).strong(),
            value_body: theme.text(TextRole::Body).numeric(),
            value_title: theme.text(TextRole::Title).numeric(),
            icon: f64::from(metrics.icon_control),
            mark: f64::from(metrics.icon_sm),
            action: f64::from(metrics.bar_control_height),
            action_gap: f64::from(spacing.s1),
            dot: f64::from(spacing.s2),
            radius: f64::from(theme.tokens.radii.item),
            tag_gap: f64::from(spacing.s1),
            nest: crate::chrome::disclosure_gutter(theme),
        }
    }

    fn line(text: &mut TextRenderer, style: &TextStyle) -> f64 {
        f64::from(text.measure_style("Ag", None, style).1)
    }

    /// The avatar's side: the inline size beside a one-line row, the list
    /// size beside a row with more lines. Fixed per kind of row, so a list's
    /// titles share one edge whatever each row's secondary text does.
    fn avatar(theme: &Theme, extra_lines: bool) -> f64 {
        let metrics = &theme.tokens.metrics;
        if extra_lines {
            f64::from(metrics.avatar_row)
        } else {
            f64::from(metrics.avatar_inline)
        }
    }

    /// The least a row is tall: one line 36, a title and a secondary line
    /// 52, a row led by a 40 px avatar 64 (28 / 44 / 52 compact).
    fn min_height_for(theme: &Theme, extra_lines: bool, big_leading: bool) -> f64 {
        let metrics = &theme.tokens.metrics;
        f64::from(if extra_lines && big_leading {
            metrics.row_avatar
        } else if extra_lines {
            metrics.row_double
        } else {
            metrics.row_single
        })
    }
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
    /// Where the title's line ends: the meta narrows only that line.
    title_right: f64,
    /// Where the line under the title ends: a count narrows only that line.
    second_right: f64,
    /// The persistent trailing control's slot.
    trailing: Option<Rect>,
    /// The trailing value's right edge.
    value_right: f64,
    /// The status dot's centre x.
    dot_x: Option<f64>,
    /// The face of whoever made a change the person has not seen.
    change: Option<Rect>,
    check: Option<Rect>,
    /// The disclosure chevron's slot.
    disclosure: Option<Rect>,
    /// Where the meta's right edge sits.
    meta_right: f64,
}

/// How many marks [`ListRowFace::mark`] keeps after a title.
pub const MAX_MARKS: usize = 3;

/// One mark after a row's title: an icon in tertiary ink, or a swatch in a
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
    title_lines: u8,
    title_family: Option<&'a FontChoice>,
    /// The words a search found the row by, marked where the title has them.
    found: &'a [&'a str],
    subtitle: Option<&'a str>,
    secondary: Option<&'a str>,
    secondary_lines: u8,
    meta: Option<&'a str>,
    meta_spans: &'a [(&'a str, Tone)],
    value: Option<(&'a str, RowValue)>,
    marks: [Option<RowMark>; MAX_MARKS],
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
    /// Where the leading checkbox is on its way at this frame, from the row's
    /// [`crate::widget::checkbox::CheckMotion`]; at rest on its value when unset.
    check_pose: Option<crate::widget::checkbox::CheckPose>,
}

impl<'a> ListRowFace<'a> {
    #[must_use]
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            title_lines: 1,
            title_family: None,
            found: &[],
            subtitle: None,
            secondary: None,
            secondary_lines: 1,
            meta: None,
            meta_spans: &[],
            value: None,
            marks: [None; MAX_MARKS],
            tags: &[],
            count: None,
            leading: None,
            leading_column: None,
            nested: false,
            depth: 0,
            status: None,
            change: None,
            struck: false,
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
            check_pose: None,
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

    /// A timestamp, size or count, right-aligned in tabular figures on the
    /// title's line. It narrows only the title's line.
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

    /// A figure at the trailing edge (a time, a code, a temperature) that
    /// hover actions never replace.
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

    /// A mark after the title in tertiary ink (starred, pinned, shared,
    /// has attachments). Each call adds one more, in order, up to
    /// [`MAX_MARKS`]; later ones are ignored. Star, pin and bookmark are
    /// filled.
    #[must_use]
    pub fn mark(mut self, icon: Icon) -> Self {
        self.push_mark(RowMark::Icon(icon));
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

    /// How many items the row stands for (the messages in a thread), in a
    /// quiet pill at the trailing edge of the line under the title.
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
    /// subtree's rows stand under their parents; its chevron hangs in its own gutter.
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

    /// Where the face of an unseen change sits in `rect`, when the row shows one.
    #[must_use]
    pub fn change_rect(&self, rect: Rect, text: &mut TextRenderer, theme: &Theme) -> Option<Rect> {
        self.geometry(rect, &RowMetrics::new(theme), text, theme)
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
        match self.leading {
            Some(RowLeading::Badge(badge)) => format!("{badge} {title}"),
            _ => title.into_owned(),
        }
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
        let mut value: Vec<&str> = self.tags.iter().map(|(label, _)| *label).collect();
        value.extend(
            [
                self.subtitle,
                self.secondary,
                self.meta,
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
            (Some(checked), _) | (None, Some(RowLeading::Checkbox { checked })) => Some(checked),
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

/// Owned form of [`RowLeading`] for the retained row.
enum Leading {
    Avatar {
        name: String,
        identity: String,
        image: Option<ImageData>,
        square: bool,
        glyph: Option<Icon>,
    },
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
    },
    Swatch(Color),
    AvatarSpace,
    Signal(crate::widget::SignalLevel),
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
    on_activate: Box<dyn FnMut()>,
}

/// Distance past which a released swipe completes, as a share of the row.
const SWIPE_COMMIT: f64 = 0.4;

/// The narrowest a row's text may get, in line boxes of its title, before
/// its trailing control wraps under it.
const MIN_TEXT_LINES: u8 = 8;

/// Whether `control` draws a face of one size whatever room it gets (a
/// switch, a checkbox), so stretching it under the text would only move it
/// off the text's edge. Looks through wrappers that announce nothing
/// themselves to the control they hold.
fn keeps_its_face_size(control: &dyn Widget) -> bool {
    match control.accessibility() {
        Some(node) => matches!(node.role, Role::Switch | Role::Checkbox | Role::RadioButton),
        None => match control.children().as_slice() {
            [only] => keeps_its_face_size(*only),
            _ => false,
        },
    }
}

/// The persistent control a retained row holds at its trailing edge.
struct Trailing {
    /// Stands for the text band among the row's children: the row's node
    /// holds the band's node, then the control's, and every walk that pairs
    /// a widget's children with its layout nodes (focus order, the
    /// accessibility tree) must meet the control at the control's node.
    band_slot: crate::widget::Absent,
    element: Element,
    band: Option<NodeId>,
    node: Option<NodeId>,
    /// The text band's height, settled from measured line boxes.
    band_height: f32,
    /// The control wrapped under the text.
    wrapped: bool,
    /// The control's own width, while it is not filling the row.
    natural: f32,
    /// The width the control fills under the text, from the text's edge to
    /// the row's, where a control beside the text ends.
    fill: Option<f32>,
    /// The control's own minimum width, restored when it stops filling.
    own_min: taffy::Dimension,
    /// Centred on the column a disclosure chevron stands in
    /// ([`ListRow::trailing_disclosure`]).
    on_disclosure_column: bool,
}

/// An unseen change a retained row shows ([`ListRow::change`]).
struct Changed {
    identity: String,
    name: String,
    spoken: String,
    show: Option<Box<dyn FnMut()>>,
}

/// One of a row's own pressable controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowPart {
    /// The hover action at this index.
    Action(usize),
    /// The leading checkbox.
    Check,
    /// The face of an unseen change: shows what was done.
    Change,
}

impl RowPart {
    /// What names the control across a rebuild: the row's id and which
    /// control of the row it is.
    fn key(self, row: &str) -> String {
        if row.is_empty() {
            return String::new();
        }
        match self {
            Self::Action(index) => format!("{row}/action/{index}"),
            Self::Check => format!("{row}/check"),
            Self::Change => format!("{row}/change"),
        }
    }
}

/// A retained list row. Construct with a stable id and a title; press
/// activates it (pointer, Enter/Space, or assistive technology).
pub struct ListRow {
    title: String,
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
    meta: Option<String>,
    meta_spans: Vec<(String, Tone)>,
    value: Option<(String, RowValue)>,
    mark: Option<Icon>,
    tags: Vec<(String, ChipStyle)>,
    count: Option<usize>,
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
    status: Option<(StatusKind, String)>,
    /// An unseen change of what the row shows ([`Self::change`]).
    change: Option<Changed>,
    /// It stands for a removed item ([`Self::struck`]).
    struck: bool,
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
            laid_out: false,
            title_elided: false,
            two_line_title: false,
            title_family: None,
            subtitle: None,
            secondary: None,
            secondary_line_count: 1,
            meta: None,
            meta_spans: Vec::new(),
            value: None,
            mark: None,
            tags: Vec::new(),
            count: None,
            leading: None,
            leading_column: None,
            nested: false,
            depth: 0,
            column_badges: None,
            shared_leading: None,
            shared_after_trailing: None,
            leading_tooltip: None,
            tooltip: None,
            status: None,
            change: None,
            struck: false,
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
            pressed_part: crate::pointer_replay::HeldPress::default(),
            shortcuts: Vec::new(),
            swipe: None,
            swipe_origin: None,
            swipe_offset: 0.0,
            trailing: None,
            activation: Activation::new(id),
            check_verb: None,
            check_binding: None,
            environment: crate::interaction::InteractionEnvironment::default(),
            metrics: None,
            theme: None,
            rect: Rect::ZERO,
            dirty: true,
            feel: crate::motion::Feel::Standard,
            check: crate::widget::checkbox::CheckMotion::default(),
            check_pose: None,
            hand_on: crate::motion::HandOn::default(),
        }
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

    /// A person's avatar in the leading column.
    #[must_use]
    pub fn avatar(mut self, name: impl Into<String>, identity: impl Into<String>) -> Self {
        self.leading = Some(Leading::Avatar {
            name: name.into(),
            identity: identity.into(),
            image: None,
            square: false,
            glyph: None,
        });
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
    #[cfg(feature = "local-daemons")]
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

    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.leading = Some(Leading::Icon(icon));
        self
    }

    /// See [`RowLeading::Badge`].
    #[must_use]
    pub fn badge(mut self, badge: impl Into<String>) -> Self {
        self.leading = Some(Leading::Badge(badge.into()));
        self
    }

    /// What the leading badge or icon stands for ("Retrieves the resource"
    /// for `GET`, "SSH" for a terminal glyph): the tooltip when the pointer
    /// rests on it, and the row's accessible description.
    #[must_use]
    pub fn leading_tooltip(mut self, tooltip: impl Into<String>) -> Self {
        self.leading_tooltip = Some(tooltip.into());
        self
    }

    /// See [`RowLeading::Thumbnail`].
    #[must_use]
    pub fn thumbnail(mut self, image: ImageData) -> Self {
        self.leading = Some(Leading::Thumbnail(image));
        self
    }

    /// See [`RowLeading::Media`]: art from `source` at `aspect`, the
    /// placeholder with the title's initials until it loads.
    #[must_use]
    pub fn media(mut self, source: MediaSource, aspect: MediaAspect) -> Self {
        self.leading = Some(Leading::Media {
            source,
            aspect,
            loaded: None,
        });
        self
    }

    /// See [`RowLeading::Checkbox`]: a selection checkbox. Pressing it (or
    /// Space on the focused row) runs `on_toggle` with the new state.
    #[must_use]
    pub fn checkbox(mut self, checked: bool, on_toggle: impl FnMut(bool) + 'static) -> Self {
        self.leading = Some(Leading::Checkbox {
            checked,
            on_toggle: Box::new(on_toggle),
        });
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
        });
        self
    }

    /// See [`RowLeading::Swatch`].
    #[must_use]
    pub fn swatch(mut self, colour: Color) -> Self {
        self.leading = Some(Leading::Swatch(colour));
        self
    }

    /// See [`RowLeading::Signal`]: the row leads with how strong its
    /// signal is, and is described by the meter's name ("Strong signal")
    /// unless a tooltip already says more.
    #[must_use]
    pub fn signal(mut self, level: crate::widget::SignalLevel) -> Self {
        self.leading = Some(Leading::Signal(level));
        if self.leading_tooltip.is_none() {
            self.leading_tooltip = Some(level.name());
        }
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

    /// A change `identity` (called `name`) made to what the row shows and the person has not
    /// seen: their face on the trailing edge and their colour faintly under the row, at its own
    /// height; `spoken` (who, in which run) on hover and to assistive technology; pressing the
    /// face runs `show`.
    #[must_use]
    pub fn change(
        mut self,
        identity: impl Into<String>,
        name: impl Into<String>,
        spoken: impl Into<String>,
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
            show,
        });
        self
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
        let m = RowMetrics::new(theme);
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
            on_activate: Box::new(on_activate),
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
    /// Wrapped under the text, the control fills the row from the text's
    /// edge to the row's own, where every control beside its text ends,
    /// instead of keeping its own width. It keeps filling while its own
    /// width still would not fit beside the text's narrowest. True when the
    /// control's style changed.
    fn fill_under_text(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        let (Some(theme), Some(metrics)) = (self.theme.clone(), self.metrics.as_ref()) else {
            return false;
        };
        let column_gap = metrics.column_gap;
        let min_text = f64::from(MIN_TEXT_LINES) * f64::from(metrics.title.line_height_px());
        let bleed = metrics.bleed;
        let Some(trailing) = &self.trailing else {
            return false;
        };
        let (Some(band), Some(child)) = (trailing.band, trailing.node) else {
            return false;
        };
        let (Ok(row), Ok(band_layout), Ok(child_layout)) =
            (taffy.layout(node), taffy.layout(band), taffy.layout(child))
        else {
            return false;
        };
        let row_width = f64::from(row.size.width);
        let band_height = f64::from(trailing.band_height);
        let (natural, wrapped) = if trailing.fill.is_some() {
            let natural = f64::from(trailing.natural);
            (natural, min_text + column_gap + natural > row_width + 0.5)
        } else {
            (
                f64::from(child_layout.size.width),
                child_layout.location.y >= band_layout.location.y + trailing.band_height - 0.5,
            )
        };
        let fill = (wrapped && !keeps_its_face_size(trailing.element.as_ref())).then(|| {
            let band_rect = Rect::new(-bleed, 0.0, row_width + bleed, band_height);
            let m = RowMetrics::new(&theme);
            let text_edge = self.with_face(|face| face.text_x(band_rect, &m, text, &theme));
            crate::num::f32_of_f64((row_width - text_edge.max(0.0)).max(0.0))
        });
        let Some(trailing) = &mut self.trailing else {
            return false;
        };
        if trailing.fill.is_none() {
            trailing.natural = crate::num::f32_of_f64(natural);
        }
        let same = match (trailing.fill, fill) {
            (Some(a), Some(b)) => (a - b).abs() < 0.5,
            (None, None) => true,
            _ => false,
        };
        if same {
            return false;
        }
        trailing.fill = fill;
        let own_min = trailing.own_min;
        let Ok(style) = taffy.style(child) else {
            return false;
        };
        let mut style = style.clone();
        style.min_size.width = fill.map_or(own_min, length);
        taffy.set_style(child, style).is_ok()
    }

    fn outer(&self) -> Rect {
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

    fn with_face<R>(&self, f: impl FnOnce(&ListRowFace<'_>) -> R) -> R {
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
        if let Some(pose) = self.check_pose {
            face = face.check_pose(pose);
        }
        if self.two_line_title {
            face = face.two_line_title();
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
            face = face.mark(mark);
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
        if let Some(leading) = &self.leading {
            face = face.leading(match leading {
                Leading::Avatar {
                    name,
                    identity,
                    image,
                    square,
                    glyph,
                } => {
                    let mut avatar = AvatarFace::new(name, identity).image(image.as_ref());
                    if let Some(glyph) = glyph {
                        avatar = avatar.glyph(*glyph);
                    }
                    RowLeading::Avatar(if *square { avatar.square() } else { avatar })
                }
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
                Leading::Checkbox { checked, .. } => RowLeading::Checkbox { checked: *checked },
                Leading::Swatch(colour) => RowLeading::Swatch(*colour),
                Leading::AvatarSpace => RowLeading::AvatarSpace,
                Leading::Signal(level) => RowLeading::Signal(*level),
            });
        }
        // A row checked off is done with: its title strikes through and mutes, as a removed
        // item's does. The box follows the value it is bound to, so a check shows in the row
        // itself, whoever made it and wherever its row stands.
        face = face.struck(
            self.struck || matches!(&self.leading, Some(Leading::Checkbox { checked: true, .. })),
        );
        if let Some(change) = &self.change {
            face = face.change(RowChange {
                identity: &change.identity,
                name: &change.name,
                spoken: &change.spoken,
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

    fn action_rects(&self) -> Vec<Rect> {
        let (Some(metrics), Some(theme)) = (&self.metrics, &self.theme) else {
            return Vec::new();
        };
        let band = self.text_band();
        crate::text_render::with_layout_renderer(theme.text_scale, |text| {
            self.with_face(|face| face.action_rects_with(band, metrics, text, theme))
        })
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
        if let Some(Leading::Checkbox { checked, on_toggle }) = &mut self.leading {
            *checked = !*checked;
            let now = *checked;
            self.dirty = true;
            if let Some(verb) = &mut self.check_verb {
                verb.emit(now);
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
                        (swipe.on_activate)();
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

impl Widget for ListRow {
    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn crate::resources::PublishesResources,
        dyn crate::widget::ResourceOperations,
        dyn crate::widget::ReservedShape,
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
        if let Some(kept) = self.hand_on.build(self.identity().as_deref()) {
            self.take_up(kept);
        }
        let metrics = RowMetrics::new(theme);
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
            style.margin.left = length(crate::num::f32_of_f64(column_gap));
            style.margin.right = length(crate::num::f32_of_f64(after_trailing));
            style.margin.top = length(crate::num::f32_of_f64(pad_y));
            style.margin.bottom = length(crate::num::f32_of_f64(pad_y));
            trailing.own_min = style.min_size.width;
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
        let Some(theme) = &self.theme else {
            return taffy::Size::ZERO;
        };
        let height = self.with_face(|face| face.height(text_renderer, theme));
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w,
            _ => 0.0,
        });
        taffy::Size {
            width,
            height: crate::num::f32_of_f64(height),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = crate::widget::layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.dirty = true;
        }
        self.rect = rect;
        if let Some(kept) = self.hand_on.place(rect) {
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
                let m = RowMetrics::new(&theme);
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
                .draw(scene, transform, text_renderer, theme, presence);
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
        // The face numbers the actions it shows; address each by its place
        // among all the row's actions, as its binding is.
        for (child, index) in node.children.iter_mut().zip(self.shown_actions()) {
            child.action_target = Some(format!("{id}/action/{index}"));
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
        let mut resources = self.activation.native_resources();
        resources.extend(self.check_verb.iter().map(|verb| verb.resource()));
        resources
    }
}

impl crate::widget::controls::ActionControls for ListRow {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        self.activation.install_custody(domain, attachment);
        if let Some(verb) = &mut self.check_verb {
            verb.install_custody(domain, attachment);
        }
        for action in &mut self.actions {
            if let ActionHandler::Bound(emitter) = &mut action.handler {
                let _ = emitter.install_custody(domain, attachment);
            }
        }
        for child in Widget::children_mut(self) {
            crate::widget::controls::install_control_custody(child, domain, attachment);
        }
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        if let Some(invocation) = self.activation.take_invocation() {
            return Some(invocation);
        }
        if let Some(invocation) = self
            .check_verb
            .as_mut()
            .and_then(super::control_verb::ControlVerb::take)
        {
            return Some(invocation);
        }
        for action in &mut self.actions {
            if let ActionHandler::Bound(emitter) = &mut action.handler
                && let Some(invocation) = emitter.take()
            {
                return Some(invocation);
            }
        }
        Widget::children_mut(self)
            .into_iter()
            .find_map(crate::widget::controls::take_control_invocation)
    }

    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        let mut bindings = self.activation.bindings();
        let id = self.activation.id();
        if let Some(verb) = &self.check_verb {
            bindings.push(verb.metadata(id));
        }
        for (index, action) in self.actions.iter().enumerate() {
            match &action.handler {
                ActionHandler::Bound(emitter) => {
                    bindings.push(emitter.metadata(format!("{id}/action/{index}")));
                }
                // A callback-only action reaches no door, so it is no
                // binding: the inventory names what an agent can invoke.
                ActionHandler::Callback(_) => {}
            }
        }
        for child in Widget::children(self) {
            bindings.extend(crate::widget::controls::control_action_bindings(child));
        }
        bindings
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        let mut changed = self.activation.refresh_availability(allowed);
        if let Some(verb) = &mut self.check_verb {
            verb.refresh_availability(allowed);
        }
        for action in &mut self.actions {
            if let ActionHandler::Bound(emitter) = &mut action.handler {
                let before = emitter.is_available();
                emitter.refresh_availability(allowed);
                changed |= before != emitter.is_available();
            }
        }
        if changed {
            self.dirty = true;
        }
        for child in Widget::children_mut(self) {
            crate::widget::controls::refresh_control_action_availability(child, allowed);
        }
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

mod paint;
pub(crate) use paint::paint_code_badge;

mod spans;
use spans::found_spans;

#[cfg(test)]
#[path = "list_row_tests.rs"]
mod tests;

mod component;
pub use component::{ListRowProps, SwipeActionProps};

mod control_row;
pub use control_row::ControlRow;

mod control_row_component;
pub use control_row_component::ControlRowProps;
