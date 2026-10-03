//! The artwork's vocabulary, applied the way the palette, an agent or a
//! script applies it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::float_cmp,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::actions::{AddLayer, Draw, EditMarks, NewMark, NewStroke, SaveMedium};
use canvas::media::MediumSpec;
use canvas::model::Shape;
use canvas::{ArtworkResource, CanvasAction};
use serde_json::json;
use ui::prelude::{Invoke, Replica};

fn stroke(points: &[[f64; 2]], colour: &str) -> NewStroke {
    NewStroke {
        id: None,
        name: None,
        points: points.iter().map(|point| point.to_vec()).collect(),
        size: 12.0,
        colour: colour.into(),
        opacity: None,
        wet: None,
        params: None,
    }
}

fn rect(id: &str, x: f64, fill: &str) -> NewMark {
    NewMark {
        id: Some(id.into()),
        name: Some(id.into()),
        fill: Some(fill.into()),
        ..NewMark::new(Shape::Rect {
            x,
            y: 10.0,
            width: 50.0,
            height: 40.0,
            radius: 0.0,
        })
    }
}

fn apply(art: &mut Replica<ArtworkResource>, action: CanvasAction) -> Result<(), String> {
    art.apply(Invoke::apply(&action).unwrap())
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// A new artwork has one plain layer, so the first passage of painting
/// needs no setup; a passage is one action with stable, content-derived ids.
#[test]
fn a_passage_of_painting_is_one_action_on_the_starting_layer() {
    let mut art = Replica::<ArtworkResource>::new();
    let state = art.state().unwrap();
    assert_eq!(state.layers.len(), 1);
    assert!(state.marks.is_empty(), "apps start empty");
    art.apply(ArtworkResource::PAINT.with((
        None,
        vec![
            stroke(&[[10.0, 10.0], [100.0, 40.0]], "#204080"),
            stroke(&[[10.0, 60.0], [100.0, 90.0]], "#802040"),
        ],
    )))
    .unwrap();
    let state = art.state().unwrap();
    assert_eq!(state.marks.len(), 2);
    assert!(state.marks.iter().all(|mark| mark.layer == "layer-1"));
    assert_eq!(art.log().len(), 1, "one passage, one undo step");

    // The same passage on another replica names its strokes alike.
    let mut other = Replica::<ArtworkResource>::new();
    other
        .apply(ArtworkResource::PAINT.with((
            None,
            vec![
                stroke(&[[10.0, 10.0], [100.0, 40.0]], "#204080"),
                stroke(&[[10.0, 60.0], [100.0, 90.0]], "#802040"),
            ],
        )))
        .unwrap();
    let ids = |state: &canvas::Artwork| {
        state
            .marks
            .iter()
            .map(|mark| mark.id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&state), ids(&other.state().unwrap()));
}

/// Marks are addressed by the ids they were given: recoloured, moved,
/// restacked and removed, each touching only what it names.
#[test]
fn named_marks_are_edited_arranged_and_removed_by_id() {
    let mut art = Replica::<ArtworkResource>::new();
    art.apply(ArtworkResource::DRAW.with((
        None,
        vec![rect("sky", 0.0, "#88aadd"), rect("sun", 20.0, "#ffcc33")],
    )))
    .unwrap();
    apply(
        &mut art,
        CanvasAction::edit(EditMarks {
            ids: vec!["sun".into()],
            fill: Some("#ff8800".into()),
            move_by: Some([5.0, -2.0]),
            ..EditMarks::default()
        }),
    )
    .unwrap();
    let state = art.state().unwrap();
    let sun = state.mark("sun").unwrap();
    assert_eq!(sun.style.fill.as_deref(), Some("#ff8800"));
    assert!(matches!(sun.shape, Shape::Rect { x, y, .. } if x == 25.0 && y == 8.0));
    assert_eq!(
        state.mark("sky").unwrap().style.fill.as_deref(),
        Some("#88aadd")
    );
    let top =
        |art: &Replica<ArtworkResource>| art.state().unwrap().marks.last().unwrap().id.clone();
    assert_eq!(top(&art), "sun");
    art.apply(ArtworkResource::ARRANGE.with((vec!["sky".into()], "front".into())))
        .unwrap();
    assert_eq!(top(&art), "sky");
    art.apply(ArtworkResource::REMOVE_MARKS.with(vec!["sky".into()]))
        .unwrap();
    assert_eq!(art.state().unwrap().marks.len(), 1);

    let clear = |art: &mut Replica<ArtworkResource>| {
        apply(
            art,
            CanvasAction::RemoveMarks(canvas::actions::RemoveMarks {
                ids: Vec::new(),
                layer: Some("layer-1".into()),
            }),
        )
    };
    clear(&mut art).unwrap();
    assert!(art.state().unwrap().marks.is_empty());
    clear(&mut art).expect("clearing an empty layer is already done, not a mistake");
}

/// A refusal says what was wrong and what would be right, so an agent can
/// correct itself from the message alone.
#[test]
fn refusals_name_the_mistake_and_the_valid_choices() {
    let mut art = Replica::<ArtworkResource>::new();
    let error = art
        .apply(ArtworkResource::PAINT.with((
            Some("sky".into()),
            vec![stroke(&[[0.0, 0.0], [5.0, 5.0]], "#000000")],
        )))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("no layer \"sky\"") && error.contains("layer-1"),
        "{error}"
    );

    let error = art
        .apply(ArtworkResource::PAINT.with((None, vec![stroke(&[[0.0, 0.0]], "pigment:mauve")])))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("pigment:<name>") && error.contains("ultramarine"),
        "{error}"
    );

    art.apply(ArtworkResource::DRAW.with((None, vec![rect("sun", 0.0, "#ffcc33")])))
        .unwrap();
    let error = art
        .apply(ArtworkResource::DRAW.with((None, vec![rect("sun", 0.0, "#ffcc33")])))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("already exists") && error.contains("edit-marks"),
        "{error}"
    );

    let error = apply(
        &mut art,
        CanvasAction::AddLayer(AddLayer {
            name: "Paint".into(),
            medium: Some("gouache".into()),
            ..AddLayer::default()
        }),
    )
    .unwrap_err();
    assert!(
        error.contains("oil") && error.contains("watercolour"),
        "{error}"
    );
}

