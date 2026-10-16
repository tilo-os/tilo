#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]

//! `#[derive(JsonSchema)]` describes exactly what serde reads and writes,
//! for each of the shapes serde attributes give a type.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tilo_resource::JsonSchema;

fn accepts<T: Serialize + JsonSchema>(value: &T) {
    let json = serde_json::to_value(value).unwrap();
    tilo_actions::validate_json_schema(&json, &T::json_schema())
        .unwrap_or_else(|reason| panic!("{json} is refused: {reason}"));
}

fn refuses<T: JsonSchema>(json: &Value) {
    assert!(
        tilo_actions::validate_json_schema(json, &T::json_schema()).is_err(),
        "{json} is accepted"
    );
}

/// A shape to draw.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Shape {
    /// A dot.
    Point,
    Circle {
        /// Radius in pixels.
        radius: f64,
    },
    Label(String),
    Pair(i32, i32),
    #[serde(rename = "poly")]
    Polygon {
        points: Vec<(u32, u32)>,
        #[serde(default)]
        closed: bool,
        #[serde(skip)]
        cached: Option<String>,
    },
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum Tagged {
    Unit,
    WithFields { at: usize, note: Option<String> },
    Inner(Fields),
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", content = "args")]
enum Adjacent {
    Clear,
    Rename { title: String },
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Fields {
    start_block: String,
    #[serde(rename = "len")]
    length: u8,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
struct Name(String);

#[derive(Serialize, Deserialize, JsonSchema)]
struct Generic<T> {
    items: Vec<T>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
enum Plain {
    Body,
    Header,
}

#[test]
fn externally_tagged_enums_follow_their_renames() {
    for shape in [
        Shape::Point,
        Shape::Circle { radius: 2.5 },
        Shape::Label("x".into()),
        Shape::Pair(1, -2),
        Shape::Polygon {
            points: vec![(1, 2)],
            closed: true,
            cached: None,
        },
    ] {
        accepts(&shape);
    }
    let schema = Shape::json_schema();
    assert_eq!(schema["description"], "A shape to draw.");
    assert_eq!(
        schema["oneOf"][0],
        json!({"type": "string", "const": "point", "description": "A dot."})
    );
    assert_eq!(
        schema["oneOf"][1]["properties"]["circle"]["properties"]["radius"]["description"],
        "Radius in pixels."
    );
    // A defaulted field may be left out; a skipped one is not read at all.
    let polygon = &schema["oneOf"][4]["properties"]["poly"];
    assert_eq!(polygon["required"], json!(["points"]));
    assert!(polygon["properties"].get("cached").is_none());
    let read: Shape = serde_json::from_value(json!({"poly": {"points": []}})).unwrap();
    accepts(&read);
    refuses::<Shape>(&json!({"polygon": {"points": []}}));
    refuses::<Shape>(&json!({"circle": {"radius": 1, "extra": 1}}));
    refuses::<Shape>(&json!({"pair": [1]}));
}

#[test]
fn internally_and_adjacently_tagged_enums_carry_their_tags() {
    accepts(&Tagged::Unit);
    accepts(&Tagged::WithFields { at: 3, note: None });
    accepts(&Tagged::Inner(Fields {
        start_block: "b".into(),
        length: 4,
    }));
    refuses::<Tagged>(&json!({"kind": "with-fields"}));
    refuses::<Tagged>(&json!({"at": 3}));

    accepts(&Adjacent::Clear);
    accepts(&Adjacent::Rename { title: "t".into() });
    refuses::<Adjacent>(&json!({"action": "Rename", "args": {}}));
}

#[test]
fn structs_newtypes_and_generics() {
    accepts(&Fields {
        start_block: "b".into(),
        length: 255,
    });
    assert_eq!(
        Fields::json_schema()["required"],
        json!(["startBlock", "len"])
    );
    refuses::<Fields>(&json!({"startBlock": "b", "len": 256}));
    assert_eq!(Name::json_schema(), json!({"type": "string"}));
    accepts(&Generic {
        items: vec![Name("n".into())],
    });
    refuses::<Generic<u8>>(&json!({"items": ["n"]}));
    assert_eq!(
        Plain::json_schema(),
        json!({"type": "string", "enum": ["Body", "Header"]})
    );
}

/// Numbers narrower than their types.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Ranged {
    /// 1 to 100.
    #[schema(range(min = 1, max = 100))]
    quality: Option<u8>,
    #[schema(range(min = 0.05, max = 4.0))]
    scale: f64,
    #[schema(range(max = 64))]
    count: u32,
    #[schema(range(min = -1))]
    offset: i64,
}

#[test]
fn a_field_range_narrows_its_bounds_and_is_enforced() {
    let schema = Ranged::json_schema();
    let properties = &schema["properties"];
    assert_eq!(
        properties["quality"],
        json!({"type": ["integer", "null"], "minimum": 1, "maximum": 100, "description": "1 to 100."})
    );
    assert_eq!(
        properties["scale"],
        json!({"type": "number", "minimum": 0.05, "maximum": 4.0})
    );
    assert_eq!(
        properties["count"],
        json!({"type": "integer", "minimum": 0, "maximum": 64})
    );
    assert_eq!(
        properties["offset"],
        json!({"type": "integer", "minimum": -1})
    );
    accepts(&Ranged {
        quality: Some(100),
        scale: 0.05,
        count: 64,
        offset: -1,
    });
    let valid = json!({"quality": null, "scale": 1, "count": 0, "offset": 0});
    assert!(tilo_actions::validate_json_schema(&valid, &schema).is_ok());
    for (field, value) in [
        ("quality", json!(0)),
        ("quality", json!(101)),
        ("scale", json!(5)),
        ("count", json!(65)),
        ("offset", json!(-2)),
    ] {
        let mut wrong = valid.clone();
        wrong[field] = value;
        refuses::<Ranged>(&wrong);
    }
}

#[test]
fn every_schema_is_publishable() {
    for schema in [
        Ranged::json_schema(),
        Shape::json_schema(),
        Tagged::json_schema(),
        Adjacent::json_schema(),
        Fields::json_schema(),
        Generic::<Name>::json_schema(),
    ] {
        let param = tilo_actions::ActionParam::required(
            "value",
            tilo_actions::ParamType::Object,
            "A value",
        )
        .with_schema(schema);
        assert_eq!(param.validate_declaration(), Vec::<String>::new());
    }
}
