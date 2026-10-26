//! Two people working on one artwork at once, and what an undo takes back.
//!
//! A mark's style and adjustments used to be one value each, so changing a
//! mark's fill wrote its opacity too: whoever saved last erased the other
//! person's change. They are fields of their own now, which these tests hold
//! to: two replicas changing different things both keep their change, and an
//! undo takes back only what its step wrote.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::actions::{Draw, EditMarks, NewMark};
use canvas::model::{Adjust, Artwork, Ground, Shape};
use serde_json::json;
use ui::automerge::{ObjType, ROOT, ReadDoc, Value};
use ui::prelude::history_source::{AuthorDirectory, revert_step};
use ui::tilo_resource::{Edit, EditOp, Id, Invoke, Model, Replica, Segment, Slot};

/// An artwork with one layer and one named rectangle on it.
fn drawn() -> Replica<Artwork> {
    let mut replica = Replica::<Artwork>::new();
    replica
        .apply(Artwork::DRAW.with(Draw {
            layer: None,
            marks: vec![NewMark {
                id: Some("tree".into()),
                fill: Some("#204020".into()),
                ..NewMark::new(Shape::Rect {
                    x: 10.0,
                    y: 10.0,
                    width: 200.0,
                    height: 120.0,
                    radius: 0.0,
                })
            }],
        }))
        .expect("the mark draws");
    replica
}

/// The artwork above, with its mark's adjustments already made: a contrast
/// step both people then add to.
fn adjusted() -> Replica<Artwork> {
    let mut replica = drawn();
    replica
        .apply(Artwork::EDIT_MARKS.with(EditMarks {
            ids: vec!["tree".to_owned()],
            adjust: Some(Adjust {
                contrast: Some(0.1),
                ..Adjust::default()
            }),
            ..EditMarks::default()
        }))
        .expect("the mark is adjusted");
    replica
}

fn style(art: &Artwork, id: &str) -> canvas::model::Style {
    art.mark(id)
        .expect("the mark is in the artwork")
        .style
        .clone()
}

/// A fill and an opacity changed at once both survive: one person's colour
/// does not carry the other's stale opacity.
#[test]
fn two_people_changing_different_fields_of_one_mark_keep_both() {
    let mut ours = drawn();
    let mut theirs = ours.fork();

    ours.apply(Artwork::SET_FILL.with((vec!["tree".to_owned()], "#cc2222".to_owned())))
        .expect("the fill changes");
    theirs
        .apply(Artwork::SET_OPACITY.with((vec!["tree".to_owned()], 0.4)))
        .expect("the opacity changes");
    ours.sync_with(&mut theirs).expect("the changes merge");

    let style = style(&ours.state().expect("the artwork reads"), "tree");
    assert_eq!(style.fill.as_deref(), Some("#cc2222"), "the fill is kept");
    assert_eq!(
        style.opacity,
        Some(0.4),
        "the opacity the other person set is kept"
    );
}

/// Two people adjusting different qualities of one mark both keep theirs: an
/// exposure and a saturation are not one value. The mark is adjusted first —
/// its adjustments already exist — because two people *creating* the same
/// absent record at the same moment is a last-writer-wins choice of object,
/// not a merge (see `a_first_adjustment_is_one_value_either_way` below).
#[test]
fn two_people_adjusting_different_qualities_keep_both() {
    let mut ours = adjusted();
    let mut theirs = ours.fork();

    let adjust = |ours: &mut Replica<Artwork>, step: Adjust| {
        ours.apply(Artwork::EDIT_MARKS.with(EditMarks {
            ids: vec!["tree".to_owned()],
            adjust: Some(step),
            ..EditMarks::default()
        }))
        .expect("the adjustment applies");
    };
    adjust(
        &mut ours,
        Adjust {
            exposure: Some(0.8),
            ..Adjust::default()
        },
    );
    adjust(
        &mut theirs,
        Adjust {
            saturation: Some(-0.5),
            ..Adjust::default()
        },
    );
    ours.sync_with(&mut theirs).expect("the changes merge");

    let style = style(&ours.state().expect("the artwork reads"), "tree");
    let adjust = style.adjust.expect("the mark is adjusted");
    assert_eq!(adjust.exposure, Some(0.8), "the exposure is kept");
    assert_eq!(adjust.saturation, Some(-0.5), "the saturation is kept");
    assert_eq!(
        adjust.contrast,
        Some(0.1),
        "the adjustment both started from is kept too"
    );
}

