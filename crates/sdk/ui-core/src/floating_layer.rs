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

use crate::live_activity::Summary;
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
/// Its header row: the title, the collapse and the close.
const CARD_HEADER: f64 = 24.0;
/// Its reading: the countdown or what it says.
const CARD_VALUE: f64 = 30.0;
/// Its progress bar.
const CARD_PROGRESS: f64 = 6.0;
/// Its row of controls.
const CARD_CONTROLS: f64 = 34.0;
/// The space between the rows of a card.
const CARD_ROW_GAP: f64 = 10.0;
/// A header control: the collapse and the close, square.
const CARD_HEADER_CONTROL: f64 = 24.0;
/// The tone mark before a card's title.
const CARD_MARK: f64 = 8.0;
/// What stands between the mark and the title.
const CARD_TITLE_GAP: f64 = 8.0;

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
}

impl Card {
    #[must_use]
    pub fn new(app: impl Into<String>, summary: Summary) -> Self {
        Self {
            app: app.into(),
            summary,
            loose: None,
        }
    }

    /// The activity's own name, what the layer reports it by.
    #[must_use]
    pub fn activity(&self) -> &str {
        &self.summary.id
    }
}

/// A card as the layer draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub app: String,
    pub summary: Summary,
    pub rect: Rect,
    /// The person dragged it loose: it stays where they left it.
    pub loose: bool,
}

