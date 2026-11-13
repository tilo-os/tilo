//! The layout policy the host runs: where each built part stands, and how
//! each header row fits its title, leading buttons and actions.

use std::rc::Rc;

use taffy::Size;
use vello::kurbo::Rect;

use super::band::{self, plan};
use super::status_action::place_status_action;
use super::{Column, Inputs, NavGeometry, Part, at_mut};
use crate::num::{f32_of_f64, f64_of_usize};
use crate::widget::WidgetLayoutError;
use crate::widget::layout_info::HeaderRowLayout;
use crate::widget::manual::{ManualContext, MeasureConstraint};

/// The layout policy the host runs: place every built part at the rect the
/// plan gives it, and lay each header row out from what its parts measure.
pub(super) fn place_parts(
    inputs: &Inputs,
    ctx: &mut ManualContext<'_>,
) -> Result<Size<f32>, WidgetLayoutError> {
    let size = ctx.available();
    band::measure_band(inputs, ctx, f64::from(size.width))?;
    let g = plan(inputs, f64::from(size.width), f64::from(size.height));
    let mut placed: Vec<Option<Rect>> = vec![None; inputs.parts.len()];
    for (index, part) in inputs.parts.iter().enumerate() {
        let rect = match part {
            Part::Body(column) => Some(g.body(*column)),
            Part::Rail => Some(g.rail),
            Part::Band => Some(g.band),
            Part::Filter => Some(g.filter),
            Part::Toggle => Some(bar_control(ctx, index, g.toggle)?),
            Part::Back => Some(bar_control(ctx, index, g.back)?),
            Part::InspectorToggle => Some(bar_control(ctx, index, g.inspector_toggle)?),
            Part::History(_) => Some(bar_control(ctx, index, g.history)?),
            Part::Controls => {
                let band = g.controls;
                if band.width() <= 0.0 {
                    Some(Rect::ZERO)
                } else {
                    let measured = measure(ctx, index, Some(band.width()))?;
                    let w = if measured.0 > 0.0 {
                        measured.0.min(band.width())
                    } else {
                        band.width()
                    };
                    let h = if measured.1 > 0.0 {
                        measured.1.min(band.height())
                    } else {
                        band.height()
                    };
                    let y0 = band.center().y - h / 2.0;
                    Some(Rect::new(band.x0, y0, band.x0 + w, y0 + h))
                }
            }
            Part::Title(_)
            | Part::Leading(_)
            | Part::Action(..)
            | Part::More(_)
            | Part::StatusAction => None,
        };
        if let (Some(slot), Some(rect)) = (placed.get_mut(index), rect) {
            *slot = Some(rect);
        }
    }
    for column in Column::ALL {
        place_header(inputs, &g, column, ctx, &mut placed)?;
    }
    place_status_action(inputs, &g, ctx, &mut placed)?;
    for (index, rect) in placed.into_iter().enumerate() {
        let rect = rect.unwrap_or(Rect::ZERO);
        let rect = if rect.width() < 0.0 || rect.height() < 0.0 {
            Rect::ZERO
        } else {
            rect
        };
        ctx.place(index, rect)?;
    }
    Ok(size)
}

/// A bar control's rect inside the slot the bar gives it: its own drawn size, centred there.
fn bar_control(
    ctx: &mut ManualContext<'_>,
    index: usize,
    slot: Rect,
) -> Result<Rect, WidgetLayoutError> {
    if slot.width() <= 0.0 || slot.height() <= 0.0 {
        return Ok(Rect::ZERO);
    }
    let (w, h) = measure(ctx, index, Some(slot.width()))?;
    let w = if w > 0.0 {
        w.min(slot.width())
    } else {
        slot.width()
    };
    let h = if h > 0.0 {
        h.min(slot.height())
    } else {
        slot.height()
    };
    let centre = slot.center();
    Ok(Rect::new(
        centre.x - w / 2.0,
        centre.y - h / 2.0,
        centre.x + w / 2.0,
        centre.y + h / 2.0,
    ))
}

/// A part's natural size, within `width` when given.
pub(super) fn measure(
    ctx: &mut ManualContext<'_>,
    index: usize,
    width: Option<f64>,
) -> Result<(f64, f64), WidgetLayoutError> {
    let natural = ManualContext::measure(
        ctx,
        index,
        Size {
            width: MeasureConstraint::Intrinsic,
            height: MeasureConstraint::Intrinsic,
        },
    )?
    .size;
    let natural = (f64::from(natural.width), f64::from(natural.height));
    let Some(bound) = width.map(|w| w.max(0.0)) else {
        return Ok(natural);
    };
    if natural.0 <= bound {
        return Ok(natural);
    }
    let proposal = Size {
        width: MeasureConstraint::AtMost(f32_of_f64(bound)),
        height: MeasureConstraint::Intrinsic,
    };
    let measured = ManualContext::measure(ctx, index, proposal)?.size;
    Ok((f64::from(measured.width), f64::from(measured.height)))
}