/// Creating the same missing record at once is one value, not a merge: this
/// is what a document model can do, and it is written down so nobody reads
/// the test above as more than it says.
#[test]
fn a_first_adjustment_is_one_value_either_way() {
    let mut ours = drawn();
    let mut theirs = ours.fork();
    let adjust = |replica: &mut Replica<Artwork>, step: Adjust| {
        replica
            .apply(Artwork::EDIT_MARKS.with(EditMarks {
                ids: vec!["tree".to_owned()],
                adjust: Some(step),
                ..EditMarks::default()
            }))
            .expect("the adjustment applies");
    };
    adjust(
        &mut ours,
        Adjust {
            exposure: Some(0.8),
            ..Adjust::default()
        },
    );
    adjust(
        &mut theirs,
        Adjust {
            saturation: Some(-0.5),
            ..Adjust::default()
        },
    );
    ours.sync_with(&mut theirs).expect("the changes merge");
    let style = style(&ours.state().expect("the artwork reads"), "tree");
    let adjust = style.adjust.expect("one of the two adjustments survives");
    assert!(
        adjust
            == Adjust {
                exposure: Some(0.8),
                ..Adjust::default()
            }
            || adjust
                == Adjust {
                    saturation: Some(-0.5),
                    ..Adjust::default()
                },
        "the first adjustment created the record: one of them is it, not both"
    );
}

/// A shape's fields are edited on their own: a text's position and its
/// width are not one value, so the two replicas keep both.
#[test]
fn two_people_changing_a_shapes_position_and_width_keep_both() {
    let mut ours = drawn();
    let mut theirs = ours.fork();
    let field = |name: &str, value: serde_json::Value| {
        Edit::new(
            vec![
                Segment::Field("marks".into()),
                Segment::Item(Id::from_raw("tree")),
                Segment::Field("shape".into()),
                Segment::Field("rect".into()),
                Segment::Field(name.into()),
            ],
            EditOp::Set { value },
        )
    };
    ours.apply(Invoke::edit(&field("x", json!(30.0))).unwrap())
        .expect("the position changes");
    theirs
        .apply(Invoke::edit(&field("width", json!(320.0))).unwrap())
        .expect("the width changes");
    ours.sync_with(&mut theirs).expect("the changes merge");

    let shape = ours
        .state()
        .expect("the artwork reads")
        .mark("tree")
        .expect("the mark is there")
        .shape
        .clone();
    assert_eq!(
        shape,
        Shape::Rect {
            x: 30.0,
            y: 10.0,
            width: 320.0,
            height: 120.0,
            radius: 0.0,
        },
        "each field keeps the change the other person made"
    );
}