impl Placed {
    #[must_use]
    pub fn activity(&self) -> &str {
        &self.summary.id
    }
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

/// How big an activity's card is on `work`: the sheet's token width (never
/// wider than the display) and the height of the rows the summary has.
#[must_use]
pub fn card_size(work: Rect, summary: &Summary) -> Size {
    let widest = (work.width() - 2.0 * EDGE_INSET).max(0.0);
    let width = f64::from(Metrics::BASE.panel_width_sheet).min(widest);
    let mut height = 2.0 * CARD_PADDING + CARD_HEADER;
    if !summary.value.is_empty() {
        height += CARD_ROW_GAP + CARD_VALUE;
    }
    if summary.progress.is_some() {
        height += CARD_ROW_GAP + CARD_PROGRESS;
    }
    if summary.has_actions() {
        height += CARD_ROW_GAP + CARD_CONTROLS;
    }
    Size::new(width, height)
}

/// Whether `work` is a phone-width display.
#[must_use]
pub fn narrow(work: Rect) -> bool {
    work.width() < NARROW_PX
}

/// Where the parts of a card stand on it, as the frame draws them.
#[derive(Clone, Debug, PartialEq)]
pub struct Parts {
    /// The mark before the title: a dot of the activity's own tone.
    pub mark: Rect,
    /// The title, beside the mark.
    pub title: Rect,
    /// The header's two controls, right-aligned.
    pub collapse: Rect,
    pub close: Rect,
    /// The card's reading and its progress bar, where the summary has them,
    /// and the row of controls it offers, in the order they stand below the
    /// header.
    pub value: Option<Rect>,
    pub progress: Option<Rect>,
    pub controls: Vec<(String, Rect)>,
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
    let mark_left = inside.x0 + CARD_MARK / 2.0;
    let mark = Rect::from_center_size(
        Point::new(mark_left, f64::midpoint(header.y0, header.y1)),
        Size::new(CARD_MARK, CARD_MARK),
    );
    let title = Rect::new(
        mark_left + CARD_MARK / 2.0 + CARD_TITLE_GAP,
        header.y0,
        collapse.x0 - CARD_TITLE_GAP,
        header.y1,
    );
    let mut cursor = header.y1;
    let mut row = |height: f64| {
        cursor += CARD_ROW_GAP;
        let rect = Rect::new(inside.x0, cursor, inside.x1, cursor + height);
        cursor += height;
        rect
    };
    let value = (!summary.value.is_empty()).then(|| row(CARD_VALUE));
    let progress = summary.progress.is_some().then(|| row(CARD_PROGRESS));
    let controls_row = summary.has_actions().then(|| row(CARD_CONTROLS));
    let shown = summary.action_controls();
    let mut controls = Vec::with_capacity(shown.len());
    if let Some(controls_row) = controls_row {
        let gap = 6.0;
        let count = f64::from(u32::try_from(shown.len()).unwrap_or(u32::MAX));
        let width = (controls_row.width() - gap * (count - 1.0)) / count;
        for (at, control) in shown.into_iter().enumerate() {
            let left =
                controls_row.x0 + f64::from(u32::try_from(at).unwrap_or(u32::MAX)) * (width + gap);
            controls.push((
                control.id.clone(),
                Rect::new(left, controls_row.y0, left + width, controls_row.y1),
            ));
        }
    }
    Parts {
        mark,
        title,
        collapse,
        close,
        value,
        progress,
        controls,
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
        match card.loose {
            Some(left) => layer.cards.push(Placed {
                app: card.app.clone(),
                summary: card.summary.clone(),
                rect: clamp_into(card_size(work, &card.summary), left, work),
                loose: true,
            }),
            None => docked.push(card),
        }
    }
    if narrow(work) {
        // The pill is the only resting form on a phone; expanding drops the
        // first card full-width from the bar.
        match docked.split_first() {
            Some((first, rest)) if expanded => {
                layer.cards.push(Placed {
                    app: first.app.clone(),
                    summary: first.summary.clone(),
                    rect: drop_from_the_bar(placement, first),
                    loose: false,
                });
                layer.folded.extend(rest.iter().map(|card| Folded {
                    app: card.app.clone(),
                    activity: card.summary.id.clone(),
                    reason: FoldReason::NoRoom,
                }));
            }
            _ => layer.folded.extend(docked.iter().map(|card| Folded {
                app: card.app.clone(),
                activity: card.summary.id.clone(),
                reason: FoldReason::Phone,
            })),
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
        let size = card_size(work, &card.summary);
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
                loose: false,
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
        .map(|card| card_size(work, &card.summary).width)
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
    // The readout is momentary feedback: the cards step around its band.
    if let Some(readout) = placement.readout
        && overlaps_lane(&lane, readout)
    {
        if readout.center().y > work.center().y {
            bottom = bottom.min(readout.y0 - CARD_GAP);
        } else {
            top = top.max(readout.y1 + CARD_GAP);
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
    let height = card_size(work, &card.summary).height;
    let mut top = (work.y0 + EDGE_INSET).max(placement.bar.y1 + EDGE_INSET);
    if let Some(banners) = placement.banners
        && banners.y1 + CARD_GAP > top
        && banners.y0 < top + height
    {
        top = banners.y1 + CARD_GAP;
    }
    // A card taller than what is left starts from the top and goes no lower
    // than the work area's bottom inset.
    let lowest = (work.y1 - EDGE_INSET - height).max(work.y0 + EDGE_INSET);
    Rect::from_origin_size(
        (work.x0 + EDGE_INSET, top.min(lowest)),
        Size::new(width, height),
    )
}

/// A card `size` dragged to `at`, kept inside `work` but for [`REACHABLE`]:
/// a card that fits stays wholly inside, a card larger than the work area
/// keeps a strip to grab.
fn clamp_into(size: Size, at: Rect, work: Rect) -> Rect {
    let x = clamp_axis(at.x0, size.width, work.x0, work.x1);
    let y = clamp_axis(at.y0, size.height, work.y0, work.y1);
    Rect::from_origin_size((x, y), size)
}

fn clamp_axis(origin: f64, length: f64, low: f64, high: f64) -> f64 {
    let inside = high - length;
    if inside >= low {
        origin.clamp(low, inside)
    } else {
        // Longer than the work area: at least a reachable strip stays inside.
        origin.clamp(high - REACHABLE, low)
    }
}

/// What the bar's pill shows: the activity that stands first, and how many
/// others are waiting behind it (one dot each).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pill {
    pub heading: Option<Summary>,
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
            .map(Summary::reading)
            .unwrap_or_default()
    }
}

/// The bar's pill: the most urgent activity, and how many others wait behind
/// it. A call stands before directions, directions before a timer that is due
/// within a minute, that timer before any other timer, timers before music.
#[must_use]
pub fn pill(activities: &[Summary]) -> Pill {
    let mut sorted: Vec<&Summary> = activities.iter().collect();
    sorted.sort_by(|a, b| a.pill_rank().cmp(&b.pill_rank()));
    let heading = sorted.first().map(|summary| (*summary).clone());
    Pill {
        heading,
        more: sorted.len().saturating_sub(1),
    }
}

#[cfg(test)]
#[path = "floating_layer_tests.rs"]
mod tests;
