//! Two people changing one mark at once: each keeps the field they changed.
//!
//! A mark's shape and style, a layer's adjustments and the ground were each
//! stored as one value, so whoever saved last wrote back the fields the other
//! person had just changed. Each field is stored on its own now; colours,
//! names, path data and point lists are stored whole, so two people setting
//! one at once get one of their values, never a mix of both.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::actions::{Draw, EditLayer, EditMarks, NewMark, SetArtwork, Taught};
use canvas::model::{Adjust, Artwork, Shape, Style};
use ui::automerge::{ROOT, ReadDoc as _, ScalarValue, Value, transaction::Transactable as _};
use ui::tilo_resource::{Doc, Replica, Resource as _, evolution};

const TREE: &str = "tree";

fn text(x: f64, width: Option<f64>) -> Shape {
    Shape::Text {
        x,
        y: 40.0,
        text: "Hills".into(),
        size: 24.0,
        font: Some("serif".into()),
        weight: None,
        italic: false,
        align: None,
        line_height: None,
        width,
        tracking: None,
        path: None,
    }
}

/// An artwork with one layer and one mark, `tree`, of `shape`.
fn drawn(shape: Shape) -> Replica<Artwork> {
    let mut replica = Replica::<Artwork>::new();
    replica
        .apply(Artwork::DRAW.with(Draw {
            layer: None,
            marks: vec![NewMark {
                id: Some(TREE.into()),
                fill: Some("#204020".into()),
                ..NewMark::new(shape)
            }],
        }))
        .unwrap();
    replica
}

fn rect() -> Shape {
    Shape::Rect {
        x: 10.0,
        y: 10.0,
        width: 200.0,
        height: 120.0,
        radius: 0.0,
    }
}

fn edit(replica: &mut Replica<Artwork>, edit: EditMarks) {
    replica
        .apply(Artwork::EDIT_MARKS.with(EditMarks {
            ids: vec![TREE.to_owned()],
            ..edit
        }))
        .unwrap();
}

/// `ours` and `theirs` made at once on two replicas of `start`: the artwork
/// both read once they have merged.
fn merged(
    mut start: Replica<Artwork>,
    ours: impl FnOnce(&mut Replica<Artwork>),
    theirs: impl FnOnce(&mut Replica<Artwork>),
) -> Artwork {
    let mut other = start.fork();
    ours(&mut start);
    theirs(&mut other);
    start.sync_with(&mut other).unwrap();
    let art = start.state().unwrap();
    assert_eq!(art, other.state().unwrap(), "both replicas read alike");
    art
}

fn style(art: &Artwork) -> &Style {
    &art.mark(TREE).unwrap().style
}

#[test]
fn a_fill_and_an_opacity_changed_at_once_both_survive() {
    let art = merged(
        drawn(rect()),
        |ours| {
            edit(
                ours,
                EditMarks {
                    fill: Some("#cc2222".into()),
                    ..EditMarks::default()
                },
            );
        },
        |theirs| {
            edit(
                theirs,
                EditMarks {
                    opacity: Some(0.4),
                    ..EditMarks::default()
                },
            );
        },
    );
    assert_eq!(style(&art).fill.as_deref(), Some("#cc2222"));
    assert_eq!(style(&art).opacity, Some(0.4));
}

#[test]
fn two_colours_picked_at_once_are_one_of_them() {
    let art = merged(
        drawn(rect()),
        |ours| {
            edit(
                ours,
                EditMarks {
                    fill: Some("#cc2222".into()),
                    ..EditMarks::default()
                },
            );
        },
        |theirs| {
            edit(
                theirs,
                EditMarks {
                    fill: Some("#2222cc".into()),
                    ..EditMarks::default()
                },
            );
        },
    );
    let fill = style(&art).fill.as_deref();
    assert!(
        [Some("#cc2222"), Some("#2222cc")].contains(&fill),
        "one of the two colours, not a mix: {fill:?}"
    );
}

#[test]
fn two_qualities_of_one_adjustment_changed_at_once_both_survive() {
    let mut start = drawn(rect());
    // The adjustments exist before both change them: two people creating an
    // absent record at once choose one of their records.
    edit(
        &mut start,
        EditMarks {
            adjust: Some(Adjust {
                contrast: Some(0.1),
                ..Adjust::default()
            }),
            ..EditMarks::default()
        },
    );
    let adjust = |exposure, saturation| EditMarks {
        adjust: Some(Adjust {
            exposure,
            saturation,
            ..Adjust::default()
        }),
        ..EditMarks::default()
    };
    let art = merged(
        start,
        |ours| edit(ours, adjust(Some(0.8), None)),
        |theirs| edit(theirs, adjust(None, Some(-0.5))),
    );
    let adjust = style(&art).adjust.clone().unwrap();
    assert_eq!(adjust.exposure, Some(0.8));
    assert_eq!(adjust.saturation, Some(-0.5));
    assert_eq!(adjust.contrast, Some(0.1));
}

#[test]
fn a_texts_position_and_its_width_changed_at_once_both_survive() {
    let reshape = |shape| EditMarks {
        shape: Some(Taught(shape)),
        ..EditMarks::default()
    };
    let art = merged(
        drawn(text(10.0, None)),
        |ours| edit(ours, reshape(text(90.0, None))),
        |theirs| edit(theirs, reshape(text(10.0, Some(300.0)))),
    );
    assert_eq!(art.mark(TREE).unwrap().shape, text(90.0, Some(300.0)));
}

