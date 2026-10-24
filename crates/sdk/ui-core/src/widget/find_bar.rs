//! Find in a document: the [`FindBar`] and the [`Findable`] capability.
//!
//! A document-like view (text, code, a PDF, a hex dump, a terminal) provides
//! [`Findable`]: it runs the query over its own content, reports the matches,
//! scrolls one into view and paints every match with [`paint_find_match`].
//! The framework owns the bar: [`FindScope`] places it as a band at the top
//! of the content it wraps and binds Ctrl+F (⌘F), Enter / Shift+Enter,
//! F3 / Shift+F3 and Escape. A painted view that lays out its own chrome
//! hosts a [`FindBar`] directly ([`FindBar::layout`], `draw`, `handle_event`)
//! and calls [`FindBar::drive`] with itself.
//!
//! The bar has a counter ("3 of 12"), Match case and Whole word toggles,
//! previous/next and one close control, and optionally a replace row, a
//! text/hex strip and a regular-expression toggle. A view whose engine
//! finds on its own (a web page) reports its results with
//! [`FindBar::set_results`] and steps with [`Findable::step`].

use std::collections::HashMap;

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::icons::Icon;
use crate::platform::PlatformConventions;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::capability::capability_mut;
use crate::widget::search_field::FilterCount;
use crate::widget::{
    AppModel, Button, ButtonFace, ButtonRole, ControlSize, Element, ModelPath, SearchField,
    SegmentedFace, TaffyTreeExt, ToggleGroup, Widget,
};

/// What a find runs over: text, or a byte view's hex digits.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum FindMode {
    #[default]
    Text,
    /// The query is hex bytes (`de ad be ef`), for a byte view.
    Hex,
    /// The query is a regular expression.
    Regex,
}

/// The options a find runs under: the bar's chips, the window search's
/// options.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(default)]
pub struct FindOptions {
    pub match_case: bool,
    pub whole_word: bool,
    pub mode: FindMode,
}

/// What a document's find offers beyond matching text under Match case and
/// Whole word: replacing matches, a regular expression, a byte view's hex
/// digits. The find's options offer just these.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(default)]
pub struct FindAbilities {
    pub replace: bool,
    pub regex: bool,
    pub hex: bool,
}

impl FindAbilities {
    /// What `target` offers.
    #[must_use]
    pub fn of(target: &dyn Findable) -> Self {
        Self {
            replace: target.supports_replace(),
            regex: target.supports_regex(),
            hex: target.supports_hex(),
        }
    }

    /// What the first [`Findable`] in `root`'s subtree offers; nothing
    /// beyond matching when there is none.
    #[must_use]
    pub fn of_root(root: &mut dyn Widget) -> Self {
        with_findable(root, &mut |target| Self::of(target)).unwrap_or_default()
    }

    /// Whether it offers nothing beyond matching text.
    #[must_use]
    pub fn is_plain(&self) -> bool {
        *self == Self::default()
    }

    /// `options` as this find can run them: a mode it does not offer falls
    /// back to text.
    #[must_use]
    pub fn allow(self, mut options: FindOptions) -> FindOptions {
        let offered = match options.mode {
            FindMode::Text => true,
            FindMode::Hex => self.hex,
            FindMode::Regex => self.regex,
        };
        if !offered {
            options.mode = FindMode::Text;
        }
        options
    }
}

/// One match, as a span of the findable's own content (a byte range of its
/// text, or of its bytes in hex mode). The findable interprets it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FindMatch {
    pub start: usize,
    pub end: usize,
}

impl FindMatch {
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

/// A find a view ran on its own rather than through the window's search: an
/// action went to a match of `query` (an agent's "go to result"). The
/// window's search shows it: its field holds the query, and it steps on from
/// match `current`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnFind {
    pub query: String,
    pub options: FindOptions,
    /// The 0-based match the view shows.
    pub current: usize,
}

/// A view whose content can be searched in place.
pub trait Findable {
    /// Every match of `query` under `options`, in reading order, and keep
    /// them to paint. An empty query clears the matches.
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch>;

    /// Make match `index` the current one: scroll it into view and paint it
    /// emphasised (see [`paint_find_match`]).
    fn reveal(&mut self, index: usize);

    /// Paint match `index` emphasised without moving the view: the content
    /// changed under the find and the current match kept its place. A view
    /// that keeps its current match across [`Self::find`] needs nothing.
    fn mark_current(&mut self, _index: usize) {}

    /// The bar closed: stop painting matches.
    fn clear_find(&mut self) {
        let _ = self.find("", FindOptions::default());
    }

    /// Whether this view can replace text; the bar then offers a replace row.
    fn supports_replace(&self) -> bool {
        false
    }

    /// Replace match `index` with `with`. Returns whether it changed the
    /// content; an edit goes through the view's own actions, so it is undoable.
    fn replace(&mut self, _index: usize, _with: &str) -> bool {
        false
    }

    /// Replace every match; returns how many changed.
    fn replace_all(&mut self, _with: &str) -> usize {
        0
    }

    /// Whether a replacement this view took is written later, through its
    /// host's edit path (on its next frame), rather than at once. The find
    /// then runs again once the content moved, to show the next match.
    fn replaces_later(&self) -> bool {
        false
    }

    /// Whether this view shows bytes, so the bar offers a text/hex mode.
    fn supports_hex(&self) -> bool {
        false
    }

    /// Whether this view can match a regular expression, so the bar offers
    /// the toggle for it ([`FindMode::Regex`]).
    fn supports_regex(&self) -> bool {
        false
    }

    /// Step to the next (or previous) match in a view whose engine owns the
    /// matches (a web page). Return `true` when the view stepped itself; it
    /// then reports the new position with [`FindBar::set_results`]. The
    /// default `false` lets the bar step through the matches `find`
    /// returned.
    fn step(&mut self, _forward: bool) -> bool {
        false
    }

    /// A number that changes whenever the content under the bar changes (an
    /// edit, a page's text arriving). The bar finds again on its next
    /// [`FindBar::drive`] when it moved, keeping the current match, so the
    /// counter never lags the content. The default never changes; such a view
    /// calls [`FindBar::refresh`] itself.
    fn content_revision(&self) -> u64 {
        0
    }

    /// Where a view whose engine finds on its own (see [`Self::step`]) says
    /// its matches are, when it said so since the last call: `(total,
    /// current)`, `current` 0-based. The bar's counter follows on its next
    /// [`FindBar::drive`].
    fn take_engine_results(&mut self) -> Option<(usize, Option<usize>)> {
        None
    }

    /// The find this view ran on its own since the last call ([`OwnFind`]),
    /// which the window's search takes over: it shows the query and keeps
    /// the match the view went to.
    fn take_own_find(&mut self) -> Option<OwnFind> {
        None
    }
}

/// Width of the outline that tells the current match from the others.
const CURRENT_MATCH_OUTLINE: f64 = 1.5;

/// Paint one match highlight over `rect`, the span of the matched glyphs —
/// never a whole line or block. Every match is a wash of the searcher's own
/// selection colour; the current one is stronger and outlined so it can be
/// told apart at a glance.
pub fn paint_find_match(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    current: bool,
    theme: &Theme,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let intent = &theme.tokens.colors.intent;
    // Exactly the matched glyphs' span: a padded box runs into the glyph
    // after the match.
    let radius = f64::from(theme.tokens.radii.detail) / 2.0;
    let shape = RoundedRect::from_rect(rect, radius);
    if current {
        use crate::color_compat::ColorCompat;
        let strong = intent.selection_tint.with_alpha_factor(2.0);
        scene.fill(Fill::NonZero, transform, strong, None, &shape);
        // The outline lies inside the span, so it never reaches past it.
        let half = CURRENT_MATCH_OUTLINE / 2.0;
        if rect.width() > CURRENT_MATCH_OUTLINE && rect.height() > CURRENT_MATCH_OUTLINE {
            let outline =
                RoundedRect::from_rect(rect.inflate(-half, -half), (radius - half).max(0.0));
            scene.stroke(
                &Stroke::new(CURRENT_MATCH_OUTLINE),
                transform,
                intent.accent_ink,
                None,
                &outline,
            );
        }
    } else {
        scene.fill(
            Fill::NonZero,
            transform,
            intent.selection_tint,
            None,
            &shape,
        );
    }
}

/// What the bar asked for since the last [`FindBar::drive`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Pending {
    search: bool,
    /// The content changed: find again, keeping the current match.
    refresh: bool,
    step: Option<Step>,
    replace: Option<ReplaceRequest>,
    closed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Next,
    Previous,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ReplaceRequest {
    One,
    All,
}

/// Every clickable part of the bar, in focus order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Part {
    MatchCase,
    WholeWord,
    Regex,
    Text,
    Hex,
    Previous,
    Next,
    Close,
    Replace,
    ReplaceAll,
}

impl Part {
    /// The part's name, from the toolkit's catalogue.
    fn label(self) -> String {
        tr(match self {
            Self::MatchCase => "find-bar-match-case",
            Self::WholeWord => "find-bar-whole-word",
            Self::Regex => "find-bar-regex",
            Self::Text => "find-bar-text",
            Self::Hex => "find-bar-hex",
            Self::Previous => "find-bar-previous",
            Self::Next => "find-bar-next",
            Self::Close => "find-bar-close",
            Self::Replace => "find-bar-replace",
            Self::ReplaceAll => "find-bar-replace-all",
        })
    }
}

/// A toolkit message in the host's locale.
fn tr(key: &str) -> String {
    crate::i18n::widget_format(key, &[])
}

