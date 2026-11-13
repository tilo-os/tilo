//! Where the floating layer's cards stand, as plain arithmetic.
//!
//! The session hosts one floating layer: every activity the person keeps over
//! the windows is a card in it. This module is the layer's placement as a pure
//! function, so what a frame draws can be proven from numbers alone: cards
//! never overlap each other, never sit under the bar, inside the notification
//! column while a banner shows, or in the band the hardware readout stands in,
//! and always keep a reachable strip inside the work area.
//!
//! Apps describe, the session places: nothing here knows what an activity is
//! beyond its [`Summary`].

use vello::kurbo::{Point, Rect, Size};

use crate::live_activity::{Art, Summary};
use crate::tokens::Metrics;

/// How far a card rests from the work area's edges.
pub const EDGE_INSET: f64 = 16.0;
/// The space between two docked cards.
pub const CARD_GAP: f64 = 8.0;
/// The least a card keeps inside the work area, whatever the person dragged
/// it to: enough to grab it again.
pub const REACHABLE: f64 = 44.0;
/// Below this width a display is phone-width: the pill is the only resting
/// form and a card only drops when the person expands it.
pub const NARROW_PX: f64 = 720.0;
/// The most cards expanded at once; the least recently touched folds into the
/// pill when another arrives.
pub const MAX_CARDS: usize = 3;

/// A card's inside margin.
const CARD_PADDING: f64 = 14.0;
/// Its header row: the app's glyph, the title, the collapse and the close.
const CARD_HEADER: f64 = 24.0;
/// The glyph that heads a card, square.
const CARD_ICON: f64 = 16.0;
/// What stands between the glyph and the title.
const CARD_TITLE_GAP: f64 = 8.0;
/// Its hero line: the reading, or the title when it has none.
const CARD_HERO: f64 = 36.0;
/// The line under the hero: an artist, who is speaking.
const CARD_SUBTITLE: f64 = 18.0;
/// Between a card's hero and the line under it.
const CARD_LINE_GAP: f64 = 2.0;
/// The artwork that heads a song's card, square: as tall as its title and
/// artist together, so the song reads as one block.
const CARD_ART: f64 = 56.0;
/// The shape of a picture that heads its card over the lines: the card's
/// whole width at nine sixteenths, as a screenshot's own frame is.
const CARD_HERO_ART: f64 = 9.0 / 16.0;
/// How wide a card headed with a picture stands: wide enough for its three
/// controls to say what each one does beside its glyph ("Copy", "Open", "Show
/// in folder"), and for the picture to be worth looking at.
const CARD_HERO_WIDTH: f64 = 380.0;
/// Between a song's artwork and its title.
const CARD_ART_GAP: f64 = 12.0;
/// A progress bar's time label, one at each end of it.
const CARD_TIME_LABEL: f64 = 40.0;
/// The least the bar keeps between a time label and itself.
const CARD_TIME_GAP: f64 = 8.0;
/// The row a song's progress stands in: its time labels' line.
const CARD_PROGRESS_ROW: f64 = 14.0;
/// Its progress bar.
const CARD_PROGRESS: f64 = 6.0;
/// Its row of controls.
const CARD_CONTROLS: f64 = 30.0;
/// A media transport's row: the side of its play/pause circle, and of the
/// smaller previous and next marks beside it, which are the toolkit's
/// [`crate::widget::TransportFace`] sizes.
const CARD_TRANSPORT: f64 = 40.0;
const CARD_TRANSPORT_MARK: f64 = 32.0;
/// The space between a transport's controls.
const CARD_TRANSPORT_GAP: f64 = 12.0;
/// The space between the rows of a card.
const CARD_ROW_GAP: f64 = 10.0;
/// A header control: the collapse and the close, square.
const CARD_HEADER_CONTROL: f64 = 24.0;
/// How far the field round a caret reaches above and below it: the box a person types in stands
/// this much proud of the line they type on, and no card covers any of it.
const FIELD_REACH: f64 = 16.0;

/// Which corner of the work area the cards dock in. It is the person's choice
/// on this device, never shared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    #[default]
    BottomRight,
}

impl Corner {
    /// Whether the column grows down from the top of the work area.
    #[must_use]
    pub const fn from_top(self) -> bool {
        matches!(self, Self::TopLeft | Self::TopRight)
    }

    /// Whether the column stands by the work area's left edge.
    #[must_use]
    pub const fn from_left(self) -> bool {
        matches!(self, Self::TopLeft | Self::BottomLeft)
    }
}