#[test]
fn two_outlines_drawn_at_once_are_one_of_them() {
    let polygon = |points: &[[f64; 2]]| EditMarks {
        shape: Some(Taught(Shape::Polygon {
            points: points.to_vec(),
        })),
        ..EditMarks::default()
    };
    let ours = [[0.0, 0.0], [100.0, 0.0], [50.0, 80.0]];
    let theirs = [[10.0, 10.0], [90.0, 10.0], [90.0, 90.0], [10.0, 90.0]];
    let art = merged(
        drawn(Shape::Polygon {
            points: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
        }),
        |replica| edit(replica, polygon(&ours)),
        |replica| edit(replica, polygon(&theirs)),
    );
    let Shape::Polygon { points } = &art.mark(TREE).unwrap().shape else {
        panic!("still a polygon");
    };
    assert!(
        points.as_slice() == ours || points.as_slice() == theirs,
        "one of the two outlines, not a mix: {points:?}"
    );
}

#[test]
fn a_grounds_texture_and_colour_changed_at_once_both_survive() {
    let art = merged(
        drawn(rect()),
        |ours| {
            ours.apply(Artwork::SET_ARTWORK.with(SetArtwork {
                texture: Some("linen".into()),
                ..SetArtwork::default()
            }))
            .unwrap();
        },
        |theirs| {
            theirs
                .apply(Artwork::SET_ARTWORK.with(SetArtwork {
                    ground_colour: Some("#202020".into()),
                    ..SetArtwork::default()
                }))
                .unwrap();
        },
    );
    assert_eq!(art.ground.texture, "linen");
    assert_eq!(art.ground.colour, "#202020");
}

/// Store `value` under `key` of `obj` as one JSON string, as version 1 did.
fn as_json(doc: &mut Doc, obj: &ui::automerge::ObjId, key: &str, value: &impl serde::Serialize) {
    doc.put(obj, key, serde_json::to_string(value).unwrap())
        .unwrap();
}

/// The document of `replica` as version 1 stored it: each mark's shape and
/// style, each layer's adjustments and the ground as one JSON string, and
/// no storage version.
fn version_one(replica: &mut Replica<Artwork>) -> Doc {
    let art = replica.state().unwrap();
    let mut doc = replica.doc_mut().fork();
    for (id, mark) in evolution::map_values(&doc, &ROOT, "marks").unwrap() {
        let value = art.mark(&id).unwrap().clone();
        as_json(&mut doc, &mark, "shape", &value.shape);
        as_json(&mut doc, &mark, "style", &value.style);
    }
    for (id, layer) in evolution::map_values(&doc, &ROOT, "layers").unwrap() {
        if let Some(adjust) = &art.layer(&id).unwrap().adjust {
            as_json(&mut doc, &layer, "adjust", adjust);
        }
    }
    as_json(&mut doc, &ROOT, "ground", &art.ground);
    doc.delete(&ROOT, evolution::VERSION_KEY).unwrap();
    doc
}

#[test]
fn a_version_one_artwork_reads_as_it_did_once_migrated() {
    let mut replica = drawn(text(10.0, Some(300.0)));
    replica
        .apply(Artwork::DRAW.with(Draw {
            layer: None,
            marks: vec![NewMark {
                id: Some("hill".into()),
                stroke: Some("#808080".into()),
                ..NewMark::new(Shape::Stroke {
                    points: vec![vec![0.0, 0.0, 0.5], vec![40.0, 30.0, 1.0]],
                    size: 6.0,
                    forks: None,
                    spread: None,
                    dabs: None,
                    scatter: None,
                    dab_length: None,
                    hatch: None,
                })
            }],
        }))
        .unwrap();
    edit(
        &mut replica,
        EditMarks {
            adjust: Some(Adjust {
                curve: Some(vec![[0.05, 0.0], [0.95, 1.0]]),
                ..Adjust::default()
            }),
            dash: Some(vec![4.0, 2.0]),
            ..EditMarks::default()
        },
    );
    let layer = replica.state().unwrap().layers[0].id.clone();
    replica
        .apply(Artwork::EDIT_LAYER.with(EditLayer {
            id: layer,
            adjust: Some(Taught(Box::new(Adjust {
                hue: Some(20.0),
                ..Adjust::default()
            }))),
            ..EditLayer::default()
        }))
        .unwrap();
    let art = replica.state().unwrap();
    assert!(art.layers[0].adjust.is_some());
    let mut doc = version_one(&mut replica);
    assert!(Artwork::read(&doc).is_err(), "version 1 is migrated first");

    assert!(evolution::migrate::<Artwork>(&mut doc).unwrap());
    assert_eq!(
        Replica::<Artwork>::from_doc(doc.fork()).state().unwrap(),
        art,
        "it reads as it did"
    );
    let (_, mark) = evolution::map_values(&doc, &ROOT, "marks")
        .unwrap()
        .into_iter()
        .find(|(id, _)| id == TREE)
        .unwrap();
    for key in ["shape", "style"] {
        assert!(
            matches!(doc.get(&mark, key).unwrap(), Some((Value::Object(_), _))),
            "the mark's {key} is stored as its fields"
        );
    }
    let (_, style) = doc.get(&mark, "style").unwrap().unwrap();
    assert!(
        matches!(
            doc.get(&style, "fill").unwrap(),
            Some((Value::Scalar(fill), _)) if *fill == ScalarValue::Str("#204020".into())
        ),
        "a colour is one string, written whole"
    );
}