/// What a find over one [`Findable`] has found: how many matches, which is
/// current, and the query and content they were found for. The [`FindBar`]
/// keeps one, and so does the window bar's search
/// ([`crate::widget::WindowSearch`]): one engine, whatever draws the field.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FindSession {
    matches: usize,
    current: Option<usize>,
    /// The query and options the matches were last found for.
    searched: Option<(String, FindOptions)>,
    /// The findable's [`Findable::content_revision`] the matches were found in.
    searched_revision: u64,
    /// The view reports its own results ([`Self::set_results`]).
    engine_results: bool,
    /// A replacement the view applies later, through its host's edit path:
    /// the next find, once the content moved, shows the current match.
    reveal_after_edit: bool,
}

impl FindSession {
    /// How many matches the last find found.
    #[must_use]
    pub fn matches(&self) -> usize {
        self.matches
    }

    /// `(current, total)`, 1-based current; `None` with no current match.
    #[must_use]
    pub fn position(&self) -> Option<(usize, usize)> {
        self.current
            .map(|current| (current.saturating_add(1), self.matches))
    }

    /// The results of a view whose engine finds on its own: `total`
    /// matches, `current` the 0-based one shown.
    pub fn set_results(&mut self, total: usize, current: Option<usize>) {
        self.engine_results = true;
        self.matches = total;
        self.current = current.filter(|&c| c < total);
    }

    /// Whether `query` under `options` is not what the matches were found for.
    #[must_use]
    pub fn is_stale(&self, query: &str, options: FindOptions) -> bool {
        self.searched
            .as_ref()
            .is_none_or(|(searched, with)| searched != query || *with != options)
    }

    /// Whether the content under the find changed since it last ran.
    #[must_use]
    pub fn content_moved(&self, target: &dyn Findable) -> bool {
        self.searched.is_some() && self.searched_revision != target.content_revision()
    }

    /// Find `query` again and show the match nearest `keep` (the first
    /// with `None`): the query or an option changed.
    pub fn search(
        &mut self,
        target: &mut dyn Findable,
        query: &str,
        options: FindOptions,
        keep: Option<usize>,
    ) {
        self.reveal_after_edit = false;
        if self.find(target, query, options, keep)
            && let Some(current) = self.current
        {
            target.reveal(current);
        }
    }

    /// Find `query` again without moving the view, keeping the current
    /// match: the content changed under the find. After a replacement the
    /// view applied later, the match now current is shown as after any
    /// replacement.
    pub fn refresh(&mut self, target: &mut dyn Findable, query: &str, options: FindOptions) {
        let keep = self.current;
        if self.reveal_after_edit {
            self.search(target, query, options, keep);
            return;
        }
        if self.find(target, query, options, keep)
            && let Some(current) = self.current
        {
            target.mark_current(current);
        }
    }

    /// Replace the current match of `query` under `options` with `with`, or
    /// every match (`all`), then show the match that is current after it:
    /// the one that followed the replaced match. A view that applies the
    /// edit later is found again once its content moved. Returns whether the
    /// view took the replacement.
    pub fn replace(
        &mut self,
        target: &mut dyn Findable,
        query: &str,
        options: FindOptions,
        with: &str,
        all: bool,
    ) -> bool {
        let current = self.current;
        let changed = if all {
            target.replace_all(with) > 0
        } else {
            current.is_some_and(|current| target.replace(current, with))
        };
        if changed && target.replaces_later() {
            self.reveal_after_edit = true;
        } else {
            self.search(target, query, options, current);
        }
        changed
    }

    /// Run the find; whether it produced results to show (an engine that
    /// answers later reports through [`Self::set_results`]).
    fn find(
        &mut self,
        target: &mut dyn Findable,
        query: &str,
        options: FindOptions,
        keep: Option<usize>,
    ) -> bool {
        let found = target.find(query, options);
        self.searched = Some((query.to_owned(), options));
        self.searched_revision = target.content_revision();
        if self.engine_results && found.is_empty() && !query.is_empty() {
            return false;
        }
        self.matches = found.len();
        self.current =
            (self.matches > 0).then(|| keep.unwrap_or(0).min(self.matches.saturating_sub(1)));
        true
    }

    /// Make the next (or previous) match current, wrapping, and show it.
    pub fn step(&mut self, target: &mut dyn Findable, forward: bool) {
        if target.step(forward) {
            // The view's engine steps and reports where it landed.
            return;
        }
        if self.matches == 0 {
            return;
        }
        let current = self.current.unwrap_or(0);
        let next = if forward {
            current
                .saturating_add(1)
                .checked_rem(self.matches)
                .unwrap_or(0)
        } else {
            current
                .checked_sub(1)
                .unwrap_or_else(|| self.matches.saturating_sub(1))
        };
        self.current = Some(next);
        target.reveal(next);
    }

    /// The find ended: the view stops painting matches.
    pub fn clear(&mut self, target: &mut dyn Findable) {
        target.clear_find();
        self.matches = 0;
        self.current = None;
        self.searched = None;
    }

    /// Forget the matches without telling a view (it is gone).
    pub fn forget(&mut self) {
        self.matches = 0;
        self.current = None;
        self.searched = None;
    }
}

/// The find bar: a query field with a match counter, option chips,
/// previous/next and close, and an optional replace row.
pub struct FindBar {
    field: SearchField,
    replace_field: SearchField,
    options: FindOptions,
    replace_row: bool,
    hex_mode: bool,
    regex: bool,
    /// The band's leading and trailing inset: the host's content edge, so
    /// the field lines up with the text it finds in.
    inset: Option<(f64, f64)>,
    /// What the find found.
    session: FindSession,
    pending: Pending,
    rect: Rect,
    parts: Vec<(Part, Rect)>,
    hover: Option<Part>,
    last_cursor: Point,
    platform: PlatformConventions,
    /// Which field types: the query (`false`) or the replacement.
    replace_focused: bool,
    focused: bool,
}

impl Default for FindBar {
    fn default() -> Self {
        Self::new()
    }
}

impl FindBar {
    #[must_use]
    pub fn new() -> Self {
        Self {
            // The bar has one close control; its fields clear with the
            // keyboard, not with an X of their own.
            field: SearchField::new()
                .with_placeholder(&tr("find-bar-placeholder"))
                .with_count_slot()
                .without_clear()
                .with_id("find-query"),
            replace_field: SearchField::new()
                .with_icon(Icon::Pencil)
                .with_placeholder(&tr("find-bar-replace-placeholder"))
                .without_clear()
                .with_id("find-replacement"),
            options: FindOptions::default(),
            replace_row: false,
            hex_mode: false,
            regex: false,
            inset: None,
            session: FindSession::default(),
            pending: Pending::default(),
            rect: Rect::ZERO,
            parts: Vec::new(),
            hover: None,
            last_cursor: Point::ZERO,
            platform: PlatformConventions::default(),
            replace_focused: false,
            focused: false,
        }
    }

    /// Offer the replace row (the bar also offers it whenever its findable
    /// supports replacing).
    #[must_use]
    pub fn with_replace(mut self) -> Self {
        self.replace_row = true;
        self
    }

    /// Offer the text/hex mode switch.
    #[must_use]
    pub fn with_hex_mode(mut self) -> Self {
        self.hex_mode = true;
        self
    }

    /// Offer the regular-expression toggle (the bar also offers it whenever
    /// its findable supports regular expressions).
    #[must_use]
    pub fn with_regex(mut self) -> Self {
        self.regex = true;
        self
    }

    /// Start in `mode` (a byte view that opens on hex).
    #[must_use]
    pub fn with_mode(mut self, mode: FindMode) -> Self {
        self.set_mode(mode);
        self
    }

    /// Switch the mode; the query runs again.
    pub fn set_mode(&mut self, mode: FindMode) {
        if self.options.mode != mode {
            self.options.mode = mode;
            self.pending.search = true;
        }
    }

    /// Set every option at once (restoring a view's last find); the query
    /// runs again when they changed.
    pub fn set_options(&mut self, options: FindOptions) {
        if self.options != options {
            self.options = options;
            self.pending.search = true;
        }
    }

    /// Line the band's field and controls up with the host's content edge:
    /// `leading` and `trailing` from the band's edges (default `s3`).
    #[must_use]
    pub fn with_content_inset(mut self, leading: f64, trailing: f64) -> Self {
        self.set_content_inset(leading, trailing);
        self
    }

    /// [`Self::with_content_inset`] after construction.
    pub fn set_content_inset(&mut self, leading: f64, trailing: f64) {
        self.inset = Some((leading.max(0.0), trailing.max(0.0)));
    }

    /// The content under the bar changed (output arrived, a file
    /// reloaded): find again on the next [`Self::drive`], keeping the
    /// current match where it still exists.
    pub fn refresh(&mut self) {
        self.pending.refresh = true;
    }

    /// The results of a view whose engine finds on its own: `total`
    /// matches, `current` the 0-based one shown. The counter follows.
    pub fn set_results(&mut self, total: usize, current: Option<usize>) {
        self.session.set_results(total, current);
        self.update_count();
    }

    /// Focus the replacement field (Ctrl+H on an open bar).
    pub fn focus_replace(&mut self) {
        self.replace_row = true;
        self.replace_focused = true;
        self.focused = true;
        self.field.set_focused(false);
        self.replace_field.set_focused(true);
        self.replace_field.select_all();
    }

    /// The query typed so far.
    #[must_use]
    pub fn query(&self) -> &str {
        self.field.value()
    }

    /// Start with `query` (the selection when the bar opens), selected.
    pub fn set_query(&mut self, query: &str) {
        self.field.set_query(query);
        self.field.select_all();
        self.pending.search = true;
    }

    #[must_use]
    pub fn options(&self) -> FindOptions {
        self.options
    }

    /// `(current, total)` — the counter's numbers, 1-based current.
    #[must_use]
    pub fn position(&self) -> Option<(usize, usize)> {
        self.session.position()
    }

