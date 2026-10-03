//! What the selection tool does to marks, as the artwork's actions: pick
//! them out, move, stretch and turn them as one, duplicate, group and copy
//! them.
#![allow(clippy::arithmetic_side_effects, reason = "kurbo's float geometry")]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ui::vello::kurbo::{Affine, Point, Rect, Shape as _, Vec2};

use crate::actions::{CanvasAction, Draw, EditMarks, NewMark};
use crate::model::{Artwork, Mark, valid_id};

/// Whether `mark` can be picked: its layer is shown.
fn pickable(art: &Artwork, mark: &Mark) -> bool {
    art.layer(&mark.layer).is_some_and(|layer| layer.visible)
}

/// `ids` and every other member of the groups they are in, in stacking
/// order: a group is picked as one.
pub fn with_groups(art: &Artwork, ids: &[String]) -> Vec<String> {
    let groups: Vec<&str> = ids
        .iter()
        .filter_map(|id| art.mark(id))
        .map(|mark| mark.group.as_str())
        .filter(|group| !group.is_empty())
        .collect();
    art.marks
        .iter()
        .filter(|mark| ids.contains(&mark.id) || groups.contains(&mark.group.as_str()))
        .map(|mark| mark.id.clone())
        .collect()
}

/// What a click at `at` picks: the topmost mark there and, unless `inside`
/// (picking within a group), the rest of its group.
pub fn pick(art: &Artwork, at: Point, inside: bool) -> Vec<String> {
    let Some(mark) = art.mark_at(at) else {
        return Vec::new();
    };
    if inside {
        vec![mark.id.clone()]
    } else {
        with_groups(art, std::slice::from_ref(&mark.id))
    }
}

/// The marks a marquee drawn over `area` picks: every shown mark it
/// touches, with their groups.
pub fn marquee(art: &Artwork, area: Rect) -> Vec<String> {
    let touched: Vec<String> = art
        .marks
        .iter()
        .filter(|mark| pickable(art, mark))
        .filter(|mark| {
            let bounds = art.visible_bounds(mark);
            bounds.intersect(area).area() > 0.0 || area.contains(bounds.center())
        })
        .map(|mark| mark.id.clone())
        .collect();
    with_groups(art, &touched)
}

/// Every shown mark, for select-all.
pub fn everything(art: &Artwork) -> Vec<String> {
    art.marks
        .iter()
        .filter(|mark| pickable(art, mark))
        .map(|mark| mark.id.clone())
        .collect()
}

/// The frame around `ids`: their bounds and the angle they are turned by.
/// One mark turned by its transform gets a frame along its own sides.
pub fn frame(art: &Artwork, ids: &[String]) -> Option<(Rect, f64)> {
    let marks: Vec<&Mark> = ids.iter().filter_map(|id| art.mark(id)).collect();
    if let [mark] = marks.as_slice()
        && let Some([a, b, ..]) = mark.transform
    {
        let angle = b.atan2(a);
        if angle.abs() > 1e-6 {
            // The mark's outline unturned about the origin, its box, and
            // that box put back where the turned mark is.
            let unturned = Affine::rotate(-angle) * mark.affine();
            let mut path = mark.shape.path();
            path.apply_affine(unturned);
            let reach = match &mark.shape {
                crate::model::Shape::Stroke { size, .. } => size * 0.5,
                _ => mark.style.stroke_width * 0.5,
            };
            let local = path.bounding_box().inflate(reach, reach);
            let centre = Affine::rotate(angle) * local.center();
            return Some((local + (centre - local.center()), angle));
        }
    }
    marks
        .iter()
        .map(|mark| mark.bounds())
        .reduce(|a, b| a.union(b))
        .map(|bounds| (bounds, 0.0))
}

/// The edges and centres snapping can land on: the artwork and the shown
/// marks other than `moving`.
pub fn snap_targets(art: &Artwork, moving: &[String]) -> Vec<Rect> {
    std::iter::once(art.bounds())
        .chain(
            art.marks
                .iter()
                .filter(|mark| pickable(art, mark) && !moving.contains(&mark.id))
                .map(|mark| art.visible_bounds(mark)),
        )
        .collect()
}

