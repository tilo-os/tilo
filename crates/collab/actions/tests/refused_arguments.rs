//! What a call refused for its arguments is told, for every action alike: the
//! argument's name, the kind of thing it was given, the kind it must be and
//! the shape that works. Never the value it was given: a refusal is read by
//! whoever called, and it is logged.

use serde_json::{Value, json};
use tilo_actions::{ActionArgs, ActionParam, ApiError, ParamType};

/// What every call here gives where it gives the wrong thing: no refusal may
/// repeat it.
const GIVEN: &str = "zx-made-up-7781";

/// One refused call: what it is about, what the action declares, what the
/// call gave, and the whole of what it must be told.
struct Refused {
    about: &'static str,
    params: Vec<ActionParam>,
    arguments: Value,
    told: &'static str,
}

/// What a caller reads when `arguments` are refused for `params`.
fn told(params: &[ActionParam], arguments: &Value) -> String {
    let arguments = arguments.as_object().cloned().unwrap_or_default();
    match ActionArgs::bind_json(params, &arguments) {
        Ok(_) => "(the arguments were taken)".to_owned(),
        Err(error) => ApiError::from(error).to_string(),
    }
}

/// What is asked for and the places it belongs to, as an action declares a
/// structured argument: an object with a list in it.
fn detail() -> ActionParam {
    ActionParam::required("detail", ParamType::Object, "What is asked for").with_schema(json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["kind", "places"],
        "properties": {
            "kind": {"enum": ["login", "key"]},
            "places": {"type": "array", "items": {"type": "string"}},
            "name": {"type": "string"},
        },
    }))
}

/// The answers a question offers: an optional list of objects.
fn answers() -> ActionParam {
    ActionParam::optional("answers", ParamType::Array, "The answers offered").with_schema(json!({
        "type": ["array", "null"],
        "items": {
            "type": "object",
            "additionalProperties": false,
            "properties": {"id": {"type": "string"}, "label": {"type": "string"}},
            "required": ["id"],
        },
    }))
}

/// A page of a read: both arguments optional, one a number within bounds.
fn page() -> Vec<ActionParam> {
    vec![
        ActionParam::optional("cursor", ParamType::Text, "Where the page starts"),
        ActionParam::optional("pageSize", ParamType::Integer, "Most items in the page")
            .with_schema(json!({"minimum": 1, "maximum": 64})),
    ]
}

/// A title, required, and a body.
fn titled() -> Vec<ActionParam> {
    vec![
        ActionParam::required("title", ParamType::Text, "The title"),
        ActionParam::optional("body", ParamType::Text, "The body"),
    ]
}

/// The refusals a structured argument's own schema makes, inside it.
fn inside_an_argument() -> Vec<Refused> {
    vec![
        Refused {
            about: "a list inside an object, given as text",
            params: vec![detail()],
            arguments: json!({"detail": {"kind": "login", "places": GIVEN}}),
            told: "argument `detail` property `places` was given text and must be a list: \
                   [\"<text>\"]",
        },
        Refused {
            about: "an object written with fields of the caller's own",
            params: vec![detail()],
            arguments: json!({"detail": {"what": GIVEN, "where": GIVEN}}),
            told: "argument `detail` must contain property `kind`: \"login\" | \"key\"",
        },
        Refused {
            about: "a field the object does not take, beside the right ones",
            params: vec![detail()],
            arguments: json!({"detail": {
                "kind": "login", "places": ["https://example.com"], "site": GIVEN,
            }}),
            told: "argument `detail` contains unknown property `site`; it takes \
                   {\"kind\": \"login\" | \"key\", \"places\": [\"<text>\"], \"name\"?: \"<text>\"}",
        },
        Refused {
            about: "a value that is none of the values a field takes",
            params: vec![detail()],
            arguments: json!({"detail": {"kind": GIVEN, "places": ["https://example.com"]}}),
            told: "argument `detail` property `kind` must be one of \"login\", \"key\"",
        },
        Refused {
            about: "a list of objects, given as a list of text",
            params: vec![answers()],
            arguments: json!({"answers": [GIVEN]}),
            told: "argument `answers` item 0 was given text and must be an object: \
                   {\"id\": \"<text>\", \"label\"?: \"<text>\"}",
        },
        Refused {
            about: "an object without the field that says which action is meant",
            params: vec![
                ActionParam::required("action", ParamType::Object, "The action").with_schema(
                    json!({
                        "type": "object",
                        "required": ["op"],
                        "properties": {"op": {"enum": ["field-edit"]}},
                    }),
                ),
            ],
            arguments: json!({"action": {"action": GIVEN}}),
            told: "argument `action` must contain property `op`: \"field-edit\"",
        },
    ]
}