    /// Focus the query and select it, as Ctrl+F on an open bar does.
    pub fn focus_query(&mut self) {
        self.replace_focused = false;
        self.focused = true;
        self.field.set_focused(true);
        self.replace_field.set_focused(false);
        self.field.select_all();
    }

    /// Whether the user asked to close the bar since the last
    /// [`drive`](Self::drive).
    #[must_use]
    pub fn close_requested(&self) -> bool {
        self.pending.closed
    }

    /// Whether the user asked to close the bar, once: the request is
    /// acknowledged, so the same bar opens again later without being
    /// rebuilt.
    pub fn take_close_requested(&mut self) -> bool {
        std::mem::take(&mut self.pending.closed)
    }

    /// Ask the bar to close (Escape does this).
    pub fn request_close(&mut self) {
        self.pending.closed = true;
    }

    /// Step to the next match, wrapping.
    pub fn next(&mut self) {
        self.pending.step = Some(Step::Next);
    }

    /// Step to the previous match, wrapping.
    pub fn previous(&mut self) {
        self.pending.step = Some(Step::Previous);
    }

    /// The bar's height: one control row plus padding, two with replace.
    #[must_use]
    pub fn height(&self, theme: &Theme) -> f64 {
        let row = ControlSize::Regular.height(theme);
        let pad = f64::from(theme.tokens.spacing.s2);
        let rows = if self.replace_row { 2.0 } else { 1.0 };
        rows * row + (rows + 1.0) * pad
    }

    /// Carry what the user did to `target`: re-run the query when it or an
    /// option changed, step the current match, replace, or clear on close.
    /// Call after routing each event.
    pub fn drive(&mut self, target: &mut dyn Findable) {
        self.replace_row |= target.supports_replace();
        self.hex_mode |= target.supports_hex();
        self.regex |= target.supports_regex();
        let _ = self.field.take_dirty();
        let pending = std::mem::take(&mut self.pending);
        if pending.closed {
            self.session.clear(target);
            self.pending.closed = true;
            return;
        }
        let query = self.field.value().to_owned();
        let stale = self.session.is_stale(&query, self.options);
        if let Some(request) = pending.replace {
            let with = self.replace_field.value().to_owned();
            let all = request == ReplaceRequest::All;
            let _ = self
                .session
                .replace(target, &query, self.options, &with, all);
        } else if pending.search || stale {
            self.session.search(target, &query, self.options, Some(0));
        } else if pending.refresh || self.session.content_moved(target) {
            // Content changed under the bar: the counter follows, the view
            // stays where the person is reading.
            self.session.refresh(target, &query, self.options);
        }
        if let Some(step) = pending.step {
            self.session.step(target, step == Step::Next);
        }
        if let Some((total, current)) = target.take_engine_results() {
            self.session.set_results(total, current);
        }
        self.update_count();
    }

    fn update_count(&mut self) {
        let count = (!self.field.value().is_empty()).then(|| {
            let (current, total) = self
                .session
                .position()
                .unwrap_or((0, self.session.matches()));
            FilterCount::new(current, total)
        });
        self.field.set_count(count);
    }

    fn toggle(&mut self, part: Part) {
        match part {
            Part::MatchCase => self.options.match_case = !self.options.match_case,
            Part::WholeWord => self.options.whole_word = !self.options.whole_word,
            Part::Regex => {
                self.options.mode = if self.options.mode == FindMode::Regex {
                    FindMode::Text
                } else {
                    FindMode::Regex
                };
            }
            Part::Text => self.options.mode = FindMode::Text,
            Part::Hex => self.options.mode = FindMode::Hex,
            Part::Previous => {
                self.previous();
                return;
            }
            Part::Next => {
                self.next();
                return;
            }
            Part::Close => {
                self.request_close();
                return;
            }
            Part::Replace => {
                self.pending.replace = Some(ReplaceRequest::One);
                return;
            }
            Part::ReplaceAll => {
                self.pending.replace = Some(ReplaceRequest::All);
                return;
            }
        }
        self.pending.search = true;
    }

    fn part_on(&self, part: Part) -> bool {
        match part {
            Part::MatchCase => self.options.match_case,
            Part::WholeWord => self.options.whole_word,
            Part::Regex => self.options.mode == FindMode::Regex,
            Part::Text => self.options.mode != FindMode::Hex,

            Part::Hex => self.options.mode == FindMode::Hex,
            _ => false,
        }
    }

    /// Lay the bar out in `rect` (its full band). Hosts that do not go
    /// through the layout pass call this before `draw`.
    pub fn layout(&mut self, rect: Rect, theme: &Theme, text: &mut TextRenderer) {
        self.rect = rect;
        self.parts.clear();
        let pad = f64::from(theme.tokens.spacing.s2);
        let gutter = f64::from(theme.tokens.spacing.s3);
        let (lead, trail) = self.inset.unwrap_or((gutter, gutter));
        let row_h = ControlSize::Regular.height(theme);
        let small_h = ControlSize::Small.height(theme);
        let x0 = rect.x0 + lead;
        let x1 = rect.x1 - trail;
        let y = rect.y0 + pad;
        // Trailing: previous, next, close as square icon buttons.
        let mut right = x1;
        for part in [Part::Close, Part::Next, Part::Previous] {
            self.parts
                .push((part, Rect::new(right - row_h, y, right, y + row_h)));
            right -= row_h;
        }
        right -= pad;
        // The option toggles, then the mode strip, right to left. Narrow
        // bars drop them before they squeeze the field.
        let small_y = y + (row_h - small_h) / 2.0;
        let field_min = 3.0 * row_h * 2.0;
        let mut toggles = Vec::new();
        if self.regex {
            toggles.push(Part::Regex);
        }
        toggles.extend([Part::WholeWord, Part::MatchCase]);
        for part in toggles {
            let label = part.label();
            let w = ButtonFace::new(Rect::ZERO, &label)
                .control_size(ControlSize::Small)
                .intrinsic_width(text, theme);
            if right - w < x0 + field_min {
                break;
            }
            self.parts.push((
                part,
                Rect::new(right - w, small_y, right, small_y + small_h),
            ));
            right -= w + f64::from(theme.tokens.spacing.s1);
        }
        if self.hex_mode {
            let labels = [Part::Text.label(), Part::Hex.label()];
            let labels = [labels[0].as_str(), labels[1].as_str()];
            right -= pad - f64::from(theme.tokens.spacing.s1);
            let w = SegmentedFace::intrinsic_width(&labels, ControlSize::Small, text, theme);
            if right - w >= x0 + field_min {
                let strip = Rect::new(right - w, small_y, right, small_y + small_h);
                let rects =
                    SegmentedFace::partition(strip, &labels, ControlSize::Small, text, theme);
                for (part, rect) in [Part::Text, Part::Hex].into_iter().zip(rects) {
                    self.parts.push((part, rect));
                }
                right -= w + pad;
            }
        }
        self.field
            .apply_layout_external(Rect::new(x0, y, right.max(x0), y + row_h));
        if self.replace_row {
            let y = y + row_h + pad;
            let mut right = x1;
            for part in [Part::ReplaceAll, Part::Replace] {
                let style = ControlSize::Regular.label_style(theme);
                let (w, _) = text.measure_style(&part.label(), None, &style);
                let w = f64::from(w) + 2.0 * gutter;
                self.parts
                    .push((part, Rect::new(right - w, y, right, y + row_h)));
                right -= w + pad;
            }
            self.replace_field
                .apply_layout_external(Rect::new(x0, y, right.max(x0), y + row_h));
        }
    }

    fn part_at(&self, point: Point) -> Option<Part> {
        self.parts
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(part, _)| *part)
    }

    /// Paint the bar. `layout` must have run.
    pub fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.rect.width() <= 0.0 {
            return;
        }
        // The band stands on the content surface; a hairline parts it from
        // the content below, which is the same surface.
        let ground = theme.tokens.colors.surfaces.content;
        crate::surface::separator(
            scene,
            transform,
            self.rect.x0,
            self.rect.x1,
            self.rect.y1,
            0.0,
            ground,
        );
        self.field.draw(scene, transform, text, theme, presence);
        if self.replace_row {
            self.replace_field
                .draw(scene, transform, text, theme, presence);
        }
        let parts = self.parts.clone();
        // Text / Hex is one segmented strip.
        let modes: Vec<(Part, Rect)> = parts
            .iter()
            .copied()
            .filter(|(part, _)| matches!(part, Part::Text | Part::Hex))
            .collect();
        if !modes.is_empty() {
            let rects: Vec<Rect> = modes.iter().map(|(_, rect)| *rect).collect();
            let labels: Vec<String> = modes.iter().map(|(part, _)| part.label()).collect();
            let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
            let selected = modes
                .iter()
                .position(|(part, _)| self.part_on(*part))
                .unwrap_or(0);
            let hovered = modes.iter().position(|(part, _)| self.hover == Some(*part));
            let face = SegmentedFace::new(&rects, &labels, selected)
                .control_size(ControlSize::Small)
                .hovered(hovered);
            ToggleGroup::paint(scene, transform, text, theme, &face);
        }
        for (part, rect) in parts {
            let hovered = self.hover == Some(part);
            let label = part.label();
            match part {
                Part::Text | Part::Hex => {}
                Part::MatchCase | Part::WholeWord | Part::Regex => {
                    let face = ButtonFace::new(rect, &label)
                        .role(ButtonRole::Plain)
                        .control_size(ControlSize::Small)
                        .toggled(self.part_on(part))
                        .hovered(hovered);
                    Button::paint(scene, transform, text, theme, &face);
                }
                Part::Previous | Part::Next | Part::Close => {
                    let icon = match part {
                        Part::Previous => Icon::ChevronUp,
                        Part::Next => Icon::ChevronDown,
                        _ => Icon::X,
                    };
                    let disabled =
                        matches!(part, Part::Previous | Part::Next) && self.session.matches() == 0;
                    let face = ButtonFace::icon_only(rect, icon, &label)
                        .hovered(hovered)
                        .disabled(disabled);
                    Button::paint(scene, transform, text, theme, &face);
                }
                Part::Replace | Part::ReplaceAll => {
                    let disabled = self.session.matches() == 0;
                    let face = ButtonFace::new(rect, &label)
                        .hovered(hovered)
                        .disabled(disabled);
                    Button::paint(scene, transform, text, theme, &face);
                }
            }
        }
    }

    /// Route one event; `node` is the bar's own layout node (its fields
    /// have none). Returns whether the bar consumed it.
    pub fn route(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) -> bool {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = ctx.logical(position);
            self.hover = self.part_at(self.last_cursor);
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            if let Some(part) = self.part_at(self.last_cursor) {
                self.toggle(part);
                *ctx.handled = true;
                return true;
            }
            if self.replace_row && self.replace_field.get_rect().contains(self.last_cursor) {
                self.replace_focused = true;
                self.field.set_focused(false);
            } else if self.field.get_rect().contains(self.last_cursor) {
                self.replace_focused = false;
                self.replace_field.set_focused(false);
            }
        }
        if self.focused
            && let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            let shift = ctx.modifiers.state().shift_key();
            match logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.request_close();
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter) if self.replace_focused => {
                    self.toggle(Part::Replace);
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter | NamedKey::F3) => {
                    if shift {
                        self.previous();
                    } else {
                        self.next();
                    }
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Tab) if self.replace_row => {
                    self.replace_focused = !self.replace_focused;
                    self.field.set_focused(!self.replace_focused);
                    self.replace_field.set_focused(self.replace_focused);
                    *ctx.handled = true;
                    return true;
                }
                _ => {}
            }
        }
        let field = if self.replace_focused {
            &mut self.replace_field
        } else {
            &mut self.field
        };
        if self.focused
            || matches!(
                event,
                WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
            )
        {
            field.handle_event(event, ctx);
        }
        *ctx.handled
    }
}

