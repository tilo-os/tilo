//! Text that does not fit what layout gave it: a widget's own measurement read against its
//! box while walking the tree, one widget at a time.

use ui::kurbo::Rect;
use ui::widget::Widget;

use super::{EPSILON, Kind, Problem, has_area, size};

/// Text whose widget needs more room than layout gave it: taller at its width than its box, or
/// with a word wider than its box. Each widget measures itself as layout measures it; a column
/// header's title, which the shell elides as it paints, is held to its row's rule by
/// [`header_row_problem`].
pub fn clipped_text(
    root: &mut dyn Widget,
    text: &mut ui::TextRenderer,
    viewport: (u32, u32),
) -> Vec<Problem> {
    let mut out = Vec::new();
    let clip = Rect::new(0.0, 0.0, f64::from(viewport.0), f64::from(viewport.1));
    measure_walk(root, &mut Vec::new(), clip, text, &mut out);
    out
}

/// How much room beside a title is room it should have taken: more than the widest gap two
/// neighbours in a header keep from each other.
const TITLE_ROOM: f64 = 24.0;

/// Whether a title that says `needs` wide is cut off in `has`, while `free` stands empty
/// beside it in its row.
pub(super) fn title_cut_with_room(needs: f64, has: f64, free: f64) -> bool {
    needs > has + EPSILON && free > TITLE_ROOM
}

/// What is wrong with one header row, as the shell laid it out: a title cut off while an action
/// in the row kept a label it could have shown as its icon, a title cut off with room standing
/// empty beside it, or a row that kept its menu but not the app's primary action.
pub(super) fn header_row_problem(row: &ui::widget::HeaderRowLayout) -> Option<String> {
    let needs = f64::from(row.title_needs);
    let has = f64::from(row.title_has);
    let free = f64::from(row.free_beside_title);
    if !row.primary_shown {
        return Some(format!(
            "the {} column's row keeps its menu but not its primary action",
            row.column
        ));
    }
    if needs <= has + EPSILON {
        return None;
    }
    if title_cut_with_room(needs, has, free) {
        return Some(format!(
            "the {} column's title says {needs:.0} wide, has {has:.0}, with {free:.0} free beside it",
            row.column
        ));
    }
    row.labelled_action.then(|| {
        format!(
            "the {} column's title says {needs:.0} wide, has {has:.0}, beside actions keeping \
             labels they could show as icons",
            row.column
        )
    })
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "window-space sizes narrowed for taffy; never near f32::MAX"
)]
fn measure_walk(
    widget: &mut dyn Widget,
    keys: &mut Vec<String>,
    clip: Rect,
    text: &mut ui::TextRenderer,
    out: &mut Vec<Problem>,
) {
    let pushed = widget
        .test_id()
        .map(|id| keys.push(id.to_owned()))
        .is_some();
    measure_one(widget, keys, clip, text, out);
    selection_lifts(widget, keys, out);
    prompt_fits(widget, keys, out);
    let inner = widget
        .child_clip_rect()
        .map_or(clip, |child| clip.intersect(child));
    for child in widget.children_mut() {
        measure_walk(child, keys, inner, text, out);
    }
    if pushed {
        keys.pop();
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "window-space sizes narrowed for taffy; never near f32::MAX"
)]
fn measure_one(
    widget: &mut dyn Widget,
    keys: &[String],
    clip: Rect,
    text: &mut ui::TextRenderer,
    out: &mut Vec<Problem>,
) {
    use ui::taffy::{AvailableSpace, Size};
    let own = widget.get_rect();
    if has_area(own) && !has_area(own.intersect(clip)) {
        return;
    }
    if widget.name() == "Text" && has_area(own) {
        let width = own.width() as f32;
        let tall = widget.measure(
            Size {
                width: Some(width),
                height: None,
            },
            Size {
                width: AvailableSpace::Definite(width),
                height: AvailableSpace::MaxContent,
            },
            text,
        );
        // The narrowest the text can be laid out at: a word longer than its line breaks
        // anywhere, so only text that cannot break (one line) needs more than a glyph.
        let narrowest = widget.measure(
            Size {
                width: None,
                height: None,
            },
            Size {
                width: AvailableSpace::Definite(1.0),
                height: AvailableSpace::MaxContent,
            },
            text,
        );
        let shown = widget
            .accessibility()
            .and_then(|access| access.name)
            .unwrap_or_default();
        let detail = if f64::from(tall.height) > own.height() + EPSILON {
            Some(format!(
                "needs {:.0} high at its width, has {} ({shown:?})",
                tall.height,
                size(own)
            ))
        } else if f64::from(narrowest.width) > own.width() + EPSILON {
            Some(format!(
                "needs {:.0} wide at the narrowest, has {} ({shown:?})",
                narrowest.width,
                size(own)
            ))
        } else {
            None
        };
        if let Some(detail) = detail {
            out.push(Problem {
                kind: Kind::ClippedText,
                keys: keys.iter().rev().cloned().collect(),
                widget: widget.name().to_owned(),
                id: widget.test_id().map(str::to_owned),
                detail,
            });
        }
    }
}