/// A painted layer resolves its medium's pigments by name, and a medium an
/// agent saves travels with the artwork and can be painted with.
#[test]
fn media_are_named_pigments_resolve_and_saved_media_can_be_used() {
    let mut art = Replica::<ArtworkResource>::new();
    art.apply(ArtworkResource::ADD_LAYER.with(("Sky".into(), "watercolour".into())))
        .unwrap();
    let layer = art.state().unwrap().layers.last().unwrap().id.clone();
    art.apply(ArtworkResource::PAINT.with((
        Some(layer),
        vec![stroke(&[[0.0, 0.0], [50.0, 0.0]], "pigment:ultramarine")],
    )))
    .unwrap();

    let mut medium = canvas::media::builtin()
        .into_iter()
        .find(|medium| medium.id == "oil")
        .unwrap();
    medium.id = "nocturne".into();
    medium.label = "Nocturne".into();
    medium.params.insert("load".into(), 0.3);
    apply(
        &mut art,
        CanvasAction::SaveMedium(SaveMedium {
            medium: medium.clone(),
        }),
    )
    .unwrap();
    assert_eq!(art.state().unwrap().media.get("nocturne"), Some(&medium));
    art.apply(ArtworkResource::ADD_LAYER.with(("Night".into(), "nocturne".into())))
        .unwrap();

    let mut broken: MediumSpec = medium;
    broken.params.insert("viscosity".into(), 1.0);
    let error = apply(
        &mut art,
        CanvasAction::SaveMedium(SaveMedium { medium: broken }),
    )
    .unwrap_err();
    assert!(
        error.contains("no parameter") && error.contains("load"),
        "{error}"
    );
}