impl Widget for FindBar {
    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.platform = conventions;
        self.field.set_platform_conventions(conventions);
        self.replace_field.set_platform_conventions(conventions);
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "a find bar's height is a small logical-pixel value"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: taffy::Size {
                width: percent(1.0_f32),
                height: length(self.height(theme) as f32),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let rect = self.rect;
        self.layout(rect, theme, text);
        self.paint(scene, transform, text, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.route(event, ctx);
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if focused {
            self.field.set_focused(!self.replace_focused);
            self.replace_field.set_focused(self.replace_focused);
        } else {
            self.field.set_focused(false);
            self.replace_field.set_focused(false);
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        if self.replace_focused {
            self.replace_field.ime_cursor_rect()
        } else {
            self.field.ime_cursor_rect()
        }
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        let (scope, field) = if self.replace_focused {
            (
                b"replace".as_slice(),
                self.replace_field.ime_edit_fingerprint(key)?,
            )
        } else {
            (b"find".as_slice(), self.field.ime_edit_fingerprint(key)?)
        };
        Some(crate::ime::scoped_fingerprint(scope, field, key))
    }

    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        self.field.ime_content()
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.part_at(point).is_some() {
            return Some(winit::window::CursorIcon::Pointer);
        }
        if self.replace_row && self.replace_field.get_rect().contains(point) {
            return self.replace_field.cursor_shape(point);
        }
        self.field.cursor_shape(point)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if self.replace_row {
            vec![&self.field, &self.replace_field]
        } else {
            vec![&self.field]
        }
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(&self.field);
        if self.replace_row {
            visitor(&self.replace_field);
        }
    }

    fn accessibility_represents_child(&self, child_index: usize) -> bool {
        // These manually laid-out fields are published once through this
        // admitted owner, whose semantic targets route to the real controls.
        child_index == 0 || (self.replace_row && child_index == 1)
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.replace_row {
            vec![&mut self.field, &mut self.replace_field]
        } else {
            vec![&mut self.field]
        }
    }

    fn name(&self) -> &'static str {
        "FindBar"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let mut children = Vec::new();
        if let Some(field) = self.field.accessibility() {
            let position = match self.position() {
                Some((current, total)) => crate::i18n::widget_format(
                    "find-bar-position",
                    &[("current", current.into()), ("total", total.into())],
                ),
                None if self.field.value().is_empty() => String::new(),
                None => tr("find-bar-no-matches"),
            };
            let mut field = field
                .with_name(tr("find-bar-placeholder"))
                .with_bounds(self.field.get_rect())
                .with_action_target("find-query");
            if !position.is_empty() {
                field = field.with_description(position);
            }
            children.push(field);
        }
        if self.replace_row
            && let Some(field) = self.replace_field.accessibility()
        {
            children.push(
                field
                    .with_name(tr("find-bar-replace-placeholder"))
                    .with_bounds(self.replace_field.get_rect())
                    .with_action_target("find-replacement"),
            );
        }
        for (part, rect) in &self.parts {
            let role = match part {
                Part::MatchCase | Part::WholeWord | Part::Regex => Role::ToggleButton,
                Part::Text | Part::Hex => Role::RadioButton,
                _ => Role::Button,
            };
            let mut state = AccessState::new().focusable(true);
            if matches!(role, Role::ToggleButton | Role::RadioButton) {
                state = state.checked(self.part_on(*part));
            }

            children.push(
                AccessNode::new(role)
                    .with_name(part.label())
                    .with_bounds(*rect)
                    .with_action_target(format!("find-{part:?}"))
                    .with_state(state),
            );
        }
        Some(
            AccessNode::new(Role::Toolbar)
                .with_name(tr("find-bar-name"))
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let replacement = match target {
            "find-query" => Some(false),
            "find-replacement" if self.replace_row => Some(true),
            _ => None,
        };
        if let Some(replacement) = replacement {
            if matches!(action, crate::a11y::Action::Focus) {
                self.replace_focused = replacement;
                self.set_focus(true);
                return true;
            }
            let field = if replacement {
                &mut self.replace_field
            } else {
                &mut self.field
            };
            // The shared text core owns edits, selection and validation;
            // unsupported field operations remain explicitly unhandled.
            return field.perform_action(action, &[], state, path);
        }
        if !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        let Some(part) = self
            .parts
            .iter()
            .map(|(part, _)| *part)
            .find(|part| target == format!("find-{part:?}"))
        else {
            return false;
        };
        self.toggle(part);
        true
    }
}

/// Run `visit` on the first [`Findable`] in `widget`'s subtree; `None`
/// when there is none.
pub fn with_findable<R>(
    widget: &mut dyn Widget,
    visit: &mut dyn FnMut(&mut dyn Findable) -> R,
) -> Option<R> {
    if let Some(target) = capability_mut::<dyn Findable>(&mut *widget) {
        return Some(visit(target));
    }
    for child in widget.children_mut() {
        if let Some(result) = with_findable(child, visit) {
            return Some(result);
        }
    }
    None
}

/// Content with find: a [`FindBar`] that Ctrl+F (⌘F) opens as a band at the
/// top of the content, driving the first [`Findable`] inside it.
///
/// Opened from a menu or by a pointer, the band discloses
/// (`motion::Disclosure`): while it moves it stands over the content, which
/// keeps the whole scope (its larger extent) and moves with the band's edge
/// by transform; the content is laid out under the band once, at rest.
/// Ctrl+F and Escape land at once; the close button collapses the band.
pub struct FindScope {
    bar: FindBar,
    content: Element,
    open: bool,
    /// How much of the band is drawn while it opens or closes.
    disclosure: crate::motion::Disclosure,
    /// The motion the person allows, from the last theme seen.
    motion: crate::tokens::EffectiveMotion,
    /// The layout was built with the band on its way: over the content,
    /// which keeps the whole scope. Built again once it rests.
    laid_sliding: bool,
    structure_dirty: bool,
    rect: Rect,
    /// The pointer, for which side of the band a press lands on.
    pointer: Point,
    bar_node: Option<NodeId>,
    content_node: Option<NodeId>,
    platform: PlatformConventions,
    /// What opens the bar; `None` leaves opening to the host
    /// ([`FindScope::open`]).
    shortcut: Option<crate::platform::Shortcut>,
}

impl FindScope {
    pub fn new(content: Element) -> Self {
        Self {
            bar: FindBar::new(),
            content,
            open: false,
            disclosure: crate::motion::Disclosure::closed(),
            motion: crate::tokens::EffectiveMotion::default(),
            laid_sliding: false,
            structure_dirty: false,
            rect: Rect::ZERO,
            pointer: Point::ZERO,
            bar_node: None,
            content_node: None,
            platform: PlatformConventions::default(),
            shortcut: Some(crate::platform::Shortcut::command("F")),
        }
    }

    /// Open the bar with `shortcut` instead of Ctrl+F (⌘F): a terminal,
    /// whose Ctrl+F belongs to the program it runs, uses
    /// `Shortcut::command("F").shift()`.
    #[must_use]
    pub fn with_shortcut(mut self, shortcut: crate::platform::Shortcut) -> Self {
        self.shortcut = Some(shortcut);
        self
    }

    /// No key opens the bar; the host opens it with [`Self::open`] (a menu
    /// command, its own key handling).
    #[must_use]
    pub fn without_shortcut(mut self) -> Self {
        self.shortcut = None;
        self
    }

    /// Line the bar up with the content's edge (see
    /// [`FindBar::with_content_inset`]).
    #[must_use]
    pub fn with_content_inset(mut self, leading: f64, trailing: f64) -> Self {
        self.bar.set_content_inset(leading, trailing);
        self
    }

