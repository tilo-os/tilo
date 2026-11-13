//! Alt+Tab: the window switcher's panel.
//!
//! Holding Alt and pressing Tab walks the workspace's windows in the order
//! they were last used, and letting Alt go goes to the chosen one: the walk
//! itself is the desk's (the session's overlays), which knows the windows and
//! their order. This module is the panel that walk draws: which card it lifts,
//! where the cards stand, and what the panel says to a screen reader.
//!
//! A "window" is a tab, as it is for every other window traversal on the desk:
//! groups are frames around windows, not windows themselves.
//!
//! The chrome draws the panel and the desk's reading places the cards; the
//! windows' own pixels stay the compositor's. Each card names the window it
//! stands for, and the compositor draws that window's latest buffer over the
//! card's well, in the same frame ([`Switcher::pictures`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use ui::num::f32_of_f64;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;

use ui::a11y::{AccessNode, AccessState, Role};
use ui::i18n::ArgValue;
use ui::icons::{AppImage, Icon};
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::AppKind as UiAppKind;
use ui::widget::{KeyHint, KeyHintsFace};

use tilo_desktop_model::{GroupId, StripId, TileContent};

use crate::i18n::{t, t_args};
use ui::TextRole;

/// Where a window's preview comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewSource {
    /// A client's own latest buffer, from the host's frame store. Works for every tab of a client,
    /// shown or not.
    Client { surface: u64 },
    /// A window the host draws itself, painted through the same live path a screenshot of it takes.
    /// Only the tab its group shows is painted.
    Painted { capture_id: String },
    /// Never pictured: the app holds secrets.
    Withheld,
    /// Nothing worth picturing: a hidden tab of a host-drawn app, a window that is still a
    /// placeholder card, or a host that pictures nothing.
    Unavailable,
}

impl PreviewSource {
    /// Whether this source can produce pixels at all.
    pub fn renderable(&self) -> bool {
        matches!(self, Self::Client { .. } | Self::Painted { .. })
    }
}

/// What only the host knows about a window: its application's artwork and whether (and from where)
/// it may be pictured. The defaults suit a host with neither.
pub trait WindowSource {
    /// The glyph shown for `tile` when its application has no artwork.
    fn app_kind(&self, tile: &TileContent) -> UiAppKind {
        tilo_desktop_surface::tile_app_kind(tile)
    }

    /// The application's own artwork for `tile` .
    fn app_icon(&self, _tile: &TileContent) -> Option<AppImage> {
        None
    }

    /// Where a preview of `tile` , the tab `tab` of group `group` , comes from. `shown` is whether
    /// that tab is the one its group displays.
    fn preview_source(
        &self,
        _tile: &TileContent,
        _group: GroupId,
        _tab: u64,
        _shown: bool,
    ) -> PreviewSource {
        PreviewSource::Unavailable
    }
}

/// A host that knows nothing beyond the desktop tree.
pub struct PlainWindows;

impl WindowSource for PlainWindows {}

/// Where a window lives, which is everything focusing it needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowAddress {
    pub strip: StripId,
    pub group: GroupId,
    pub tab: u64,
}

/// Which application a window belongs to, and how to draw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowApp {
    /// Two windows of one application share this key: the Wayland app id for a client, the app kind
    /// for one of Tilo's own.
    pub key: String,
    /// The glyph shown when the application has no artwork of its own.
    pub kind: UiAppKind,
    pub icon: Option<AppImage>,
    /// The toolkit's own symbol for the application, the one its launcher entry wears: what a
    /// window picture shows when there is no live picture and no artwork. [`Icon::AppWindow`] for
    /// an app it has no symbol for, which then shows [`Self::kind`]'s mark.
    pub glyph: Icon,
}

impl WindowApp {
    pub fn of(tile: &TileContent, source: &dyn WindowSource) -> Self {
        let kind = source.app_kind(tile);
        match tile {
            TileContent::Foreign { app_id, .. }
            | TileContent::LocalApp { app_id, .. }
            | TileContent::HostedWindow { app_id, .. } => Self {
                key: format!("app:{app_id}"),
                kind,
                icon: source.app_icon(tile),
                glyph: ui::icons::app_glyph(app_id, None),
            },
            TileContent::AppPlaceholder { kind: app, .. } | TileContent::Stub { kind: app, .. } => {
                Self {
                    key: format!("kind:{}", app.as_str()),
                    kind,
                    icon: None,
                    glyph: Icon::AppWindow,
                }
            }
        }
    }

    /// A window as the desk's reading names it: the app it shows, and the symbol that app wears
    /// where a window has no picture.
    #[must_use]
    pub fn named(app_key: &str) -> Self {
        let glyph = ui::icons::app_glyph(app_key, None);
        Self {
            key: app_key.to_owned(),
            kind: UiAppKind::App(glyph),
            icon: None,
            glyph,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwitcherWindow {
    pub address: WindowAddress,
    pub title: String,
    pub app: WindowApp,
    /// Where its live preview comes from, if it may have one.
    pub preview: PreviewSource,
}

/// The shape a card takes when the desk knows nothing of its window's: the desk's own 16:10.
const STANDING_ASPECT: f64 = 16.0 / 10.0;

/// The shortest a preview well goes, however small the display: a card still shows what it holds.
const LEAST_WELL: f64 = 24.0;

/// The tallest a preview goes, as a share of the display it stands on: previews grow with the
/// screen rather than staying thumbnails on a large one.
const CARD_HEIGHT_SHARE: f64 = 0.42;

/// The widest one card goes, as a share of the display: a window far wider than its row-mates
/// leaves the rest of the display in view beside it.
const CARD_WIDTH_SHARE: f64 = 0.6;

/// One card of the panel: a window, the name over it, and the app whose face stands in its well
/// until the compositor draws the window there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwitcherCard {
    /// The compositor's window, when its live picture stands in the well.
    pub window: Option<u64>,
    /// The window's size in logical pixels, as the compositor lays it out: the card takes the
    /// window's own shape, so its picture fills it. `None` where there is no window to measure.
    pub size: Option<(u32, u32)>,
    pub title: String,
    /// The app the window shows, as the desk's reading names it.
    pub app_key: String,
}

impl SwitcherCard {
    /// The window's width over its height, or the standing shape of a card with no window to
    /// measure.
    fn aspect(&self) -> f64 {
        self.size
            .filter(|(width, height)| *width > 0 && *height > 0)
            .map_or(STANDING_ASPECT, |(width, height)| {
                f64::from(width) / f64::from(height)
            })
    }
}

/// What the panel shows: the windows the walk crosses, in its order, the one it has reached, and
/// whether it crosses the app in front's windows alone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SwitcherView {
    pub cards: Vec<SwitcherCard>,
    /// Which card the walk reached, an index into `cards` .
    pub selected: usize,
    /// The walk crosses the front app's windows (Alt+Grave) rather than the workspace's.
    pub app_scope: bool,
}

/// What the pointer asks of the walk while the switcher's panel is up.
pub use tilo_desktop_surface::host::contract::SwitcherAsk;

/// Sizes of the panel, all read off the theme's tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Metrics {
    /// The app mark standing in for a window without a preview.
    icon: f64,
    /// The app mark beside a previewed window's title.
    mark: f64,
    /// The height a card's well measures: the standing card's at the theme's own size, and the
    /// page's shared one once the layout has fitted it to the display.
    well_h: f64,
    well_radius: f64,
    inset: f64,
    gap: f64,
    pad: f64,
    margin: f64,
    radius: f64,
    card_radius: f64,
    /// The title line under a card's well.
    title_h: f64,
    /// The height of the keys drawn under the cards.
    footer_h: f64,
}

