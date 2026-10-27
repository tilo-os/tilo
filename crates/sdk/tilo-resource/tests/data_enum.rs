//! An enum whose variants carry fields is a model type: stored as a map
//! naming its variant, its fields stored by variant, merged field by field,
//! and migrated from the shapes older versions stored.
#![allow(clippy::unwrap_used, reason = "test assertions")]

use automerge::ReadDoc;
use automerge::transaction::Transactable as _;
use tilo_resource::evolution;
use tilo_resource::{
    Edit, EditOp, Invoke, Model, ModelError, Replica, Resource, Segment, Shape, VARIANT_KEY,
    resource,
};

#[derive(Model, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Matcher {
    Host { name: String, port: u16 },
    Path(String),
    Anything,
}

/// Two variants naming a field alike, with types that do not fit each other:
/// the shape Canvas draws a rectangle and a text with, whose widths are
/// numbers in one and absent-or-a-number in the other.
#[derive(Model, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Figure {
    /// A box, `width` units wide.
    Rect { width: f64 },
    /// Text, wrapping at `width` units, or not at all when it is absent.
    Text { width: Option<f64> },
}

#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct Rules {
    pub matcher: Option<Matcher>,
    pub all: Vec<Matcher>,
}

#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct Card {
    pub figure: Option<Figure>,
}

/// Two variants naming a field `value` with types that do not fit: an edit
/// of one must never be read as the other's.
#[derive(Model, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Either {
    Text { value: String },
    Count { value: u64 },
}

#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct Choice {
    pub either: Option<Either>,
}

/// Variant and field names that contain the key codec's delimiters: the
/// storage keys they are stored under are escaped, so one variant's field is
/// never another's.
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
        if !(1..=2).contains(&from) {
            return Err(ModelError::invalid(format!("no migration from {from}")));
        }
        if from == 1 {
            // Version 1 stored each value as one JSON string.
            evolution::retype_json::<Option<Matcher>>(doc, &automerge::ROOT, "matcher")?;
            evolution::retype_json::<Vec<Matcher>>(doc, &automerge::ROOT, "all")?;
            return Ok(());
        }
        // Version 2 stored a variant's fields flat, beside its name; they are
        // stored by variant now.
        evolution::retype_variant_fields::<Option<Matcher>>(doc, &automerge::ROOT, "matcher")?;
        evolution::retype_variant_fields::<Vec<Matcher>>(doc, &automerge::ROOT, "all")?;
        Ok(())
    }
}

#[resource(kind = "test.card", label = "Card", version = 3, migrate = Card::upgrade)]
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

    /// Make the box wider, when the figure is a box.
    #[action]
    pub fn widen(&mut self, width: f64) {
        if let Some(Figure::Rect { width: drawn }) = &mut self.figure {
            *drawn = width;
        }
    }

    /// Change where the text wraps, when the figure is text.
    #[action]
    pub fn set_wrap(&mut self, width: f64) {
        if let Some(Figure::Text { width: drawn }) = &mut self.figure {
            *drawn = Some(width);
        }
    }

    fn upgrade(doc: &mut tilo_resource::Doc, from: u32) -> Result<(), ModelError> {
        if !(1..=2).contains(&from) {
            return Err(ModelError::invalid(format!("no migration from {from}")));
        }
        if from == 1 {
            // Version 1 stored the figure as one JSON string, last writer wins.
            evolution::retype_json::<Option<Figure>>(doc, &automerge::ROOT, "figure")?;
            return Ok(());
        }
        // Version 2 stored the variant's field flat, beside its name.
        evolution::retype_variant_fields::<Option<Figure>>(doc, &automerge::ROOT, "figure")?;
        Ok(())
    }
}

#[resource(kind = "test.choice", label = "Choice")]
impl Choice {
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

/// A field is stored by its variant: changing the variant leaves the fields
/// it had, so a value that returns to it reads them again (what an undo of
/// the change does, and what a change back does).
#[test]
fn a_variant_keeps_the_fields_changing_away_from_it_leaves() {
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
        vec![
            "$field/host".to_owned(),
            "$field/host/name".to_owned(),
            "$field/host/port".to_owned(),
            "$field/path".to_owned(),
            "$field/path/0".to_owned(),
            VARIANT_KEY.to_owned(),
        ],
        "the host's fields are still there, under its own keys"
    );
    assert_eq!(
        replica.state().unwrap().matcher,
        Some(Matcher::Path("/b".into()))
    );