/// The generic `apply` publishes the whole vocabulary's schema, so an agent
/// reads every edit's shape from the descriptor; a typed value with a
/// misspelt field is refused by name rather than half-applied.
#[test]
fn the_vocabulary_is_published_as_a_schema_and_typos_are_refused() {
    use ui::tilo_resource::Resource as _;
    let schema = ArtworkResource::action_schema().unwrap().to_string();
    for action in ["paint", "draw", "edit-marks", "save-medium", "add-layer"] {
        assert!(
            schema.contains(&format!("\"{action}\"")),
            "{action} in {schema}"
        );
    }
    let mut art = Replica::<ArtworkResource>::new();
    let mut invoke = Invoke::<ArtworkResource>::apply(&CanvasAction::Draw(Draw {
        layer: None,
        marks: vec![rect("box", 0.0, "#000000")],
    }))
    .unwrap();
    invoke.arguments.insert(
        "action".into(),
        json!({"action":"draw","marks":[{"shape":{"kind":"rect","x":0,"y":0,"width":5,"height":5},"colour":"#000000"}]}),
    );
    assert!(
        art.apply(invoke).is_err(),
        "`colour` is not a field of a shape mark"
    );
}

/// A group is addressed by its id: edits move, scale and rotate its
/// members as one about the group's centre, and remove-marks takes them
/// all. A clip names the mark whose shape cuts another.
#[test]
fn groups_transform_as_one_and_clips_name_a_mark() {
    let mut art = Replica::<ArtworkResource>::new();
    let member = |id: &str, x: f64| NewMark {
        group: Some("logo".into()),
        ..rect(id, x, "#224466")
    };
    art.apply(ArtworkResource::DRAW.with((
        None,
        vec![
            member("left", 0.0),
            member("right", 150.0),
            NewMark {
                clip: Some("left".into()),
                ..rect("inside", 10.0, "#ff0000")
            },
        ],
    )))
    .unwrap();
    let state = art.state().unwrap();
    assert_eq!(state.groups(), vec!["logo"]);
    assert_eq!(state.mark("inside").unwrap().clip, "left");

    // Scaling the group doubles the distance between its members: they
    // scale about the group's centre (100, 30), not each their own.
    apply(
        &mut art,
        CanvasAction::edit(EditMarks {
            ids: vec!["logo".into()],
            scale: Some(2.0),
            ..EditMarks::default()
        }),
    )
    .unwrap();
    let state = art.state().unwrap();
    let left = state.mark("left").unwrap().bounds();
    let right = state.mark("right").unwrap().bounds();
    assert!((left.x0 - -100.0).abs() < 1e-6, "{left:?}");
    assert!((right.x1 - 300.0).abs() < 1e-6, "{right:?}");
    assert!((left.width() - 100.0).abs() < 1e-6);

    // Moving a transformed mark moves it in artwork units.
    apply(
        &mut art,
        CanvasAction::edit(EditMarks {
            ids: vec!["logo".into()],
            move_by: Some([10.0, 0.0]),
            ..EditMarks::default()
        }),
    )
    .unwrap();
    let moved = art.state().unwrap().mark("left").unwrap().bounds();
    assert!((moved.x0 - -90.0).abs() < 1e-6, "{moved:?}");

    let error = apply(
        &mut art,
        CanvasAction::edit(EditMarks {
            ids: vec!["logos".into()],
            ..EditMarks::default()
        }),
    )
    .unwrap_err();
    assert!(
        error.contains("logo"),
        "the refusal names the groups: {error}"
    );
    let error = apply(
        &mut art,
        CanvasAction::edit(EditMarks {
            ids: vec!["inside".into()],
            clip: Some("nothing".into()),
            ..EditMarks::default()
        }),
    )
    .unwrap_err();
    assert!(error.contains("not a mark"), "{error}");

    art.apply(ArtworkResource::REMOVE_MARKS.with(vec!["logo".into()]))
        .unwrap();
    let left: Vec<String> = art
        .state()
        .unwrap()
        .marks
        .iter()
        .map(|mark| mark.id.clone())
        .collect();
    assert_eq!(left, vec!["inside".to_owned()]);
}