/// `affine` applied to `ids` as one edit: a plain move when that is all it
/// is, so history reads as what happened.
pub fn transform(ids: Vec<String>, affine: Affine) -> Option<CanvasAction> {
    let [sx, ky, kx, sy, ..] = affine.as_coeffs();
    let shift = affine.translation();
    let moved =
        (sx - 1.0).abs() < 1e-9 && ky.abs() < 1e-9 && kx.abs() < 1e-9 && (sy - 1.0).abs() < 1e-9;
    if moved {
        if shift.hypot() < 1e-6 {
            return None;
        }
        return Some(CanvasAction::edit(EditMarks {
            ids,
            move_by: Some([round(shift.x), round(shift.y)]),
            ..EditMarks::default()
        }));
    }
    Some(CanvasAction::edit(EditMarks {
        ids,
        transform: Some(affine.as_coeffs()),
        ..EditMarks::default()
    }))
}

/// A thousandth of a unit is as fine as anything is placed.
fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// A fresh id with `prefix` that no mark, group or id in `taken` has.
pub fn fresh_id(art: &Artwork, prefix: &str, taken: &[String]) -> String {
    loop {
        let random = uuid::Uuid::new_v4().simple().to_string();
        let id = format!("{prefix}-{}", random.get(..10).unwrap_or_default());
        let used = art.mark(&id).is_some()
            || art.marks.iter().any(|mark| mark.group == id)
            || taken.contains(&id);
        if !used && valid_id(&id) {
            return id;
        }
    }
}

/// A group id for the selection: `group-1`, `group-2`, … the first free.
pub fn group_id(art: &Artwork) -> String {
    let groups = art.groups();
    (1_u32..=10_000)
        .map(|n| format!("group-{n}"))
        .find(|id| !groups.contains(&id.as_str()) && art.mark(id).is_none())
        .unwrap_or_else(|| fresh_id(art, "group", &[]))
}

/// Put `ids` in one new group.
pub fn group(art: &Artwork, ids: &[String]) -> Option<CanvasAction> {
    (ids.len() >= 2).then(|| {
        CanvasAction::edit(EditMarks {
            ids: ids.to_vec(),
            group: Some(group_id(art)),
            ..EditMarks::default()
        })
    })
}

/// Take `ids` out of their groups.
pub fn ungroup(art: &Artwork, ids: &[String]) -> Option<CanvasAction> {
    let grouped: Vec<String> = ids
        .iter()
        .filter(|id| art.mark(id).is_some_and(|mark| !mark.group.is_empty()))
        .cloned()
        .collect();
    (!grouped.is_empty()).then(|| {
        CanvasAction::edit(EditMarks {
            ids: grouped,
            group: Some(String::new()),
            ..EditMarks::default()
        })
    })
}

/// A mark as `draw` would make it again.
pub fn new_mark(mark: &Mark) -> NewMark {
    let style = &mark.style;
    NewMark {
        id: Some(mark.id.clone()),
        name: (!mark.name.is_empty()).then(|| mark.name.clone()),
        fill: style.fill.clone(),
        gradient: style.gradient.clone(),
        stroke: style.stroke.clone(),
        stroke_width: Some(style.stroke_width),
        opacity: style.opacity,
        brush: style.brush,
        angle: style.angle,
        wet: style.wet.then_some(true),
        params: (!style.params.is_empty()).then(|| style.params.clone()),
        dash: (!style.dash.is_empty()).then(|| style.dash.clone()),
        cap: style.cap.clone(),
        join: style.join.clone(),
        blend: style.blend.clone(),
        shadow: style.shadow.clone(),
        group: (!mark.group.is_empty()).then(|| mark.group.clone()),
        clip: (!mark.clip.is_empty()).then(|| mark.clip.clone()),
        transform: mark.transform,
        ..NewMark::new(mark.shape.clone())
    }
}

/// Marks as they travel on the clipboard: each with the layer it was on.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Clip {
    /// Tells a canvas clipboard apart from any other JSON.
    #[serde(rename = "tilo-canvas")]
    pub marks: Vec<(String, NewMark)>,
}

/// The clipboard text for `ids`, bottom to top.
pub fn copy(art: &Artwork, ids: &[String]) -> Option<String> {
    let marks: Vec<(String, NewMark)> = art
        .marks
        .iter()
        .filter(|mark| ids.contains(&mark.id))
        .map(|mark| (mark.layer.clone(), new_mark(mark)))
        .collect();
    if marks.is_empty() {
        return None;
    }
    serde_json::to_string(&Clip { marks }).ok()
}

