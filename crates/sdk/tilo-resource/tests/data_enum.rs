//! An enum whose variants carry fields is a model type: stored as a map
//! naming its variant, its fields stored by variant, merged field by field,
//! and migrated from the shapes older versions stored.
#![allow(clippy::unwrap_used, reason = "test assertions")]

use automerge::ReadDoc;
use tilo_resource::evolution;
use tilo_resource::{Invoke, Model, ModelError, Replica, Resource, VARIANT_KEY, resource};

#[derive(Model, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Matcher {
    Host { name: String, port: u16 },
    Path(String),
    Anything,
}

/// Two variants naming a field alike, with types that do not fit each other:
/// a box `width` units wide, and text wrapping at `width` units or not at
/// all.
#[derive(Model, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Figure {
    Rect { width: f64 },
    Text { width: Option<f64> },
}

/// Two variants naming a field `value` stored as a text and as a number.
#[derive(Model, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Either {
    Text { value: String },
    Count { value: u64 },
}

/// Variant and field names holding the storage key's delimiters.
#[derive(Model, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Escaped {
    #[model(rename = "left/right")]
    Slash {
        #[model(rename = "value/one")]
        value: String,
    },
    #[model(rename = "left%2Fright")]
    Percent {
        #[model(rename = "value/one")]
        value: u64,
    },
}

#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct Rules {
    pub matcher: Option<Matcher>,
    pub all: Vec<Matcher>,
}

#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct Card {
    pub figure: Option<Figure>,
    pub either: Option<Either>,
}

#[resource(kind = "test.card", label = "Card")]
impl Card {
    /// Draw a box `width` units wide.
    #[action]
    pub fn draw_rect(&mut self, width: f64) {
        self.figure = Some(Figure::Rect { width });
    }

    /// Draw text wrapping at `width` units, or not wrapping.
    #[action]
    pub fn draw_text(&mut self, width: Option<f64>) {
        self.figure = Some(Figure::Text { width });
    }

    /// Make the box `width` units wide, when the figure is a box.
    #[action]
    pub fn widen(&mut self, width: f64) {
        if let Some(Figure::Rect { width: drawn }) = &mut self.figure {
            *drawn = width;
        }
    }

    /// Wrap the text at `width` units, when the figure is text.
    #[action]
    pub fn set_wrap(&mut self, width: f64) {
        if let Some(Figure::Text { width: drawn }) = &mut self.figure {
            *drawn = Some(width);
        }
    }

    /// Make the text, saying `value`.
    #[action]
    pub fn text(&mut self, value: String) {
        self.either = Some(Either::Text { value });
    }

    /// Make the count, counting `value`.
    #[action]
    pub fn count(&mut self, value: u64) {
        self.either = Some(Either::Count { value });
    }
}

#[resource(kind = "test.rules", label = "Rules", version = 3, migrate = Rules::upgrade)]
impl Rules {
    /// Match a host.
    #[action]
    pub fn match_host(&mut self, name: String, port: u16) {
        self.matcher = Some(Matcher::Host { name, port });
    }

    /// Match a path.
    #[action]
    pub fn match_path(&mut self, path: String) {
        self.matcher = Some(Matcher::Path(path));
    }

    /// Rename the host matched.
    #[action]
    pub fn rename_host(&mut self, to: String) {
        if let Some(Matcher::Host { name, .. }) = &mut self.matcher {
            *name = to;
        }
    }

    /// Change the port matched.
    #[action]
    pub fn set_port(&mut self, to: u16) {
        if let Some(Matcher::Host { port, .. }) = &mut self.matcher {
            *port = to;
        }
    }

    fn upgrade(doc: &mut tilo_resource::Doc, from: u32) -> Result<(), ModelError> {
        match from {
            // Each value was one JSON string.
            1 => {
                evolution::retype_json::<Option<Matcher>>(doc, &automerge::ROOT, "matcher")?;
                evolution::retype_json::<Vec<Matcher>>(doc, &automerge::ROOT, "all")?;
            }
            // A variant's fields were flat, beside its name.
            2 => {
                evolution::retype_variant_fields::<Option<Matcher>>(
                    doc,
                    &automerge::ROOT,
                    "matcher",
                )?;
                evolution::retype_variant_fields::<Vec<Matcher>>(doc, &automerge::ROOT, "all")?;
            }
            _ => return Err(ModelError::invalid(format!("no migration from {from}"))),
        }
        Ok(())
    }
}