/// What a column's header row settled on, published for an audit, or cleared where the window
/// folds the row away.
fn publish(inputs: &Inputs, column: Column, row: Option<HeaderRowLayout>) {
    let mut fits = inputs.fit.borrow_mut();
    at_mut(&mut fits, column).header = row;
}

/// Lay out one header row: the title at its measured width after the
/// leading buttons, the actions trailing (labelled buttons collapsing to
/// their icons, then the rest moving into the More menu when they do not
/// fit), and the leading fill between them.
#[allow(
    clippy::too_many_lines,
    reason = "one row's layout is one ordered decision: title, actions, overflow, leading"
)]
fn place_header(
    inputs: &Inputs,
    g: &NavGeometry,
    column: Column,
    ctx: &mut ManualContext<'_>,
    placed: &mut [Option<Rect>],
) -> Result<(), WidgetLayoutError> {
    let header = g.header(column);
    let ci = column.index();
    let index_of = |part: Part| inputs.parts.iter().position(|p| *p == part);
    publish(inputs, column, None);
    if header == Rect::ZERO || header.width() <= 0.0 {
        return Ok(());
    }
    let m = &inputs.metrics;
    let cy = header.center().y;
    let lead = g.lead_end(column, m);
    let trail = g.trail_start(column, m).max(lead);
    let avail = trail - lead;
    let gap = m.gap;
    let centred = |x0: f64, w: f64, h: f64| -> Rect {
        let h = h.min(header.height()).max(0.0);
        Rect::new(x0, cy - h / 2.0, x0 + w.max(0.0), cy + h / 2.0)
    };

    let count = inputs.actions.get(ci).copied().unwrap_or(0);
    let mut items: Vec<(usize, f64, f64)> = Vec::with_capacity(count);
    for i in 0..count {
        if let Some(index) = index_of(Part::Action(column, i)) {
            let (w, h) = measure(ctx, index, Some(avail))?;
            items.push((index, w, h));
        }
    }
    let fit_cell = Rc::clone(&inputs.fit);
    let mut fits = fit_cell.borrow_mut();
    let fit = at_mut(&mut fits, column);
    if fit.capable.len() != items.len() {
        fit.capable.resize(items.len(), false);
        fit.compact_now.resize(items.len(), false);
        fit.full.resize(items.len(), None);
    }
    let mut full = Vec::with_capacity(items.len());
    let mut compact = Vec::with_capacity(items.len());
    for (i, (_, w, h)) in items.iter().enumerate() {
        let capable = fit.capable.get(i).copied().unwrap_or(false);
        let now = fit.compact_now.get(i).copied().unwrap_or(false);
        if capable && now {
            full.push(fit.full.get(i).copied().flatten().unwrap_or(*w));
            compact.push(*w);
        } else {
            if let Some(slot) = fit.full.get_mut(i) {
                *slot = Some(*w);
            }
            full.push(*w);
            compact.push(if capable { w.min(*h) } else { *w });
        }
    }
    let run = |widths: &[f64]| -> f64 {
        if widths.is_empty() {
            0.0
        } else {
            widths.iter().sum::<f64>() + gap * f64_of_usize(widths.len().saturating_sub(1))
        }
    };
    let more = index_of(Part::More(column));
    let door = more.filter(|_| !inputs.actions_listed);
    let door_w = match door {
        Some(index) => measure(ctx, index, None)?.0,
        None => 0.0,
    };
    let trailing = |widths: &[f64], shown: usize| -> f64 {
        let mut width = run(widths.get(..shown).unwrap_or(&[]));
        if shown < items.len() {
            width += if shown > 0 { gap + door_w } else { door_w };
        }
        width
    };
    // The actions the row never gives up: the column's primary one and those before it, since a
    // row stands its controls from the start.
    let never = if inputs.actions_listed {
        0
    } else {
        fit.primary
            .map_or(0, |primary| primary + 1)
            .min(items.len())
    };
    // A column too narrow to hold even those keeps none of them in its row: what it cannot stand
    // waits in the row's menu, rather than spilling past the column's own edge.
    let must = if trailing(&compact, never) <= avail {
        never
    } else {
        0
    };
    let kept = if inputs.actions_listed {
        0
    } else {
        fit.menu_from
            .map_or(items.len(), |from| from.min(items.len()))
    };

    // The title: what it says, and what its actions must leave it.
    let mut start = lead;
    let mut title: Option<(usize, f64, f64, f64)> = None;
    if let Some(index) = index_of(Part::Title(column)) {
        let (tw, th) = measure(ctx, index, None)?;
        let floor = trailing(&compact, must);
        let room = if floor > 0.0 {
            (avail - floor - gap).max(0.0)
        } else {
            avail
        };
        let width = tw.min(room).clamp(0.0, avail);
        title = Some((index, width, tw, th));
        if let Some(slot) = placed.get_mut(index) {
            *slot = Some(centred(lead, width, th));
        }
        if width > 0.0 {
            start = lead + width + gap;
        }
    }
    let band = (trail - start).max(0.0);

    let (use_compact, shown) = if trailing(&full, kept) <= band {
        (false, kept)
    } else if trailing(&compact, kept) <= band {
        (true, kept)
    } else {
        // The toolkit's one overflow rule: how many of the row's controls fit
        // beside the door's own button. What does not fit is named in the
        // menu, never dropped from the row.
        let fits = crate::widget::plan_overflow(band, compact.iter().copied(), gap, gap + door_w);
        (true, fits.min(kept).max(must))
    };
    fit.compact = use_compact;
    fit.shown = Some(shown);
    let keeps = (0..shown).any(|i| fit.capable.get(i).copied().unwrap_or(false));
    let primary = fit.primary;
    let widths = if use_compact { &compact } else { &full };
    let overflow = shown < items.len();
    let more_size = match (overflow, door) {
        (true, Some(index)) => Some((index, measure(ctx, index, None)?)),
        _ => None,
    };
    let mut total = run(widths.get(..shown).unwrap_or(&[]));
    if let Some((_, (mw, _))) = more_size {
        total += if shown > 0 { gap + mw } else { mw };
    }
    let mut x = (trail - total).max(start.min(trail));
    let actions_x0 = if total > 0.0 { x } else { trail };
    // The title was given what the actions must leave it, and the row often keeps fewer of them
    // than that: the room between the title and the first control is the title's, up to what it
    // says, where no leading fill stands there to take it.
    if let Some((index, width, tw, th)) = title {
        let end = if total > 0.0 { actions_x0 - gap } else { trail };
        let room = (end - lead).max(0.0);
        let wanted = tw.min(room).clamp(0.0, avail);
        if index_of(Part::Leading(column)).is_none() && wanted > width {
            if let Some(slot) = placed.get_mut(index) {
                *slot = Some(centred(lead, wanted, th));
            }
            start = lead + wanted + gap;
            title = Some((index, wanted, tw, th));
        }
    }
    for (k, (index, _, h)) in items.iter().enumerate() {
        let rect = if k < shown {
            let w = widths.get(k).copied().unwrap_or(0.0);
            let rect = centred(x, w, *h);
            x += w + gap;
            rect
        } else {
            Rect::ZERO
        };
        if let Some(slot) = placed.get_mut(*index) {
            *slot = Some(rect);
        }
    }
    if let Some(index) = more {
        let rect = match more_size {
            Some((_, (mw, mh))) => centred(x, mw, mh),
            None => Rect::ZERO,
        };
        if let Some(slot) = placed.get_mut(index) {
            *slot = Some(rect);
        }
    }
    drop(fits);

    if let Some(index) = index_of(Part::Leading(column)) {
        let end = if total > 0.0 { actions_x0 - gap } else { trail };
        let room = (end - start).max(0.0);
        let (lw, lh) = measure(ctx, index, Some(room))?;
        if let Some(slot) = placed.get_mut(index) {
            *slot = Some(centred(start, lw.min(room), lh));
        }
    }
    let placed_at = |index: usize| placed.get(index).copied().flatten().unwrap_or(Rect::ZERO);
    if avail > 0.0 {
        let (needs, has, free) = match title {
            Some((index, width, tw, _)) => {
                let rect = placed_at(index);
                let next = [Part::Leading(column), Part::More(column)]
                    .into_iter()
                    .filter_map(index_of)
                    .chain((0..items.len()).filter_map(|i| index_of(Part::Action(column, i))))
                    .map(placed_at)
                    .map(|part| part.x0)
                    .filter(|x0| *x0 >= rect.x1 - 0.5)
                    .fold(trail, f64::min);
                (tw, width, (next - rect.x1).max(0.0))
            }
            None => (0.0, 0.0, 0.0),
        };
        publish(
            inputs,
            column,
            Some(HeaderRowLayout {
                column: column.key(),
                title_needs: f32_of_f64(needs),
                title_has: f32_of_f64(has),
                free_beside_title: f32_of_f64(free),
                labelled_action: !use_compact && keeps,
                primary_shown: must == 0 || primary.is_none_or(|primary| primary < shown),
            }),
        );
    }
    Ok(())
}