    // The value returns to the host it was: its name and port are the ones it
    // had, not the path's text read as a host.
    replica
        .apply(Rules::MATCH_HOST.with(("example.org".to_owned(), 80_u16)))
        .unwrap();
    assert_eq!(
        replica.state().unwrap().matcher,
        Some(Matcher::Host {
            name: "example.org".into(),
            port: 80,
        })
    );
}

/// The regression this storage exists for: two variants name a field alike,
/// with types that do not fit each other, and one person edits the field of
/// the variant that is active while another changes the variant. However the
/// two merge, the active variant reads its own field — never the other
/// variant's value in its place, which an `Option` would read without
/// complaining.
#[test]
fn the_active_variant_never_reads_another_variants_field_of_the_same_name() {
    let mut ours = Replica::<Card>::new();
    ours.apply(Card::DRAW_RECT.with(120.0)).unwrap();
    let mut theirs = Replica::<Card>::from_doc(ours.doc_mut().fork());

    // One person makes the box wider; another changes the figure to text that
    // does not wrap. The variant write is theirs alone, so the text is what
    // the merged value is.
    ours.apply(Card::WIDEN.with(80.0)).unwrap();
    theirs.apply(Card::DRAW_TEXT.with(None)).unwrap();
    tilo_automerge::ApplyIncoming::merge_incoming(ours.doc_mut(), theirs.doc_mut()).unwrap();
    assert_eq!(
        ours.state().unwrap().figure,
        Some(Figure::Text { width: None }),
        "the text's own width, not the box's 80 read as its wrap width"
    );

    // The other way round: text changed where it wraps, and the figure changed
    // to a box. The box reads its own width, not the text's.
    let mut ours = Replica::<Card>::new();
    ours.apply(Card::DRAW_TEXT.with(Some(40.0))).unwrap();
    let mut theirs = Replica::<Card>::from_doc(ours.doc_mut().fork());
    ours.apply(Card::SET_WRAP.with(60.0)).unwrap();
    theirs.apply(Card::DRAW_RECT.with(10.0)).unwrap();
    tilo_automerge::ApplyIncoming::merge_incoming(ours.doc_mut(), theirs.doc_mut()).unwrap();
    assert_eq!(
        ours.state().unwrap().figure,
        Some(Figure::Rect { width: 10.0 }),
        "the box's own width, not the text's 60"
    );

    // And both variants' values are still in the document: the one the change
    // left reads again when the value returns to it.
    let mut ours = Replica::<Card>::new();
    ours.apply(Card::DRAW_RECT.with(120.0)).unwrap();
    let mut theirs = Replica::<Card>::from_doc(ours.doc_mut().fork());
    ours.apply(Card::WIDEN.with(80.0)).unwrap();
    theirs.apply(Card::DRAW_TEXT.with(Some(40.0))).unwrap();
    tilo_automerge::ApplyIncoming::merge_incoming(ours.doc_mut(), theirs.doc_mut()).unwrap();
    assert_eq!(
        ours.state().unwrap().figure,
        Some(Figure::Text { width: Some(40.0) })
    );
    // What a Ctrl+Z of the change writes: the variant it was, and none of the
    // text's fields (the box's were never touched).
    let doc = ours.doc_mut();
    let (_, figure) = doc.get(automerge::ROOT, "figure").unwrap().unwrap();
    tilo_resource::Slot::Key(&figure, VARIANT_KEY)
        .put_scalar(doc, automerge::ScalarValue::Str("rect".into()))
        .unwrap();
    doc.delete(&figure, "$field/text/width").unwrap();
    doc.commit();
    assert_eq!(
        ours.state().unwrap().figure,
        Some(Figure::Rect { width: 80.0 }),
        "the box's width is the one it had before the change"
    );
}

/// A variant's fields that are objects merge as records' fields do; and a
/// field of one variant is never written where another's is.
#[test]
fn fields_of_one_variant_are_stored_where_only_it_reads_them() {
    let mut replica = Replica::<Card>::new();
    replica.apply(Card::DRAW_RECT.with(5.0)).unwrap();
    replica.apply(Card::DRAW_TEXT.with(Some(7.0))).unwrap();
    let doc = replica.doc();
    let (_, figure) = doc.get(automerge::ROOT, "figure").unwrap().unwrap();
    let read = |key: &str| {
        doc.get(&figure, key)
            .unwrap()
            .map(|(value, _)| value.into_owned())
    };
    assert_eq!(
        read("$field/rect/width"),
        Some(automerge::Value::Scalar(std::borrow::Cow::Owned(
            automerge::ScalarValue::F64(5.0)
        )))
    );
    assert_eq!(
        read("$field/text/width"),
        Some(automerge::Value::Scalar(std::borrow::Cow::Owned(
            automerge::ScalarValue::F64(7.0)
        )))
    );
    assert_eq!(read("width"), None, "no flat field key is written");
}

