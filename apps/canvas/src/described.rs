//! What history calls each edit: the step it makes, with what it made it
//! on, in the words a person scanning the history panel and an agent
//! reading `history` both follow: "Recolour 2 marks", "Move Sun", "Brush
//! size 24 for a stroke", "Hide Sky". Read from the artwork as it is
//! before the edit applies, so a mark is named as it was.

use ui::automerge::ROOT;
use ui::tilo_resource::Doc;

use crate::actions::{CanvasAction, EditLayer, EditMarks};
use crate::doc::{LAYERS, MARKS, child, text};
use crate::model::{MarkBody, Shape};

/// The history name of `action` on `doc`, or `None` for its action's own.
pub fn label(action: &CanvasAction, doc: &Doc) -> Option<String> {
    let names = Names { doc };
    Some(match action {
        CanvasAction::Paint(paint) => {
            let erasing = paint
                .strokes
                .iter()
                .all(|stroke| stroke.brush.as_deref() == Some("eraser"));
            if erasing {
                "Erase".to_owned()
            } else {
                count(
                    paint.strokes.len(),
                    "brush stroke",
                    "brush strokes",
                    "Paint",
                )
            }
        }
        CanvasAction::Draw(draw) => match draw.marks.as_slice() {
            [mark] => match &mark.shape {
                Shape::Text { text, .. } => format!("Add text {}", quoted(text)),
                Shape::Image { .. } => "Place picture".to_owned(),
                shape => format!("Draw {}", kind(shape)),
            },
            marks => count(marks.len(), "shape", "shapes", "Draw"),
        },
        CanvasAction::Chart(chart) => match chart.title.as_deref() {
            Some(title) if !title.is_empty() => format!("Draw chart {}", quoted(title)),
            _ => "Draw chart".to_owned(),
        },
        CanvasAction::EditMarks(edit) => edit_marks(edit, &names),
        CanvasAction::RemoveMarks(remove) => {
            if remove.along.is_some() {
                "Erase strokes".to_owned()
            } else if let Some(layer) = &remove.layer {
                format!("Clear {}", names.layer(layer))
            } else {
                format!("Delete {}", names.marks(&remove.ids))
            }
        }
        CanvasAction::Arrange(arrange) => {
            let marks = names.marks(&arrange.ids);
            match arrange.to.as_str() {
                "front" => format!("Bring {marks} to front"),
                "back" => format!("Send {marks} to back"),
                "forward" => format!("Bring {marks} forward"),
                "backward" => format!("Send {marks} backward"),
                _ => format!("Restack {marks}"),
            }
        }
        CanvasAction::Align(align) => {
            let marks = names.marks(&align.ids);
            match (&align.edge, &align.distribute) {
                (_, Some(axis)) => format!("Space {marks} {axis}ly"),
                (Some(edge), None) if align.to.as_deref() == Some("artwork") => {
                    format!("Align {marks} {edge} on the artwork")
                }
                (Some(edge), None) => format!("Align {marks} {edge}"),
                (None, None) => return None,
            }
        }
        CanvasAction::AddLayer(add) => {
            if add.name.trim().is_empty() {
                let names = names.layer_names();
                let name = crate::actions::numbered_name(names.iter().map(String::as_str), "Layer");
                format!("Add {name}")
            } else {
                format!("Add layer {}", add.name)
            }
        }
        CanvasAction::EditLayer(edit) => edit_layer(edit, &names)?,
        CanvasAction::RemoveLayer(remove) => format!("Remove {}", names.layer(&remove.id)),
        CanvasAction::UseSelection(using) => match using.what.as_str() {
            "fill" => "Fill the selection".to_owned(),
            "stroke" => "Outline the selection".to_owned(),
            "mask" => match &using.layer {
                Some(layer) => format!("Mask {} to the selection", names.layer(layer)),
                None => "Mask to the selection".to_owned(),
            },
            "clear" => "Clear the selection".to_owned(),
            _ => return None,
        },
        CanvasAction::Combine(combine) => {
            let shapes = names.marks(&combine.ids);
            match combine.op.as_str() {
                "union" => format!("Unite {shapes}"),
                "subtract" => format!("Subtract {shapes}"),
                "intersect" => format!("Intersect {shapes}"),
                "exclude" => format!("Exclude {shapes}"),
                _ => return None,
            }
        }
        other => return area_or_artwork(other),
    })
}