/// Outlines take dashes, caps, joins, a blend and a shadow; a wrong name is
/// refused with the valid ones listed.
#[test]
fn stroke_styling_blend_and_shadow_are_validated_by_name() {
    let mut art = Replica::<ArtworkResource>::new();
    art.apply(ArtworkResource::DRAW.with((
        None,
        vec![NewMark {
            stroke: Some("#000000".into()),
            stroke_width: Some(4.0),
            dash: Some(vec![8.0, 4.0]),
            cap: Some("butt".into()),
            join: Some("miter".into()),
            blend: Some("multiply".into()),
            shadow: Some(canvas::model::Shadow {
                offset: [4.0, 6.0],
                blur: 8.0,
                colour: None,
            }),
            ..rect("frame", 0.0, "#ffeeaa")
        }],
    )))
    .unwrap();
    let state = art.state().unwrap();
    let style = &state.mark("frame").unwrap().style;
    assert_eq!(style.dash, vec![8.0, 4.0]);
    assert_eq!(style.blend.as_deref(), Some("multiply"));
    assert_eq!(style.shadow.as_ref().unwrap().blur, 8.0);

    let error = apply(
        &mut art,
        CanvasAction::edit(EditMarks {
            ids: vec!["frame".into()],
            blend: Some("burn".into()),
            ..EditMarks::default()
        }),
    )
    .unwrap_err();
    assert!(
        error.contains("color-burn") && error.contains("screen"),
        "{error}"
    );
    apply(
        &mut art,
        CanvasAction::edit(EditMarks {
            ids: vec!["frame".into()],
            blend: Some("normal".into()),
            dash: Some(Vec::new()),
            shadow: Some(canvas::model::Shadow {
                offset: [0.0, 0.0],
                blur: 0.0,
                colour: None,
            }),
            ..EditMarks::default()
        }),
    )
    .unwrap();
    let state = art.state().unwrap();
    let style = &state.mark("frame").unwrap().style;
    assert!(style.blend.is_none() && style.dash.is_empty() && style.shadow.is_none());
}

/// Text takes a family, weight, italic, alignment, line height and a wrap
/// width; `\n` breaks lines, and the box an agent reads grows with them.
#[test]
fn text_takes_typography_and_lines() {
    let mut art = Replica::<ArtworkResource>::new();
    let text = |align: &str, weight: u16| Shape::Text {
        x: 600.0,
        y: 100.0,
        text: "VECTOR\nDESIGN".into(),
        size: 40.0,
        font: Some("serif".into()),
        weight: Some(weight),
        italic: false,
        align: Some(align.into()),
        line_height: Some(1.5),
        width: None,
    };
    art.apply(ArtworkResource::DRAW.with((
        None,
        vec![NewMark {
            id: Some("title".into()),
            ..NewMark::new(text("center", 700))
        }],
    )))
    .unwrap();
    let bounds = art.state().unwrap().mark("title").unwrap().bounds();
    assert!(
        (bounds.center().x - 600.0).abs() < 1e-6,
        "centred on x: {bounds:?}"
    );
    assert!(bounds.height() > 90.0, "two lines: {bounds:?}");
    let error = art
        .apply(ArtworkResource::DRAW.with((None, vec![NewMark::new(text("middle", 400))])))
        .unwrap_err()
        .to_string();
    assert!(error.contains("start, center or end"), "{error}");
}