/// A field edit names the variant and the field (`matcher.host.port`), so
/// two edits of one variant's different fields merge; the discriminant is
/// written only when the variant changes.
#[test]
fn field_edits_of_one_variant_merge_field_by_field() {
    let mut left = Replica::<Rules>::new();
    left.apply(Rules::MATCH_HOST.with(("example.org".to_owned(), 80_u16)))
        .unwrap();
    let mut right = left.fork();
    let field = |variant: &str, name: &str, value: serde_json::Value| {
        Edit::new(
            vec![
                Segment::Field("matcher".into()),
                Segment::Field(variant.into()),
                Segment::Field(name.into()),
            ],
            EditOp::Set { value },
        )
    };
    left.apply(Invoke::edit(&field("host", "name", serde_json::json!("tilo.example"))).unwrap())
        .unwrap();
    right
        .apply(Invoke::edit(&field("host", "port", serde_json::json!(8080))).unwrap())
        .unwrap();
    left.sync_with(&mut right).unwrap();
    assert_eq!(
        left.state().unwrap().matcher,
        Some(Matcher::Host {
            name: "tilo.example".into(),
            port: 8080,
        })
    );
}

/// A field edit of a variant that is no longer active writes that variant's
/// unused history; a later change back reads it, while the active variant is
/// unchanged.
#[test]
fn a_field_edit_of_an_inactive_variant_is_kept_as_unused_history() {
    let mut replica = Replica::<Rules>::new();
    replica
        .apply(Rules::MATCH_HOST.with(("example.org".to_owned(), 80_u16)))
        .unwrap();
    replica
        .apply(Rules::MATCH_PATH.with("/b".to_owned()))
        .unwrap();
    let port = Edit::new(
        vec![
            Segment::Field("matcher".into()),
            Segment::Field("host".into()),
            Segment::Field("port".into()),
        ],
        EditOp::Set {
            value: serde_json::json!(8080),
        },
    );
    replica.apply(Invoke::edit(&port).unwrap()).unwrap();
    assert_eq!(
        replica.state().unwrap().matcher,
        Some(Matcher::Path("/b".into())),
        "the active variant is untouched"
    );
    let doc = replica.doc();
    let (_, matcher) = doc.get(automerge::ROOT, "matcher").unwrap().unwrap();
    let (automerge::Value::Scalar(port), _) = doc.get(&matcher, "port").unwrap().unwrap() else {
        panic!("the port stays a scalar");
    };
    assert!(
        matches!(
            port.as_ref(),
            automerge::ScalarValue::Int(8080) | automerge::ScalarValue::Uint(8080)
        ),
        "the inactive variant's port is stored: {port:?}"
    );
}