/// Changes of the selected area and of the artwork itself.
fn area_or_artwork(action: &CanvasAction) -> Option<String> {
    Some(match action {
        CanvasAction::Select(select) => {
            if select.mode.as_deref() == Some("none") {
                "Deselect".to_owned()
            } else if select.invert == Some(true) {
                "Invert the selection".to_owned()
            } else if select.grow.is_some_and(|grow| grow < 0.0) {
                "Shrink the selection".to_owned()
            } else if select.grow.is_some() {
                "Grow the selection".to_owned()
            } else if select.feather.is_some() {
                "Soften the selection".to_owned()
            } else {
                return None;
            }
        }
        CanvasAction::SetArtwork(set) => {
            if let Some(title) = &set.title {
                format!("Title {}", quoted(title))
            } else if set.crop.is_some() || set.fit.is_some() {
                "Crop the artwork".to_owned()
            } else if let (Some(width), Some(height)) = (set.width, set.height) {
                format!("Artwork size {width:.0} by {height:.0}")
            } else if set.texture.is_some() || set.ground_colour.is_some() {
                "Change the ground".to_owned()
            } else {
                return None;
            }
        }
        _ => return None,
    })
}

/// "Paint a brush stroke", "Paint 3 brush strokes".
fn count(n: usize, one: &str, many: &str, verb: &str) -> String {
    if n == 1 {
        format!("{verb} a {one}")
    } else {
        format!("{verb} {n} {many}")
    }
}

/// Up to a few words of what a text says, in quotes.
fn quoted(text: &str) -> String {
    const MOST: usize = 24;
    let line = text.lines().next().unwrap_or_default().trim();
    if line.chars().count() > MOST {
        let cut: String = line.chars().take(MOST).collect();
        format!("“{}…”", cut.trim_end())
    } else {
        format!("“{line}”")
    }
}

/// What kind of mark a shape makes.
fn kind(shape: &Shape) -> &'static str {
    match shape {
        Shape::Rect { .. } => "rectangle",
        Shape::Ellipse { .. } => "ellipse",
        Shape::Polygon { .. } => "polygon",
        Shape::Line { .. } => "line",
        Shape::Path { .. } => "path",
        Shape::Text { .. } => "text",
        Shape::Stroke { .. } => "stroke",
        Shape::Image { .. } => "picture",
        #[allow(unreachable_patterns, reason = "a shape added later is still a shape")]
        _ => "shape",
    }
}

/// Reads names from the artwork as it is.
struct Names<'a> {
    doc: &'a Doc,
}

impl Names<'_> {
    fn body(&self, id: &str) -> Option<MarkBody> {
        let marks = child(self.doc, &ROOT, MARKS)?;
        let mark = child(self.doc, &marks, id)?;
        crate::doc::json(self.doc, &mark, "body")
    }

    /// One mark by its name, or by what it is ("Sun", "a rectangle", the
    /// text it says); several by how many and of what kind.
    fn marks(&self, ids: &[String]) -> String {
        let bodies: Vec<Option<MarkBody>> = ids.iter().map(|id| self.body(id)).collect();
        match (ids, bodies.as_slice()) {
            ([], _) => "nothing".to_owned(),
            ([id], [body]) => match body {
                Some(body) if !body.name.trim().is_empty() => body.name.trim().to_owned(),
                Some(body) => match &body.shape {
                    Shape::Text { text, .. } => quoted(text),
                    shape => format!("a {}", kind(shape)),
                },
                // A group, named by its id.
                None => id.clone(),
            },
            (ids, bodies) => {
                let kinds: Vec<&str> = bodies
                    .iter()
                    .map(|body| body.as_ref().map_or("mark", |body| kind(&body.shape)))
                    .collect();
                let same = kinds.windows(2).all(|pair| pair.first() == pair.last());
                let noun = match kinds.first() {
                    Some(&"stroke") if same => "strokes",
                    Some(&"text") if same => "texts",
                    Some(&"picture") if same => "pictures",
                    _ => "marks",
                };
                format!("{} {noun}", ids.len())
            }
        }
    }

    fn layer(&self, id: &str) -> String {
        child(self.doc, &ROOT, LAYERS)
            .and_then(|layers| child(self.doc, &layers, id))
            .and_then(|layer| text(self.doc, &layer, "name"))
            .unwrap_or_else(|| id.to_owned())
    }

    fn layer_names(&self) -> Vec<String> {
        let Some(layers) = child(self.doc, &ROOT, LAYERS) else {
            return Vec::new();
        };
        crate::doc::ids(self.doc, Some(&layers))
            .into_iter()
            .filter_map(|id| text(self.doc, &child(self.doc, &layers, &id)?, "name"))
            .collect()
    }
}