    /// Configure the bar (`FindBar::with_replace`, `with_hex_mode`).
    #[must_use]
    pub fn with_bar(mut self, bar: FindBar) -> Self {
        self.bar = bar;
        self
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    #[must_use]
    pub fn bar(&self) -> &FindBar {
        &self.bar
    }

    pub fn bar_mut(&mut self) -> &mut FindBar {
        &mut self.bar
    }

    #[must_use]
    pub fn content(&self) -> &dyn Widget {
        self.content.as_ref()
    }

    /// The content the scope wraps, mutably.
    pub fn content_mut(&mut self) -> &mut dyn Widget {
        self.content.as_mut()
    }

    /// Open the bar on `query` and find it, as typing it would: a host's
    /// "Find…" command with a selection, a capture.
    pub fn find(&mut self, query: &str) {
        self.open();
        self.bar.set_query(query);
        self.drive();
    }

    /// Open the bar (as Ctrl+F does) and focus its query.
    pub fn open(&mut self) {
        if !self.open {
            self.open = true;
            self.structure_dirty = true;
            self.disclosure.show(true, self.motion);
        }
        self.bar.focus_query();
    }

    /// Close the bar and clear the matches.
    pub fn close(&mut self) {
        self.bar.request_close();
        self.drive();
    }

    fn drive(&mut self) {
        let bar = &mut self.bar;
        if with_findable(self.content.as_mut(), &mut |target| bar.drive(target)).is_none() {
            return;
        }
        if self.bar.close_requested() {
            self.bar.pending.closed = false;
            if self.open {
                self.open = false;
                self.structure_dirty = true;
                self.disclosure.show(false, self.motion);
            }
        }
    }

    /// How far below its laid-out place the content is drawn now: as far
    /// as the band shows while the layout keeps the content at the top of
    /// the scope. Zero at rest.
    fn content_offset(&self) -> f64 {
        if !self.laid_sliding {
            return 0.0;
        }
        self.disclosure.reveal(self.bar.get_rect().height()).height
    }

    /// The layout no longer matches the band: it started or stopped moving.
    fn layout_stale(&self) -> bool {
        self.laid_sliding != self.disclosure.is_sliding()
    }

    fn is_find_shortcut(&self, event: &WindowEvent, modifiers: winit::event::Modifiers) -> bool {
        let Some(shortcut) = &self.shortcut else {
            return false;
        };
        let state = modifiers.state();
        matches!(
            event,
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    logical_key: Key::Character(c),
                    state: ElementState::Pressed,
                    ..
                },
                ..
            } if c.eq_ignore_ascii_case(shortcut.key_name())
                && self.platform.command_modifier_down(state) == shortcut.has_command()
                && state.shift_key() == shortcut.has_shift()
                && state.alt_key() == shortcut.has_alt()
        )
    }
}