/// The old shape: each field one JSON string, as a field of JSON text stored it.
fn old_json_document() -> tilo_resource::Doc {
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
    let mut doc = old_json_document();
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

/// The shape version 2 stored a variant in: its name under `$variant`, and
/// its fields flat beside it, one key per field.
fn version_two_document() -> tilo_resource::Doc {
    let mut doc = tilo_resource::Doc::new();
    let matcher = tilo_resource::Slot::Key(&automerge::ROOT, "matcher")
        .object(&mut doc, automerge::ObjType::Map)
        .unwrap();
    tilo_resource::Slot::Key(&matcher, VARIANT_KEY)
        .put_scalar(&mut doc, automerge::ScalarValue::Str("host".into()))
        .unwrap();
    tilo_resource::Slot::Key(&matcher, "name")
        .put_scalar(&mut doc, automerge::ScalarValue::Str("flat.example".into()))
        .unwrap();
    tilo_resource::Slot::Key(&matcher, "port")
        .put_scalar(&mut doc, automerge::ScalarValue::Uint(2222))
        .unwrap();
    // An older version could leave the fields of the variant it was before.
    tilo_resource::Slot::Key(&matcher, "0")
        .put_scalar(&mut doc, automerge::ScalarValue::Str("/left".into()))
        .unwrap();
    let all = tilo_resource::Slot::Key(&automerge::ROOT, "all")
        .object(&mut doc, automerge::ObjType::List)
        .unwrap();
    let anything = tilo_resource::Slot::Insert(&all, 0)
        .object(&mut doc, automerge::ObjType::Map)
        .unwrap();
    tilo_resource::Slot::Key(&anything, VARIANT_KEY)
        .put_scalar(&mut doc, automerge::ScalarValue::Str("anything".into()))
        .unwrap();
    let path = tilo_resource::Slot::Insert(&all, 1)
        .object(&mut doc, automerge::ObjType::Map)
        .unwrap();
    tilo_resource::Slot::Key(&path, VARIANT_KEY)
        .put_scalar(&mut doc, automerge::ScalarValue::Str("path".into()))
        .unwrap();
    tilo_resource::Slot::Key(&path, "0")
        .put_scalar(&mut doc, automerge::ScalarValue::Str("/y".into()))
        .unwrap();
    tilo_resource::Slot::Key(&automerge::ROOT, evolution::VERSION_KEY)
        .put_scalar(&mut doc, automerge::ScalarValue::Uint(2))
        .unwrap();
    doc.commit();
    doc
}

/// A document of the shape version 2 stored — a variant's fields flat beside
/// its name, in a `Vec` of them too — reads as it was written, then the
/// migration stores each field by its variant.
#[test]
fn a_flat_variant_migrates_to_fields_stored_by_variant() {
    let mut doc = version_two_document();
    // Read as it was written, before the migration: the flat fields are the
    // variant's, and an absent one is its empty value.
    assert_eq!(
        evolution::read_checked::<Rules>(&doc),
        Err(evolution::Unreadable {
            reason: "stored at version 2; it is brought up to version 3 when it opens".into(),
            stored: 2,
            expected: 3,
        }),
        "a document of another version is unreadable until it is migrated"
    );
    let read = |doc: &tilo_resource::Doc| {
        (
            tilo_resource::read_key::<Option<Matcher>>(doc, &automerge::ROOT, "matcher").unwrap(),
            tilo_resource::read_key::<Vec<Matcher>>(doc, &automerge::ROOT, "all").unwrap(),
        )
    };
    assert_eq!(
        read(&doc),
        (
            Some(Matcher::Host {
                name: "flat.example".into(),
                port: 2222,
            }),
            vec![Matcher::Anything, Matcher::Path("/y".into())],
        ),
        "the flat fields read as the variant they were stored for"
    );

    assert!(evolution::migrate::<Rules>(&mut doc).unwrap());
    assert_eq!(evolution::stored_version(&doc), 3);
    assert_eq!(
        read(&doc),
        (
            Some(Matcher::Host {
                name: "flat.example".into(),
                port: 2222,
            }),
            vec![Matcher::Anything, Matcher::Path("/y".into())],
        ),
        "the value is what it was, stored by variant now"
    );
    let matcher = tilo_resource::read_key::<Option<Matcher>>(&doc, &automerge::ROOT, "matcher")
        .unwrap()
        .unwrap();
    let (_, stored) = doc.get(automerge::ROOT, "matcher").unwrap().unwrap();
    let mut keys: Vec<String> = doc.keys(&stored).collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "$field/host".to_owned(),
            "$field/host/name".to_owned(),
            "$field/host/port".to_owned(),
            VARIANT_KEY.to_owned(),
            // What the variant before the host left under a flat key: the
            // migration leaves it for the variant it was stored for, and
            // stores the host's own fields by variant.
            "0".to_owned(),
        ],
        "the host's fields are stored by variant, and the flat keys it was read from are gone"
    );
    assert_eq!(
        matcher,
        Matcher::Host {
            name: "flat.example".into(),
            port: 2222,
        }
    );
}

/// A field stored both ways at once — the flat key an older version wrote
/// beside the fields this one stores — is a write one of the two would lose,
/// so it is reported, never read as if the other were not there.
#[test]
fn a_flat_field_beside_a_variants_own_fields_is_reported() {
    let mut replica = Replica::<Card>::new();
    replica.apply(Card::DRAW_RECT.with(5.0)).unwrap();
    let doc = replica.doc_mut();
    let (_, figure) = doc.get(automerge::ROOT, "figure").unwrap().unwrap();
    // What a replica of the version that stored fields flat wrote, merged in.
    tilo_resource::Slot::Key(&figure, "width")
        .put_scalar(doc, automerge::ScalarValue::F64(9.0))
        .unwrap();
    doc.commit();
    let error = Card::read(doc).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("stored both by variant and flat"),
        "{error}"
    );
}