/// The screen the cards stand on, in the layer's own coordinates: the work
/// area under the bar, the bar itself, the notification column while a banner
/// shows, and the band the readout stands in while it does.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Placement {
    /// The display's work area: where windows and cards may stand.
    pub work: Rect,
    /// The bar over it: nothing goes under the bar.
    pub bar: Rect,
    /// The notification column, while a banner shows.
    pub banners: Option<Rect>,
    /// The band the hardware readout stands in, while it is up.
    pub readout: Option<Rect>,
    /// The caret of the field the person types into, when it stands outside every card: the
    /// cards keep clear of the line it is on.
    pub caret: Option<Rect>,
}

/// One activity the layer holds, and where the person left it when they
/// dragged it loose.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Card {
    pub app: String,
    pub summary: Summary,
    /// The rect the person dragged it to: `None` docks it in the corner
    /// column.
    pub loose: Option<Rect>,
    /// The app draws this card itself, in a window of its own
    /// ([`crate::floating_surface::FLOATING_SURFACE_APP_ID_PREFIX`]): the
    /// layer reserves the card's place and draws nothing of it.
    pub rich: bool,
    /// The size the card's app asked for; `None` is the size the summary's
    /// rows take. A card with a size is one its app draws, whether or not
    /// its window is up yet.
    pub size: Option<Size>,
}

impl Card {
    #[must_use]
    pub fn new(app: impl Into<String>, summary: Summary) -> Self {
        Self {
            app: app.into(),
            summary,
            loose: None,
            rich: false,
            size: None,
        }
    }
}

/// A card as the layer draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub app: String,
    pub summary: Summary,
    pub rect: Rect,
    /// The app draws it: the layer keeps its place and paints nothing.
    pub rich: bool,
}

/// Why a card is not on the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FoldReason {
    /// There is no room for it beside the bar, a banner, the readout or the
    /// other cards: it lives in the pill until there is.
    NoRoom,
    /// The display is phone-width and the person has not expanded it.
    Phone,
}

/// A card the layer leaves in the pill.
#[derive(Clone, Debug, PartialEq)]
pub struct Folded {
    pub app: String,
    pub activity: String,
    pub reason: FoldReason,
}

/// What the layer shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layer {
    pub cards: Vec<Placed>,
    pub folded: Vec<Folded>,
}

/// How big an activity's card is on `work`: one token's width (a hero picture
/// takes its own), never wider than the display, and the height of the rows
/// the summary has, so a card is as tall as what it says and no taller.
#[must_use]
pub fn card_size(work: Rect, summary: &Summary) -> Size {
    let widest = (work.width() - 2.0 * EDGE_INSET).max(0.0);
    let width = if summary.art == Art::Hero && !summary.artwork.is_empty() {
        CARD_HERO_WIDTH
    } else {
        f64::from(Metrics::BASE.panel_width_utility)
    }
    .min(widest);
    Size::new(width, card_parts(probe(width), summary).height)
}

/// A card of `width` tall enough for every row a summary can have: what
/// [`card_size`] measures its rows on.
fn probe(width: f64) -> Rect {
    Rect::from_origin_size((0.0, 0.0), Size::new(width, 4096.0))
}

/// How big `card` stands on `work`: every card in a column is as wide as the
/// layer's one column, so the cards share a width and an edge, and the layer's
/// place wins over what its app asked for. A card its app draws keeps the
/// height its app asked for; every other card the height its summary's rows
/// take. The app lays its body out at the width the layer's place gives it.
#[must_use]
pub fn card_extent(work: Rect, card: &Card) -> Size {
    let column = card_size(work, &card.summary);
    match card.size {
        Some(size) => Size::new(column.width, size.height.max(1.0)),
        None => column,
    }
}

/// Whether `work` is a phone-width display.
#[must_use]
pub fn narrow(work: Rect) -> bool {
    work.width() < NARROW_PX
}