/// Draws that place copies of `marks` moved by `offset`, each on its own
/// layer where that still exists, else on `fallback`: fresh ids, groups
/// and clips among the copies kept among the copies. Returns the draws and
/// the copies' ids.
pub fn place(
    art: &Artwork,
    marks: Vec<(String, NewMark)>,
    offset: Vec2,
    fallback: Option<&str>,
) -> (Vec<CanvasAction>, Vec<String>) {
    let mut ids: BTreeMap<String, String> = BTreeMap::new();
    let mut groups: BTreeMap<String, String> = BTreeMap::new();
    let mut taken: Vec<String> = Vec::new();
    let mut fresh_ids: Vec<String> = Vec::new();
    for (_, mark) in &marks {
        let fresh = fresh_id(art, "m", &taken);
        taken.push(fresh.clone());
        fresh_ids.push(fresh.clone());
        if let Some(id) = &mark.id {
            ids.insert(id.clone(), fresh);
        }
        if let Some(group) = &mark.group
            && !groups.contains_key(group)
        {
            let fresh = fresh_id(art, "g", &taken);
            taken.push(fresh.clone());
            groups.insert(group.clone(), fresh);
        }
    }
    let mut draws: Vec<(Option<String>, Vec<NewMark>)> = Vec::new();
    let mut placed = Vec::new();
    for (index, (layer, mut mark)) in marks.into_iter().enumerate() {
        let layer = art
            .layer(&layer)
            .map(|layer| layer.id.clone())
            .or_else(|| fallback.map(str::to_owned));
        mark.id = fresh_ids.get(index).cloned();
        mark.group = mark
            .group
            .as_ref()
            .and_then(|group| groups.get(group).cloned());
        mark.clip = mark.clip.as_ref().and_then(|clip| {
            ids.get(clip)
                .cloned()
                .or_else(|| art.mark(clip).map(|_| clip.clone()))
        });
        let to = Affine::translate(offset);
        match mark.transform {
            Some(transform) => mark.transform = Some((to * Affine::new(transform)).as_coeffs()),
            None => mark.shape = mark.shape.translated(offset.x, offset.y),
        }
        placed.extend(mark.id.clone());
        match draws.last_mut() {
            Some((on, batch)) if *on == layer => batch.push(mark),
            _ => draws.push((layer, vec![mark])),
        }
    }
    let actions = draws
        .into_iter()
        .map(|(layer, marks)| CanvasAction::Draw(Draw { layer, marks }))
        .collect();
    (actions, placed)
}

/// Copies of `ids` beside them.
pub fn duplicate(art: &Artwork, ids: &[String], offset: Vec2) -> (Vec<CanvasAction>, Vec<String>) {
    let marks = art
        .marks
        .iter()
        .filter(|mark| ids.contains(&mark.id))
        .map(|mark| (mark.layer.clone(), new_mark(mark)))
        .collect();
    place(art, marks, offset, None)
}

