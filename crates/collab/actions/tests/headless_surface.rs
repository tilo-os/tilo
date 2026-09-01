//! Driving an app's action surface with no window: enumerate the projection,
//! invoke from it, and prove the action that comes out is the *same* action the
//! on-screen affordance builds.
//!
//! The covenant under test is one sentence: **an action invoked from a
//! serialized description must be byte-identical to the one the affordance
//! produces.** If that ever stops holding, "drive the app headlessly" quietly
//! becomes "drive a second, parallel implementation of the app" — which is the
//! failure this whole surface exists to prevent.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration-test setup and assertions"
)]

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue};
use serde::{Deserialize, Serialize};

use qos_actions::{
    ActionArgs, ActionError, ActionInput, ActionParam, ActionSpec, ArgError, ArgValue, Doc,
    EncodedAction, ExecutionLocus, InvokeError, PROMPTED_PARAM, ParamType, Resource,
    SerializableResource, build_from_flat, build_from_json, project, project_flat, resolve,
};

// ---------------------------------------------------------------------------
// A small but real app domain: a clip with an opacity, a name, and a filter.
// These are exactly the three shapes whose prose descriptions used to be parsed
// leniently — a unit float, free text, and a fixed vocabulary.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum ClipAction {
    SetOpacity { clip: String, value: f64 },
    Rename { clip: String, name: String },
    SetFilter { clip: String, filter: String },
    Delete { clip: String },
}

const CLIP_TYPE: &str = "editor.clip";

impl qos_actions::Action for ClipAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let clips = match doc.get(&ROOT, "clips")? {
            Some((automerge::Value::Object(ObjType::Map), id)) => id,
            _ => doc.put_object(&ROOT, "clips", ObjType::Map)?,
        };
        match self {
            ClipAction::SetOpacity { clip, value } => {
                let c = match doc.get(&clips, clip)? {
                    Some((automerge::Value::Object(ObjType::Map), id)) => id,
                    _ => doc.put_object(&clips, clip, ObjType::Map)?,
                };
                doc.put(&c, "opacity", ScalarValue::F64(*value))?;
            }
            ClipAction::Rename { clip, name } => {
                let c = match doc.get(&clips, clip)? {
                    Some((automerge::Value::Object(ObjType::Map), id)) => id,
                    _ => doc.put_object(&clips, clip, ObjType::Map)?,
                };
                doc.put(&c, "name", ScalarValue::Str(name.as_str().into()))?;
            }
            ClipAction::SetFilter { clip, filter } => {
                let c = match doc.get(&clips, clip)? {
                    Some((automerge::Value::Object(ObjType::Map), id)) => id,
                    _ => doc.put_object(&clips, clip, ObjType::Map)?,
                };
                doc.put(&c, "filter", ScalarValue::Str(filter.as_str().into()))?;
            }
            ClipAction::Delete { clip } => {
                if doc.get(&clips, clip)?.is_some() {
                    doc.delete(&clips, clip)?;
                }
            }
        }
        Ok(())
    }
}

fn encode(action: &ClipAction) -> EncodedAction {
    EncodedAction::encode(CLIP_TYPE, action).unwrap()
}

const FILTERS: [&str; 3] = ["grayscale", "sepia", "invert"];

/// The resource forest the app publishes for one clip. Deliberately mixes all
/// three spec constructors, because the covenant has to hold for each.
fn clip_resources(clip_id: &str) -> Vec<Resource> {
    let for_delete = clip_id.to_string();
    let for_rename = clip_id.to_string();
    let for_opacity = clip_id.to_string();
    let for_filter = clip_id.to_string();

    vec![
        Resource::new(
            format!("editor.clip:{clip_id}"),
            "editor.clip",
            "Opening shot",
        )
        // Nullary.
        .with_action(
            ActionSpec::immediate(
                "delete",
                "Delete clip",
                CLIP_TYPE,
                &ClipAction::Delete { clip: for_delete },
            )
            .unwrap(),
        )
        // The existing free-text shape, untouched by this change.
        .with_action(ActionSpec::prompted(
            "rename",
            "Rename clip",
            "Name",
            "clip name…",
            ExecutionLocus::ClientLocal,
            move |name: &str| {
                encode(&ClipAction::Rename {
                    clip: for_rename.clone(),
                    name: name.trim().to_string(),
                })
            },
        ))
        // A typed number, replacing a prose "Value 0–1" a lenient parser guessed at.
        .with_action(ActionSpec::typed(
            "set-opacity",
            "Set opacity",
            "0.75",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required(
                "opacity",
                ParamType::Number,
                "Opacity from 0 (invisible) to 1 (opaque).",
            )],
            move |args: &ActionArgs| {
                encode(&ClipAction::SetOpacity {
                    clip: for_opacity.clone(),
                    value: args.number("opacity").unwrap_or(1.0),
                })
            },
        ))
        // A typed choice, replacing a prose "grayscale / sepia / invert".
        .with_action(ActionSpec::typed(
            "set-filter",
            "Set filter",
            "sepia",
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required("filter", ParamType::Text, "Which look to apply.")
                    .with_choices(FILTERS),
            ],
            move |args: &ActionArgs| {
                // A supplied value is already known good — binding enforced the
                // choice list before this ran — so the builder only decides what an
                // *absent* one means.
                encode(&ClipAction::SetFilter {
                    clip: for_filter.clone(),
                    filter: args.text("filter").unwrap_or("grayscale").to_string(),
                })
            },
        )),
    ]
}