/// Where the parts of a card stand on it, as the frame draws them.
#[derive(Clone, Debug, PartialEq)]
pub struct Parts {
    /// The app's glyph at the header's leading edge, tinted by the tone.
    pub icon: Rect,
    /// The line on the header beside the app's glyph: the title, when the
    /// hero is the card's reading, else the app the activity belongs to.
    pub title: Option<Rect>,
    /// The header's two controls, right-aligned.
    pub collapse: Rect,
    pub close: Rect,
    /// The hero line: the reading, or the title when it has none.
    pub hero: Rect,
    /// The line under the hero, where the summary has one.
    pub subtitle: Option<Rect>,
    /// The artwork: beside the song's lines, or over them at nine sixteenths
    /// of the card's width when the summary says it heads the card.
    pub artwork: Option<Rect>,
    /// Where it is in the song, and what is left of it, at the ends of the
    /// progress bar, where the summary has a length.
    pub elapsed: Option<Rect>,
    pub remaining: Option<Rect>,
    /// The card's progress bar, where the summary has one, and the row of
    /// controls it offers, in the order they stand below the header.
    pub progress: Option<Rect>,
    /// The row the offered controls stand in, where the summary has any.
    pub controls_row: Option<Rect>,
    /// Each control's place: its square on a media transport (a circle of its
    /// own size), its slot on any other card.
    pub controls: Vec<(String, Rect)>,
    /// The card's height: every row above, and the margin at its foot.
    pub height: f64,
}

