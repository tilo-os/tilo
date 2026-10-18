//! An enum whose variants carry fields is a model type: stored as a map
//! naming its variant, merged field by field, and migrated from the one
//! JSON string an old whole-value field held.
#![allow(clippy::unwrap_used, reason = "test assertions")]

use automerge::ReadDoc;
use tilo_resource::evolution;
use tilo_resource::{Model, ModelError, Replica, Resource, VARIANT_KEY, resource};

#[derive(Model, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Matcher {
    Host { name: String, port: u16 },
    Path(String),
    Anything,
}

#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct Rules {
    pub matcher: Option<Matcher>,
    pub all: Vec<Matcher>,
}

#[resource(kind = "test.rules", label = "Rules", version = 2, migrate = Rules::upgrade)]
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
        if from != 1 {
            return Err(ModelError::invalid(format!("no migration from {from}")));
        }
        evolution::retype_json::<Option<Matcher>>(doc, &automerge::ROOT, "matcher")?;
        evolution::retype_json::<Vec<Matcher>>(doc, &automerge::ROOT, "all")?;
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
    assert_eq!(keys, vec!["$variant".to_owned(), "0".to_owned()]);
    assert_eq!(
        replica.state().unwrap().matcher,
        Some(Matcher::Path("/b".into()))
    );
}

/// The old shape: each field one JSON string, as `Json<T>` stored it.
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