/// What the on-screen affordance would build, straight from the spec's palette
/// closure — the reference every headless path is measured against.
fn affordance_encode(resources: &[Resource], key: &str, typed_text: &str) -> EncodedAction {
    let spec = resolve(resources, key).expect("spec is exported");
    match &spec.input {
        ActionInput::Immediate(enc) => enc.clone(),
        ActionInput::Prompted { build, .. } => build(typed_text),
    }
}

// ---------------------------------------------------------------------------
// The round trip.
// ---------------------------------------------------------------------------

/// THE HEADLINE TEST. Serialize the projection, throw the live tree's closures
/// out of the picture, invoke purely from what came back off the wire, and the
/// action is byte-identical to the affordance's.
#[test]
fn action_invoked_from_serialized_projection_is_byte_identical_to_the_affordance() {
    let resources = clip_resources("c1");

    // 1. Project and serialize — this is all a headless caller ever sees.
    let flat = project_flat(&resources);
    let json = serde_json::to_string(&flat).expect("the projection serializes");

    // 2. A caller with no window reads it back. No closure survived the trip.
    let seen: Vec<qos_actions::FlatSpec> =
        serde_json::from_str(&json).expect("the projection deserializes");

    for entry in &seen {
        // 3. Build arguments *only* from what the descriptor advertised.
        let args: Vec<(String, ArgValue)> = entry
            .spec
            .params
            .iter()
            .map(|p| {
                let v = match (p.name.as_ref(), p.ty) {
                    (_, ParamType::Number) => ArgValue::Number(0.25),
                    ("filter", ParamType::Text) => ArgValue::Text("sepia".into()),
                    (_, ParamType::Text) => ArgValue::Text("Establishing".into()),
                    (_, ParamType::Integer) => ArgValue::Integer(3),
                    (_, ParamType::Boolean) => ArgValue::Boolean(true),
                };
                (p.name.to_string(), v)
            })
            .collect();

        // The same values a human would have typed into the palette's one field.
        let typed_text = match entry.spec.action_id.as_str() {
            "rename" => "Establishing",
            "set-opacity" => "0.25",
            "set-filter" => "sepia",
            _ => "",
        };

        let from_wire = build_from_flat(&resources, &entry.key, args)
            .unwrap_or_else(|e| panic!("invoking `{}` from the wire: {e}", entry.key));
        let from_affordance = affordance_encode(&resources, &entry.key, typed_text);

        assert_eq!(
            from_wire, from_affordance,
            "`{}` built a different action headlessly than on screen",
            entry.key
        );
    }

    assert_eq!(seen.len(), 4, "every exported action was projected");
}

/// The projection is *data*: no handle, no callback, nothing that could fire.
/// Surveying "what can I do here" must never be able to do anything.
#[test]
fn the_projection_carries_no_way_to_invoke() {
    let resources = clip_resources("c1");
    let json = serde_json::to_value(project_flat(&resources)).unwrap();
    let text = json.to_string();
    // A structural check, not a spelling one: the projected value round-trips
    // through plain JSON, which by construction cannot carry a closure.
    let back: Vec<qos_actions::FlatSpec> = serde_json::from_value(json).unwrap();
    assert_eq!(back.len(), 4);
    assert!(
        text.contains("\"set-opacity\"") && text.contains("\"number\""),
        "the descriptor states the parameter type: {text}"
    );
}

/// The nested shape survives too — a caller can see that the actions belong to
/// a clip, and which clip, rather than getting a flat pile of verbs.
#[test]
fn the_resource_tree_projects_with_its_nesting() {
    let child = Resource::new("editor.marker:m1", "editor.marker", "Cut here").with_action(
        ActionSpec::immediate(
            "delete",
            "Delete marker",
            CLIP_TYPE,
            &ClipAction::Delete { clip: "m1".into() },
        )
        .unwrap(),
    );
    let tree = vec![clip_resources("c1").remove(0).with_child(child)];

    let projected = project(&tree);
    let round: Vec<SerializableResource> =
        serde_json::from_str(&serde_json::to_string(&projected).unwrap()).unwrap();

    assert_eq!(round, projected, "the tree survives a JSON round trip");
    assert_eq!(round[0].kind, "editor.clip");
    assert_eq!(round[0].actions.len(), 4);
    assert_eq!(round[0].children.len(), 1, "the marker hangs off the clip");
    assert_eq!(round[0].children[0].actions[0].action_id, "delete");
}

