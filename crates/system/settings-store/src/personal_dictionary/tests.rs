use super::*;
use std::sync::Arc;

fn word(spelling: &str, language: Option<&str>) -> Word {
    Word::new(spelling, language).unwrap()
}

fn document(section: Option<Value>) -> Arc<crate::HostDocument> {
    let mut initial = Map::new();
    if let Some(section) = section {
        initial.insert(SECTION.to_owned(), section);
    }
    Arc::new(crate::HostDocument::new(initial, |_| Ok(())))
}

#[test]
fn add_edit_remove_and_reset_change_the_persons_settings() {
    let source = document(None);
    let osk = Reader::bind(Arc::clone(&source) as Arc<dyn SettingsSource>);
    let dictation = osk.clone();
    let added = edit_in(source.as_ref(), 0, Edit::Add(word("Tilo", None))).unwrap();
    assert_eq!(dictation.snapshot().unwrap(), added);
    assert_eq!(added.completions("ti", "en", 3), ["Tilo"]);
    assert!(added.contains("tilo", "nl"));
    assert!(!added.contains("tile", "en"));
    let edited = edit_in(
        source.as_ref(),
        added.revision,
        Edit::Replace {
            old: word("Tilo", None),
            new: word("TiLo", None),
        },
    )
    .unwrap();
    assert_eq!(
        osk.snapshot().unwrap().hints("en").collect::<Vec<_>>(),
        ["TiLo"]
    );
    let removed = edit_in(
        source.as_ref(),
        edited.revision,
        Edit::Remove(word("tilo", None)),
    )
    .unwrap();
    assert!(dictation.snapshot().unwrap().words.is_empty());
    edit_in(source.as_ref(), removed.revision, Edit::Reset).unwrap();
    assert!(
        source.load()[SECTION]["words"]["words"]
            .as_array()
            .is_some()
    );
    assert!(osk.snapshot().unwrap().words.is_empty());
}

#[test]
fn the_words_are_a_section_of_the_persons_document_and_survive_a_restart() {
    let directory = crate::test_dir();
    let store = crate::Store::open(directory.path()).unwrap();
    edit_in(store.as_ref(), 0, Edit::Add(word("Tilo", None))).unwrap();
    let words = store.load()[SECTION]["words"].clone();
    assert_eq!(
        words["words"],
        serde_json::json!([{"spelling": "Tilo", "language": null}])
    );
    store.flush_pending();
    drop(store);
    let restarted = crate::Store::open(directory.path()).unwrap();
    assert!(snapshot_in(restarted.as_ref()).contains("tilo", "en"));
    assert!(restarted.load().get("app_device_local").is_none());
}

#[test]
fn unicode_languages_and_duplicate_intent_are_preserved() {
    let source = document(None);
    let mut snapshot = Snapshot::default();
    for added in [
        word("Élodie", Some("FR")),
        word("E\u{301}LODIE", Some("fr")),
        word("東京", Some("ja")),
        word("İpek", Some("tr")),
        word("Tilo", None),
    ] {
        snapshot = edit_in(source.as_ref(), snapshot.revision, Edit::Add(added)).unwrap();
    }
    assert_eq!(snapshot.words.len(), 4);
    assert_eq!(snapshot.completions("e\u{301}l", "fr-CA", 3), ["Élodie"]);
    assert!(snapshot.contains("ÉLODIE", "fr"));
    assert!(!snapshot.contains("Élodie", "en"));
    assert_eq!(snapshot.hints("ja").collect::<Vec<_>>(), ["東京", "Tilo"]);
    assert_eq!(snapshot.hints("tr").collect::<Vec<_>>(), ["İpek", "Tilo"]);
    assert!(snapshot.completions("", "fr", 3).is_empty());
    assert!(snapshot.completions("T", "fr", 0).is_empty());
    for invalid in ["", "two words", "word\ncommand", "123"] {
        assert!(Word::new(invalid, None).is_err());
    }
    assert!(Word::new("name", Some("../en")).is_err());
}

#[test]
fn a_stale_edit_is_refused_and_leaves_the_words_alone() {
    let source = document(None);
    let first = Reader::bind(Arc::clone(&source) as Arc<dyn SettingsSource>);
    let second = Reader::bind(Arc::clone(&source) as Arc<dyn SettingsSource>);
    let added = edit_in(source.as_ref(), 0, Edit::Add(word("Tilo", None))).unwrap();
    assert_eq!(second.snapshot().unwrap(), added);
    assert_eq!(
        edit_in(source.as_ref(), 0, Edit::Reset).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(first.snapshot().unwrap().contains("Tilo", "en"));
    let removed = edit_in(
        source.as_ref(),
        added.revision,
        Edit::Remove(word("Tilo", None)),
    )
    .unwrap();
    assert_eq!(removed.revision, added.revision + 1);
    assert!(!first.snapshot().unwrap().contains("Tilo", "en"));
}

#[test]
fn a_revoked_reader_and_every_clone_fail() {
    let source = document(None);
    let reader = Reader::bind(source);
    let clone = reader.clone();
    assert!(clone.snapshot().is_ok());
    assert!(reader.same_binding(&clone));
    reader.revoke();
    assert_eq!(
        clone.snapshot().unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    let other = Reader::bind(document(None));
    assert!(!reader.same_binding(&other));
}

#[test]
fn an_unreadable_section_is_empty_and_never_fails_a_read() {
    for section in [
        serde_json::json!({"words": "not a snapshot"}),
        serde_json::json!({"words": {"revision": 1, "words": [{"spelling": "two words"}]}}),
        serde_json::json!([1, 2]),
    ] {
        let source = document(Some(section));
        assert_eq!(snapshot_in(source.as_ref()), Snapshot::default());
    }
}