/// Align lines marks (or groups) up along an edge; distribute spaces them
/// with equal gaps between the outermost.
#[test]
fn align_and_distribute_move_marks_and_groups() {
    let mut art = Replica::<ArtworkResource>::new();
    art.apply(ArtworkResource::DRAW.with((
        None,
        vec![
            rect("a", 0.0, "#111111"),
            NewMark {
                shape: Shape::Rect {
                    x: 70.0,
                    y: 200.0,
                    width: 20.0,
                    height: 20.0,
                    radius: 0.0,
                },
                ..rect("b", 0.0, "#222222")
            },
            rect("c", 300.0, "#333333"),
        ],
    )))
    .unwrap();
    apply(
        &mut art,
        CanvasAction::Align(canvas::actions::Align {
            ids: vec!["a".into(), "b".into(), "c".into()],
            edge: Some("top".into()),
            distribute: Some("horizontal".into()),
            to: None,
        }),
    )
    .unwrap();
    let state = art.state().unwrap();
    let bounds = |id: &str| state.mark(id).unwrap().bounds();
    assert!(
        ["a", "b", "c"]
            .iter()
            .all(|id| (bounds(id).y0 - 10.0).abs() < 1e-6)
    );
    let first_gap = bounds("b").x0 - bounds("a").x1;
    let second_gap = bounds("c").x0 - bounds("b").x1;
    assert!(
        (first_gap - second_gap).abs() < 1e-6,
        "{first_gap} {second_gap}"
    );
    assert!((bounds("a").x0).abs() < 1e-6 && (bounds("c").x1 - 350.0).abs() < 1e-6);

    apply(
        &mut art,
        CanvasAction::Align(canvas::actions::Align {
            ids: vec!["b".into()],
            edge: Some("center".into()),
            to: Some("artwork".into()),
            ..canvas::actions::Align::default()
        }),
    )
    .unwrap();
    let centre = art.state().unwrap().mark("b").unwrap().bounds().center();
    assert!((centre.x - 600.0).abs() < 1e-6, "{centre:?}");
    let error = apply(
        &mut art,
        CanvasAction::Align(canvas::actions::Align {
            ids: vec!["a".into()],
            edge: Some("centre".into()),
            ..canvas::actions::Align::default()
        }),
    )
    .unwrap_err();
    assert!(error.contains("left, center, right"), "{error}");
}

/// A clipped mark measures as what of it shows, so aligning to a group
/// whose sheen spills past its clip centres on what is seen, and a click
/// outside the clip does not find the mark.
#[test]
fn clipped_marks_measure_what_shows() {
    let mut art = Replica::<ArtworkResource>::new();
    let tile = NewMark {
        group: Some("icon".into()),
        ..rect("tile", 100.0, "#3050a0")
    };
    let sheen = NewMark {
        id: Some("sheen".into()),
        fill: Some("#ffffff".into()),
        group: Some("icon".into()),
        clip: Some("tile".into()),
        ..NewMark::new(Shape::Ellipse {
            cx: 100.0,
            cy: 10.0,
            rx: 90.0,
            ry: 60.0,
        })
    };
    let label = rect("label", 0.0, "#000000");
    apply(
        &mut art,
        CanvasAction::Draw(Draw {
            layer: None,
            marks: vec![tile, sheen, label],
        }),
    )
    .unwrap();
    apply(
        &mut art,
        CanvasAction::Align(canvas::actions::Align {
            ids: vec!["label".into()],
            edge: Some("center".into()),
            distribute: None,
            to: Some("icon".into()),
        }),
    )
    .unwrap();
    let state = art.state().unwrap();
    let centre = state.mark("label").unwrap().bounds().center();
    assert_eq!(
        centre.x, 125.0,
        "centred on the tile, not the spilled sheen"
    );
    let sheen = state.mark("sheen").unwrap();
    assert_eq!(
        state.visible_bounds(sheen),
        state.mark("tile").unwrap().bounds()
    );
    let hit = state.mark_at(ui::vello::kurbo::Point::new(60.0, 20.0));
    assert!(
        hit.is_none_or(|mark| mark.id != "sheen"),
        "the sheen is cut away there: {hit:?}"
    );
}