/// Changing a shape's variant is one write of its name; a concurrent edit of
/// a field of the old variant stays as unused history, so undoing the
/// transition brings that edit back rather than an older value.
#[test]
fn undoing_a_variant_change_keeps_a_concurrent_edit_of_the_old_variant() {
    let mut ours = drawn();
    let mut theirs = ours.fork();
    let before_transition = theirs.heads();
    let radius = Edit::new(
        vec![
            Segment::Field("marks".into()),
            Segment::Item(Id::from_raw("tree")),
            Segment::Field("shape".into()),
            Segment::Field("rect".into()),
            Segment::Field("radius".into()),
        ],
        EditOp::Set { value: json!(12.0) },
    );
    ours.apply(Invoke::edit(&radius).unwrap())
        .expect("the corner rounds");
    let path = Shape::Path {
        d: "M 10 10 L 90 90".into(),
    };
    let change = Edit::new(
        vec![
            Segment::Field("marks".into()),
            Segment::Item(Id::from_raw("tree")),
            Segment::Field("shape".into()),
        ],
        EditOp::Set {
            value: path.to_json(),
        },
    );
    theirs
        .apply(Invoke::edit(&change).unwrap())
        .expect("the shape becomes a path");
    let taken = theirs
        .heads()
        .into_iter()
        .find(|head| !before_transition.contains(head))
        .expect("the transition was one change");

    ours.sync_with(&mut theirs).expect("the changes merge");
    assert!(
        matches!(
            ours.state()
                .expect("the artwork reads")
                .mark("tree")
                .unwrap()
                .shape,
            Shape::Path { .. }
        ),
        "the variant change wins while it is in force"
    );
    let (Value::Object(ObjType::Map), marks) = theirs.doc().get(ROOT, "marks").unwrap().unwrap()
    else {
        panic!("the marks are a map");
    };
    let (Value::Object(ObjType::Map), mark) = theirs.doc().get(&marks, "tree").unwrap().unwrap()
    else {
        panic!("a mark is a map");
    };
    let (Value::Object(ObjType::Map), shape) = theirs.doc().get(&mark, "shape").unwrap().unwrap()
    else {
        panic!("the shape names its variant");
    };
    assert!(
        theirs.doc().get(&shape, "radius").unwrap().is_some(),
        "the old variant's concurrent field edit is still stored, unused"
    );

    let kept = revert_step(theirs.doc_mut(), &taken, None, &AuthorDirectory::default())
        .expect("the transition is in the document");
    theirs.doc_mut().commit();
    assert!(kept.kept.is_empty(), "nothing of the undo is overruled");
    ours.sync_with(&mut theirs).expect("the undo merges");
    let shape = ours
        .state()
        .expect("the artwork reads")
        .mark("tree")
        .expect("the mark is there")
        .shape
        .clone();
    assert_eq!(
        shape,
        Shape::Rect {
            x: 10.0,
            y: 10.0,
            width: 200.0,
            height: 120.0,
            radius: 12.0,
        },
        "the old variant's concurrent field edit is still there"
    );
}

/// A ground's texture and its colour are separate choices too.
#[test]
fn two_people_changing_the_grounds_texture_and_colour_keep_both() {
    let mut ours = drawn();
    let mut theirs = ours.fork();

    ours.apply(Artwork::SET_TEXTURE.with("linen".to_owned()))
        .expect("the texture changes");
    theirs
        .apply(Artwork::SET_GROUND.with("#101820".to_owned()))
        .expect("the colour changes");
    ours.sync_with(&mut theirs).expect("the changes merge");

    let art = ours.state().expect("the artwork reads");
    assert_eq!(art.ground.texture, "linen");
    assert_eq!(art.ground.colour, "#101820");
    assert_eq!(
        art.ground,
        Ground {
            texture: "linen".into(),
            colour: "#101820".into(),
        }
    );
}

/// Undoing a step takes back what that step wrote, and only that: the other
/// person's change to another field of the same mark stays.
#[test]
fn an_undo_keeps_the_change_someone_else_made_meanwhile() {
    let mut ours = drawn();
    let mut theirs = ours.fork();

    let heads_before = ours.heads();
    ours.apply(Artwork::SET_FILL.with((vec!["tree".to_owned()], "#cc2222".to_owned())))
        .expect("the fill changes");
    theirs
        .apply(Artwork::SET_OPACITY.with((vec!["tree".to_owned()], 0.4)))
        .expect("the opacity changes");

    // Our own last step, as the app's Ctrl+Z takes it back.
    let taken = ours
        .heads()
        .into_iter()
        .find(|head| !heads_before.contains(head))
        .expect("the fill was one change");
    let kept = revert_step(ours.doc_mut(), &taken, None, &AuthorDirectory::default())
        .expect("the step is in the document");
    ours.doc_mut().commit();
    assert!(kept.kept.is_empty(), "nothing of the undo is overruled");
    assert_eq!(
        style(
            &Artwork::from_doc(ours.doc()).expect("the artwork reads"),
            "tree"
        )
        .fill
        .as_deref(),
        Some("#204020"),
        "the undone fill is back before anything merges"
    );

    ours.sync_with(&mut theirs).expect("the changes merge");
    let art = Artwork::from_doc(ours.doc()).expect("the artwork reads");
    let style = style(&art, "tree");
    assert_eq!(
        style.fill.as_deref(),
        Some("#204020"),
        "the undone fill is back"
    );
    assert_eq!(
        style.opacity,
        Some(0.4),
        "the opacity someone else set is still there"
    );
}