/// An `edit-marks` named by the first thing it changes, in the order a
/// person thinks of them.
fn edit_marks(edit: &EditMarks, names: &Names<'_>) -> String {
    let marks = names.marks(&edit.ids);
    if let Some(name) = &edit.name {
        return format!("Rename {marks} to {}", quoted(name));
    }
    if edit.fill.is_some() || edit.stroke.is_some() || edit.gradient.is_some() {
        return format!("Recolour {marks}");
    }
    if let Some(size) = edit.size.or(edit.brush_size) {
        return format!("Brush size {size:.0} for {marks}");
    }
    if let Some(layer) = &edit.layer {
        return format!("Move {marks} to {}", names.layer(layer));
    }
    if let Some(group) = &edit.group {
        return if group.is_empty() {
            format!("Ungroup {marks}")
        } else {
            format!("Group {marks}")
        };
    }
    if edit.move_by.is_some() {
        return format!("Move {marks}");
    }
    if let Some([a, b, c, d, _, _]) = edit.transform {
        let still = |x: f64, to: f64| (x - to).abs() < 1e-9;
        return if still(a, 1.0) && still(b, 0.0) && still(c, 0.0) && still(d, 1.0) {
            format!("Move {marks}")
        } else if still(a * a + b * b, 1.0) && still(c * c + d * d, 1.0) {
            format!("Rotate {marks}")
        } else {
            format!("Resize {marks}")
        };
    }
    if edit.scale.is_some() {
        return format!("Resize {marks}");
    }
    if edit.rotate.is_some() {
        return format!("Rotate {marks}");
    }
    if edit.flip.is_some() {
        return format!("Flip {marks}");
    }
    if let Some(opacity) = edit.opacity {
        return format!("Opacity {:.0}% for {marks}", opacity * 100.0);
    }
    let verbs = [
        (edit.crop.is_some(), "Crop"),
        (edit.straighten.is_some(), "Straighten"),
        (edit.adjust.is_some(), "Adjust"),
        (edit.smooth.is_some(), "Smooth"),
        (edit.simplify.is_some(), "Simplify"),
        (edit.extend.is_some(), "Extend"),
        (edit.split.is_some(), "Split"),
        (edit.merge == Some(true), "Merge"),
        (edit.shape.is_some(), "Reshape"),
        (edit.brush.is_some(), "Rebrush"),
        (edit.clip.is_some(), "Clip"),
        (
            edit.shadow.is_some() || edit.inner_shadow.is_some(),
            "Shade",
        ),
        (
            edit.stroke_width.is_some()
                || edit.dash.is_some()
                || edit.cap.is_some()
                || edit.join.is_some(),
            "Restyle the outline of",
        ),
        (edit.blend.is_some(), "Blend"),
        (edit.grain.is_some() || edit.feather.is_some(), "Texture"),
    ];
    verbs.iter().find(|(changed, _)| *changed).map_or_else(
        || format!("Edit {marks}"),
        |(_, verb)| format!("{verb} {marks}"),
    )
}

