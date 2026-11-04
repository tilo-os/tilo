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
            Part::Toggle => Some(g.toggle),
            Part::Back => Some(g.back),
            Part::InspectorToggle => Some(g.inspector_toggle),
            Part::History(_) => Some(g.history),
            Part::Controls => {
                let band = g.controls;
                if band.width() <= 0.0 {
                    Some(Rect::ZERO)
                } else {
                    // Its own width (the band's when it has none of its
                    // own), a control's height: the band is one control tall.
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
            // Placed with the header rows; the status action once they are
            // ([`place_status_action`]).
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

/// A part's natural size, within `width` when given.
///
/// The natural size comes first: a bounded proposal replaces the part's own
/// width with `auto`, so a control that states a fixed size and measures
/// nothing (a switch) would come back zero wide. Only a part wider than
/// `width` is measured again within it, so a label can wrap or shrink.
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
    if header == Rect::ZERO || header.width() <= 0.0 {
        return Ok(());
    }
    let m = &inputs.metrics;
    let cy = header.center().y;
    let lead = g.lead_end(column, m);
    let mut trail = g.trail_start(column, m).max(lead);
    // The window has one bar over its columns: a section takes the room of the columns after
    // it while they have nothing to say (no title, no controls, no actions), so that the bar's
    // actions stand at the window's end rather than at a column edge nothing follows.
    let quiet = |other: Column| {
        !inputs.titled.get(other.index()).copied().unwrap_or(false)
            && inputs.actions.get(other.index()).copied().unwrap_or(0) == 0
            && !inputs.leading.get(other.index()).copied().unwrap_or(false)
    };
    if matches!(column, Column::Overview | Column::Content) {
        let after_quiet = Column::ALL
            .into_iter()
            .skip(ci + 1)
            .filter(|other| *other != Column::Inspector || g.header(*other) != Rect::ZERO)
            .all(&quiet);
        if after_quiet {
            let mut end = if g.window_controls.width() > 0.0 {
                g.window_controls.x0 - m.gap + m.inset
            } else {
                g.size.x - m.inset
            };
            for button in [g.inspector_toggle, g.history] {
                if button.width() > 0.0 {
                    end = end.min(button.x0 - m.gap);
                }
            }
            trail = trail.max(end.max(lead));
        }
    }
    let avail = trail - lead;
    let gap = m.gap;
    let centred = |x0: f64, w: f64, h: f64| -> Rect {
        let h = h.min(header.height()).max(0.0);
        Rect::new(x0, cy - h / 2.0, x0 + w.max(0.0), cy + h / 2.0)
    };

    // The actions, at their natural sizes.
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
    let reserve = run(&compact);

    // The title: its measured width, never pushing the actions past their
    // icons, and keeping a third of the row however many actions there are.
    let mut start = lead;
    if let Some(index) = index_of(Part::Title(column)) {
        let (tw, th) = measure(ctx, index, None)?;
        let room = if reserve > 0.0 {
            avail - reserve - gap
        } else {
            avail
        };
        let width = tw.min(room.max(tw.min(avail / 3.0))).clamp(0.0, avail);
        if let Some(slot) = placed.get_mut(index) {
            *slot = Some(centred(lead, width, th));
        }
        if width > 0.0 {
            start = lead + width + gap;
        }
    }
    let band = (trail - start).max(0.0);

    // Labels, icons, or the More menu. Where the host lists the window's
    // actions, the row keeps none of them and has no door either: the
    // person reaches them where the host lists them.
    let more = index_of(Part::More(column));
    let door = more.filter(|_| !inputs.actions_listed);
    let (use_compact, shown) = if run(&full) <= band {
        (false, items.len())
    } else if run(&compact) <= band {
        (true, items.len())
    } else {
        let more_w = match door {
            Some(index) => measure(ctx, index, None)?.0,
            None => 0.0,
        };
        // The toolkit's one overflow rule: how many of the row's controls fit
        // beside the door's own button. What does not fit is named in the
        // menu, never dropped from the row.
        (
            true,
            crate::widget::plan_overflow(band, compact.iter().copied(), gap, gap + more_w),
        )
    };
    // The row shows its everyday actions and its menu holds the rest, whatever room it has.
    // Where the host lists the window's actions, the row keeps none of them: each stands once,
    // where the host lists it, not twice.
    let shown = if inputs.actions_listed {
        0
    } else {
        fit.menu_from.map_or(shown, |from| shown.min(from))
    };
    fit.compact = use_compact;
    fit.shown = Some(shown);
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

    // The leading fill: its own width, or what is left when it needs more.
    if let Some(index) = index_of(Part::Leading(column)) {
        let end = if total > 0.0 { actions_x0 - gap } else { trail };
        let room = (end - start).max(0.0);
        let (lw, lh) = measure(ctx, index, Some(room))?;
        if let Some(slot) = placed.get_mut(index) {
            *slot = Some(centred(start, lw.min(room), lh));
        }
    }
    Ok(())
}