#[test]
fn a_data_enum_round_trips_through_the_document_and_json() {
    let mut replica = Replica::<Rules>::new();
    let host = Matcher::Host {
        name: "example.org".into(),
        port: 443,
    };
    replica
        .apply(Rules::MATCH_HOST.with(("example.org".to_owned(), 443_u16)))
        .unwrap();
    assert_eq!(replica.state().unwrap().matcher, Some(host.clone()));
    for value in [host, Matcher::Path("/a".into()), Matcher::Anything] {
        assert_eq!(Matcher::from_json(&value.to_json()).unwrap(), value);
    }
    assert_eq!(
        Matcher::Path("/a".into()).to_json(),
        serde_json::json!({ VARIANT_KEY: "path", "0": "/a" })
    );
}

#[test]
fn concurrent_edits_of_one_variant_merge_field_by_field() {
    let mut left = Replica::<Rules>::new();
    left.apply(Rules::MATCH_HOST.with(("example.org".to_owned(), 80_u16)))
        .unwrap();
    let mut right = Replica::<Rules>::from_doc(left.doc_mut().fork());
    left.apply(Rules::RENAME_HOST.with("tilo.example".to_owned()))
        .unwrap();
    right.apply(Rules::SET_PORT.with(8080_u16)).unwrap();
    tilo_automerge::ApplyIncoming::merge_incoming(left.doc_mut(), right.doc_mut()).unwrap();
    let merged = Rules::read(left.doc()).unwrap();
    assert_eq!(
        merged.matcher,
        Some(Matcher::Host {
            name: "tilo.example".into(),
            port: 8080,
        }),
        "each side's field survives"
    );
}

#[test]
fn changing_the_variant_removes_the_fields_of_the_old_one() {
    let mut replica = Replica::<Rules>::new();
    replica
        .apply(Rules::MATCH_HOST.with(("example.org".to_owned(), 80_u16)))
        .unwrap();
    replica
        .apply(Rules::MATCH_PATH.with("/b".to_owned()))
        .unwrap();
    let doc = replica.doc();
    let (_, matcher) = doc.get(automerge::ROOT, "matcher").unwrap().unwrap();
    let mut keys: Vec<String> = doc.keys(&matcher).collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["$field/path/0".to_owned(), VARIANT_KEY.to_owned()]
    );
    assert_eq!(
        replica.state().unwrap().matcher,
        Some(Matcher::Path("/b".into()))
    );
}

/// The old shape: each field one JSON string, as a field of JSON text stored it.
fn old_document() -> tilo_resource::Doc {
    let mut doc = tilo_resource::Doc::new();
    let matcher = serde_json::to_string(&Some(Matcher::Host {
        name: "old.example".into(),
        port: 22,
    }))
    .unwrap();
    let all = serde_json::to_string(&vec![Matcher::Anything, Matcher::Path("/x".into())]).unwrap();
    tilo_resource::write_key(
        &mut doc,
        &automerge::ROOT,
        "matcher",
        &tilo_resource::Atom(matcher),
    )
    .unwrap();
    tilo_resource::write_key(&mut doc, &automerge::ROOT, "all", &tilo_resource::Atom(all)).unwrap();
    doc.commit();
    doc
}

#[test]
fn a_json_field_of_the_old_shape_migrates_to_the_data_enum() {
    let mut doc = old_document();
    assert!(
        evolution::read_checked::<Rules>(&doc).is_err(),
        "unmigrated"
    );
    assert!(evolution::migrate::<Rules>(&mut doc).unwrap());
    let rules = evolution::read_checked::<Rules>(&doc).unwrap();
    assert_eq!(
        rules.matcher,
        Some(Matcher::Host {
            name: "old.example".into(),
            port: 22,
        })
    );
    assert_eq!(
        rules.all,
        vec![Matcher::Anything, Matcher::Path("/x".into())]
    );
}

/// One person edits a field of the active variant while another changes the
/// variant to one naming a field alike. However the two merge, the variant
/// reads its own field: never the other's value in its place (which an
/// `Option` would read without complaint), and never a value of another
/// type that leaves the document unreadable.
#[test]
fn the_active_variant_never_reads_another_variants_field_of_the_same_name() {
    let merged = |first: Invoke<Card>, ours: Invoke<Card>, theirs: Invoke<Card>| {
        let mut left = Replica::<Card>::new();
        left.apply(first).unwrap();
        let mut right = Replica::<Card>::from_doc(left.doc_mut().fork());
        left.apply(ours).unwrap();
        right.apply(theirs).unwrap();
        tilo_automerge::ApplyIncoming::merge_incoming(left.doc_mut(), right.doc_mut()).unwrap();
        Card::read(left.doc()).unwrap()
    };

    let card = merged(
        Card::DRAW_RECT.with(120.0),
        Card::WIDEN.with(80.0),
        Card::DRAW_TEXT.with(None),
    );
    assert_eq!(
        card.figure,
        Some(Figure::Text { width: None }),
        "the text's own width, not the box's 80 read as where it wraps"
    );

    let card = merged(
        Card::DRAW_TEXT.with(Some(40.0)),
        Card::SET_WRAP.with(60.0),
        Card::DRAW_RECT.with(10.0),
    );
    assert_eq!(
        card.figure,
        Some(Figure::Rect { width: 10.0 }),
        "the box's own width, not the text's 60"
    );

    let card = merged(
        Card::TEXT.with("hello".to_owned()),
        Card::TEXT.with("world".to_owned()),
        Card::COUNT.with(7_u64),
    );
    assert_eq!(
        card.either,
        Some(Either::Count { value: 7 }),
        "the count reads its own number, not the text's"
    );
}