/// An `edit-layer` named by what it changes.
fn edit_layer(edit: &EditLayer, names: &Names<'_>) -> Option<String> {
    let layer = names.layer(&edit.id);
    Some(if let Some(name) = &edit.name {
        format!("Rename {layer} to {}", quoted(name))
    } else if let Some(visible) = edit.visible {
        format!("{} {layer}", if visible { "Show" } else { "Hide" })
    } else if let Some(locked) = edit.locked {
        format!("{} {layer}", if locked { "Lock" } else { "Unlock" })
    } else if let Some(opacity) = edit.opacity {
        format!("Opacity {:.0}% for {layer}", opacity * 100.0)
    } else if let Some(to) = &edit.to {
        match to.as_str() {
            "front" | "forward" => format!("Raise {layer}"),
            _ => format!("Lower {layer}"),
        }
    } else if let Some(mask) = &edit.mask {
        match mask.as_str() {
            "remove" => format!("Remove the mask of {layer}"),
            "unlink" => format!("Keep the mask of {layer} in place"),
            "link" => format!("Move the mask with {layer}"),
            _ => format!("Mask {layer}"),
        }
    } else if edit.invert.is_some() {
        format!("Invert the mask of {layer}")
    } else if let Some(clip) = edit.clip {
        if clip {
            format!("Clip {layer} to the layer below")
        } else {
            format!("Unclip {layer}")
        }
    } else if let Some(medium) = &edit.medium {
        if medium.is_empty() {
            format!("Make {layer} plain")
        } else {
            format!("Paint {layer} in {medium}")
        }
    } else if edit.adjust.is_some() {
        format!("Adjust {layer}")
    } else if edit.texture.is_some() {
        format!("Texture {layer}")
    } else {
        return None;
    })
}

#[cfg(test)]
mod tests {
    use ui::tilo_actions::Action as _;

    use super::*;
    use crate::actions::{AddLayer, Draw, NewMark};

    fn doc_with_sun() -> Doc {
        let mut doc = Doc::new();
        <crate::resource::ArtworkResource as ui::tilo_resource::Resource>::initialize(&mut doc)
            .expect("a new artwork");
        CanvasAction::Draw(Draw {
            layer: None,
            marks: vec![
                NewMark {
                    id: Some("sun".into()),
                    name: Some("Sun".into()),
                    ..NewMark::new(Shape::Ellipse {
                        cx: 10.0,
                        cy: 10.0,
                        rx: 5.0,
                        ry: 5.0,
                    })
                },
                NewMark {
                    id: Some("box".into()),
                    ..NewMark::new(Shape::Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 4.0,
                        height: 4.0,
                        radius: 0.0,
                    })
                },
            ],
        })
        .apply(&mut doc)
        .expect("drawn");
        doc
    }

    #[test]
    fn edits_are_named_by_what_they_did_and_to_what() {
        let doc = doc_with_sun();
        let edit = |edit: EditMarks| label(&CanvasAction::edit(edit), &doc);
        assert_eq!(
            edit(EditMarks {
                ids: vec!["sun".into(), "box".into()],
                fill: Some("#ff0000".into()),
                ..EditMarks::default()
            })
            .as_deref(),
            Some("Recolour 2 marks")
        );
        assert_eq!(
            edit(EditMarks {
                ids: vec!["sun".into()],
                move_by: Some([4.0, 0.0]),
                ..EditMarks::default()
            })
            .as_deref(),
            Some("Move Sun")
        );
        assert_eq!(
            edit(EditMarks {
                ids: vec!["box".into()],
                size: Some(24.0),
                ..EditMarks::default()
            })
            .as_deref(),
            Some("Brush size 24 for a rectangle")
        );
        assert_eq!(
            label(&CanvasAction::AddLayer(AddLayer::default()), &doc).as_deref(),
            Some("Add Layer 2")
        );
        assert_eq!(
            label(
                &CanvasAction::EditLayer(EditLayer {
                    id: "layer-1".into(),
                    visible: Some(false),
                    ..EditLayer::default()
                }),
                &doc
            )
            .as_deref(),
            Some("Hide Layer 1")
        );
    }
}