/// A figure kept as one JSON string migrates to a field by variant, and one
/// of a variant whose fields were stored flat migrates too — reading, never
/// defaulting, what the document holds.
#[test]
fn a_card_of_the_older_shapes_migrates() {
    let mut doc = tilo_resource::Doc::new();
    let figure = serde_json::to_string(&Some(Figure::Text { width: Some(12.0) })).unwrap();
    tilo_resource::write_key(
        &mut doc,
        &automerge::ROOT,
        "figure",
        &tilo_resource::Atom(figure),
    )
    .unwrap();
    doc.commit();
    assert!(evolution::migrate::<Card>(&mut doc).unwrap());
    assert_eq!(
        evolution::read_checked::<Card>(&doc).unwrap().figure,
        Some(Figure::Text { width: Some(12.0) })
    );

    // A version-2 card (this app's, before fields were stored by variant).
    let mut doc = tilo_resource::Doc::new();
    let stored = tilo_resource::Slot::Key(&automerge::ROOT, "figure")
        .object(&mut doc, automerge::ObjType::Map)
        .unwrap();
    tilo_resource::Slot::Key(&stored, VARIANT_KEY)
        .put_scalar(&mut doc, automerge::ScalarValue::Str("rect".into()))
        .unwrap();
    tilo_resource::Slot::Key(&stored, "width")
        .put_scalar(&mut doc, automerge::ScalarValue::F64(3.0))
        .unwrap();
    tilo_resource::Slot::Key(&automerge::ROOT, evolution::VERSION_KEY)
        .put_scalar(&mut doc, automerge::ScalarValue::Uint(2))
        .unwrap();
    doc.commit();
    assert!(evolution::migrate::<Card>(&mut doc).unwrap());
    assert_eq!(
        evolution::read_checked::<Card>(&doc).unwrap().figure,
        Some(Figure::Rect { width: 3.0 })
    );
}

/// A field edit names the variant and the field (`matcher.host.port`), so
/// two edits of one variant's different fields merge; the discriminant is
/// written only when the variant changes.
#[test]
fn field_edits_of_one_variant_merge_field_by_field() {
    let mut left = Replica::<Rules>::new();
    left.apply(Rules::MATCH_HOST.with(("example.org".to_owned(), 80_u16)))
        .unwrap();
    let mut right = left.fork();
    let field = |variant: &str, name: &str, value: serde_json::Value| {
        Edit::new(
            vec![
                Segment::Field("matcher".into()),
                Segment::Field(variant.into()),
                Segment::Field(name.into()),
            ],
            EditOp::Set { value },
        )
    };
    left.apply(Invoke::edit(&field("host", "name", serde_json::json!("tilo.example"))).unwrap())
        .unwrap();
    right
        .apply(Invoke::edit(&field("host", "port", serde_json::json!(8080))).unwrap())
        .unwrap();
    left.sync_with(&mut right).unwrap();
    assert_eq!(
        left.state().unwrap().matcher,
        Some(Matcher::Host {
            name: "tilo.example".into(),
            port: 8080,
        })
    );
}

/// A field edit of a variant that is no longer active writes that variant's
/// unused history; a later change back reads it, while the active variant is
/// unchanged.
#[test]
fn a_field_edit_of_an_inactive_variant_is_kept_as_unused_history() {
    let mut replica = Replica::<Rules>::new();
    replica
        .apply(Rules::MATCH_HOST.with(("example.org".to_owned(), 80_u16)))
        .unwrap();
    replica
        .apply(Rules::MATCH_PATH.with("/b".to_owned()))
        .unwrap();
    let port = Edit::new(
        vec![
            Segment::Field("matcher".into()),
            Segment::Field("host".into()),
            Segment::Field("port".into()),
        ],
        EditOp::Set {
            value: serde_json::json!(8080),
        },
    );
    replica.apply(Invoke::edit(&port).unwrap()).unwrap();
    assert_eq!(
        replica.state().unwrap().matcher,
        Some(Matcher::Path("/b".into())),
        "the active variant is untouched"
    );
    let doc = replica.doc();
    let (_, matcher) = doc.get(automerge::ROOT, "matcher").unwrap().unwrap();
    let (automerge::Value::Scalar(port), _) =
        doc.get(&matcher, "$field/host/port").unwrap().unwrap()
    else {
        panic!("the port is stored under its variant");
    };
    assert!(
        matches!(
            port.as_ref(),
            automerge::ScalarValue::Int(8080) | automerge::ScalarValue::Uint(8080)
        ),
        "the inactive variant's port is stored: {port:?}"
    );
}