// ---------------------------------------------------------------------------
// Strictness: a bad call is a refusal, never a partial mutation.
// ---------------------------------------------------------------------------

/// A wrong-typed argument is refused, and — the part that matters — refused
/// *before* an action exists to apply. Nothing reaches the document.
#[test]
fn a_wrong_typed_argument_is_refused_before_the_document_is_touched() {
    let resources = clip_resources("c1");
    let mut doc = AutoCommit::new();
    doc.put(&ROOT, "seed", ScalarValue::Str("s".into()))
        .unwrap();
    let before = doc.get_heads();

    let key = format!("editor.clip:c1{}set-opacity", qos_actions::KEY_SEP);
    let err = build_from_flat(
        &resources,
        &key,
        // "half" is what a caller guesses when the description is prose. The
        // old lenient parse turned this into 0.5 and applied it.
        vec![("opacity".to_string(), ArgValue::Text("half".into()))],
    )
    .expect_err("text is not a number");

    assert_eq!(err.code(), "invalid_arguments");
    assert!(
        matches!(
            &err,
            InvokeError::Arguments(ArgError::WrongType { name, expected, got })
                if name == "opacity"
                    && *expected == ParamType::Number
                    && *got == ParamType::Text
        ),
        "the refusal names the parameter and both types: {err}"
    );
    assert_eq!(
        doc.get_heads(),
        before,
        "a refused call leaves the document exactly as it found it"
    );
}

/// A value outside a declared choice list is refused by name, rather than
/// silently falling through to whatever the builder defaults to.
#[test]
fn a_value_outside_the_declared_choices_is_refused() {
    let resources = clip_resources("c1");
    let key = format!("editor.clip:c1{}set-filter", qos_actions::KEY_SEP);

    let err = build_from_flat(
        &resources,
        &key,
        vec![("filter".to_string(), ArgValue::Text("sephia".into()))],
    )
    .expect_err("`sephia` is not one of the filters");

    match err {
        InvokeError::Arguments(ArgError::NotAllowed {
            name,
            value,
            allowed,
        }) => {
            assert_eq!(name, "filter");
            assert_eq!(value, "sephia");
            let expected: Vec<String> = FILTERS
                .iter()
                .map(std::string::ToString::to_string)
                .collect();
            assert_eq!(allowed, expected);
        }
        other => panic!("expected a choice refusal, got {other}"),
    }

    // And the good value still passes, so the check is a gate and not a wall.
    assert!(
        build_from_flat(
            &resources,
            &key,
            vec![("filter".to_string(), ArgValue::Text("sepia".into()))],
        )
        .is_ok()
    );
}

/// A missing required argument is refused, naming what was not supplied.
#[test]
fn a_missing_required_argument_is_refused_by_name() {
    let resources = clip_resources("c1");
    let key = format!("editor.clip:c1{}set-opacity", qos_actions::KEY_SEP);
    let err = build_from_flat(&resources, &key, vec![]).expect_err("opacity is required");
    assert!(
        matches!(&err, InvokeError::Arguments(ArgError::Missing { name }) if name == "opacity"),
        "got {err}"
    );
}

/// An argument the action never declared is refused rather than dropped. A
/// silently ignored argument is the worst outcome available: the caller
/// believes it asked for something it did not get.
#[test]
fn an_undeclared_argument_is_refused_rather_than_ignored() {
    let resources = clip_resources("c1");
    let key = format!("editor.clip:c1{}set-opacity", qos_actions::KEY_SEP);
    let err = build_from_flat(
        &resources,
        &key,
        vec![
            ("opacity".to_string(), ArgValue::Number(0.5)),
            ("easing".to_string(), ArgValue::Text("linear".into())),
        ],
    )
    .expect_err("`easing` is not a parameter");
    assert!(
        matches!(&err, InvokeError::Arguments(ArgError::Unknown { name }) if name == "easing"),
        "got {err}"
    );
}

/// A stale or invented address is a *different* failure from a bad argument —
/// a caller fixes the two differently, so they must not arrive as one code.
#[test]
fn an_unknown_address_is_a_distinct_refusal() {
    let resources = clip_resources("c1");
    let key = format!("editor.clip:c9{}set-opacity", qos_actions::KEY_SEP);
    let err = build_from_flat(
        &resources,
        &key,
        vec![("opacity".to_string(), ArgValue::Number(0.5))],
    )
    .expect_err("clip c9 is not exported");
    assert_eq!(err.code(), "unknown_action");
    assert_ne!(err.code(), "invalid_arguments");
}