impl Widget for FindScope {
    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.platform = conventions;
        self.bar.set_platform_conventions(conventions);
        self.content.set_platform_conventions(conventions);
    }

    /// Content that changes with no input under an open bar (output
    /// arriving in a terminal) is found again as it syncs, so the counter
    /// follows it (see [`Findable::content_revision`]).
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        for child in self.children_mut() {
            child.sync(state, path);
        }
        if self.open {
            self.drive();
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.motion = theme.motion();
        let mut children = Vec::new();
        // On its way the band stands over the content, out of the layout:
        // the content keeps the whole scope and moves by transform.
        self.laid_sliding = self.disclosure.is_sliding();
        self.bar_node = if self.open || self.laid_sliding {
            let node = self.bar.build_layout(taffy, theme, node_map);
            if self.laid_sliding
                && let Ok(style) = taffy.style(node)
            {
                let mut style = style.clone();
                style.position = taffy::Position::Absolute;
                style.inset = taffy::Rect {
                    left: length(0.0_f32),
                    right: length(0.0_f32),
                    top: length(0.0_f32),
                    bottom: taffy::LengthPercentageAuto::auto(),
                };
                let _ = taffy.set_style(node, style);
            }
            children.push(node);
            Some(node)
        } else {
            None
        };
        let content = self.content.build_layout(taffy, theme, node_map);
        children.push(content);
        self.content_node = Some(content);
        let node = taffy.parent_node(
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                size: taffy::Size::from_percent(1.0_f32, 1.0_f32),
                flex_grow: 1.0,
                ..Default::default()
            },
            &children,
        );
        if let Ok(mut style) = taffy.style(content).cloned() {
            style.flex_grow = 1.0;
            style.flex_shrink = 1.0;
            style.min_size.height = length(0.0_f32);
            let _ = taffy.set_style(content, style);
        }
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        if let Some(bar) = self.bar_node {
            self.bar.apply_layout(taffy, bar, inner);
        }
        if let Some(content) = self.content_node {
            self.content.apply_layout(taffy, content, inner);
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty) || self.layout_stale();
        let content = self.content.take_structure_dirty();
        own || content
    }

    fn is_layout_dirty(&self) -> bool {
        self.structure_dirty || self.layout_stale() || self.content.is_layout_dirty()
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.motion = theme.motion();
        if !self.laid_sliding {
            self.content.draw(scene, transform, text, theme, presence);
            if self.open {
                self.bar.draw(scene, transform, text, theme, presence);
            }
            return;
        }
        // The band opens or closes from the top: the content, laid out once
        // over the whole scope, follows the band's edge by transform,
        // clipped to the scope; the band's controls fade in over the second
        // half.
        let reveal = self.disclosure.reveal(self.bar.get_rect().height());
        let offset = self.content_offset();
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &self.rect,
        );
        self.content.draw(
            scene,
            transform * Affine::translate((0.0, offset)),
            text,
            theme,
            presence,
        );
        let band = self.bar.get_rect();
        if reveal.height > 0.0 && reveal.opacity > 0.0 {
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                reveal.opacity,
                transform,
                &Rect::new(band.x0, band.y0, band.x1, band.y0 + reveal.height),
            );
            self.bar.draw(scene, transform, text, theme, presence);
            scene.pop_layer();
        }
        scene.pop_layer();
    }

    /// Frames while the band opens or closes, and one more to lay it out
    /// at rest; none after.
    fn needs_animation_frame(&self) -> bool {
        self.disclosure.is_moving()
            || self.layout_stale()
            || super::defaults::needs_animation_frame(&|visit| self.for_each_child(visit))
    }

    fn needs_redraw(&self) -> bool {
        self.disclosure.is_moving()
            || self.layout_stale()
            || super::defaults::needs_redraw(&|visit| self.for_each_child(visit))
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.is_find_shortcut(event, *ctx.modifiers)
            && with_findable(self.content.as_mut(), &mut |_| ()).is_some()
        {
            self.open();
            if let Some(bar) = self.bar_node {
                *ctx.focus_target = Some(bar);
            }
            *ctx.handled = true;
            return;
        }
        if self.open {
            // Opened since the last layout, the bar has no node of its own
            // yet; it still takes its keys, under the scope's.
            let bar = self.bar_node.unwrap_or(ctx.node_id);
            // A press in the band gives the query the keyboard; a press in
            // the content hands it back and leaves the bar open.
            if let WindowEvent::CursorMoved { position, .. } = event {
                self.pointer = ctx.logical(position);
            }
            if matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                }
            ) {
                let in_band = self.bar.get_rect().contains(self.pointer);
                if in_band && !self.bar.is_focused() {
                    self.bar.set_focus(true);
                    *ctx.focus_target = Some(bar);
                } else if !in_band && self.bar.is_focused() {
                    self.bar.set_focus(false);
                }
            }
            // While the query has the keyboard, keys are the bar's: one it
            // has no use for does not reach the content behind it (a
            // terminal would type it into its program).
            let typing =
                self.bar.is_focused() && matches!(event, WindowEvent::KeyboardInput { .. });
            self.bar.route(event, &mut ctx.reborrow(ctx.path, bar));
            self.drive();
            if !self.open {
                // Closed (Escape, the close button): the keyboard goes back
                // to the content.
                *ctx.focus_target = self.content_node;
            }
            if *ctx.handled || typing {
                return;
            }
        }
        let content = self.content_node.unwrap_or(ctx.node_id);
        // While the band moves, the content is hit where it is drawn.
        let event = super::event_offset(
            event,
            vello::kurbo::Vec2::new(0.0, self.content_offset()),
            ctx.scale_factor,
        );
        self.content
            .handle_event(&event, &mut ctx.reborrow(ctx.path, content));
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if self.open {
            vec![&self.bar, self.content.as_ref()]
        } else {
            vec![self.content.as_ref()]
        }
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if self.open {
            visitor(&self.bar);
        }
        visitor(self.content.as_ref());
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.open {
            vec![&mut self.bar, self.content.as_mut()]
        } else {
            vec![self.content.as_mut()]
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.content.wants_pointer_capture()
    }

    fn name(&self) -> &'static str {
        "FindScope"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Dispatch;

    /// A match highlight paints the matched span and nothing past it: the
    /// glyph after a match keeps its own ground, the current match's outline
    /// included.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_match_highlight_paints_exactly_the_matched_span() {
        let theme = Theme::dark();
        let (w, h) = (64_u32, 32_u32);
        let span = Rect::new(10.0, 8.0, 40.0, 24.0);
        for current in [false, true] {
            let mut scene = Scene::new();
            paint_find_match(&mut scene, Affine::IDENTITY, span, current, &theme);
            let mut pixels = vec![0_u8; (w * h * 4) as usize];
            {
                let mut target = tilo_cpu_raster::Target::new(&mut pixels, w, h, w as usize * 4)
                    .expect("a target");
                tilo_cpu_raster::CpuRasterizer::default().render(
                    scene.encoding(),
                    &mut target,
                    None,
                    tilo_cpu_raster::color::AlphaColor::from_rgba8(0, 0, 0, 255),
                );
            }
            let painted = |x: u32, y: u32| {
                let at = ((y * w + x) * 4) as usize;
                pixels[at..at + 3].iter().any(|channel| *channel != 0)
            };
            assert!(painted(25, 16), "the span is painted (current: {current})");
            for y in 0..h {
                for x in [9, 40] {
                    assert!(
                        !painted(x, y),
                        "nothing is painted beside the span at ({x}, {y}) (current: {current})"
                    );
                }
            }
            for x in 0..w {
                for y in [7, 24] {
                    assert!(
                        !painted(x, y),
                        "nothing is painted above or below the span at ({x}, {y}) (current: {current})"
                    );
                }
            }
        }
    }

    /// A text a find runs over: case-folding and whole-word aware, keeping
    /// what it found and which match is current.
    #[derive(Default)]
    struct Doc {
        text: String,
        found: Vec<FindMatch>,
        current: Option<usize>,
        replaced: Vec<(usize, String)>,
    }

    impl Findable for Doc {
        fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
            self.found.clear();
            if query.is_empty() {
                return Vec::new();
            }
            let (hay, needle) = if options.match_case {
                (self.text.clone(), query.to_owned())
            } else {
                (self.text.to_lowercase(), query.to_lowercase())
            };
            let mut at = 0;
            while let Some(found) = hay.get(at..).and_then(|rest| rest.find(&needle)) {
                let start = at + found;
                let end = start + needle.len();
                let bounded = |i: Option<char>| i.is_none_or(|c| !c.is_alphanumeric());
                if !options.whole_word
                    || (bounded(hay.get(..start).and_then(|s| s.chars().last()))
                        && bounded(hay.get(end..).and_then(|s| s.chars().next())))
                {
                    self.found.push(FindMatch::new(start, end));
                }
                at = end;
            }
            self.found.clone()
        }
        fn reveal(&mut self, index: usize) {
            self.current = Some(index);
        }
        fn supports_replace(&self) -> bool {
            true
        }
        fn replace(&mut self, index: usize, with: &str) -> bool {
            self.replaced.push((index, with.to_owned()));
            true
        }
    }

    #[test]
    fn the_bar_counts_steps_and_wraps_through_matches() {
        let mut doc = Doc {
            text: "Cat cat concat cat".into(),
            ..Doc::default()
        };
        let mut bar = FindBar::new();
        bar.set_query("cat");
        bar.drive(&mut doc);
        assert_eq!(bar.position(), Some((1, 4)));
        assert_eq!(doc.current, Some(0));
        bar.next();
        bar.drive(&mut doc);
        assert_eq!(bar.position(), Some((2, 4)));
        bar.previous();
        bar.drive(&mut doc);
        bar.previous();
        bar.drive(&mut doc);
        assert_eq!(bar.position(), Some((4, 4)), "previous wraps to the last");
        assert_eq!(bar.field.count(), Some(FilterCount::new(4, 4)));

        bar.toggle(Part::MatchCase);
        bar.drive(&mut doc);
        assert_eq!(bar.position(), Some((1, 3)), "Match case drops `Cat`");
        bar.toggle(Part::WholeWord);
        bar.drive(&mut doc);
        assert_eq!(bar.position(), Some((1, 2)), "Whole word drops `concat`");

        assert!(bar.replace_row, "a replacing findable gets the replace row");
        bar.replace_field.set_query("dog");
        bar.toggle(Part::Replace);
        bar.drive(&mut doc);
        assert_eq!(doc.replaced, [(0, "dog".to_owned())]);

        bar.request_close();
        bar.drive(&mut doc);
        assert!(doc.found.is_empty(), "closing clears the highlights");
        assert_eq!(bar.position(), None);
    }

    struct Page {
        doc: Doc,
        rect: Rect,
    }

    impl Findable for Page {
        fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
            self.doc.find(query, options)
        }
        fn reveal(&mut self, index: usize) {
            self.doc.reveal(index);
        }
        fn supports_replace(&self) -> bool {
            self.doc.supports_replace()
        }
        fn replace(&mut self, index: usize, with: &str) -> bool {
            self.doc.replace(index, with)
        }
    }

    impl Widget for Page {
        crate::provides!(dyn Findable);
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let style = Style {
                size: taffy::Size {
                    width: percent(1.0_f32),
                    height: length(200.0_f32),
                },
                ..Default::default()
            };
            taffy.widget_leaf(style, node_map, self)
        }
        crate::rect_geometry!();
        fn name(&self) -> &'static str {
            "page"
        }
    }

    fn ctrl_f() -> WindowEvent {
        key(Key::Character("f".into()))
    }

    fn escape() -> WindowEvent {
        key(Key::Named(NamedKey::Escape))
    }

    fn key(logical: Key) -> WindowEvent {
        crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
            logical,
            None,
            ElementState::Pressed,
        ))
    }

    fn send(
        scope: &mut FindScope,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
    ) -> Option<NodeId> {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map = HashMap::new();
        let node = scope.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(640.0),
                    height: taffy::AvailableSpace::Definite(400.0),
                },
            )
            .unwrap();
        scope.apply_layout(&taffy, node, Point::ZERO);
        Dispatch::new()
            .modifiers(modifiers)
            .layout(&taffy, node)
            .send(scope, event)
            .focus_target
    }

    #[test]
    fn ctrl_f_opens_the_bar_as_a_band_above_the_content_and_escape_closes_it() {
        let page = Page {
            doc: Doc {
                text: "one two one".into(),
                ..Doc::default()
            },
            rect: Rect::ZERO,
        };
        let mut scope = FindScope::new(Box::new(page));
        scope.set_platform_conventions(PlatformConventions::new(crate::platform::Platform::Linux));
        let ctrl = winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
        let plain = winit::event::Modifiers::default();

        let _ = send(&mut scope, &ctrl_f(), plain);
        assert!(!scope.is_open(), "plain F is typing, not find");
        let _ = send(&mut scope, &ctrl_f(), ctrl);
        assert!(scope.is_open());
        assert!(scope.take_structure_dirty(), "the band joins the layout");
        let focus = send(&mut scope, &WindowEvent::Focused(true), plain);
        assert!(focus.is_none());
        assert!(scope.bar.get_rect().height() > 0.0);
        assert!(
            scope.content.get_rect().y0 >= scope.bar.get_rect().y1 - 0.5,
            "the content starts under the band"
        );
        scope.bar.field.set_query("one");
        scope.drive();
        assert_eq!(scope.bar.position(), Some((1, 2)));

        let _ = send(&mut scope, &escape(), plain);
        assert!(!scope.is_open(), "Escape closes the bar");
    }

    fn page_scope() -> FindScope {
        let page = Page {
            doc: Doc {
                text: "one two one".into(),
                ..Doc::default()
            },
            rect: Rect::ZERO,
        };
        let mut scope = FindScope::new(Box::new(page));
        scope.set_platform_conventions(PlatformConventions::new(crate::platform::Platform::Linux));
        scope
    }

    fn lay_out(scope: &mut FindScope) {
        let _ = send(
            scope,
            &WindowEvent::Focused(true),
            winit::event::Modifiers::default(),
        );
        let _ = scope.take_structure_dirty();
    }

    fn pointer_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    #[test]
    fn opened_from_a_menu_the_band_discloses_over_content_laid_out_at_its_larger_extent() {
        let mut scope = page_scope();
        lay_out(&mut scope);
        // A menu item clicked: the host opens the bar. The frame's instant
        // is held, so the test reads the motion where it starts.
        let _frame = crate::motion::pin_frame();
        crate::motion::note_input(&pointer_press());
        scope.open();
        lay_out(&mut scope);
        assert!(scope.disclosure.is_moving(), "the band discloses");
        assert!(Widget::needs_animation_frame(&scope));
        let band = scope.bar.get_rect();
        let laid = scope.content.get_rect();
        assert!(band.height() > 0.0, "the band is laid out at its height");
        assert!(
            (laid.y0 - scope.rect.y0).abs() < 0.5 && (laid.y1 - scope.rect.y1).abs() < 0.5,
            "the content keeps the whole scope while the band moves, so nothing under it is bare"
        );
        assert!(
            scope.content_offset().abs() < 0.5,
            "and is drawn where it stood, moving down with the band's edge"
        );
        // A frame of the motion moves only the drawing: nothing asks for a
        // new layout and the content's box stays put.
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        scope.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            &PresenceCtx::default(),
        );
        assert!(!scope.structure_dirty);
        assert_eq!(scope.content.get_rect(), laid);

        // Once it rests, one more frame lays the content out under the
        // band; then it costs nothing.
        let _settled = crate::motion::testing::settled();
        assert!(!scope.disclosure.is_moving());
        assert!(
            Widget::needs_animation_frame(&scope) && Widget::is_layout_dirty(&scope),
            "the layout takes the band in once, at rest"
        );
        lay_out(&mut scope);
        let band = scope.bar.get_rect();
        assert!(scope.content.get_rect().y0 >= band.y1 - 0.5);
        assert!(scope.content_offset().abs() < f64::EPSILON);
        assert!(!Widget::needs_animation_frame(&scope));
        assert!(!scope.layout_stale(), "and lays out nothing more");
    }

    #[test]
    fn ctrl_f_and_escape_land_and_the_close_button_collapses_the_band() {
        let mut scope = page_scope();
        let ctrl = winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
        let plain = winit::event::Modifiers::default();
        lay_out(&mut scope);

        crate::motion::note_input(&ctrl_f());
        let _ = send(&mut scope, &ctrl_f(), ctrl);
        assert!(scope.is_open());
        assert!(!scope.disclosure.is_moving(), "Ctrl+F lands at once");
        lay_out(&mut scope);
        assert!(scope.content_offset().abs() < f64::EPSILON);

        crate::motion::note_input(&escape());
        let _ = send(&mut scope, &escape(), plain);
        assert!(!scope.is_open());
        assert!(!scope.disclosure.is_moving(), "Escape lands at once");

        crate::motion::note_input(&ctrl_f());
        let _ = send(&mut scope, &ctrl_f(), ctrl);
        lay_out(&mut scope);
        let full = scope.bar.get_rect().height();
        // The close button, pressed.
        let _frame = crate::motion::pin_frame();
        crate::motion::note_input(&pointer_press());
        scope.bar.toggle(Part::Close);
        scope.drive();
        assert!(!scope.is_open(), "the bar is closed at once for input");
        assert!(scope.disclosure.is_moving(), "and its band collapses");
        lay_out(&mut scope);
        assert!(
            (scope.content_offset() - full).abs() < 0.5,
            "the content, laid out at the top, is drawn under the closing band"
        );
        assert!(
            scope.content.get_rect().y0 - scope.rect.y0 < 0.5,
            "laid out once over the whole scope"
        );
        assert!(Widget::needs_animation_frame(&scope));
    }

    #[test]
    fn under_reduced_motion_the_band_lands_and_its_controls_fade_in() {
        let mut scope = page_scope();
        lay_out(&mut scope);
        scope.motion = Theme::dark().with_reduce_motion(true).motion();
        let _frame = crate::motion::pin_frame();
        crate::motion::note_input(&pointer_press());
        scope.open();
        lay_out(&mut scope);
        let full = scope.bar.get_rect().height();
        let reveal = scope.disclosure.reveal(full);
        assert!(
            (reveal.height - full).abs() < f64::EPSILON,
            "the height lands"
        );
        assert!(reveal.opacity < 1.0, "the controls fade in");
        assert!(scope.content_offset().abs() < f64::EPSILON, "nothing moves");
    }

    #[test]
    fn content_moving_with_the_band_is_hit_where_it_is_drawn() {
        let pointer = WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: (10.0, 50.0).into(),
        };
        let moved = super::super::event_offset(&pointer, vello::kurbo::Vec2::new(0.0, -20.0), 2.0);
        let WindowEvent::CursorMoved { position, .. } = moved.as_ref() else {
            panic!("a pointer move stays a pointer move");
        };
        assert_eq!((position.x, position.y), (10.0, 90.0));
        let key = escape();
        assert!(matches!(
            super::super::event_offset(&key, vello::kurbo::Vec2::new(0.0, -20.0), 2.0),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn hosted_accessibility_edits_the_real_find_query_and_replacement() {
        let page = Page {
            doc: Doc {
                text: "one two one".into(),
                ..Doc::default()
            },
            rect: Rect::ZERO,
        };
        let mut scope = FindScope::new(Box::new(page)).with_bar(FindBar::new().with_replace());
        scope.open();
        let mut harness = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            scope,
            (900, 400),
            Theme::dark(),
        );
        let field = |harness: &crate::headless::FrameHarness, name: &str| {
            harness
                .a11y_nodes()
                .into_iter()
                .find(|(_, node)| {
                    node.role() == accesskit::Role::TextInput && node.label() == Some(name)
                })
                .map(|(id, _)| id)
                .unwrap_or_else(|| panic!("missing exported find field {name}"))
        };
        for name in [
            tr("find-bar-placeholder"),
            tr("find-bar-replace-placeholder"),
        ] {
            assert_eq!(
                harness
                    .a11y_nodes()
                    .into_iter()
                    .filter(|(_, node)| {
                        node.role() == accesskit::Role::TextInput
                            && node.label() == Some(name.as_str())
                    })
                    .count(),
                1,
                "each actual find field has one exported accessibility node"
            );
        }
        let query = field(&harness, &tr("find-bar-placeholder"));
        harness
            .perform_a11y_action(query, &crate::a11y::Action::SetValue("one".into()))
            .expect("the exported query enters the real find scope");
        assert_eq!(harness.root::<FindScope>().bar.query(), "one");
        assert_eq!(harness.root::<FindScope>().bar.position(), Some((1, 2)));
        let query = field(&harness, &tr("find-bar-placeholder"));
        harness
            .perform_a11y_action(query, &crate::a11y::Action::Focus)
            .expect("the exported query can take keyboard focus");
        let query = field(&harness, &tr("find-bar-placeholder"));
        harness
            .perform_a11y_action(
                query,
                &crate::a11y::Action::SetSelection {
                    anchor: 0,
                    focus: 3,
                },
            )
            .unwrap();
        harness.type_text("two");
        assert_eq!(harness.root::<FindScope>().bar.query(), "two");
        assert_eq!(harness.root::<FindScope>().bar.position(), Some((1, 1)));
        let replacement = field(&harness, &tr("find-bar-replace-placeholder"));
        harness
            .perform_a11y_action(replacement, &crate::a11y::Action::SetValue("three".into()))
            .unwrap();
        assert_eq!(
            harness.root::<FindScope>().bar.replace_field.value(),
            "three"
        );
        assert_eq!(harness.root::<FindScope>().bar.query(), "two");
        let query = field(&harness, &tr("find-bar-placeholder"));
        assert!(
            harness
                .perform_a11y_action(query, &crate::a11y::Action::ToggleCheck)
                .is_err()
        );
        assert_eq!(harness.root::<FindScope>().bar.query(), "two");
    }

    #[test]
    fn semantic_tree_publishes_each_find_field_once_under_its_real_owner() {
        let mut scope = FindScope::new(Box::new(Page {
            doc: Doc {
                text: "one two one".into(),
                ..Doc::default()
            },
            rect: Rect::ZERO,
        }))
        .with_bar(FindBar::new().with_replace());
        scope.open();
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map = HashMap::new();
        let node = scope.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(900.0),
                    height: taffy::AvailableSpace::Definite(400.0),
                },
            )
            .unwrap();
        scope.apply_layout(&taffy, node, Point::ZERO);
        let mut tree = crate::a11y_tree::SemanticTree::new();
        tree.refresh(&scope, &node_map, None, &[]);
        for (name, target) in [
            (tr("find-bar-placeholder"), "find-query"),
            (tr("find-bar-replace-placeholder"), "find-replacement"),
        ] {
            let fields: Vec<_> = tree
                .iter()
                .filter(|node| {
                    node.role == crate::a11y::Role::TextInput
                        && node.name.as_deref() == Some(name.as_str())
                })
                .collect();
            assert_eq!(fields.len(), 1, "one canonical semantic field for {name}");
            assert_eq!(fields[0].semantic_target.as_deref(), Some(target));
            assert!(fields[0].widget_taffy_id.is_some(), "real admitted owner");
        }
    }

    #[test]
    fn a_match_highlight_marks_only_its_span() {
        let theme = Theme::dark();
        let mut scene = Scene::new();
        paint_find_match(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(10.0, 0.0, 30.0, 14.0),
            true,
            &theme,
        );
        paint_find_match(&mut scene, Affine::IDENTITY, Rect::ZERO, false, &theme);
        assert!(!scene.encoding().is_empty());
    }
}