/// A lift a list painted past the row it selects: it stands around what the row shows, grown by
/// the bleed a row grows its own face by ([`ui::widget::RowLift`]), so a lift reaching further
/// fills the gutters beside the row with the person's choice.
fn selection_lifts(widget: &dyn Widget, keys: &[String], out: &mut Vec<Problem>) {
    let Some(lifts) = ui::widget::capability::capability::<dyn ui::widget::PaintedRowLifts>(widget)
        .map(ui::widget::PaintedRowLifts::painted_row_lifts)
    else {
        return;
    };
    for lift in lifts {
        let Some(over) = reach_past(lift) else {
            continue;
        };
        out.push(Problem {
            kind: Kind::SelectionBleed,
            keys: keys.iter().rev().cloned().collect(),
            widget: widget.name().to_owned(),
            id: widget.test_id().map(str::to_owned),
            detail: format!(
                "selects {} of the {} its row shows, reaching {over:.0} past it",
                size(lift.rect),
                size(lift.content)
            ),
        });
    }
}

/// A prompt a field painted wider than the line it prompts for: the text a person reads cut off
/// mid-word at whatever the field's clip falls on ([`ui::widget::PlaceholderFit`]).
fn prompt_fits(widget: &dyn Widget, keys: &[String], out: &mut Vec<Problem>) {
    let Some(fit) = ui::widget::capability::capability::<dyn ui::widget::PlaceholderFit>(widget)
        .and_then(ui::widget::PlaceholderFit::placeholder_fit)
    else {
        return;
    };
    if f64::from(fit.width) <= f64::from(fit.room) + EPSILON {
        return;
    }
    out.push(Problem {
        kind: Kind::ClippedText,
        keys: keys.iter().rev().cloned().collect(),
        widget: widget.name().to_owned(),
        id: widget.test_id().map(str::to_owned),
        detail: format!(
            "prompts {:?} {:.0} wide, has {:.0}",
            fit.shown, fit.width, fit.room
        ),
    });
}

/// How far a lift reaches past the row it selects, more than the bleed a row's face grows by.
pub(super) fn reach_past(lift: ui::widget::RowLift) -> Option<f64> {
    let room = Rect::new(
        lift.content.x0 - lift.bleed,
        lift.content.y0 - lift.bleed,
        lift.content.x1 + lift.bleed,
        lift.content.y1 + lift.bleed,
    );
    if lift.rect.width() <= 0.0 || lift.rect.height() <= 0.0 {
        return None;
    }
    [
        room.x0 - lift.rect.x0,
        lift.rect.x1 - room.x1,
        room.y0 - lift.rect.y0,
        lift.rect.y1 - room.y1,
    ]
    .into_iter()
    .fold(None, |most: Option<f64>, over| {
        (over > EPSILON)
            .then(|| most.map_or(over, |most| most.max(over)))
            .or(most)
    })
}