/// Where each of `summary`'s parts stands on a card in `card`: the rows
/// [`card_size`] reserves, laid out whatever the card's real height.
#[must_use]
pub fn card_parts(card: Rect, summary: &Summary) -> Parts {
    let inside = card.inflate(-CARD_PADDING, -CARD_PADDING);
    let header = Rect::new(
        inside.x0,
        inside.y0,
        inside.x1,
        inside.y0 + CARD_HEADER.min(inside.height()),
    );
    let close = Rect::new(
        header.x1 - CARD_HEADER_CONTROL,
        header.y0,
        header.x1,
        header.y0 + CARD_HEADER_CONTROL,
    );
    let gap = 4.0;
    let collapse = Rect::new(
        close.x0 - gap - CARD_HEADER_CONTROL,
        header.y0,
        close.x0 - gap,
        header.y0 + CARD_HEADER_CONTROL,
    );
    let icon = Rect::from_center_size(
        Point::new(
            inside.x0 + CARD_ICON / 2.0,
            f64::midpoint(header.y0, header.y1),
        ),
        Size::new(CARD_ICON, CARD_ICON),
    );
    let title = (!summary.header_line().is_empty()).then(|| {
        Rect::new(
            icon.x1 + CARD_TITLE_GAP,
            header.y0,
            collapse.x0 - CARD_TITLE_GAP,
            header.y1,
        )
    });
    let mut cursor = header.y1;
    let mut row = |height: f64, gap: f64| {
        cursor += gap;
        let rect = Rect::new(inside.x0, cursor, inside.x1, cursor + height);
        cursor += height;
        rect
    };
    // A song's lines stand beside its artwork, centred on it; everything
    // else's use the card's whole width.
    let lines = CARD_HERO
        + if summary.subtitle.is_empty() {
            0.0
        } else {
            CARD_LINE_GAP + CARD_SUBTITLE
        };
    let (artwork, lines_x0, lines_top) = if summary.artwork.is_empty() {
        (None, inside.x0, None)
    } else if summary.art == Art::Hero {
        // The picture stands over the lines, the card's whole width at nine
        // sixteenths: what it shows is the card's point.
        let side = (inside.width().max(0.0) * CARD_HERO_ART).max(0.0);
        (Some(row(side, CARD_ROW_GAP)), inside.x0, None)
    } else {
        let block = row(CARD_ART.max(lines), CARD_ROW_GAP);
        let art = Rect::from_origin_size(block.origin(), Size::new(CARD_ART, CARD_ART));
        let top = block.y0 + (block.height() - lines) / 2.0;
        (Some(art), art.x1 + CARD_ART_GAP, Some(top))
    };
    let hero = match lines_top {
        Some(top) => Rect::new(
            lines_x0,
            top,
            inside.x1,
            top + CARD_HERO.min(inside.height()),
        ),
        None => row(CARD_HERO, CARD_ROW_GAP),
    };
    // The line under the hero: beside a song's artwork it follows the hero,
    // and anywhere else it is a row of its own, so what stands under it
    // starts below it.
    let subtitle = (!summary.subtitle.is_empty()).then(|| match lines_top {
        Some(top) => {
            let top = top + CARD_HERO.min(inside.height()) + CARD_LINE_GAP;
            Rect::new(lines_x0, top, inside.x1, top + CARD_SUBTITLE)
        }
        None => row(CARD_SUBTITLE, CARD_LINE_GAP),
    });
    let (progress, elapsed, remaining) = match summary.progress {
        None => (None, None, None),
        Some(_) if summary.length.is_some() => {
            let row = row(CARD_PROGRESS_ROW, CARD_ROW_GAP);
            let bar = Rect::new(
                row.x0 + CARD_TIME_LABEL + CARD_TIME_GAP,
                row.y0 + (row.height() - CARD_PROGRESS) / 2.0,
                row.x1 - CARD_TIME_LABEL - CARD_TIME_GAP,
                row.y0 + (row.height() + CARD_PROGRESS) / 2.0,
            );
            (
                Some(bar),
                Some(Rect::new(row.x0, row.y0, row.x0 + CARD_TIME_LABEL, row.y1)),
                Some(Rect::new(row.x1 - CARD_TIME_LABEL, row.y0, row.x1, row.y1)),
            )
        }
        Some(_) => (Some(row(CARD_PROGRESS, CARD_ROW_GAP)), None, None),
    };
    let transport = summary.transport();
    let controls_row = summary.has_actions().then(|| {
        row(
            if transport.is_some() {
                CARD_TRANSPORT
            } else {
                CARD_CONTROLS
            },
            CARD_ROW_GAP,
        )
    });
    let shown = summary.action_controls();
    let mut controls = Vec::with_capacity(shown.len());
    if let Some(controls_row) = controls_row {
        if let Some(steps) = &transport {
            // A transport's controls are circles of their own sizes, centred
            // as one row: play/pause stands a size up from its neighbours.
            let side = |step: &crate::widget::TransportControl| {
                if step.is_play() {
                    CARD_TRANSPORT
                } else {
                    CARD_TRANSPORT_MARK
                }
            };
            let count = steps.len();
            let rows = f64::from(u32::try_from(count).unwrap_or(u32::MAX));
            let marked = steps.iter().filter(|(_, step)| !step.is_play()).count();
            let marks = f64::from(u32::try_from(marked).unwrap_or(u32::MAX));
            let total = marks * CARD_TRANSPORT_MARK
                + (rows - marks) * CARD_TRANSPORT
                + CARD_TRANSPORT_GAP * (rows - 1.0);
            let mut left = controls_row.x0 + (controls_row.width() - total) / 2.0;
            for (id, step) in steps {
                let width = side(step);
                let at = Rect::from_center_size(
                    Point::new(left + width / 2.0, controls_row.center().y),
                    Size::new(width, width),
                );
                controls.push(((*id).to_owned(), at));
                left += width + CARD_TRANSPORT_GAP;
            }
        } else {
            let gap = 6.0;
            let count = f64::from(u32::try_from(shown.len()).unwrap_or(u32::MAX));
            let width = (controls_row.width() - gap * (count - 1.0)) / count;
            for (at, control) in shown.into_iter().enumerate() {
                let left = controls_row.x0
                    + f64::from(u32::try_from(at).unwrap_or(u32::MAX)) * (width + gap);
                controls.push((
                    control.id.clone(),
                    Rect::new(left, controls_row.y0, left + width, controls_row.y1),
                ));
            }
        }
    }
    Parts {
        icon,
        title,
        collapse,
        close,
        hero,
        subtitle,
        artwork,
        elapsed,
        remaining,
        progress,
        controls_row,
        controls,
        height: cursor - card.y0 + CARD_PADDING,
    }
}