#[cfg(test)]
mod host_driven_tests {
    use super::*;
    use crate::testing::Dispatch;

    /// Lines that grow under an open bar, found case-insensitively.
    #[derive(Default)]
    struct Log {
        text: String,
        current: Option<usize>,
        regex_offered: bool,
    }

    impl Findable for Log {
        fn find(&mut self, query: &str, _options: FindOptions) -> Vec<FindMatch> {
            if query.is_empty() {
                return Vec::new();
            }
            self.text
                .match_indices(query)
                .map(|(i, m)| FindMatch::new(i, i + m.len()))
                .collect()
        }
        fn reveal(&mut self, index: usize) {
            self.current = Some(index);
        }
        fn supports_regex(&self) -> bool {
            self.regex_offered
        }
    }

    /// A web page: its engine finds and steps on its own.
    #[derive(Default)]
    struct Engine {
        steps: Vec<bool>,
        queries: Vec<String>,
        /// What the engine said since the bar last asked.
        reported: Option<(usize, Option<usize>)>,
    }

    impl Findable for Engine {
        fn find(&mut self, query: &str, _options: FindOptions) -> Vec<FindMatch> {
            self.queries.push(query.to_owned());
            Vec::new()
        }
        fn reveal(&mut self, _index: usize) {}
        fn step(&mut self, forward: bool) -> bool {
            self.steps.push(forward);
            true
        }
        fn take_engine_results(&mut self) -> Option<(usize, Option<usize>)> {
            self.reported.take()
        }
    }

    #[test]
    fn refresh_keeps_the_current_match_as_content_grows() {
        let mut log = Log {
            text: "error error".into(),
            ..Log::default()
        };
        let mut bar = FindBar::new();
        bar.set_query("error");
        bar.drive(&mut log);
        bar.next();
        bar.drive(&mut log);
        assert_eq!(bar.position(), Some((2, 2)));
        log.text.push_str(" error");
        bar.refresh();
        bar.drive(&mut log);
        assert_eq!(
            bar.position(),
            Some((2, 3)),
            "the count follows; the place stays"
        );
    }

    /// An editable buffer that reports a revision for every edit.
    struct Buffer(Log, u64);