/// The refusals of an argument itself: its kind, its name, its absence.
fn of_an_argument() -> Vec<Refused> {
    vec![
        Refused {
            about: "a list, given as text",
            params: vec![
                ActionParam::required("places", ParamType::Array, "The places")
                    .with_schema(json!({"type": "array", "items": {"type": "string"}})),
            ],
            arguments: json!({"places": GIVEN}),
            told: "argument `places` was given text and must be a list: [\"<text>\"]",
        },
        Refused {
            about: "a list of objects, given as one object",
            params: vec![answers()],
            arguments: json!({"answers": {"id": GIVEN}}),
            told: "argument `answers` was given an object and must be a list: \
                   [{\"id\": \"<text>\", \"label\"?: \"<text>\"}]",
        },
        Refused {
            about: "an argument the action does not take",
            params: page(),
            arguments: json!({"fields": [GIVEN]}),
            told: "`fields` is not a parameter of this action; its arguments are \
                   {\"cursor\"?: \"<text>\", \"pageSize\"?: <integer from 1 to 64>}",
        },
        Refused {
            about: "a required argument left out",
            params: titled(),
            arguments: json!({"body": GIVEN}),
            told: "required argument `title` was not supplied: it must be text: \"<text>\"",
        },
        Refused {
            about: "a value that is none of an argument's choices",
            params: vec![
                ActionParam::required("mode", ParamType::Text, "The mode")
                    .with_choices(["light", "dark"]),
            ],
            arguments: json!({"mode": GIVEN}),
            told: "argument `mode` must be one of light, dark; it was given text that is none \
                   of them",
        },
        Refused {
            about: "null where a value is required",
            params: titled(),
            arguments: json!({"title": null}),
            told: "argument `title` was given null and must be text: \"<text>\"",
        },
        Refused {
            about: "a number where text is taken",
            params: titled(),
            arguments: json!({"title": 7781}),
            told: "argument `title` was given an integer and must be text: \"<text>\"",
        },
    ]
}

/// Every refused call of this file.
fn refused() -> Vec<Refused> {
    let mut all = inside_an_argument();
    all.extend(of_an_argument());
    all
}

#[test]
fn a_refused_argument_is_told_its_kind_the_kind_it_must_be_and_the_shape_that_works() {
    let all = refused();
    let unsaid: Vec<String> = all
        .iter()
        .filter_map(|call| {
            let told = told(&call.params, &call.arguments);
            (told != call.told)
                .then(|| format!("{}:\n  told:   {told}\n  wanted: {}", call.about, call.told))
        })
        .collect();
    assert!(
        unsaid.is_empty(),
        "{} of {} refusals do not say what works:\n{}",
        unsaid.len(),
        all.len(),
        unsaid.join("\n")
    );
}

#[test]
fn no_refusal_repeats_what_the_call_gave() {
    let repeated: Vec<&str> = refused()
        .iter()
        .filter(|call| told(&call.params, &call.arguments).contains(GIVEN))
        .map(|call| call.about)
        .collect();
    assert!(
        repeated.is_empty(),
        "a refusal holds the value it was given: {repeated:?}"
    );
}