/// Where the cards stand: `corner`'s column, in priority order from the
/// corner, or — phone-width and `expanded` — one full-width card under the
/// bar.
#[must_use]
pub fn place(placement: &Placement, corner: Corner, cards: &[Card], expanded: bool) -> Layer {
    let work = placement.work;
    let mut order: Vec<&Card> = cards.iter().collect();
    // Sorted, so the layer's answer never depends on the order the board was
    // read in, and the most urgent activity rests nearest the corner.
    order.sort_by(|a, b| {
        a.summary
            .pill_rank()
            .cmp(&b.summary.pill_rank())
            .then_with(|| a.app.cmp(&b.app))
            .then_with(|| a.summary.id.cmp(&b.summary.id))
    });
    let mut layer = Layer::default();
    let mut docked: Vec<&Card> = Vec::new();
    for card in order {
        match card.loose.filter(|_| !card.rich) {
            Some(left) => layer.cards.push(Placed {
                app: card.app.clone(),
                summary: card.summary.clone(),
                rect: keep_inside(card_extent(work, card), left, work, 0.0, REACHABLE),
                rich: false,
            }),
            None => docked.push(card),
        }
    }
    if narrow(work) {
        // The pill is the only resting form on a phone, but a card its app
        // presents is the activity's own surface: it stands, full width,
        // whether or not the person pulled anything out of the bar — and
        // whether or not its window has come up yet, which the place this
        // gives it is what brings about. Anything else drops only when the
        // person expands it.
        let dropped = docked
            .iter()
            .position(|card| card.size.is_some())
            .or_else(|| (!docked.is_empty() && expanded).then_some(0));
        let mut rest = docked.clone();
        if let Some(at) = dropped {
            let first = rest.remove(at);
            layer.cards.push(Placed {
                app: first.app.clone(),
                summary: first.summary.clone(),
                rect: drop_from_the_bar(placement, first),
                rich: first.rich,
            });
            layer.folded.extend(rest.iter().map(|card| Folded {
                app: card.app.clone(),
                activity: card.summary.id.clone(),
                reason: FoldReason::NoRoom,
            }));
        } else {
            layer.folded.extend(docked.iter().map(|card| Folded {
                app: card.app.clone(),
                activity: card.summary.id.clone(),
                reason: FoldReason::Phone,
            }));
        }
        return layer;
    }
    let column = column(placement, corner, &docked);
    let mut cursor = if corner.from_top() {
        column.top
    } else {
        column.bottom
    };
    for card in docked {
        let size = card_extent(work, card);
        let rect = if corner.from_top() {
            Rect::from_origin_size((column.x, cursor), size)
        } else {
            Rect::from_origin_size((column.x, cursor - size.height), size)
        };
        let fits = if corner.from_top() {
            rect.y1 <= column.bottom + 0.5
        } else {
            rect.y0 >= column.top - 0.5
        };
        if fits {
            layer.cards.push(Placed {
                app: card.app.clone(),
                summary: card.summary.clone(),
                rect,
                rich: card.rich,
            });
            cursor = if corner.from_top() {
                rect.y1 + CARD_GAP
            } else {
                rect.y0 - CARD_GAP
            };
        } else {
            layer.folded.push(Folded {
                app: card.app.clone(),
                activity: card.summary.id.clone(),
                reason: FoldReason::NoRoom,
            });
        }
    }
    layer
}

/// The strip of the work area the cards may dock in: between the bar, the
/// notification column while a banner shows, the readout band while it is up
/// and the bottom or top inset.
struct Column {
    x: f64,
    top: f64,
    bottom: f64,
}

fn column(placement: &Placement, corner: Corner, docked: &[&Card]) -> Column {
    let work = placement.work;
    let width = docked
        .iter()
        .map(|card| card_extent(work, card).width)
        .fold(0.0_f64, f64::max);
    let x = if corner.from_left() {
        work.x0 + EDGE_INSET
    } else {
        work.x1 - EDGE_INSET - width
    };
    let lane = Rect::new(x, work.y0, x + width, work.y1);
    let mut top = (work.y0 + EDGE_INSET).max(placement.bar.y1 + EDGE_INSET);
    let mut bottom = work.y1 - EDGE_INSET;
    // The notification column takes the top-right corner while a banner
    // shows: the cards stand below it, never under it.
    if let Some(banners) = placement.banners
        && overlaps_lane(&lane, banners)
    {
        top = top.max(banners.y1 + CARD_GAP);
    }
    // The readout is momentary feedback, and the field the person types in is what they look at:
    // the cards step around both, above a band in the lower half and below one in the upper.
    let field = placement.caret.map(|caret| caret.inflate(0.0, FIELD_REACH));
    for band in [placement.readout, field].into_iter().flatten() {
        if !overlaps_lane(&lane, band) {
            continue;
        }
        if band.center().y > work.center().y {
            bottom = bottom.min(band.y0 - CARD_GAP);
        } else {
            top = top.max(band.y1 + CARD_GAP);
        }
    }
    Column { x, top, bottom }
}

/// Whether `band` lies across the cards' lane at all: a band beside the
/// column (the readout's pill, not its full-width surface) leaves it alone.
fn overlaps_lane(lane: &Rect, band: Rect) -> bool {
    band.x0 < lane.x1 && band.x1 > lane.x0 && band.y0 < lane.y1 && band.y1 > lane.y0
}