// ---------------------------------------------------------------------------
// The JSON path a control socket actually uses.
// ---------------------------------------------------------------------------

/// Arguments delivered as a JSON object bind by the same rules, using the same
/// spelling the compositor's control socket already accepts.
#[test]
fn json_arguments_bind_and_build_the_same_action() {
    let resources = clip_resources("c1");
    let key = format!("editor.clip:c1{}set-opacity", qos_actions::KEY_SEP);

    let args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(r#"{"opacity": 0.25}"#).unwrap();
    let from_json = build_from_json(&resources, &key, &args).unwrap();

    assert_eq!(from_json, affordance_encode(&resources, &key, "0.25"));
}

/// JSON has one numeric type and writes `1.0` as `1`. A rate of exactly 1 must
/// stay passable, so an integer widens into a number parameter — one way only.
#[test]
fn an_integer_json_literal_satisfies_a_number_parameter() {
    let resources = clip_resources("c1");
    let key = format!("editor.clip:c1{}set-opacity", qos_actions::KEY_SEP);

    let args: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(r#"{"opacity": 1}"#).unwrap();
    let widened = build_from_json(&resources, &key, &args).unwrap();
    assert_eq!(widened, affordance_encode(&resources, &key, "1.0"));

    // The widening does not run the other way: a fraction is not an integer.
    let params = [ActionParam::required("n", ParamType::Integer, "count")];
    assert!(matches!(
        ActionArgs::bind(&params, vec![("n".to_string(), ArgValue::Number(1.5))]),
        Err(ArgError::WrongType { .. })
    ));
}

// ---------------------------------------------------------------------------
// The existing in-process ergonomics keep working.
// ---------------------------------------------------------------------------

/// The ~40 `prompted` specs already in the tree gain a typed projection without
/// changing: one required text parameter, and the *same* closure behind both
/// paths.
#[test]
fn a_prompted_spec_projects_to_one_text_parameter_over_the_same_closure() {
    let resources = clip_resources("c1");
    let key = format!("editor.clip:c1{}rename", qos_actions::KEY_SEP);
    let spec = resolve(&resources, &key).unwrap();

    assert_eq!(spec.params().len(), 1);
    assert_eq!(spec.params()[0].name, PROMPTED_PARAM);
    assert_eq!(spec.params()[0].ty, ParamType::Text);
    assert!(spec.params()[0].required);
    assert_eq!(
        spec.params()[0].description,
        "Name",
        "the human prompt becomes the parameter's description"
    );
    assert!(
        matches!(&spec.input, ActionInput::Prompted { .. }),
        "the palette still sees a prompted action"
    );

    let headless = spec
        .build_args(vec![(
            PROMPTED_PARAM.to_string(),
            ArgValue::Text("Establishing".into()),
        )])
        .unwrap();
    assert_eq!(
        headless,
        affordance_encode(&resources, &key, "Establishing")
    );
}

/// A nullary spec projects to zero parameters and reproduces its pre-encoded
/// action exactly.
#[test]
fn an_immediate_spec_projects_to_zero_parameters() {
    let resources = clip_resources("c1");
    let key = format!("editor.clip:c1{}delete", qos_actions::KEY_SEP);
    let spec = resolve(&resources, &key).unwrap();

    assert!(spec.params().is_empty());
    let headless = spec.build_args(vec![]).unwrap();
    match &spec.input {
        ActionInput::Immediate(enc) => assert_eq!(&headless, enc),
        ActionInput::Prompted { .. } => panic!("expected an immediate spec, got a prompted one"),
    }
}

/// A typed spec is still usable from the keyboard: the palette collects one
/// line, parses it into the primary parameter's declared type, and reaches the
/// same builder. Typing a value does not become a second code path.
#[test]
fn a_typed_spec_is_still_invokable_from_the_palettes_single_field() {
    let resources = clip_resources("c1");
    let key = format!("editor.clip:c1{}set-opacity", qos_actions::KEY_SEP);

    let typed_in = affordance_encode(&resources, &key, "0.25");
    let over_wire = build_from_flat(
        &resources,
        &key,
        vec![("opacity".to_string(), ArgValue::Number(0.25))],
    )
    .unwrap();
    assert_eq!(typed_in, over_wire);

    // And the palette's derived prompt is the parameter's description, so the
    // human sees what the machine was told.
    match &resolve(&resources, &key).unwrap().input {
        ActionInput::Prompted { prompt, .. } => {
            assert_eq!(prompt, "Opacity from 0 (invisible) to 1 (opaque).");
        }
        ActionInput::Immediate(_) => panic!("expected a prompted palette entry, got an immediate"),
    }
}
