//! A schema that names its own definitions admits exactly what the same
//! schema written out in full admits, and a reference that could reach
//! outside it is refused before any value is read.

use serde_json::{Value, json};
use tilo_actions::{params::share_repeated_subschemas, validate_json_schema};

/// Samples a schema's values: what it admits, what it refuses, and what is
/// near the edge of each bound — a type, a bound, an alternative, a required
/// key, an unknown property, an array's length and depth.
fn probes() -> Vec<Value> {
    let mut probes = vec![
        Value::Null,
        json!(true),
        json!(0),
        json!(1),
        json!(5),
        json!(6),
        json!(""),
        json!("text"),
        json!("rect"),
        json!([]),
        json!([1, 2]),
        json!([1, 2, 3]),
        json!([[1, 2], [3, 4]]),
        json!({}),
        json!({"op": "set"}),
        json!({"op": "unknown"}),
        json!({"op": 7}),
        json!({"op": "set", "extra": true}),
        json!({"op": "set", "then": "splice"}),
        json!({"op": "set", "then": "unknown"}),
        json!({"op": "set", "crop": [0, 0, 4, 4]}),
        json!({"op": "set", "crop": [0, 0, 4]}),
        json!({"op": "set", "crop": [0, 0, 4, "4"]}),
        json!({"op": "set", "from": [0, 0], "to": [1, 1]}),
        json!({"op": "set", "from": [0, 0, 0]}),
        json!({"op": "set", "path": [{"field": "title"}]}),
        json!({"op": "set", "path": ["title"]}),
        json!({"op": "set", "kind": "rect"}),
        json!({"op": "set", "kind": "circle"}),
        json!({"op": "set", "steps": {"count": 2}}),
        json!({"op": "set", "steps": {"count": 9}}),
        json!({"op": "set", "steps": {}}),
        json!({"op": "set", "steps": {"count": 2, "name": "abcd"}}),
        json!({"op": "set", "steps": {"count": 2, "name": "abcde"}}),
        json!({"op": "set", "steps": {"count": 2, "items": [{"x": 1}]}}),
        json!({"op": "set", "steps": {"count": 2, "items": [1]}}),
        json!({"op": "set", "steps": {"count": 2, "extra": 1}}),
    ];
    let rect = json!({"kind": "rect", "x": 0, "y": 0, "width": 4, "height": 4});
    let points = json!([[0, 0], [1, 1]]);
    probes.extend([
        json!({"op": "set", "shape": rect}),
        json!({"op": "set", "shape": {"kind": "rect", "x": 0, "y": 0, "width": 9, "height": 4}}),
        json!({"op": "set", "shape": {"kind": "rect", "x": 0.5, "y": 0, "width": 4, "height": 4}}),
        json!({"op": "set", "shape": {"kind": "rect", "x": 0, "y": 0, "width": 4, "height": 4, "radius": 1}}),
        json!({"op": "set", "shape": {"kind": "rect", "x": 0, "y": 0}}),
        json!({"op": "set", "shape": {"kind": "rect", "x": "no", "y": 0, "width": 4, "height": 4}}),
        json!({"op": "set", "shape": {"kind": "ellipse", "points": points}}),
        json!({"op": "set", "shape": {"kind": "text", "points": points}}),
        json!({"op": "set", "shape": {"kind": "text", "points": [[0]]}}),
        json!({"op": "set", "shape": {"kind": "text"}}),
        json!({"op": "set", "shape": {"points": points}}),
        json!({"op": "set", "shape": {"kind": "unknown", "points": points}}),
    ]);
    probes
}