/// What a saved artwork holds does not change: a mark's style is the same
/// JSON object its file had, which imports and exports still read.
#[test]
fn a_marks_style_is_saved_as_the_json_object_it_was() {
    let replica = drawn();
    let art = replica.state().expect("the artwork reads");
    let saved = serde_json::to_value(canvas::document::ArtworkFile::of(&art))
        .expect("the artwork file serialises");
    assert_eq!(saved["marks"][0]["style"]["fill"], json!("#204020"));
    let read: canvas::document::ArtworkFile =
        serde_json::from_value(saved).expect("the artwork file reads back");
    assert_eq!(read.marks[0].body.style, art.mark("tree").unwrap().style);
}

/// The shape a version-1 document was written in: a mark's style, a mark's
/// and a layer's adjustments and the ground as one JSON string each, and
/// every string a text object.
fn version_one() -> serde_json::Value {
    let style = canvas::model::Style {
        fill: Some("#204020".into()),
        stroke_width: 1.0,
        ..canvas::model::Style::default()
    };
    let adjust = Adjust {
        contrast: Some(0.1),
        ..Adjust::default()
    };
    let shape = Shape::Rect {
        x: 10.0,
        y: 10.0,
        width: 200.0,
        height: 120.0,
        radius: 0.0,
    };
    json!({
        "title": { "$text": "Study" },
        "width": 1200.0,
        "height": 900.0,
        "ground": serde_json::to_string(&Ground {
            texture: "linen".into(),
            colour: "#101820".into(),
        })
        .unwrap(),
        "layers": {
            "layer-1": {
                "id": { "$text": "layer-1" },
                "name": { "$text": "Layer 1" },
                "group": { "$text": "" },
                "order": { "$text": "a" },
                "medium": { "$text": "" },
                "opacity": 1.0,
                "visible": true,
                "texture": 1.0,
                "locked": false,
                "adjust": serde_json::to_string(&adjust).unwrap(),
                "mask_of": { "$text": "" },
                "base": 1.0,
                "invert": false,
                "clip": false,
                "unlinked": false,
            },
        },
        "marks": {
            "tree": {
                "id": { "$text": "tree" },
                "layer": { "$text": "layer-1" },
                "order": { "$text": "a" },
                "name": { "$text": "" },
                "shape": serde_json::to_string(&shape).unwrap(),
                "style": serde_json::to_string(&style).unwrap(),
                "group": { "$text": "" },
                "clip": { "$text": "" },
                "area": { "$text": "" },
            },
        },
        "media": {},
    })
}