impl Metrics {
    fn from_theme(theme: &Theme) -> Self {
        let tokens = &theme.tokens;
        let spacing = &tokens.spacing;
        let line = |role| f64::from(tokens.typography.style(role).line_height_px());
        let inset = f64::from(spacing.s2);
        // The standing card is the one a window with no shape of its own gets: the same size the
        // panel has always shown.
        let well_w = f64::from(spacing.s16) * 2.5;
        let well_h = well_w * 10.0 / 16.0;
        Self {
            icon: f64::from(spacing.s12),
            mark: f64::from(tokens.metrics.icon_sm),
            well_h,
            well_radius: f64::from(tokens.radii.control),
            inset,
            gap: f64::from(spacing.s2),
            pad: f64::from(spacing.s4),
            margin: f64::from(spacing.s8),
            radius: f64::from(tokens.radii.panel),
            card_radius: f64::from(tokens.radii.control),
            title_h: line(TextRole::Label),
            footer_h: KeyHintsFace::height(theme).max(line(TextRole::Subheading)),
        }
    }

    /// What a card spends on its insets and its title line, beside its well.
    fn text_part(self) -> f64 {
        3.0 * self.inset + self.title_h
    }

    /// The tallest a well goes on this display: a share of the display, and never under the
    /// standing card.
    fn box_h(self, host: Rect) -> f64 {
        (host.height() * CARD_HEIGHT_SHARE).max(self.well_h)
    }

    /// The widest a card goes on this display, its insets included: a share of the display, and
    /// never under the standing card.
    fn box_w(self, host: Rect) -> f64 {
        let standing = self.well_h * STANDING_ASPECT + 2.0 * self.inset;
        (host.width() * CARD_WIDTH_SHARE).max(standing)
    }

    /// What the panel spends under its cards: a gap, then the keys.
    fn footer_band(self) -> f64 {
        self.inset + self.footer_h
    }

    /// These metrics with the outer margin given up to the card inset: what a display too small for
    /// the margin keeps of the panel.
    fn tightened(self) -> Self {
        Self {
            margin: self.margin.min(self.inset),
            ..self
        }
    }
}

/// Where everything of the panel sits.
#[derive(Clone, Debug, Default, PartialEq)]
struct Geometry {
    panel: Rect,
    /// `(card index, card)` for the page on screen.
    cards: Vec<(usize, Rect)>,
    /// The row under the cards: the keys, and the page's place when it pages.
    footer: Rect,
    /// Whether the windows outnumber the cards on the page.
    paged: bool,
    /// The metrics the cards were laid out at, fitted to the host.
    metrics: Metrics,
}

fn usize_to_f64(n: usize) -> f64 {
    u32::try_from(n).map_or_else(|_| f64::from(u32::MAX), f64::from)
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a non-negative count of cards that fit on a display, floored"
)]
fn fit_count(span: f64, pitch: f64) -> usize {
    if pitch <= 0.0 || !span.is_finite() {
        return 1;
    }
    ((span / pitch).floor().max(1.0)) as usize
}

/// Whether `inner` stands inside `outer` .
fn within(outer: Rect, inner: Rect) -> bool {
    inner.x0 >= outer.x0 - 1e-9
        && inner.y0 >= outer.y0 - 1e-9
        && inner.x1 <= outer.x1 + 1e-9
        && inner.y1 <= outer.y1 + 1e-9
}

/// Cards in rows, centred on the display, each the shape of its own window at the height the page
/// shares, with the selected window's page on screen when there are more windows than fit.
fn geometry(host: Rect, view: &SwitcherView, m: Metrics) -> Geometry {
    if view.cards.is_empty() {
        return Geometry::default();
    }
    let whole = geometry_with(host, view, m);
    if within(host, whole.panel) {
        return whole;
    }
    geometry_with(host, view, m.tightened())
}

/// How the cards sit at one well height: their widths, the rows they wrap into,
/// and how many of those rows the page holds.
struct Layout {
    widths: Vec<f64>,
    rows: Vec<Vec<usize>>,
    card_h: f64,
    /// How many rows stand between the panel's pads and its keys.
    fit: usize,
}

impl Layout {
    /// Whether one page stands every card.
    fn all_stand(&self) -> bool {
        self.rows.len() <= self.fit
    }
}

