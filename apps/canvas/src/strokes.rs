//! Brush strokes edited after they are laid: continued, cut in two, joined,
//! and found along a path a stroke eraser drags.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "float geometry, and point indices bounded by the strokes they index"
)]

use ui::tilo_actions::ActionError;
use ui::tilo_resource::{Doc, order};
use ui::vello::kurbo::{Line, ParamCurveNearest as _, Point};

use crate::doc;
use crate::model::{Artwork, MAX_POINTS, Mark, Shape, StrokePoint};

fn refuse(message: impl Into<String>) -> ActionError {
    ActionError::Refused(message.into())
}

fn at(point: &[f64]) -> Point {
    Point::new(
        point.first().copied().unwrap_or(0.0),
        point.get(1).copied().unwrap_or(0.0),
    )
}

/// Continue `points` through `more`: from the end they start nearer, so a
/// stroke can be carried on from either end.
pub fn extend(points: &mut Vec<StrokePoint>, more: &[StrokePoint]) {
    let (Some(first), Some(last), Some(next)) = (points.first(), points.last(), more.first())
    else {
        return;
    };
    let room = MAX_POINTS.saturating_sub(points.len());
    let more = more.iter().take(room).cloned();
    if at(next).distance(at(first)) < at(next).distance(at(last)) {
        let mut joined: Vec<StrokePoint> = more.collect();
        joined.reverse();
        joined.append(points);
        *points = joined;
    } else {
        points.extend(more);
    }
}

/// The stroke a mark id names, or why it cannot be cut or joined.
fn stroke<'a>(art: &'a Artwork, id: &str) -> Result<&'a Mark, ActionError> {
    let mark = art
        .mark(id)
        .ok_or_else(|| refuse(format!("there is no mark {id:?}; inspect lists the marks")))?;
    if !matches!(mark.shape, Shape::Stroke { .. }) {
        return Err(refuse(format!(
            "{id:?} is a {}, not a brush stroke; only strokes are split and merged",
            mark.shape.kind()
        )));
    }
    Ok(mark)
}

fn points_of(mark: &Mark) -> &[StrokePoint] {
    match &mark.shape {
        Shape::Stroke { points, .. } => points,
        _ => &[],
    }
}

fn with_points(mark: &Mark, points: Vec<StrokePoint>) -> Mark {
    let mut mark = mark.clone();
    if let Shape::Stroke { points: old, .. } = &mut mark.shape {
        *old = points;
    }
    mark
}

/// Cut the one stroke `ids` names at the point of its path nearest `cut`
/// (in artwork units): it keeps the part before, and the part after
/// becomes a new stroke just above it with the same brush. Answers both
/// ids.
///
/// # Errors
/// Refuses anything but one stroke, and a cut at either end.
pub fn split(
    doc: &mut Doc,
    art: &Artwork,
    ids: &[String],
    cut: [f64; 2],
) -> Result<Vec<String>, ActionError> {
    let [id] = ids else {
        return Err(refuse("split cuts one stroke: name exactly one id"));
    };
    let mark = stroke(art, id)?;
    let points = points_of(mark);
    let local = mark.affine().inverse() * Point::new(cut[0], cut[1]);
    // The nearest place on the path: a segment and how far along it.
    let mut best: Option<(f64, usize, f64)> = None;
    for (index, pair) in points.windows(2).enumerate() {
        let (Some(a), Some(b)) = (pair.first(), pair.get(1)) else {
            continue;
        };
        let nearest = Line::new(at(a), at(b)).nearest(local, 1e-9);
        if best.is_none_or(|(distance, ..)| nearest.distance_sq < distance) {
            best = Some((nearest.distance_sq, index, nearest.t));
        }
    }
    let Some((_, index, t)) = best else {
        return Err(refuse("a stroke of one point cannot be split"));
    };
    let (Some(a), Some(b)) = (points.get(index), points.get(index + 1)) else {
        return Err(refuse("a stroke of one point cannot be split"));
    };
    let middle: StrokePoint = a
        .iter()
        .zip(b.iter().chain(std::iter::repeat(&1.0)))
        .map(|(a, b)| a + (b - a) * t)
        .collect();
    let mut before: Vec<StrokePoint> = points.get(..=index).unwrap_or_default().to_vec();
    before.push(middle.clone());
    let mut after = vec![middle];
    after.extend(points.get(index + 1..).unwrap_or_default().iter().cloned());
    let short = |part: &[StrokePoint]| {
        part.windows(2)
            .map(|pair| match pair {
                [a, b] => at(a).distance(at(b)),
                _ => 0.0,
            })
            .sum::<f64>()
            < 0.5
    };
    if short(&before) || short(&after) {
        return Err(refuse(
            "the cut is at an end of the stroke; give a point along it",
        ));
    }
    let new_id = crate::actions::derived_id("s", &(id, cut), 0);
    if art.mark(&new_id).is_some() {
        return Err(refuse(format!(
            "the stroke was already cut there ({new_id:?})"
        )));
    }
    let next = art
        .marks_of(&mark.layer)
        .map(|other| other.order.as_str())
        .filter(|order| *order > mark.order.as_str())
        .min();
    let far = Mark {
        id: new_id.clone(),
        order: order::between(Some(mark.order.as_str()), next),
        name: String::new(),
        ..with_points(mark, after)
    };
    doc::write_mark(doc, &with_points(mark, before))?;
    doc::write_mark(doc, &far)?;
    Ok(vec![id.clone(), new_id])
}