/// A phone-width card, full width, dropped from under the bar.
fn drop_from_the_bar(placement: &Placement, card: &Card) -> Rect {
    let work = placement.work;
    let width = (work.width() - 2.0 * EDGE_INSET).max(0.0);
    let height = card_extent(work, card).height;
    let below_bar = (work.y0 + EDGE_INSET).max(placement.bar.y1 + EDGE_INSET);
    // It drops below a banner, and below the field the person types in when that stands where
    // it would land.
    let field = placement.caret.map(|caret| caret.inflate(0.0, FIELD_REACH));
    let top = [placement.banners, field]
        .into_iter()
        .flatten()
        .fold(below_bar, |top, band| {
            if band.y1 + CARD_GAP > top && band.y0 < top + height {
                band.y1 + CARD_GAP
            } else {
                top
            }
        });
    // A card taller than what is left starts from the top and goes no lower
    // than the work area's bottom inset.
    let y = clamp_axis(top, height, below_bar, work.y1 - EDGE_INSET, height);
    Rect::from_origin_size((work.x0 + EDGE_INSET, y), Size::new(width, height))
}

/// A card the size of `size` dragged to `at`, kept inside `work` and `inset`
/// from its edges but for a strip `reachable` long the person can grab: a
/// card that fits stays wholly inside, a card larger than the work area keeps
/// that strip. Every place a card is dropped ends here, so a settled card and
/// a remembered one land the same way.
#[must_use]
pub fn keep_inside(size: Size, at: Rect, work: Rect, inset: f64, reachable: f64) -> Rect {
    let left = work.x0 + inset;
    let top = work.y0 + inset;
    let x = clamp_axis(at.x0, size.width, left, work.x1 - inset, reachable);
    let y = clamp_axis(at.y0, size.height, top, work.y1 - inset, reachable);
    Rect::from_origin_size((x, y), size)
}

fn clamp_axis(origin: f64, length: f64, low: f64, high: f64, reachable: f64) -> f64 {
    let inside = high - length;
    if inside >= low {
        origin.clamp(low, inside)
    } else {
        origin.clamp(low, (high - reachable).max(low))
    }
}

/// An activity the bar's pill names: the app that shows it and its summary.
/// An activity's id is its own app's, so the app is part of what names it.
#[derive(Clone, Debug, PartialEq)]
pub struct Named {
    pub app: String,
    pub summary: Summary,
}

/// What the bar's pill shows: the activity that stands first, and how many
/// others are waiting behind it (one dot each).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pill {
    pub heading: Option<Named>,
    pub more: usize,
}

impl Pill {
    /// Whether the pill says anything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.heading.is_none()
    }

    /// What the pill says: the heading activity's title and reading.
    #[must_use]
    pub fn reading(&self) -> String {
        self.heading
            .as_ref()
            .map(|named| named.summary.reading())
            .unwrap_or_default()
    }

    /// The ink the reading stands in: the heading activity's tone.
    #[must_use]
    pub fn ink(&self, theme: &crate::theme::Theme) -> vello::peniko::Color {
        self.heading.as_ref().map_or_else(
            || theme.tokens.colors.text_primary,
            |named| named.summary.tone.ink(theme),
        )
    }

    /// The picture the pill shows beside its words, when the activity that
    /// heads it has artwork: a song reads as its cover and its title.
    #[must_use]
    pub fn artwork(&self) -> Option<crate::widget::PictureSource> {
        let summary = &self.heading.as_ref()?.summary;
        (!summary.artwork.is_empty()).then(|| crate::widget::PictureSource {
            src: summary.artwork.clone(),
        })
    }
}

/// The bar's pill: the most urgent activity, and how many others wait behind
/// it. A call stands before directions, directions before a timer that is due
/// within a minute, that timer before any other timer, timers before music.
#[must_use]
pub fn pill(activities: &[(String, Summary)]) -> Pill {
    let mut sorted: Vec<&(String, Summary)> = activities.iter().collect();
    sorted.sort_by(|a, b| {
        a.1.pill_rank()
            .cmp(&b.1.pill_rank())
            .then_with(|| a.0.cmp(&b.0))
    });
    let heading = sorted.first().map(|(app, summary)| Named {
        app: app.clone(),
        summary: summary.clone(),
    });
    Pill {
        heading,
        more: sorted.len().saturating_sub(1),
    }
}

#[cfg(test)]
#[path = "floating_layer_tests.rs"]
mod tests;