/// The cards at `well_h` : each the shape of its own window, all sharing one height, wrapped onto
/// as many rows as `room_w` takes, with `room_h` standing as many of those rows as fit.
fn rows_at(aspects: &[f64], well_h: f64, m: Metrics, room_w: f64, room_h: f64) -> Layout {
    let widths: Vec<f64> = aspects
        .iter()
        .map(|aspect| well_h * aspect + 2.0 * m.inset)
        .collect();
    let card_h = well_h + m.text_part();
    // Rows take every card that fits on their line, in the walk's order.
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut row: Vec<usize> = Vec::new();
    let mut row_w = 0.0;
    for (at, width) in widths.iter().copied().enumerate() {
        if !row.is_empty() && row_w + m.gap + width > room_w + 1e-6 {
            rows.push(std::mem::take(&mut row));
            row_w = 0.0;
        }
        row_w = if row.is_empty() {
            width
        } else {
            row_w + m.gap + width
        };
        row.push(at);
    }
    if !row.is_empty() {
        rows.push(row);
    }
    let pitch_y = card_h + m.gap;
    Layout {
        widths,
        rows,
        card_h,
        fit: fit_count(room_h + m.gap, pitch_y).max(1),
    }
}

/// The tallest well that stands every card on one line of `room_w` .
fn one_line(aspects: &[f64], m: Metrics, room_w: f64) -> f64 {
    let shapes: f64 = aspects.iter().sum();
    if shapes <= 0.0 {
        return f64::INFINITY;
    }
    let spent = usize_to_f64(aspects.len()) * 2.0 * m.inset
        + m.gap * usize_to_f64(aspects.len().saturating_sub(1));
    ((room_w - spent) / shapes).max(0.0)
}

/// The tallest well that still stands every card on the page, found by halving: smaller cards wrap
/// onto fewer rows and leave more of those rows on the page. `least` when even the least well
/// pages.
fn tallest_standing_on_page(
    aspects: &[f64],
    m: Metrics,
    room_w: f64,
    room_h: f64,
    least: f64,
    tallest: f64,
) -> f64 {
    if !rows_at(aspects, least, m, room_w, room_h).all_stand() {
        return least;
    }
    let (mut fits, mut over) = (least, tallest);
    for _ in 0..24 {
        let mid = f64::midpoint(fits, over);
        if rows_at(aspects, mid, m, room_w, room_h).all_stand() {
            fits = mid;
        } else {
            over = mid;
        }
    }
    fits
}

fn geometry_with(host: Rect, view: &SwitcherView, m: Metrics) -> Geometry {
    let count = view.cards.len();
    let aspects: Vec<f64> = view.cards.iter().map(SwitcherCard::aspect).collect();
    let widest = aspects.iter().copied().fold(STANDING_ASPECT, f64::max);
    let room_w = (host.width() - 2.0 * (m.margin + m.pad)).max(0.0);
    let room_h = (host.height() - 2.0 * (m.margin + m.pad) - m.footer_band()).max(0.0);
    // Every card shares one height, so the row reads as a row: as tall as the display's box allows,
    // and low enough that the widest window still fits the box's width. No card is wider than its
    // window's shape asks for, so the window's picture fills it rather than letterboxing in it.
    let usable_h = (room_h - m.text_part()).max(0.0);
    let widest_h = ((room_w.min(m.box_w(host)) - 2.0 * m.inset).max(0.0)) / widest;
    let tallest = usable_h.min(m.box_h(host)).min(widest_h);
    // The least a card goes: the standing card the panel has always shown, or as much as a display
    // too small for it allows. Cards never shrink under it to fit more on a page; the walk turns
    // the page instead.
    let least = m.well_h.min(tallest).max(LEAST_WELL.min(tallest));
    // A handful of windows stands on one line, as tall as that line allows, so the walk reads at a
    // glance. More windows than a line of standing cards holds give way to rows: as tall as stand
    // every one of them on the page.
    let line_h = one_line(&aspects, m, room_w).min(tallest);
    let well_h = if line_h >= least {
        line_h
    } else {
        tallest_standing_on_page(&aspects, m, room_w, room_h, least, tallest)
    };
    let Layout {
        widths,
        rows,
        card_h,
        fit: rows_fit,
    } = rows_at(&aspects, well_h, m, room_w, room_h);
    let row_width = |row: &[usize]| {
        let gaps = m.gap * usize_to_f64(row.len().saturating_sub(1));
        row.iter()
            .map(|at| widths.get(*at).copied().unwrap_or(0.0))
            .sum::<f64>()
            + gaps
    };
    let pitch_y = card_h + m.gap;
    let selected_row = rows
        .iter()
        .position(|row| row.contains(&view.selected))
        .unwrap_or(0);
    let first_row = selected_row
        .checked_div(rows_fit)
        .unwrap_or(0)
        .saturating_mul(rows_fit);
    let shown_rows = rows
        .get(first_row..(first_row.saturating_add(rows_fit)).min(rows.len()))
        .unwrap_or_default();
    let shown = shown_rows.iter().map(Vec::len).sum::<usize>();
    let grid_w = shown_rows
        .iter()
        .map(|row| row_width(row))
        .fold(0.0, f64::max);
    let grid_h = usize_to_f64(shown_rows.len()) * pitch_y - m.gap;
    let panel_w = grid_w + 2.0 * m.pad;
    let panel_h = grid_h + m.footer_band() + 2.0 * m.pad;
    let center = host.center();
    let panel = Rect::new(
        center.x - panel_w / 2.0,
        center.y - panel_h / 2.0,
        center.x + panel_w / 2.0,
        center.y + panel_h / 2.0,
    );
    let mut cards = Vec::with_capacity(shown);
    for (row_at, row) in shown_rows.iter().enumerate() {
        let mut x = panel.x0 + m.pad + (grid_w - row_width(row)) / 2.0;
        let y0 = panel.y0 + m.pad + usize_to_f64(row_at) * pitch_y;
        for at in row {
            let width = widths.get(*at).copied().unwrap_or(0.0);
            cards.push((*at, Rect::new(x, y0, x + width, y0 + card_h)));
            x += width + m.gap;
        }
    }
    let footer_y0 = panel.y0 + m.pad + grid_h + m.inset;
    let footer = Rect::new(
        panel.x0 + m.pad,
        footer_y0,
        panel.x1 - m.pad,
        footer_y0 + m.footer_h,
    );
    Geometry {
        panel,
        cards,
        footer,
        paged: shown < count,
        metrics: Metrics { well_h, ..m },
    }
}

