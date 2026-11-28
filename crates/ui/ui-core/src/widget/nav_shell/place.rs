//! The layout policy the host runs: where each built part stands, and how
//! each header row fits its title, leading buttons and actions.

use std::rc::Rc;

use taffy::Size;
use vello::kurbo::Rect;

use super::band::{self, plan};
use super::status_action::place_status_action;
use super::{Column, Inputs, NavGeometry, Part, at_mut};
use tilo_ui_base::widget::WidgetLayoutError;
use tilo_ui_base::{num::f32_of_f64, num::f64_of_usize};
use tilo_ui_base::{widget::manual::ManualContext, widget::manual::MeasureConstraint};

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

/// What a column's header row settled on, published for the shell to name for an audit, or
/// cleared where the window folds the row away.
fn publish(inputs: &Inputs, column: Column, row: Option<super::HeaderRowFacts>) {
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
    for (i, (_, w, _)) in items.iter().enumerate() {
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
            compact.push(if capable { m.chip } else { *w });
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
    let more_control = more.filter(|_| !inputs.actions_listed);
    let more_w = match more_control {
        Some(index) => measure(ctx, index, None)?.0,
        None => 0.0,
    };
    let trailing = |widths: &[f64], shown: usize| -> f64 {
        let mut width = run(widths.get(..shown).unwrap_or(&[]));
        if shown < items.len() {
            width += if shown > 0 { gap + more_w } else { more_w };
        }
        width
    };
    // The actions the row never gives up: the column's primary one, every control it hosts, and
    // those before them, since a row stands its controls from the start.
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
    // A column too narrow for any of its buttons beside the control it hosts stands the control
    // alone, in the narrow form it fits itself to (a strip of choices folds into a picker), and
    // every button waits in the row's menu, which runs them. A control itself waits there for
    // nobody: its row would run nothing.
    let mut alone: Option<(usize, f64)> = None;
    if must == 0
        && !inputs.actions_listed
        && let Some(control) = fit.controls.last().copied()
        && let Some((part, _, _)) = items.get(control)
    {
        let narrow = ctx
            .widget(*part)
            .and_then(|widget| {
                tilo_ui_base::widget::capability::capability::<
                    dyn tilo_ui_base::widget::capability::Narrows,
                >(widget)
            })
            .and_then(|narrows| narrows.narrow_width());
        if let Some(width) = narrow
            && width + gap + more_w <= avail
        {
            if let Some(slot) = full.get_mut(control) {
                *slot = width;
            }
            if let Some(slot) = compact.get_mut(control) {
                *slot = width;
            }
            alone = Some((control, width));
        }
    }
    let kept = if inputs.actions_listed {
        0
    } else {
        fit.menu_from
            .map_or(items.len(), |from| from.min(items.len()))
    };

    // The title: what it says, and what its actions must leave it. A title the row can give
    // only an ellipsis is given no width at all rather than a name that reads as nothing: the
    // column goes unnamed there, as it does where the page shows its own title.
    let mut start = lead;
    let mut title: Option<(usize, f64, f64, f64)> = None;
    // What the title says, measured (which also finds the least width its words read in).
    let measured_title = match index_of(Part::Title(column)) {
        Some(index) => {
            let (tw, th) = measure(ctx, index, None)?;
            Some((index, tw, th))
        }
        None => None,
    };
    let least = measured_title.map_or(0.0, |(index, _, _)| {
        ctx.widget(index)
            .and_then(|widget| widget.as_any())
            .and_then(|any| any.downcast_ref::<super::header_parts::HeaderTitle>())
            .map_or(0.0, super::header_parts::HeaderTitle::least_width)
    });
    if let Some((index, tw, th)) = measured_title {
        let floor = alone.map_or_else(
            || trailing(&compact, must),
            |(_, width)| width + gap + more_w,
        );
        let spare = if floor > 0.0 {
            (avail - floor - gap).max(0.0)
        } else {
            avail
        };
        let width = if spare + 0.5 >= least {
            tw.min(spare).clamp(0.0, avail)
        } else {
            0.0
        };
        title = Some((index, width, tw, th));
        if let Some(slot) = placed.get_mut(index) {
            *slot = Some(centred(lead, width, th));
        }
        if width > 0.0 {
            start = lead + width + gap;
        }
    }
    let band = (trail - start).max(0.0);

    // The title is cut short only once every action has given what it can: where the row leaves it
    // nothing at all, its buttons show their icons rather than their words.
    let title_starved = title.is_some_and(|(_, width, words, _)| words > 0.5 && width <= 0.5);
    // The form the row stands in: every action's words where they fit, else the secondary ones
    // as their icons with the primary keeping its words, else the primary alone with the rest in
    // the row's menu, else all of them as icons. What fits last gives way first.
    let sum_of = |widths: &[f64], shown: usize| -> f64 {
        let mut width = run(widths.get(..shown).unwrap_or(&[]));
        if shown < items.len() {
            width += if shown > 0 { gap + more_w } else { more_w };
        }
        width
    };
    // The row's wordless form: its secondary actions narrowed, the app's primary as it says.
    let words = fit.words.filter(|words| *words < kept);
    let secondary_icons: Option<Vec<f64>> = words.map(|words| {
        (0..items.len())
            .map(|i| {
                if Some(i) == Some(words) {
                    full[i]
                } else {
                    compact[i]
                }
            })
            .collect()
    });
    let (use_compact, words_labelled, shown) = if let Some((control, _)) = alone {
        // The control stands alone: it is the only action the row keeps.
        let _ = control;
        (false, false, 0)
    } else if !title_starved && trailing(&full, kept) <= band {
        (false, false, kept)
    } else if !title_starved
        && let Some(icons) = &secondary_icons
        && sum_of(icons, kept) <= band
    {
        (true, true, kept)
    } else if !title_starved
        && let Some(words) = words
        && sum_of(&full, words + 1) <= band
    {
        (false, true, words + 1)
    } else if trailing(&compact, kept) <= band {
        (true, false, kept)
    } else {
        // The toolkit's one overflow rule: how many of the row's controls fit
        // beside the API's own button. What does not fit is named in the
        // menu, never dropped from the row.
        let fits = crate::widget::plan_overflow(band, compact.iter().copied(), gap, gap + more_w);
        (true, false, fits.min(kept).max(must))
    };
    fit.compact = use_compact;
    fit.shown = Some(shown);
    fit.control_only = alone.map(|(control, _)| control);
    let primary = fit.primary;
    fit.words_labelled = words_labelled;
    let widths: Vec<f64> = (0..items.len())
        .map(|i| {
            if use_compact && !(words_labelled && Some(i) == words) {
                compact[i]
            } else {
                full[i]
            }
        })
        .collect();
    // The actions the row stands, in their order: the control it hosts alone, or those it keeps.
    let standing: Vec<usize> = match alone {
        Some((control, _)) => vec![control],
        None => (0..shown).collect(),
    };
    let keeps = standing
        .iter()
        .any(|index| fit.capable.get(*index).copied().unwrap_or(false));
    let overflow = standing.len() < items.len();
    let more_size = match (overflow, more_control) {
        (true, Some(index)) => Some((index, measure(ctx, index, None)?)),
        _ => None,
    };
    let mut total: f64 = standing
        .iter()
        .map(|index| widths.get(*index).copied().unwrap_or(0.0))
        .sum::<f64>()
        + gap * f64_of_usize(standing.len().saturating_sub(1));
    if let Some((_, (mw, _))) = more_size {
        total += if standing.is_empty() { mw } else { gap + mw };
    }
    let mut x = (trail - total).max(start.min(trail));
    let actions_x0 = if total > 0.0 { x } else { trail };
    // The title was given what the actions must leave it, and the row often keeps fewer of them
    // than that: the gap between the title and the first control is the title's where no
    // leading fill stands there to take it. It takes all of that gap, not just the width its
    // words measure: a box sized to the measure itself cuts the last glyph as soon as the
    // renderer that paints the words shapes them a hair wider than the measure that sized the
    // box. The column a status message stands beside is the exception: the message starts where
    // the title ends, so that title keeps to what it says and leaves the rest of the row to the message.
    if let Some((index, width, tw, th)) = title
        && width > 0.0
        && index_of(Part::Leading(column)).is_none()
        && column != g.status_column()
    {
        let end = if total > 0.0 { actions_x0 - gap } else { trail };
        let spare = (end - lead).clamp(0.0, avail);
        if spare > width + 0.5 {
            if let Some(slot) = placed.get_mut(index) {
                *slot = Some(centred(lead, spare, th));
            }
            start = lead + spare + gap;
            title = Some((index, spare, tw, th));
        }
    }
    for (k, (index, _, h)) in items.iter().enumerate() {
        let rect = if standing.contains(&k) {
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
    if column.key() == "overview" {
        let mut listed: Vec<(
            Option<String>,
            Option<(super::ButtonRole, bool, bool, bool)>,
        )> = Vec::new();
        for (index, _, _) in &items {
            let name = ctx.widget(*index).map(|widget| widget.name().to_owned());
            let facts = ctx.widget(*index).and_then(super::button_of).map(|button| {
                (
                    button.role_of(),
                    button.is_menu_only(),
                    button.shrinks_to_icon(),
                    button.is_shown_compact(),
                )
            });
            listed.push((name, facts));
        }
        eprintln!(
            "OVERVIEW primary={:?} controls={:?} must={must} kept={kept} shown={shown} listed={listed:?}",
            fit.primary, fit.controls
        );
    }
    drop(fits);

    if let Some(index) = index_of(Part::Leading(column)) {
        let end = if total > 0.0 { actions_x0 - gap } else { trail };
        let spare = (end - start).max(0.0);
        let (lw, lh) = measure(ctx, index, Some(spare))?;
        if let Some(slot) = placed.get_mut(index) {
            *slot = Some(centred(start, lw.min(spare), lh));
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
                // The width beside the title it could take: what stands before its neighbour's
                // gap, never the gap the row keeps between two neighbours.
                (tw, width, (next - rect.x1 - gap).max(0.0))
            }
            None => (0.0, 0.0, 0.0),
        };
        publish(
            inputs,
            column,
            Some(super::HeaderRowFacts {
                title_needs: needs,
                title_has: has,
                free_beside_title: free,
                labelled_action: !use_compact && keeps,
                primary_shown: alone.is_some()
                    || must == 0
                    || words
                        .or(primary)
                        .is_none_or(|names| standing.contains(&names)),
            }),
        );
    }
    Ok(())
}