/// A schema that states the same shapes several times, as a published
/// surface does: points, rectangles, an edit's operations, an alternative
/// per shape, nested arrays, bounds and closed objects.
fn surface() -> Value {
    let point = json!({"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2});
    let region =
        json!({"type": "array", "items": {"type": "number"}, "minItems": 4, "maxItems": 4});
    let operations = json!({
        "type": "string",
        "enum": ["set", "splice", "replace", "add", "insert", "remove", "insert-item", "move-to"]
    });
    json!({
        "type": "object",
        "properties": {
            "kind": {"type": "string", "enum": ["rect", "ellipse", "text"], "description": "The shape."},
            "shape": {
                "oneOf": [
                    {
                        "type": "object",
                        "required": ["kind", "x", "y", "width", "height"],
                        "properties": {
                            "kind": {"type": "string", "const": "rect"},
                            "x": {"type": "number"},
                            "y": {"type": "number"},
                            "width": {"type": "number", "minimum": 0, "maximum": 5},
                            "height": {"type": "number"}
                        },
                        "additionalProperties": false,
                        "description": "An axis-aligned rectangle."
                    },
                    {
                        "type": "object",
                        "required": ["kind", "points"],
                        "properties": {
                            "kind": {"type": "string", "const": "ellipse"},
                            "points": {
                                "type": "array",
                                "items": {
                                    "type": "array",
                                    "items": {"type": "number"},
                                    "minItems": 2,
                                    "maxItems": 2
                                }
                            }
                        },
                        "additionalProperties": false,
                        "description": "A closed curve through points."
                    },
                    {
                        "type": "object",
                        "required": ["kind", "points"],
                        "properties": {
                            "kind": {"type": "string", "const": "text"},
                            "points": {
                                "type": "array",
                                "items": {
                                    "type": "array",
                                    "items": {"type": "number"},
                                    "minItems": 2,
                                    "maxItems": 2
                                }
                            }
                        },
                        "additionalProperties": false,
                        "description": "Words at the points."
                    }
                ]
            },
            "crop": region,
            "frame": region,
            "from": point,
            "to": point,
            "path": {"type": "array", "items": {"type": "object"}},
            "op": operations,
            "then": operations,
            "steps": {
                "type": "object",
                "properties": {
                    "name": {"type": "string", "minLength": 1, "maxLength": 4},
                    "count": {"type": "integer", "minimum": 0, "maximum": 4},
                    "items": {"type": "array", "items": {"type": "object"}}
                },
                "required": ["count"],
                "additionalProperties": false
            }
        },
        "required": ["op"],
        "additionalProperties": false
    })
}

#[test]
fn a_schema_says_a_shape_once_and_admits_exactly_what_it_did() {
    let stated = surface();
    let mut shared = stated.clone();
    share_repeated_subschemas(&mut shared);
    assert!(
        shared.get("$defs").is_some(),
        "the schema states its shapes once: {shared}"
    );

    let mut admitted = 0_usize;
    let mut refused = 0_usize;
    for probe in probes() {
        let before = validate_json_schema(&probe, &stated);
        let after = validate_json_schema(&probe, &shared);
        match (&before, &after) {
            (Ok(()), Ok(())) => admitted = admitted.saturating_add(1),
            (Err(_), Err(_)) => refused = refused.saturating_add(1),
            (Ok(()), Err(reason)) => {
                panic!(
                    "{probe} was admitted before and refused after the shapes were shared: {reason}\n{shared}"
                );
            }
            (Err(before), Ok(())) => {
                panic!(
                    "{probe} was refused before ({before}) and admitted after the shapes were shared\n{shared}"
                );
            }
        }
    }
    assert!(
        admitted > 10 && refused > 10,
        "{admitted} admitted, {refused} refused"
    );
}

#[test]
fn every_subschema_the_sharing_states_is_read_by_the_validator() {
    let mut shared = surface();
    share_repeated_subschemas(&mut shared);
    let definitions = shared["$defs"]
        .as_object()
        .expect("the schema carries the shapes it repeats");
    assert!(definitions.len() >= 3, "{shared}");
    // A reference is only as good as what it names: every one resolves, and
    // the definition is a schema the vocabulary accepts.
    let mut references = 0_usize;
    let mut pending = vec![&shared];
    while let Some(schema) = pending.pop() {
        match schema {
            Value::Object(fields) => {
                for (key, value) in fields {
                    if key == "$ref" {
                        let reference = value.as_str().unwrap();
                        let name = reference.strip_prefix("#/$defs/").unwrap_or_else(|| {
                            panic!("`{reference}` is a local reference to this schema's `$defs`")
                        });
                        assert!(definitions.contains_key(name), "`{reference}` names {name}");
                        references = references.saturating_add(1);
                    }
                    pending.push(value);
                }
            }
            Value::Array(items) => pending.extend(items),
            _ => {}
        }
    }
    assert!(references >= 4, "{references} references: {shared}");
}

#[test]
fn what_a_reference_says_beside_it_still_holds() {
    // `$ref` is one keyword among the others (JSON Schema 2020-12): what a
    // subschema states beside it applies as well.
    let schema = json!({
        "$defs": {"count": {"type": "integer", "minimum": 0}},
        "type": "object",
        "properties": {
            "a": {"$ref": "#/$defs/count", "maximum": 2},
            "b": {"$ref": "#/$defs/count", "description": "Zero or more."}
        },
        "additionalProperties": false
    });
    assert!(validate_json_schema(&json!({"a": 1}), &schema).is_ok());
    assert_eq!(
        validate_json_schema(&json!({"a": 3}), &schema),
        Err("property `a` must be at most 2".into()),
        "the sibling bound holds"
    );
    assert_eq!(
        validate_json_schema(&json!({"b": -1}), &schema),
        Err("property `b` must be at least 0".into()),
        "the named definition holds"
    );
    assert!(validate_json_schema(&json!({"b": 9}), &schema).is_ok());
    assert!(
        validate_json_schema(&json!({"b": 1.5}), &schema).is_err(),
        "the named definition's type holds"
    );
}

#[test]
fn a_reference_outside_the_schema_is_refused_rather_than_fetched() {
    for reference in [
        "https://example.test/mark.schema.json",
        "https://example.test/mark.schema.json#/$defs/mark",
        "file:///etc/passwd#/$defs/mark",
        "other.json#/$defs/mark",
        "/schemas/mark.json",
        "#/definitions/mark",
        "#/$defs/missing",
    ] {
        let schema = json!({"$ref": reference});
        let refusal = validate_json_schema(&json!({}), &schema)
            .expect_err("a reference to something else resolves nothing");
        assert!(
            refusal.contains("$ref"),
            "`{reference}` is refused in its own words: {refusal}"
        );
    }
    let not_a_string = json!({"$ref": 7});
    assert!(validate_json_schema(&json!({}), &not_a_string).is_err());
}

#[test]
fn a_reference_that_would_follow_itself_is_refused() {
    let direct = json!({"$defs": {"loop": {"$ref": "#/$defs/loop"}}, "$ref": "#/$defs/loop"});
    assert!(
        validate_json_schema(&json!({}), &direct)
            .is_err_and(|reason| reason.contains("follows itself")),
        "a definition that names itself is refused"
    );
    let indirect = json!({
        "$defs": {
            "a": {"$ref": "#/$defs/b"},
            "b": {"type": "object", "properties": {"next": {"$ref": "#/$defs/a"}}}
        },
        "$ref": "#/$defs/a"
    });
    assert!(
        validate_json_schema(&json!({}), &indirect)
            .is_err_and(|reason| reason.contains("follows itself")),
        "so is one that names itself through another"
    );
    let root = json!({"$ref": "#"});
    assert!(validate_json_schema(&json!({}), &root).is_err());
}

#[test]
fn a_schema_too_large_to_read_is_refused() {
    let definitions: serde_json::Map<String, Value> = (0..65)
        .map(|index| (format!("d{index}"), json!({"type": "string"})))
        .collect();
    let oversized = json!({"$defs": definitions, "type": "object"});
    assert!(
        validate_json_schema(&json!({}), &oversized)
            .is_err_and(|reason| reason.contains("definitions")),
        "a schema states a bounded number of definitions"
    );
    // A reference chain is bounded too, however it is written.
    let mut chained = json!({"$defs": {}, "type": "object"});
    for index in 0..40_u32 {
        let name = format!("d{index}");
        let next = format!("#/$defs/d{}", index.saturating_add(1));
        chained["$defs"][&name] = json!({"$ref": next});
    }
    chained["$defs"]["d40"] = json!({"type": "object"});
    chained["$ref"] = json!("#/$defs/d0");
    assert!(validate_json_schema(&json!({}), &chained).is_ok());
}

#[test]
fn a_keyword_tilo_does_not_know_is_still_refused() {
    for schema in [
        json!({"patternProperties": {"^x": {"type": "string"}}}),
        json!({"anyOf": [{"type": "string"}]}),
        json!({"$defs": {"x": {"type": "string"}}, "not": {"type": "string"}}),
        json!({"$defs": {"x": {"$ref": "#/$defs/x", "minContains": 1}}, "type": "object"}),
    ] {
        let refusal = validate_json_schema(&json!("text"), &schema)
            .expect_err("an unknown keyword is a contract Tilo cannot hold");
        assert!(refusal.contains("unsupported keyword"), "{refusal}");
    }
}

#[test]
fn a_definition_named_with_an_escaped_key_is_reached() {
    let schema = json!({
        "$defs": {"mark/style": {"type": "string", "minLength": 2}},
        "type": "object",
        "properties": {"fill": {"$ref": "#/$defs/mark~1style"}},
        "additionalProperties": false
    });
    assert!(validate_json_schema(&json!({"fill": "ok"}), &schema).is_ok());
    assert!(validate_json_schema(&json!({"fill": "o"}), &schema).is_err());
    let unescaped = json!({
        "$defs": {"mark/style": {"type": "string"}},
        "properties": {"fill": {"$ref": "#/$defs/mark/style"}}
    });
    assert!(
        validate_json_schema(&json!({"fill": "ok"}), &unescaped).is_err(),
        "a `/` in a name is `~1` in a pointer, never a path of its own"
    );
}