/// Alt+Tab's panel: what it shows, where it stands, and the windows whose pictures stand in its
/// wells.
/// The desk owns the walk; this is one panel's worth of it, shared by the session (which sets it),
/// the chrome (which draws it) and the compositor (which pictures the windows it names).
#[derive(Clone)]
pub struct Switcher {
    inner: Rc<Inner>,
}

struct Inner {
    view: RefCell<Option<SwitcherView>>,
    /// Moves whenever what the panel shows moves.
    revision: Cell<u64>,
    /// The revision last drawn: anything else owes a frame.
    drawn: Cell<u64>,
    /// Where the panel and its cards stood at the last draw, in the chrome's coordinates: the wells
    /// the compositor pictures windows in.
    geometry: RefCell<Geometry>,
    /// The card the pointer stands over.
    hover: Cell<Option<usize>>,
}

impl Default for Switcher {
    fn default() -> Self {
        Self {
            inner: Rc::new(Inner {
                view: RefCell::new(None),
                revision: Cell::new(0),
                drawn: Cell::new(0),
                geometry: RefCell::new(Geometry::default()),
                hover: Cell::new(None),
            }),
        }
    }
}

impl Switcher {
    /// Show `view` , or take the panel down with `None` . Returns whether what the panel shows
    /// changed, which is what owes a frame.
    pub fn set(&self, view: Option<SwitcherView>) -> bool {
        let inner = &*self.inner;
        if *inner.view.borrow() == view {
            return false;
        }
        inner.view.replace(view);
        inner.hover.set(None);
        inner.revision.set(inner.revision.get().wrapping_add(1));
        true
    }

    /// Take the panel down.
    pub fn clear(&self) -> bool {
        self.set(None)
    }

    /// Whether the panel is up at all.
    #[must_use]
    pub fn is_up(&self) -> bool {
        self.inner.view.borrow().is_some()
    }

    /// What the panel shows: the windows the walk crosses, in its order, and the one it reached.
    #[must_use]
    pub fn view(&self) -> Option<SwitcherView> {
        self.inner.view.borrow().clone()
    }

    /// Where each card stands, as the panel was last drawn: the card's place in the view,
    /// and its rect in the chrome's frame.
    #[must_use]
    pub fn card_rects(&self) -> Vec<(usize, Rect)> {
        self.inner.geometry.borrow().cards.clone()
    }

    /// Whether the panel shows something it has not drawn yet.
    #[must_use]
    pub fn needs_redraw(&self) -> bool {
        let inner = &*self.inner;
        inner.drawn.get() != inner.revision.get()
    }

    /// The card the pointer stands over, as the cursor drew it.
    pub fn set_hover(&self, hover: Option<usize>) {
        if self.inner.hover.replace(hover) != hover {
            self.inner
                .revision
                .set(self.inner.revision.get().wrapping_add(1));
        }
    }

    /// The card the pointer stands over.
    #[must_use]
    pub fn hover(&self) -> Option<usize> {
        self.inner.hover.get()
    }

    /// Where the panel stands as last drawn, the chrome's frame.
    #[must_use]
    pub fn panel_rect(&self) -> Rect {
        self.inner.geometry.borrow().panel
    }

    /// The card under `point` , as the panel was last drawn: the pointer's presses and its hover.
    #[must_use]
    pub fn card_at(&self, point: Point) -> Option<usize> {
        let view = self.inner.view.borrow();
        let view = view.as_ref()?;
        self.inner
            .geometry
            .borrow()
            .cards
            .iter()
            .find(|(index, card)| {
                *index < view.cards.len()
                    && point.x >= card.x0
                    && point.x < card.x1
                    && point.y >= card.y0
                    && point.y < card.y1
            })
            .map(|(index, _)| *index)
    }

    /// Whether `point` is on the panel at all, as it was last drawn.
    #[must_use]
    pub fn on_panel(&self, point: Point) -> bool {
        self.panel_rect().contains(point)
    }

    /// The pictures of the windows this panel names, where its wells stand: each window's latest
    /// buffer, in the desk's coordinates. The session hands them to the compositor, which draws
    /// them over the chrome, so a preview is the window as it is now, not a copy of it.
    #[must_use]
    pub fn pictures(&self) -> Vec<Picture> {
        let view = self.inner.view.borrow();
        let Some(view) = view.as_ref() else {
            return Vec::new();
        };
        let geometry = self.inner.geometry.borrow();
        geometry
            .cards
            .iter()
            .filter_map(|(index, card)| {
                let window = view.cards.get(*index)?.window?;
                Some(Picture {
                    window,
                    rect: Self::well_of_rect(*card, geometry.metrics),
                })
            })
            .collect()
    }

    /// The well of a card: the window's picture's place inside it.
    fn well_of_rect(card: Rect, m: Metrics) -> Rect {
        Rect::new(
            card.x0 + m.inset,
            card.y0 + m.inset,
            card.x1 - m.inset,
            card.y0 + m.inset + m.well_h,
        )
    }

    /// What the panel says to assistive technology: the windows it shows, and which of them the
    /// walk has reached, which a reader meets as a list of choices.
    #[must_use]
    pub fn accessibility(&self) -> Option<AccessNode> {
        let view = self.inner.view.borrow();
        let view = view.as_ref()?;
        let items = view
            .cards
            .iter()
            .enumerate()
            .map(|(index, card)| {
                let selected = index == view.selected;
                let mut state = AccessState::new().selected(selected);
                if selected {
                    state = state.focused(true);
                }
                AccessNode::new(Role::ListItem)
                    .with_name(title_of(card))
                    .with_state(state)
            })
            .collect();
        let (title, description) = if view.app_scope {
            (
                "window-switcher-app-title",
                "window-switcher-app-description",
            )
        } else {
            ("window-switcher-title", "window-switcher-description")
        };
        // The keys under the cards, as a reader hears them: "Alt + Tab: Walk,
        // Enter: Go there, Esc: Stay".
        let hints = Self::hints(view.app_scope);
        let spoken = KeyHintsFace::new(&hints).spoken().join(", ");
        let footer = self.inner.geometry.borrow().footer;
        let keys = AccessNode::new(Role::StaticText)
            .with_name(spoken)
            .with_bounds(footer);
        Some(
            AccessNode::new(Role::Dialog)
                .with_name(t(title))
                .with_description(t(description))
                .with_children(vec![AccessNode::new(Role::List).with_children(items), keys]),
        )
    }

