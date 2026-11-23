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

/// What a question's kind keeps with it: an argument whose shape another
/// argument's value decides, as its declaration says. A secret keeps what is
/// asked for and the places it belongs to; a kind the declaration names no
/// shape for keeps any object.
fn kept() -> ActionParam {
    ActionParam::optional("detail", ParamType::Object, "What the kind keeps").with_schema(json!({
        "type": ["object", "null"],
        "shaped_by": {
            "argument": "kind",
            "shapes": {
                "secret": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["kind", "belongsTo"],
                    "properties": {
                        "belongsTo": {
                            "type": "array",
                            "minItems": 1,
                            "items": {"type": "string", "format": "uri"},
                        },
                        "kind": {"enum": ["login", "key"]},
                        "username": {"type": "string"},
                    },
                },
            },
        },
    }))
}

/// A question's kind, and what the kind keeps.
fn kept_by_kind() -> Vec<ActionParam> {
    vec![
        ActionParam::optional("kind", ParamType::Text, "The kind of question"),
        kept(),
    ]
}

/// The refusals of an argument whose shape another argument's value decides.
fn shaped_by_another_argument() -> Vec<Refused> {
    vec![
        Refused {
            about: "a list in the shape its kind names, given as text",
            params: kept_by_kind(),
            arguments: json!({
                "kind": "secret",
                "detail": {"kind": "login", "belongsTo": GIVEN, "username": GIVEN},
            }),
            told: "argument `detail` property `belongsTo` was given text and must be a list: \
                   [\"<address such as https://example.com>\"]",
        },
        Refused {
            about: "the shape its kind names, written with fields of the caller's own",
            params: kept_by_kind(),
            arguments: json!({"kind": "secret", "detail": {"what": GIVEN, "where": GIVEN}}),
            told: "argument `detail` must contain property `kind`: \"login\" | \"key\"",
        },
        Refused {
            about: "the shape its kind names, put under a key of the caller's own",
            params: kept_by_kind(),
            arguments: json!({
                "kind": "secret",
                "detail": {"secret": {"kind": "login", "belongsTo": [GIVEN]}},
            }),
            told: "argument `detail` must contain property `kind`: \"login\" | \"key\"",
        },
        Refused {
            about: "an address in the shape its kind names, given as words",
            params: kept_by_kind(),
            arguments: json!({
                "kind": "secret",
                "detail": {"kind": "login", "belongsTo": [GIVEN]},
            }),
            told: "argument `detail` property `belongsTo` item 0 must match format `uri`: \
                   \"<address such as https://example.com>\"",
        },
        Refused {
            about: "the argument left out where the shape its kind names requires it",
            params: kept_by_kind(),
            arguments: json!({"kind": "secret"}),
            told: "required argument `detail` was not supplied: it must be an object: \
                   {\"kind\": \"login\" | \"key\", \
                   \"belongsTo\": [\"<address such as https://example.com>\"], \
                   \"username\"?: \"<text>\"}",
        },
        Refused {
            about: "text where the argument another one shapes takes an object",
            params: kept_by_kind(),
            arguments: json!({"kind": "secret", "detail": GIVEN}),
            told: "argument `detail` was given text and must be an object: \
                   {<key>: <any value>} (when \"kind\" is \"secret\": \
                   {\"kind\": \"login\" | \"key\", \
                   \"belongsTo\": [\"<address such as https://example.com>\"], \
                   \"username\"?: \"<text>\"})",
        },
        Refused {
            about: "an argument the action does not take, beside one another argument shapes",
            params: kept_by_kind(),
            arguments: json!({"kind": "secret", "details": {"kind": "login"}}),
            told: "`details` is not a parameter of this action; its arguments are \
                   {\"kind\"?: \"<text>\", \"detail\"?: {<key>: <any value>} \
                   (when \"kind\" is \"secret\": {\"kind\": \"login\" | \"key\", \
                   \"belongsTo\": [\"<address such as https://example.com>\"], \
                   \"username\"?: \"<text>\"})}",
        },
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
    all.extend(shaped_by_another_argument());
    all.extend(of_a_map());
    all
}

/// How many of each thing: a map, whose keys are the caller's own and whose
/// values are all of one kind.
fn counts() -> ActionParam {
    ActionParam::required("counts", ParamType::Object, "How many of each").with_schema(json!({
        "type": "object",
        "additionalProperties": {"type": "integer", "minimum": 0},
    }))
}

/// What was carried since a day, by place and by thing: a map of maps beside
/// a field the object names itself.
fn carried() -> ActionParam {
    ActionParam::optional("carried", ParamType::Object, "What was carried").with_schema(json!({
        "type": "object",
        "required": ["since"],
        "properties": {"since": {"type": "string"}},
        "additionalProperties": {
            "type": "object",
            "additionalProperties": {"type": "integer"},
        },
    }))
}

/// The refusals of a map. Its keys are the caller's own, so no refusal says
/// one: it says `<key>`, as the shape does.
fn of_a_map() -> Vec<Refused> {
    vec![
        Refused {
            about: "a map, given as a list",
            params: vec![counts()],
            arguments: json!({"counts": [GIVEN]}),
            told: "argument `counts` was given a list and must be an object: \
                   {<key>: <integer, at least 0>}",
        },
        Refused {
            about: "a value of a map, of another kind than the map holds",
            params: vec![counts()],
            arguments: json!({"counts": {GIVEN: GIVEN}}),
            told: "argument `counts` property <key> was given text and must be an integer: \
                   <integer, at least 0>",
        },
        Refused {
            about: "a map with a field of its own, given as text",
            params: vec![carried()],
            arguments: json!({"carried": GIVEN}),
            told: "argument `carried` was given text and must be an object: \
                   {\"since\": \"<text>\", <key>: {<key>: <integer>}}",
        },
        Refused {
            about: "a value two maps deep, of another kind than the inner map holds",
            params: vec![carried()],
            arguments: json!({"carried": {"since": "a day", GIVEN: {GIVEN: [1]}}}),
            told: "argument `carried` property <key> property <key> was given a list and must \
                   be an integer: <integer>",
        },
    ]
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

/// An argument another one shapes is held to the shape that argument's value
/// names, and to nothing more where it names none.
#[test]
fn an_argument_another_one_shapes_is_held_only_to_the_shape_that_value_names() {
    let taken = [
        (
            "the shape its kind names",
            json!({
                "kind": "secret",
                "detail": {
                    "kind": "key", "belongsTo": ["https://example.com"], "username": "ada",
                },
            }),
        ),
        (
            "a kind that names no shape, with an object of its own",
            json!({"kind": "action", "detail": {"draft": {"anything": 1}}}),
        ),
        (
            "no kind, with an object of its own",
            json!({"detail": {"anything": 1}}),
        ),
        (
            "a kind that names no shape, and nothing kept",
            json!({"kind": "choice"}),
        ),
        ("neither argument", json!({})),
    ];
    let refused: Vec<String> = taken
        .iter()
        .filter_map(|(about, arguments)| {
            let told = told(&kept_by_kind(), arguments);
            (told != "(the arguments were taken)").then(|| format!("{about}: {told}"))
        })
        .collect();
    assert!(
        refused.is_empty(),
        "calls that give what the declaration takes were refused:\n{}",
        refused.join("\n")
    );
}

/// What a listing shows before a call says the shape another argument decides.
#[test]
fn a_listing_writes_out_the_shape_another_argument_decides() {
    assert_eq!(
        kept().declared_shape().as_deref(),
        Some(
            "{<key>: <any value>} (when \"kind\" is \"secret\": {\"kind\": \"login\" | \"key\", \
             \"belongsTo\": [\"<address such as https://example.com>\"], \
             \"username\"?: \"<text>\"})"
        )
    );
}

/// Such a declaration is checked where it is published: it names the argument
/// that decides, and every shape it names is a schema that can be held.
#[test]
fn a_shape_another_argument_decides_is_checked_where_it_is_declared() {
    assert_eq!(kept().validate_declaration(), Vec::<String>::new());
    let declared = |schema: Value| {
        ActionParam::optional("detail", ParamType::Object, "What the kind keeps")
            .with_schema(schema)
            .validate_declaration()
    };
    assert_eq!(
        declared(json!({"shaped_by": {"shapes": {}}})),
        [
            "invalid schema: schema `shaped_by` must name an `argument` and the `shapes` its values decide"
        ]
    );
    assert_eq!(
        declared(json!({
            "shaped_by": {"argument": "kind", "shapes": {"secret": {"type": "nonsense"}}},
        })),
        ["invalid schema: `shaped_by` shape `secret` schema has an unsupported `type`"]
    );
}

/// A map takes every key and holds the value under each to what it declares;
/// a field the object names itself is held to its own schema, not the map's.
#[test]
fn a_map_takes_any_key_and_holds_every_value_to_what_it_declares() {
    const TAKEN: &str = "(the arguments were taken)";
    assert_eq!(told(&[counts()], &json!({"counts": {}})), TAKEN);
    assert_eq!(
        told(&[counts()], &json!({"counts": {"apples": 2, "pears": 0}})),
        TAKEN
    );
    assert_eq!(
        told(
            &[carried()],
            &json!({"carried": {"since": "a day", "cellar": {"apples": 2}, "attic": {}}})
        ),
        TAKEN
    );
}

/// What a listing shows before a call says what a map's values are.
#[test]
fn a_listing_writes_out_what_the_values_of_a_map_are() {
    assert_eq!(
        counts().declared_shape().as_deref(),
        Some("{<key>: <integer, at least 0>}")
    );
    assert_eq!(
        carried().declared_shape().as_deref(),
        Some("{\"since\": \"<text>\", <key>: {<key>: <integer>}}")
    );
}

/// What a map declares of its values is checked where it is published: it is
/// a schema that can be held, or `false` for an object that takes no key of
/// the caller's own.
#[test]
fn what_a_map_declares_of_its_values_is_checked_where_it_is_declared() {
    assert_eq!(counts().validate_declaration(), Vec::<String>::new());
    assert_eq!(carried().validate_declaration(), Vec::<String>::new());
    let declared = |of_its_own: Value| {
        ActionParam::required("counts", ParamType::Object, "How many of each")
            .with_schema(json!({"type": "object", "additionalProperties": of_its_own}))
            .validate_declaration()
    };
    assert_eq!(declared(json!(false)), Vec::<String>::new());
    assert_eq!(
        declared(json!(1)),
        ["invalid schema: schema `additionalProperties` must be a boolean or a schema"]
    );
    assert_eq!(
        declared(json!({"type": "nonsense"})),
        ["invalid schema: additionalProperties schema has an unsupported `type`"]
    );
}