/// What the clipboard's text pastes as: copied marks, or SVG markup.
pub fn paste(
    art: &Artwork,
    text: &str,
    offset: Vec2,
    layer: Option<&str>,
) -> Option<(Vec<CanvasAction>, Vec<String>)> {
    if let Ok(clip) = serde_json::from_str::<Clip>(text) {
        return Some(place(art, clip.marks, offset, layer));
    }
    if !text.trim_start().starts_with('<') {
        return None;
    }
    let draw = crate::svg::import(&crate::svg::ImportSvg {
        svg: text.to_owned(),
        layer: layer.map(str::to_owned),
        group: None,
        fit: None,
    })
    .ok()?;
    let marks = draw
        .marks
        .into_iter()
        .map(|mark| (layer.unwrap_or_default().to_owned(), mark))
        .collect();
    Some(place(art, marks, Vec2::ZERO, layer))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Layer, Shape, Style};

    fn mark(id: &str, x: f64, group: &str) -> Mark {
        Mark {
            id: id.into(),
            layer: "ink".into(),
            order: id.into(),
            name: String::new(),
            shape: Shape::Rect {
                x,
                y: 0.0,
                width: 10.0,
                height: 10.0,
                radius: 0.0,
            },
            style: Style {
                fill: Some("#112233".into()),
                ..Style::default()
            },
            transform: None,
            group: group.into(),
            clip: String::new(),
        }
    }

    fn art() -> Artwork {
        Artwork {
            layers: vec![Layer {
                id: "ink".into(),
                name: "Ink".into(),
                order: "a".into(),
                medium: String::new(),
                opacity: 1.0,
                visible: true,
                texture: 0.0,
                locked: false,
                adjust: None,
            }],
            marks: vec![
                mark("a", 0.0, "pair"),
                mark("b", 20.0, "pair"),
                mark("c", 40.0, ""),
            ],
            ..Artwork::default()
        }
    }

    #[test]
    fn a_click_picks_the_whole_group_unless_inside_it() {
        let art = art();
        assert_eq!(pick(&art, Point::new(5.0, 5.0), false), ["a", "b"]);
        assert_eq!(pick(&art, Point::new(5.0, 5.0), true), ["a"]);
        assert_eq!(marquee(&art, Rect::new(35.0, -5.0, 60.0, 5.0)), ["c"]);
        assert_eq!(
            marquee(&art, Rect::new(15.0, -5.0, 60.0, 5.0)),
            ["a", "b", "c"]
        );
    }

    #[test]
    fn a_pure_move_reads_as_a_move() {
        let Some(CanvasAction::EditMarks(edit)) =
            transform(vec!["a".into()], Affine::translate((3.0, 4.0)))
        else {
            panic!("an edit")
        };
        assert_eq!(edit.move_by, Some([3.0, 4.0]));
        assert!(edit.transform.is_none());
        assert!(transform(vec!["a".into()], Affine::IDENTITY).is_none());
        let Some(CanvasAction::EditMarks(edit)) =
            transform(vec!["a".into()], Affine::scale_non_uniform(2.0, 1.0))
        else {
            panic!("an edit")
        };
        assert!(edit.transform.is_some());
    }

    #[test]
    fn copies_get_fresh_ids_and_their_own_group() {
        let art = art();
        let text = copy(&art, &["a".into(), "b".into()]).unwrap();
        let (actions, ids) = paste(&art, &text, Vec2::new(10.0, 10.0), None).unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.iter().all(|id| art.mark(id).is_none()));
        let [CanvasAction::Draw(draw)] = actions.as_slice() else {
            panic!("one draw: {actions:?}")
        };
        assert_eq!(draw.layer.as_deref(), Some("ink"));
        let group = draw.marks[0].group.clone().unwrap();
        assert_ne!(group, "pair");
        assert_eq!(draw.marks[1].group.as_deref(), Some(group.as_str()));
        assert_eq!(
            draw.marks[0].shape,
            Shape::Rect {
                x: 10.0,
                y: 10.0,
                width: 10.0,
                height: 10.0,
                radius: 0.0
            }
        );
        assert!(paste(&art, "hello", Vec2::ZERO, None).is_none());
    }

    #[test]
    fn grouping_names_a_free_group() {
        let art = art();
        let Some(CanvasAction::EditMarks(edit)) = group(&art, &["c".into(), "a".into()]) else {
            panic!("a group edit")
        };
        assert_eq!(edit.group.as_deref(), Some("group-1"));
        let Some(CanvasAction::EditMarks(edit)) = ungroup(&art, &["a".into(), "c".into()]) else {
            panic!("an ungroup edit")
        };
        assert_eq!(edit.ids, ["a"]);
        assert_eq!(edit.group.as_deref(), Some(""));
    }

    #[test]
    fn a_turned_mark_is_framed_along_its_sides() {
        let mut art = art();
        art.marks[2].transform = Some(Affine::rotate_about(0.5, Point::new(45.0, 5.0)).as_coeffs());
        let (bounds, angle) = frame(&art, &["c".into()]).unwrap();
        assert!((angle - 0.5).abs() < 1e-9);
        assert!((bounds.width() - 10.0).abs() < 1e-6, "{bounds:?}");
        assert!(
            bounds.center().distance(Point::new(45.0, 5.0)) < 1e-6,
            "{bounds:?}"
        );
    }
}