    /// Paint the panel, over the desk and under the windows' own pictures. `host` is the display it
    /// centres on, in the chrome's coordinates.
    pub fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        host: Rect,
    ) {
        let inner = &*self.inner;
        let view = inner.view.borrow();
        let Some(view) = view.as_ref().filter(|view| !view.cards.is_empty()) else {
            inner.geometry.replace(Geometry::default());
            inner.drawn.set(inner.revision.get());
            return;
        };
        let metrics = Metrics::from_theme(theme);
        let geometry = geometry(host, view, metrics);
        // The panel is modal while it is up: the desk behind it waits.
        ui::overlay::scrim::modal(scene, transform, host, 1.0, theme);
        let m = geometry.metrics;
        let panel = geometry.panel;
        let tokens = &theme.tokens;

        // A Panel (HIG §9.1): the shell's floating-surface ground, edge and elevation, like the
        // overview and the palette.
        ui::surface::shadow(
            scene,
            transform,
            panel,
            m.radius,
            &tokens.elevation.floating,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            ui::chrome::shell::panel_ground(theme),
            None,
            &RoundedRect::from_rect(panel, m.radius),
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            ui::chrome::shell::panel_border(theme),
            None,
            &ui::surface::edge_outline(panel, m.radius),
        );

        let hover = inner.hover.get();
        // Theme type tokens already carry the user's text scale (HIG §7a).
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        for (index, card) in &geometry.cards {
            if let Some(window) = view.cards.get(*index) {
                let state = ui::chrome::SelectionState::new(
                    *index == view.selected,
                    hover == Some(*index),
                    false,
                );
                Self::draw_card(
                    &mut PaintCtx::new(scene, transform, text_renderer, theme),
                    (*card, m),
                    window,
                    state,
                );
            }
        }

        // The keys under the cards, and the page's place among the windows when they do not all
        // fit. The selected window's title is on its card already; repeating it here only said the
        // same thing twice.
        if geometry.footer.height() > 0.0 {
            Self::draw_footer(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                geometry.footer,
                view,
                geometry.paged,
            );
        }
        text_renderer.set_text_scale(renderer_scale);
        inner.geometry.replace(geometry);
        inner.drawn.set(inner.revision.get());
    }

    /// How a card paints: the card stands on the panel, not on the desk, so its lift is taken off
    /// the panel's own ground — the surface step that makes it read as lifted there rather than off
    /// the desk behind.
    fn card_tier(theme: &Theme, state: ui::chrome::SelectionState) -> ui::chrome::SelectionTier {
        ui::chrome::selection_tier_on(theme, state, ui::chrome::shell::panel_ground(theme))
    }

    /// One window's card: its preview well (or app mark) over its elided title, lifted off the
    /// panel's own ground when selected or hovered, and ringed in the person's colour when the walk
    /// has reached it. A card whose window the compositor pictures draws no face: the window itself
    /// stands there.
    fn draw_card(
        ctx: &mut PaintCtx<'_>,
        (card, m): (Rect, Metrics),
        window: &SwitcherCard,
        state: ui::chrome::SelectionState,
    ) {
        let (scene, transform, text_renderer, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        Self::card_tier(theme, state).paint(scene, transform, card, m.card_radius);
        if state.active {
            // The card the walk reached wears the person's own colour, just outside its edge:
            // chosen at a glance on either theme, without taking any room from the window it shows.
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.border_focus,
                None,
                &RoundedRect::from_rect(card.inflate(1.0, 1.0), m.card_radius + 1.0),
            );
        }
        let well = Self::well_of_rect(card, m);
        let app = WindowApp::named(&window.app_key);
        draw_preview_well(
            scene,
            transform,
            theme,
            PreviewWell {
                rect: well,
                radius: m.well_radius,
                icon: m.icon,
            },
            window.window.is_none().then_some(&app),
        );

        // The title line, led by the app mark when the well shows the window instead of the mark.
        let pictured = window.window.is_some();
        let gap = f64::from(tokens.spacing.s1);
        let lead = if pictured { m.mark + gap } else { 0.0 };
        let title_w = (card.width() - 2.0 * m.inset - lead).max(0.0);
        let title = text_renderer.elide_to_width_style(
            &title_of(window),
            f32_of_f64(title_w),
            &theme.text(TextRole::Label),
        );
        let (w, h) = text_renderer.measure_style(&title, None, &theme.text(TextRole::Label));
        let line_top = well.y1 + m.inset;
        let line_w = lead + f64::from(w);
        let x0 = card.center().x - line_w * 0.5;
        if pictured {
            let mark_top = line_top + (f64::from(h) - m.mark) / 2.0;
            ui::widget::draw_app_mark(
                scene,
                transform,
                Rect::new(x0, mark_top, x0 + m.mark, mark_top + m.mark),
                app.icon.as_ref(),
                app.kind,
                colors.text_primary,
            );
        }
        text_renderer.draw_style(
            scene,
            &title,
            if state.active {
                colors.text_primary
            } else {
                colors.text_dim
            },
            transform.pre_translate((x0 + lead, line_top).into()),
            None,
            &theme.text(TextRole::Label),
        );
    }

    /// The keys the walk answers to, drawn under the cards in the person's own keyboard legends,
    /// and where this page stands among the windows when they do not all fit.
    fn draw_footer(ctx: &mut PaintCtx<'_>, footer: Rect, view: &SwitcherView, paged: bool) {
        let theme = ctx.theme;
        let hints = Self::hints(view.app_scope);
        let face = KeyHintsFace::new(&hints);
        let hints_w = face.width(&mut *ctx.text, theme);
        let style = theme.text(TextRole::Subheading);
        let caption = paged.then(|| {
            t_args(
                "window-switcher-caption-paged",
                &[
                    (
                        "position",
                        ArgValue::Number(usize_to_f64(view.selected.saturating_add(1))),
                    ),
                    ("count", ArgValue::Number(usize_to_f64(view.cards.len()))),
                ],
            )
        });
        let caption_w = caption.as_ref().map_or(0.0, |text| {
            f64::from(ctx.text.measure_style(text, None, &style).0)
        });
        let gap = if caption.is_some() {
            f64::from(theme.tokens.spacing.rhythm.related)
        } else {
            0.0
        };
        let x0 = footer.center().x - (hints_w + gap + caption_w) * 0.5;
        let painted = face.paint(ctx, Rect::new(x0, footer.y0, x0 + hints_w, footer.y1));
        let Some(caption) = caption else {
            return;
        };
        let x0 = x0 + painted + gap;
        if x0 + caption_w > footer.x1 + 1.0 {
            // A panel too narrow for both keeps its keys whole: the page's place is told to a
            // reader instead.
            return;
        }
        let (_, line) = ctx.text.measure_style(&caption, None, &style);
        ctx.text.draw_style(
            &mut *ctx.scene,
            &caption,
            theme.tokens.colors.text_dim,
            ctx.transform
                .pre_translate((x0, footer.center().y - f64::from(line) / 2.0).into()),
            None,
            &style,
        );
    }

    /// The keys under the panel: the chord that walks, and what Enter and Escape do to the walk.
    /// The app's walk answers to the key above Tab.
    fn hints(app_scope: bool) -> Vec<KeyHint> {
        let walk = if app_scope {
            ["Alt", "`"]
        } else {
            ["Alt", "Tab"]
        };
        vec![
            KeyHint::new(walk, t("window-switcher-hint-walk")),
            KeyHint::new(["Enter"], t("window-switcher-hint-go")),
            KeyHint::new(["Esc"], t("window-switcher-hint-stay")),
        ]
    }
}