    impl Findable for Buffer {
        fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
            self.0.find(query, options)
        }
        fn reveal(&mut self, index: usize) {
            self.0.reveal(index);
        }
        fn content_revision(&self) -> u64 {
            self.1
        }
    }

    #[test]
    fn an_edit_under_an_open_bar_updates_the_counter_on_the_next_drive() {
        let mut buffer = Buffer(
            Log {
                text: "fn fn".into(),
                ..Log::default()
            },
            1,
        );
        let mut bar = FindBar::new();
        bar.set_query("fn");
        bar.drive(&mut buffer);
        bar.next();
        bar.drive(&mut buffer);
        assert_eq!(bar.position(), Some((2, 2)));
        buffer.0.text.push_str(" fn");
        buffer.1 += 1;
        bar.drive(&mut buffer);
        assert_eq!(
            bar.position(),
            Some((2, 3)),
            "the count follows the edit without a refresh; the place stays"
        );
        assert_eq!(buffer.0.current, Some(1), "the view is not moved");
    }

    #[test]
    fn a_close_request_is_acknowledged_once_and_the_bar_reopens() {
        let mut log = Log {
            text: "abc".into(),
            ..Log::default()
        };
        let mut bar = FindBar::new().with_mode(FindMode::Hex).with_hex_mode();
        assert_eq!(
            bar.options().mode,
            FindMode::Hex,
            "a byte view opens on hex"
        );
        bar.request_close();
        bar.drive(&mut log);
        assert!(bar.take_close_requested());
        assert!(!bar.take_close_requested(), "acknowledged once");
        assert!(!bar.close_requested());
        bar.set_options(FindOptions {
            match_case: true,
            ..bar.options()
        });
        assert!(bar.options().match_case);
        assert_eq!(bar.options().mode, FindMode::Hex);
    }

    #[test]
    fn an_engine_reports_its_own_results_and_steps() {
        let mut page = Engine::default();
        let mut bar = FindBar::new();
        bar.set_query("tilo");
        bar.drive(&mut page);
        assert_eq!(page.queries, ["tilo"]);
        bar.set_results(7, Some(2));
        assert_eq!(bar.position(), Some((3, 7)));
        bar.next();
        bar.drive(&mut page);
        assert_eq!(page.steps, [true], "the engine steps");
        assert_eq!(
            bar.position(),
            Some((3, 7)),
            "until it reports where it landed"
        );
        bar.set_results(7, Some(3));
        assert_eq!(bar.position(), Some((4, 7)));
    }

    #[test]
    fn an_engine_says_where_it_landed_through_the_view() {
        let mut page = Engine::default();
        let mut bar = FindBar::new();
        bar.set_query("tilo");
        bar.drive(&mut page);
        assert_eq!(bar.position(), None, "nothing known until the engine says");
        page.reported = Some((4, Some(0)));
        bar.drive(&mut page);
        assert_eq!(bar.position(), Some((1, 4)));
        bar.next();
        page.reported = Some((4, Some(1)));
        bar.drive(&mut page);
        assert_eq!(page.steps, [true]);
        assert_eq!(bar.position(), Some((2, 4)));
        bar.drive(&mut page);
        assert_eq!(
            bar.position(),
            Some((2, 4)),
            "a quiet engine changes nothing"
        );
    }

    #[test]
    fn options_are_toggles_and_a_strip_and_the_bar_has_one_close_control() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut log = Log {
            regex_offered: true,
            ..Log::default()
        };
        let mut bar = FindBar::new()
            .with_hex_mode()
            .with_content_inset(40.0, 40.0);
        bar.drive(&mut log);
        bar.layout(
            Rect::new(0.0, 0.0, 900.0, bar.height(&theme)),
            &theme,
            &mut text,
        );
        let parts: Vec<Part> = bar.parts.iter().map(|(p, _)| *p).collect();
        assert!(
            parts.contains(&Part::Regex),
            "a findable that matches patterns offers it"
        );
        assert_eq!(parts.iter().filter(|p| **p == Part::Close).count(), 1);
        bar.field.set_query("needle");
        bar.paint(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::default(),
        );
        assert_eq!(
            bar.field.clear_rect(),
            Rect::ZERO,
            "the field has no X of its own"
        );
        assert!((bar.field.get_rect().x0 - 40.0).abs() < 1e-9);
        let text_rect = bar.parts.iter().find(|(p, _)| *p == Part::Text).unwrap().1;
        let hex_rect = bar.parts.iter().find(|(p, _)| *p == Part::Hex).unwrap().1;
        assert!(
            (text_rect.x1 - hex_rect.x0).abs() < 1.5,
            "adjacent segments"
        );
        bar.toggle(Part::Regex);
        assert_eq!(bar.options().mode, FindMode::Regex);
        let node = Widget::accessibility(&bar).unwrap();
        assert!(
            node.children
                .iter()
                .any(|c| c.name.as_deref() == Some("Regular expression")
                    && c.role == crate::a11y::Role::ToggleButton),
            "named from the toolkit's catalogue"
        );
        bar.focus_replace();
        assert!(bar.replace_row && bar.replace_focused);
    }

    #[test]
    fn a_scope_opens_on_its_own_shortcut_or_only_when_asked() {
        struct Page(Log);
        impl Findable for Page {
            fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
                self.0.find(query, options)
            }
            fn reveal(&mut self, index: usize) {
                self.0.reveal(index);
            }
        }
        impl Widget for Page {
            crate::provides!(dyn Findable);
            fn build_layout(
                &mut self,
                taffy: &mut TaffyTree<()>,
                _theme: &Theme,
                node_map: &mut HashMap<NodeId, *mut dyn Widget>,
            ) -> NodeId {
                taffy.widget_leaf(Style::default(), node_map, self)
            }
            fn name(&self) -> &'static str {
                "Page"
            }
        }
        let press = |scope: &mut FindScope, modifiers: winit::keyboard::ModifiersState| {
            let ev = crate::synthetic_input::key_event(
                Key::Character("f".into()),
                None,
                ElementState::Pressed,
            );
            let event = crate::synthetic_input::keyboard_input(ev);
            Dispatch::new().modifiers(modifiers).send(scope, &event);
        };
        use winit::keyboard::ModifiersState;
        let mut terminal = FindScope::new(Box::new(Page(Log::default())))
            .with_shortcut(crate::platform::Shortcut::command("F").shift());
        terminal
            .set_platform_conventions(PlatformConventions::new(crate::platform::Platform::Linux));
        press(&mut terminal, ModifiersState::CONTROL);
        assert!(!terminal.is_open(), "Ctrl+F stays with the program");
        press(
            &mut terminal,
            ModifiersState::CONTROL | ModifiersState::SHIFT,
        );
        assert!(terminal.is_open(), "its own chord opens find");

        let mut quiet = FindScope::new(Box::new(Page(Log::default()))).without_shortcut();
        quiet.set_platform_conventions(PlatformConventions::new(crate::platform::Platform::Linux));
        press(&mut quiet, ModifiersState::CONTROL);
        assert!(!quiet.is_open());
        quiet.open();
        assert!(quiet.is_open(), "the host opens it");
    }

    /// A document whose host writes a replacement on its next frame, as the
    /// block editor's does; it forgets its current match on every find.
    #[derive(Default)]
    struct Deferred {
        text: String,
        found: Vec<FindMatch>,
        queued: Option<(usize, String)>,
        revision: u64,
        current: Option<usize>,
        /// Each match scrolled into view, in order.
        revealed: Vec<usize>,
    }

    impl Deferred {
        /// The host's frame: write the queued replacement.
        fn write(&mut self) {
            if let Some((index, with)) = self.queued.take() {
                let found = self.found[index].clone();
                self.text.replace_range(found.start..found.end, &with);
                self.revision += 1;
            }
        }
    }

    impl Findable for Deferred {
        fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
            self.current = None;
            self.found = Log {
                text: self.text.clone(),
                ..Log::default()
            }
            .find(query, options);
            self.found.clone()
        }
        fn reveal(&mut self, index: usize) {
            self.current = Some(index);
            self.revealed.push(index);
        }
        fn mark_current(&mut self, index: usize) {
            self.current = Some(index);
        }
        fn supports_replace(&self) -> bool {
            true
        }
        fn replace(&mut self, index: usize, with: &str) -> bool {
            self.queued = Some((index, with.to_owned()));
            true
        }
        fn replaces_later(&self) -> bool {
            true
        }
        fn content_revision(&self) -> u64 {
            self.revision
        }
    }

    #[test]
    fn after_a_replacement_written_later_the_next_match_is_current_and_shown() {
        let mut doc = Deferred {
            text: "a1 a2 a3".into(),
            ..Deferred::default()
        };
        let options = FindOptions::default();
        let mut session = FindSession::default();
        session.search(&mut doc, "a", options, Some(0));
        session.step(&mut doc, true);
        assert_eq!(session.position(), Some((2, 3)));
        assert!(session.replace(&mut doc, "a", options, "b", false));
        doc.write();
        assert!(session.content_moved(&doc));
        session.refresh(&mut doc, "a", options);
        assert_eq!(doc.text, "a1 b2 a3");
        assert_eq!(session.position(), Some((2, 2)), "a3 is current");
        assert_eq!(doc.current, Some(1), "the document paints it as current");
        assert_eq!(doc.revealed.last(), Some(&1), "and scrolls it into view");
    }

    #[test]
    fn an_edit_from_elsewhere_keeps_the_current_match_painted_without_scrolling() {
        let mut doc = Deferred {
            text: "a1 a2".into(),
            ..Deferred::default()
        };
        let options = FindOptions::default();
        let mut session = FindSession::default();
        session.search(&mut doc, "a", options, Some(0));
        session.step(&mut doc, true);
        let shown = doc.revealed.len();
        doc.text.push_str(" a3");
        doc.revision += 1;
        session.refresh(&mut doc, "a", options);
        assert_eq!(session.position(), Some((2, 3)));
        assert_eq!(doc.current, Some(1), "the current match stays emphasised");
        assert_eq!(doc.revealed.len(), shown, "the view stays where it was");
    }
}