/// Typed field edits name the variant before the field, so a text's `value`
/// and a count's `value` — types that do not fit each other — are stored and
/// read apart, whoever edits which first.
#[test]
fn typed_field_edits_of_different_variants_never_read_each_others_value() {
    let mut ours = Replica::<Choice>::new();
    ours.apply(Choice::TEXT.with("hello".into())).unwrap();
    let mut theirs = Replica::<Choice>::from_doc(ours.doc_mut().fork());
    let field = |variant: &str, value: serde_json::Value| {
        Edit::new(
            vec![
                Segment::Field("either".into()),
                Segment::Field(variant.into()),
                Segment::Field("value".into()),
            ],
            EditOp::Set { value },
        )
    };
    ours.apply(Invoke::edit(&field("text", serde_json::json!("world"))).unwrap())
        .unwrap();
    theirs
        .apply(Invoke::edit(&field("count", serde_json::json!(7))).unwrap())
        .unwrap();
    ours.sync_with(&mut theirs).unwrap();
    assert_eq!(
        ours.state().unwrap().either,
        Some(Either::Text {
            value: "world".into(),
        }),
        "the active text reads its own value, never the count's 7"
    );

    // The count's field is its own, so changing the variant to it reads the 7
    // the other replica wrote, not the text's value.
    let doc = ours.doc_mut();
    let (_, either) = doc.get(automerge::ROOT, "either").unwrap().unwrap();
    tilo_resource::Slot::Key(&either, VARIANT_KEY)
        .put_scalar(doc, automerge::ScalarValue::Str("count".into()))
        .unwrap();
    doc.commit();
    assert_eq!(
        ours.state().unwrap().either,
        Some(Either::Count { value: 7 }),
        "the count stored its own 7"
    );
}

fn collect_shape_keys(shape: &Shape, out: &mut Vec<&'static str>) {
    match *shape {
        Shape::Variant(variants) => {
            for variant in variants {
                out.push(variant.key);
                for field in variant.fields {
                    out.push(field.key);
                    collect_shape_keys(&field.shape, out);
                }
            }
        }
        Shape::Record(fields) => {
            for field in fields {
                out.push(field.key);
                collect_shape_keys(&field.shape, out);
            }
        }
        Shape::Collection(inner) | Shape::Keyed(inner) | Shape::Map(inner) => {
            collect_shape_keys(inner, out);
        }
        Shape::Value => {}
    }
}

/// The shape the agent, history and surface read publishes the variant and
/// field names — never the `$field/…` keys the document stores them under.
#[test]
fn published_paths_name_the_variant_and_field_not_their_storage() {
    let mut keys = Vec::new();
    collect_shape_keys(&Either::SHAPE, &mut keys);
    assert!(keys.contains(&"text"));
    assert!(keys.contains(&"count"));
    assert!(keys.contains(&"value"));
    assert!(
        keys.iter().all(|key| !key.starts_with('$')),
        "no storage key is published: {keys:?}"
    );

    let mut escaped = Vec::new();
    collect_shape_keys(&Escaped::SHAPE, &mut escaped);
    assert!(escaped.contains(&"left/right"));
    assert!(escaped.contains(&"left%2Fright"));
    assert!(escaped.contains(&"value/one"));
    assert!(
        escaped.iter().all(|key| !key.starts_with("$field")),
        "no escaped storage key is published: {escaped:?}"
    );
}

/// A variant and field whose names contain the codec's delimiters are stored
/// under keys distinct from another variant's, and those keys are exactly
/// [`tilo_resource::model::field_key`].
#[test]
fn storage_keys_escape_their_variant_and_field() {
    let mut storages = Vec::new();
    for variant in Escaped::__VARIANTS {
        for field in variant.fields {
            let storage = field.storage_key().into_owned();
            assert_eq!(
                storage,
                tilo_resource::model::field_key(variant.key, field.key),
                "the derive and the runtime codec agree"
            );
            assert!(
                !field.key.starts_with('$'),
                "the published field key is logical"
            );
            storages.push(storage);
        }
    }
    storages.sort();
    storages.dedup();
    assert_eq!(
        storages.len(),
        2,
        "the two variants' same-named field is stored apart: {storages:?}"
    );
    assert_eq!(
        tilo_resource::model::field_key("left/right", "value/one"),
        "$field/left%2Fright/value%2Fone"
    );
}