/// One window the panel pictures, and where its well stands: the session hands each to the
/// compositor, which draws the window's own buffer there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Picture {
    /// The compositor's window: the client surface the card's window shows.
    pub window: u64,
    /// Its well, in the chrome's coordinates.
    pub rect: Rect,
}

/// The words a card shows for its window: an unnamed window is an untitled one, as the desk's own
/// list calls it.
fn title_of(card: &SwitcherCard) -> String {
    if card.title.trim().is_empty() {
        t("window-switcher-untitled")
    } else {
        card.title.clone()
    }
}

/// Where a window's preview well sits and how its stand-in mark is sized.
#[derive(Clone, Copy, Debug)]
pub struct PreviewWell {
    pub rect: Rect,
    pub radius: f64,
    /// Edge of the app mark shown when there is no live preview.
    pub icon: f64,
}

/// A 16:10 well on the window-card surface, holding a window's live preview or, when `app` is
/// given, that app's own face. Shared by the switcher and the overview so a window looks the same
/// in both. A host that pictures the window itself passes `None` : the picture stands in the well.
pub fn draw_preview_well(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    well: PreviewWell,
    app: Option<&WindowApp>,
) {
    let colors = &theme.tokens.colors;
    let well_rr = RoundedRect::from_rect(well.rect, well.radius);
    scene.fill(
        Fill::NonZero,
        transform,
        colors.surfaces.content,
        None,
        &well_rr,
    );
    scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        transform,
        colors.surfaces.content_edge,
        None,
        &well_rr,
    );
    if let Some(app) = app {
        draw_app_face(scene, transform, well, app, colors.on_content().ink);
    }
}