/// A document written when a mark's style, the adjustments and the ground
/// were one JSON string each is brought up to the storage its model reads
/// now, with nothing else about it changed.
#[test]
fn a_document_from_before_a_style_was_fields_is_migrated_on_open() {
    let mut replica = Replica::<Artwork>::new();
    let doc = replica.doc_mut();
    let before = version_one();
    ui::tilo_resource::evolution::fixture(doc, &ROOT, &before).expect("the old document writes");
    Slot::Key(&ROOT, ui::tilo_resource::evolution::VERSION_KEY)
        .put_scalar(doc, ui::automerge::ScalarValue::Uint(1))
        .expect("the old version writes");
    assert_eq!(ui::tilo_resource::evolution::stored_version(doc), 1);

    // The new model cannot read a string where it now expects fields: the
    // document is unreadable until it is migrated, never silently empty.
    assert!(
        Artwork::from_doc(doc).is_err(),
        "a version-1 document is unreadable until it is migrated"
    );

    let migrated =
        ui::tilo_resource::evolution::migrate::<Artwork>(doc).expect("the document migrates");
    assert!(migrated, "the document was older than the app");
    assert_eq!(ui::tilo_resource::evolution::stored_version(doc), 3);

    let art = Artwork::from_doc(doc).expect("the migrated artwork reads");
    let mark = art.mark("tree").expect("the mark is back");
    assert_eq!(mark.style.fill.as_deref(), Some("#204020"));
    assert_eq!(mark.style.stroke_width, 1.0);
    assert_eq!(
        mark.style.adjust, None,
        "a mark with no adjustments keeps none"
    );
    assert_eq!(art.ground.texture, "linen");
    assert_eq!(art.ground.colour, "#101820");
    assert_eq!(
        mark.shape,
        Shape::Rect {
            x: 10.0,
            y: 10.0,
            width: 200.0,
            height: 120.0,
            radius: 0.0,
        },
        "the shape the old file held is the shape now read"
    );
    assert_eq!(
        art.layer("layer-1").expect("the layer is back").adjust,
        Some(Adjust {
            contrast: Some(0.1),
            ..Adjust::default()
        }),
        "a layer's adjustments are what it had"
    );
    assert_eq!(art.settled(), art, "nothing else moved");

    // And the stored form is a map of fields now.
    let (Value::Object(ObjType::Map), marks) = doc.get(&ROOT, "marks").unwrap().unwrap() else {
        panic!("the marks are a map");
    };
    let (Value::Object(ObjType::Map), mark) = doc.get(&marks, "tree").unwrap().unwrap() else {
        panic!("a mark is a map of its fields");
    };
    assert!(
        matches!(
            doc.get(&mark, "style"),
            Ok(Some((Value::Object(ObjType::Map), _)))
        ),
        "the style is a map of its fields now"
    );
    let (Value::Object(ObjType::Map), shape) = doc.get(&mark, "shape").unwrap().unwrap() else {
        panic!("the shape is a map naming its variant");
    };
    assert_eq!(
        doc.get(&shape, ui::tilo_resource::VARIANT_KEY)
            .unwrap()
            .and_then(|(value, _)| match value {
                Value::Scalar(scalar) => match scalar.as_ref() {
                    ui::automerge::ScalarValue::Str(name) => Some(name.to_string()),
                    _ => None,
                },
                _ => None,
            })
            .as_deref(),
        Some("rect"),
        "the old shape string is the data enum's map now"
    );
}

/// The shape a version-2 document was written in: its style, adjustments and
/// ground are field maps already, but each mark's shape is still one JSON
/// string.
fn version_two() -> serde_json::Value {
    let mut before = version_one();
    let style = canvas::model::Style {
        fill: Some("#204020".into()),
        stroke_width: 1.0,
        ..canvas::model::Style::default()
    };
    let adjust = Adjust {
        contrast: Some(0.1),
        ..Adjust::default()
    };
    before["ground"] = serde_json::to_value(Ground {
        texture: "linen".into(),
        colour: "#101820".into(),
    })
    .unwrap();
    before["layers"]["layer-1"]["adjust"] = serde_json::to_value(&adjust).unwrap();
    before["marks"]["tree"]["style"] = serde_json::to_value(&style).unwrap();
    before
}

/// A version-2 document, whose style and ground are already fields but whose
/// shape is still one string, is brought to version 3 through the artwork's
/// production resource migration.
#[test]
fn a_document_from_before_a_shape_was_fields_is_migrated_on_open() {
    let mut replica = Replica::<Artwork>::new();
    let doc = replica.doc_mut();
    let before = version_two();
    ui::tilo_resource::evolution::fixture(doc, &ROOT, &before).expect("the old document writes");
    Slot::Key(&ROOT, ui::tilo_resource::evolution::VERSION_KEY)
        .put_scalar(doc, ui::automerge::ScalarValue::Uint(2))
        .expect("the old version writes");
    assert_eq!(ui::tilo_resource::evolution::stored_version(doc), 2);
    assert!(
        Artwork::from_doc(doc).is_err(),
        "a version-2 shape is unreadable until it is migrated"
    );

    assert!(
        ui::tilo_resource::evolution::migrate::<Artwork>(doc).expect("the document migrates"),
        "the document was older than the app"
    );
    assert_eq!(ui::tilo_resource::evolution::stored_version(doc), 3);
    let art = Artwork::from_doc(doc).expect("the migrated artwork reads");
    let mark = art.mark("tree").expect("the mark is back");
    assert_eq!(
        mark.shape,
        Shape::Rect {
            x: 10.0,
            y: 10.0,
            width: 200.0,
            height: 120.0,
            radius: 0.0,
        }
    );
    assert_eq!(mark.style.fill.as_deref(), Some("#204020"));
    assert_eq!(art.ground.texture, "linen");
}