/// A variant's fields are stored under keys of its own, escaped so that no
/// two variants' fields share one.
#[test]
fn a_variants_fields_are_stored_under_keys_of_its_own() {
    let mut doc = tilo_resource::Doc::new();
    let keys = |doc: &tilo_resource::Doc| {
        let (_, stored) = doc.get(automerge::ROOT, "value").unwrap().unwrap();
        let mut keys: Vec<String> = doc.keys(&stored).collect();
        keys.sort();
        keys
    };
    let slash = Escaped::Slash { value: "a".into() };
    tilo_resource::write_key(&mut doc, &automerge::ROOT, "value", &slash).unwrap();
    assert_eq!(
        keys(&doc),
        vec![
            "$field/left%2Fright/value%2Fone".to_owned(),
            VARIANT_KEY.to_owned()
        ]
    );
    assert_eq!(
        tilo_resource::read_key::<Escaped>(&doc, &automerge::ROOT, "value").unwrap(),
        slash
    );
    let percent = Escaped::Percent { value: 3 };
    tilo_resource::write_key(&mut doc, &automerge::ROOT, "value", &percent).unwrap();
    assert_eq!(
        keys(&doc),
        vec![
            "$field/left%252Fright/value%2Fone".to_owned(),
            VARIANT_KEY.to_owned()
        ]
    );
    assert_eq!(
        tilo_resource::read_key::<Escaped>(&doc, &automerge::ROOT, "value").unwrap(),
        percent
    );
}

/// A document of the layout version 2 wrote: a variant's name under
/// `$variant`, and its fields flat beside it.
fn version_two_document() -> tilo_resource::Doc {
    let mut doc = tilo_resource::Doc::new();
    evolution::fixture(
        &mut doc,
        &automerge::ROOT,
        &serde_json::json!({
            "matcher": { VARIANT_KEY: "host", "name": {"$text": "flat.example"}, "port": 2222 },
            "all": [{ VARIANT_KEY: "anything" }, { VARIANT_KEY: "path", "0": {"$text": "/y"} }],
            (evolution::VERSION_KEY): 2,
        }),
    )
    .unwrap();
    doc.commit();
    doc
}

/// A document of the flat layout reads as it was written, and its migration
/// stores each field by its variant, reading what the document holds.
#[test]
fn a_flat_variant_reads_as_written_and_migrates_to_fields_by_variant() {
    let read = |doc: &tilo_resource::Doc| {
        (
            tilo_resource::read_key::<Option<Matcher>>(doc, &automerge::ROOT, "matcher").unwrap(),
            tilo_resource::read_key::<Vec<Matcher>>(doc, &automerge::ROOT, "all").unwrap(),
        )
    };
    let held = (
        Some(Matcher::Host {
            name: "flat.example".into(),
            port: 2222,
        }),
        vec![Matcher::Anything, Matcher::Path("/y".into())],
    );
    let mut doc = version_two_document();
    assert_eq!(read(&doc), held, "the flat fields are the variant's");
    assert!(
        evolution::read_checked::<Rules>(&doc).is_err(),
        "unmigrated"
    );

    assert!(evolution::migrate::<Rules>(&mut doc).unwrap());
    assert_eq!(evolution::stored_version(&doc), 3);
    assert_eq!(read(&doc), held, "the value is what it was");
    let rules = evolution::read_checked::<Rules>(&doc).unwrap();
    assert_eq!((rules.matcher, rules.all), held);
    let (_, stored) = doc.get(automerge::ROOT, "matcher").unwrap().unwrap();
    let mut keys: Vec<String> = doc.keys(&stored).collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "$field/host/name".to_owned(),
            "$field/host/port".to_owned(),
            VARIANT_KEY.to_owned(),
        ],
        "stored by variant, the flat keys gone"
    );
}
