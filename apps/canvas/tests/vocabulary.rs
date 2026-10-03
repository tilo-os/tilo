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
        shape: Shape::Rect {
            x,
            y: 10.0,
            width: 50.0,
            height: 40.0,
            radius: 0.0,
        },
        fill: Some(fill.into()),
        gradient: None,
        stroke: None,
        stroke_width: None,
        opacity: None,
        brush: None,
        angle: None,
        wet: None,
        params: None,
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
        CanvasAction::EditMarks(EditMarks {
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

/// `media` lists each palette once and every medium names its palette, with
/// parameters to two decimals: the agent reads it whole before painting.
#[test]
fn media_list_each_palette_once() {
    let mut art = Replica::<ArtworkResource>::new();
    art.apply(ArtworkResource::ADD_LAYER.with(("Sky".into(), "oil".into())))
        .unwrap();
    let listed = canvas::view_ops::media(&art.state().unwrap());
    let media = listed["media"].as_array().unwrap();
    let palettes = listed["palettes"].as_object().unwrap();
    assert!(palettes.len() < media.len(), "{listed}");
    for medium in media {
        let palette = medium["palette"].as_str().unwrap();
        assert!(palettes.contains_key(palette), "{medium}");
        for value in medium["params"].as_object().unwrap().values() {
            let value = value.as_f64().unwrap();
            assert_eq!(value, (value * 100.0).round() / 100.0, "{medium}");
        }
    }
    let oil = media.iter().find(|medium| medium["id"] == "oil").unwrap();
    assert!(
        palettes[oil["palette"].as_str().unwrap()]
            .as_array()
            .unwrap()
            .iter()
            .any(|pigment| pigment.as_str().unwrap().starts_with("ultramarine #")),
        "{listed}"
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