/// What a well shows for `app` with no live picture: the app's artwork,
/// else the toolkit's symbol for it, else its kind's mark, at the well's
/// mark size, in `ink`. Neutral ink, as the launcher's symbols are: the
/// accent marks a choice, never an app at rest.
fn draw_app_face(
    scene: &mut Scene,
    transform: Affine,
    well: PreviewWell,
    app: &WindowApp,
    ink: vello::peniko::Color,
) {
    let center = well.rect.center();
    let half = well.icon / 2.0;
    let mark = Rect::new(
        center.x - half,
        center.y - half,
        center.x + half,
        center.y + half,
    );
    if app
        .icon
        .as_ref()
        .is_some_and(|image| ui::icons::draw_app_image(scene, transform, mark, image))
    {
        return;
    }
    if app.glyph == Icon::AppWindow {
        ui::widget::draw_app_mark(scene, transform, mark, None, app.kind, ink);
    } else {
        // The symbol's stroke grows with it, as the launcher's large symbols are drawn, so a big
        // face does not read as a hairline.
        let stroke = (well.icon / 24.0 * 1.5).clamp(1.5, 3.0);
        app.glyph
            .draw_with_stroke(scene, transform, mark, ink, stroke);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window with no live picture and no artwork shows its app's own symbol, so two apps'
    /// windows look different, in either host.
    #[test]
    fn a_window_without_a_picture_shows_its_apps_symbol() {
        let app = |app_id: &str| {
            WindowApp::of(
                &TileContent::LocalApp {
                    app_id: app_id.into(),
                    label: app_id.into(),
                    note: String::new(),
                },
                &PlainWindows,
            )
        };
        let (weather, chat, unknown) = (
            app("so.tilo.weather"),
            app("so.tilo.chat"),
            app("org.example.unknown"),
        );
        assert_eq!(weather.glyph, Icon::Cloud);
        assert_eq!(chat.glyph, Icon::MessageSquare);
        assert_eq!(unknown.glyph, Icon::AppWindow, "no symbol: the kind's mark");

        let theme = Theme::dark();
        let well = PreviewWell {
            rect: Rect::new(0.0, 0.0, 160.0, 100.0),
            radius: 8.0,
            icon: 48.0,
        };
        let drawn = |app: &WindowApp| {
            let mut scene = Scene::new();
            draw_app_face(&mut scene, Affine::IDENTITY, well, app, theme.text_primary);
            scene.encoding().path_data.clone()
        };
        assert!(!drawn(&weather).is_empty());
        assert_ne!(drawn(&weather), drawn(&chat), "each app has its own face");
        assert!(
            !drawn(&unknown).is_empty(),
            "an unknown app still has a mark"
        );
        // A window the desk names the same way wears the same face.
        assert_eq!(WindowApp::named("so.tilo.weather").glyph, Icon::Cloud);
    }

    #[test]
    fn cards_stay_inside_the_display_and_page_to_the_selection() {
        let m = Metrics::from_theme(&Theme::dark());
        let host = Rect::new(0.0, 0.0, 800.0, 600.0);
        let few = geometry(host, &view(3, 0), m);
        assert_eq!(few.cards.len(), 3);
        assert!(
            few.cards
                .iter()
                .all(|(_, card)| few.panel.contains(card.center()))
        );
        let many = geometry(host, &view(200, 150), m);
        assert!(many.cards.len() < 200, "not every window fits");
        assert!(many.cards.iter().any(|(index, _)| *index == 150));
        assert!(host.contains(many.panel.origin()));
        assert!(many.panel.x1 <= host.x1 && many.panel.y1 <= host.y1);
    }

    /// The panel and every card on it stay inside the display at every size, from a phone-class
    /// window to the desktop, whatever the window count: a host too small for one full card gets a
    /// smaller card, never a panel that runs off the screen.
    #[test]
    fn the_switcher_panel_fits_every_host() {
        for theme in [Theme::dark(), Theme::dark().with_text_scale(1.5)] {
            let m = Metrics::from_theme(&theme);
            for (w, h) in [
                (240.0, 160.0),
                (320.0, 480.0),
                (480.0, 300.0),
                (640.0, 400.0),
                (800.0, 600.0),
                (1280.0, 800.0),
                (1472.0, 920.0),
            ] {
                let host = Rect::new(0.0, 0.0, w, h);
                for count in [1, 3, 12] {
                    for selected in [0, count - 1] {
                        let g = geometry(host, &view(count, selected), m);
                        assert!(
                            within(host, g.panel),
                            "panel {:?} leaves {host:?} with {count} windows",
                            g.panel
                        );
                        assert!(!g.cards.is_empty(), "a card is always on screen");
                        assert!(
                            g.cards.iter().any(|(index, _)| *index == selected),
                            "the selected window is on the page"
                        );
                        for (index, card) in &g.cards {
                            assert!(
                                g.panel.contains(card.origin())
                                    && card.x1 <= g.panel.x1 + 1e-9
                                    && card.y1 <= g.panel.y1 + 1e-9,
                                "card {index} {card:?} leaves the panel {:?} on {host:?}",
                                g.panel
                            );
                        }
                        assert_eq!(
                            g.paged,
                            g.cards.len() < count,
                            "a page is reported only when the windows outnumber it"
                        );
                        assert!(
                            g.footer.height() > 0.0 && within(g.panel, g.footer),
                            "the keys stand under the cards inside the panel"
                        );
                    }
                }
            }
        }
    }

    /// Every card takes the shape of the window it shows, at the one height the row shares: the
    /// compositor's picture of the window then fills the card instead of standing in it with bare
    /// bars at its sides.
    #[test]
    fn every_card_takes_its_windows_shape_at_the_rows_one_height() {
        let theme = Theme::dark();
        let mut shaped = view(4, 0);
        let sizes = [(1600, 1000), (800, 1600), (1000, 1000), (1920, 1080)];
        for (card, size) in shaped.cards.iter_mut().zip(sizes) {
            card.size = Some(size);
        }
        let host = Rect::new(0.0, 0.0, 1472.0, 920.0);
        let g = geometry(host, &shaped, Metrics::from_theme(&theme));
        let heights: Vec<f64> = g.cards.iter().map(|(_, card)| card.height()).collect();
        assert_eq!(heights.len(), 4, "a row of four");
        assert!(
            heights
                .windows(2)
                .all(|pair| (pair[0] - pair[1]).abs() < 1e-9),
            "the row shares one height, so it reads as a row: {heights:?}"
        );
        for (index, card) in &g.cards {
            let (width, height) = sizes[*index];
            let aspect = f64::from(width) / f64::from(height);
            let well = Switcher::well_of_rect(*card, g.metrics);
            assert!(
                (well.width() / well.height() - aspect).abs() < 1e-6,
                "card {index} keeps its window's shape: {well:?} against {width}x{height}",
            );
            assert!(
                (well.width() - (card.width() - 2.0 * g.metrics.inset)).abs() < 1e-6,
                "the window's picture fills the card across"
            );
            assert!(
                (card.height() - (well.height() + g.metrics.text_part())).abs() < 1e-6,
                "under the well only the title's room"
            );
        }
        // A card the desk has no window to measure stands at the desk's own shape, so an unpictured
        // window's app face is not squeezed either.
        let mut standing = shaped.clone();
        for card in &mut standing.cards {
            card.size = None;
        }
        let g = geometry(host, &standing, Metrics::from_theme(&theme));
        for (index, card) in &g.cards {
            let well = Switcher::well_of_rect(*card, g.metrics);
            assert!(
                (well.width() / well.height() - STANDING_ASPECT).abs() < 1e-6,
                "card {index} with no window to measure stands at 16:10"
            );
        }
    }

    /// A preview grows with the display it stands on: a handful of windows on a desktop show far
    /// larger than the standing thumbnail, while a small display still stands the panel inside
    /// itself.
    #[test]
    fn previews_grow_with_the_display_they_stand_on() {
        let m = Metrics::from_theme(&Theme::dark());
        let desktop = Rect::new(0.0, 0.0, 1472.0, 920.0);
        let g = geometry(desktop, &view(3, 0), m);
        assert!(
            g.metrics.well_h > 2.0 * m.well_h,
            "three windows on a desktop stand well over thumbnail size: {}",
            g.metrics.well_h
        );
        assert_eq!(g.cards.len(), 3, "and all three stand on one line");
        assert!(!g.paged);
        let phone = Rect::new(0.0, 0.0, 320.0, 480.0);
        let small = geometry(phone, &view(3, 0), m);
        assert!(within(phone, small.panel), "the panel stays on a small one");
        assert!(
            small.metrics.well_h < g.metrics.well_h,
            "and its previews are as large as that display affords"
        );
    }

    /// The card the walk reached is lifted off the panel's own ground by the selection's full step
    /// and ringed in the person's colour; the cards the walk has not reached are the panel itself,
    /// unringed.
    #[test]
    fn the_walked_card_stands_off_the_panels_own_ground() {
        for theme in [Theme::dark(), Theme::light()] {
            let ground = ui::chrome::shell::panel_ground(&theme);
            let rest =
                Switcher::card_tier(&theme, ui::chrome::SelectionState::new(false, false, false));
            assert_eq!(rest.fill.components[3], 0.0, "a card at rest is the panel");
            assert!(rest.border.is_none() && rest.inner_glow.is_none());
            let chosen =
                Switcher::card_tier(&theme, ui::chrome::SelectionState::new(true, false, false));
            assert!(
                ui::chrome::lightness_step(chosen.fill, ground) >= ui::chrome::SELECTION_MIN_STEP,
                "the walked card reads as lifted: {:?} on {ground:?}",
                chosen.fill
            );
            assert!(chosen.border.is_some(), "and holds the lift at its edge");
            assert_eq!(
                theme.border_focus, theme.tokens.colors.intent.accent_ink,
                "the walk's ring is the person's own colour"
            );
        }
    }

    /// The keys the walk answers to stand under the cards in the person's words, and a reader hears
    /// them there too, after the windows.
    #[test]
    fn the_panel_names_the_keys_under_its_cards() {
        let switcher = Switcher::default();
        switcher.set(Some(view(2, 0)));
        let root = switcher.accessibility().expect("the panel reads");
        let keys = root
            .children
            .iter()
            .find(|child| child.role == Role::StaticText)
            .expect("the keys are read under the windows");
        let spoken = keys.name.as_deref().expect("the keys are named");
        let said = KeyHintsFace::new(&Switcher::hints(false))
            .spoken()
            .join(", ");
        assert_eq!(spoken, said);
        for (key, word) in [
            ("Alt + Tab", t("window-switcher-hint-walk")),
            ("Enter", t("window-switcher-hint-go")),
            ("Esc", t("window-switcher-hint-stay")),
        ] {
            assert!(
                spoken.contains(&format!("{key}: {word}")),
                "{key} is spoken with its word: {spoken}"
            );
        }
        // The app's walk answers to the key above Tab, and says so.
        let mut app = view(2, 0);
        app.app_scope = true;
        switcher.set(Some(app));
        let spoken = switcher
            .accessibility()
            .expect("the panel reads")
            .children
            .iter()
            .find(|child| child.role == Role::StaticText)
            .and_then(|keys| keys.name.clone())
            .expect("the keys are read");
        assert!(
            spoken.starts_with("Alt + `: "),
            "the app's own chord: {spoken}"
        );
        assert!(!spoken.contains("Alt + Tab"));
    }

    fn card(title: &str, window: Option<u64>) -> SwitcherCard {
        SwitcherCard {
            window,
            // The standing shape's pixels: a card drawn before its window is measured.
            size: window.map(|_| (1600, 1000)),
            title: title.to_owned(),
            app_key: "so.tilo.notes".to_owned(),
        }
    }

    fn view(count: usize, selected: usize) -> SwitcherView {
        SwitcherView {
            cards: (0..count)
                .map(|at| {
                    card(
                        &format!("window {at}"),
                        Some(u64::try_from(at).unwrap_or(0).saturating_add(1)),
                    )
                })
                .collect(),
            selected,
            app_scope: false,
        }
    }

    /// The panel names each window it shows where its well stands, in the desk's coordinates, so
    /// the compositor can draw the window itself there.
    #[test]
    fn the_panel_hands_the_compositor_each_windows_place_on_the_desk() {
        let switcher = Switcher::default();
        assert!(!switcher.is_up());
        assert!(switcher.set(Some(view(2, 0))));
        assert!(switcher.is_up());
        assert!(switcher.needs_redraw(), "a new panel owes a frame");
        assert!(
            !switcher.set(Some(view(2, 0))),
            "the same panel is no new frame"
        );
        let theme = Theme::dark();
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        switcher.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            Rect::new(0.0, 0.0, 1280.0, 800.0),
        );
        assert!(!switcher.needs_redraw(), "drawn");
        let pictures = switcher.pictures();
        assert_eq!(pictures.len(), 2);
        let panel = switcher.panel_rect();
        assert!(panel.width() > 0.0);
        for picture in &pictures {
            let well = picture.rect;
            assert!(
                well.x0 >= panel.x0 && well.y0 >= panel.y0,
                "a well is inside the panel"
            );
            assert!(well.width() > 0.0 && well.height() > 0.0);
        }
        // The pointer finds the card it is on, and only the panel's cards.
        let first = switcher
            .pictures()
            .first()
            .map(|picture| picture.rect)
            .expect("a picture");
        let point = first.center();
        assert_eq!(switcher.card_at(point), Some(0));
        assert!(switcher.on_panel(switcher.panel_rect().center()));
        assert!(
            switcher.card_at(Point::new(-10.0, -10.0)).is_none(),
            "nothing is off the panel"
        );
        assert!(switcher.clear());
        assert!(
            switcher.pictures().is_empty(),
            "a closed panel pictures nothing"
        );
    }

    /// A window whose app holds secrets has no picture: its card says so with the app's face, and
    /// the compositor is asked for nothing.
    #[test]
    fn a_card_without_a_window_shows_its_apps_face() {
        let switcher = Switcher::default();
        let mut hidden = view(1, 0);
        hidden.cards[0].window = None;
        switcher.set(Some(hidden));
        let mut scene = Scene::new();
        let theme = Theme::dark();
        switcher.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
            Rect::new(0.0, 0.0, 1280.0, 800.0),
        );
        assert!(switcher.pictures().is_empty());
        assert!(
            !scene.encoding().path_data.is_empty(),
            "the card still draws its material and its app's face"
        );
    }

    /// A reader meets the windows as a list of choices, the one the walk reached chosen and
    /// focused.
    #[test]
    fn the_panel_reads_as_a_list_of_windows_with_the_reached_one_chosen() {
        let switcher = Switcher::default();
        assert!(
            switcher.accessibility().is_none(),
            "no panel, nothing to read"
        );
        switcher.set(Some(view(3, 1)));
        let root = switcher.accessibility().expect("the panel reads");
        assert_eq!(root.role, Role::Dialog);
        let list = root.children.first().expect("one list");
        assert_eq!(list.role, Role::List);
        assert_eq!(list.children.len(), 3);
        let chosen = &list.children[1];
        assert_eq!(chosen.role, Role::ListItem);
        assert!(chosen.state.selected, "the one the walk reached");
        assert!(chosen.state.focused);
        assert!(!list.children[0].state.selected);
    }
}