/// Join the strokes `ids` names into the first, in the order named, each
/// turned to run on from the end nearest it; the others are removed.
/// Answers the joined stroke's id.
///
/// # Errors
/// Refuses fewer than two strokes, marks that are not strokes, strokes on
/// different layers or under different transforms, and a join past the
/// point limit.
pub fn merge(doc: &mut Doc, art: &Artwork, ids: &[String]) -> Result<String, ActionError> {
    let marks = ids
        .iter()
        .map(|id| stroke(art, id))
        .collect::<Result<Vec<&Mark>, ActionError>>()?;
    let [first, rest @ ..] = marks.as_slice() else {
        return Err(refuse("merge joins two or more strokes: name them in ids"));
    };
    if rest.is_empty() {
        return Err(refuse("merge joins two or more strokes: name them in ids"));
    }
    if let Some(other) = rest
        .iter()
        .find(|mark| mark.layer != first.layer || mark.transform != first.transform)
    {
        return Err(refuse(format!(
            "{:?} is on another layer or turned differently from {:?}; merge strokes of one layer",
            other.id, first.id
        )));
    }
    let mut joined: Vec<StrokePoint> = points_of(first).to_vec();
    for mark in rest {
        let mut next = points_of(mark).to_vec();
        let (Some(end), Some(head), Some(tail)) = (joined.last(), next.first(), next.last()) else {
            continue;
        };
        if at(end).distance(at(tail)) < at(end).distance(at(head)) {
            next.reverse();
        }
        joined.extend(next);
    }
    if joined.len() > MAX_POINTS {
        return Err(refuse(format!(
            "the joined stroke would have over {MAX_POINTS} points; simplify them first"
        )));
    }
    doc::write_mark(doc, &with_points(first, joined))?;
    for mark in rest {
        doc::remove(doc, doc::MARKS, &mark.id)?;
    }
    Ok(first.id.clone())
}

/// The brush strokes on shown, unlocked layers (only `layer`'s when given)
/// that the path `along` crosses, as a stroke eraser takes them; erasers
/// themselves are left.
pub fn crossed(art: &Artwork, along: &[[f64; 2]], layer: Option<&str>) -> Vec<String> {
    let mut path: Vec<Point> = Vec::new();
    for pair in along.windows(2) {
        let (Some([x0, y0]), Some([x1, y1])) = (pair.first(), pair.get(1)) else {
            continue;
        };
        let (from, to) = (Point::new(*x0, *y0), Point::new(*x1, *y1));
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 1..=4096 before the cast"
        )]
        let steps = (from.distance(to) / 1.5).ceil().clamp(1.0, 4096.0) as u32;
        for step in 0..steps {
            path.push(from.lerp(to, f64::from(step) / f64::from(steps)));
        }
    }
    if let Some([x, y]) = along.last() {
        path.push(Point::new(*x, *y));
    }
    art.marks
        .iter()
        .filter(|mark| {
            matches!(mark.shape, Shape::Stroke { .. })
                && layer.is_none_or(|layer| mark.layer == layer)
                && art
                    .layer(&mark.layer)
                    .is_some_and(|layer| layer.visible && !layer.locked)
        })
        .filter(|mark| {
            let bounds = mark.bounds();
            path.iter()
                .any(|point| bounds.contains(*point) && mark.hit(*point))
        })
        .map(|mark| mark.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extending_carries_on_from_the_nearer_end() {
        let mut points = vec![vec![0.0, 0.0], vec![10.0, 0.0]];
        extend(&mut points, &[vec![12.0, 0.0], vec![20.0, 0.0]]);
        assert_eq!(points.last(), Some(&vec![20.0, 0.0]));
        extend(&mut points, &[vec![-2.0, 0.0], vec![-9.0, 0.0]]);
        assert_eq!(points.first(), Some(&vec![-9.0, 0.0]));
        assert_eq!(points.len(), 6);
    }
}
