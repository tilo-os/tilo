use super::*;

fn word(spelling: &str, language: Option<&str>) -> Word {
    Word::new(spelling, language).unwrap()
}

#[test]
fn add_edit_remove_reset_and_restart_use_one_persisted_dictionary() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(root.path().join("vocabulary")).unwrap();
    let osk = service.bind(Some([1; 32])).unwrap();
    let dictation = osk.clone();
    let added = service
        .edit(&osk, 0, Edit::Add(word("Tilo", None)))
        .unwrap();
    assert_eq!(dictation.snapshot().unwrap(), added);
    assert_eq!(added.completions("ti", "en", 3), ["Tilo"]);
    assert!(added.contains("tilo", "nl"));
    assert!(!added.contains("tile", "en"));
    let restarted = Service::new(root.path().join("vocabulary")).unwrap();
    let reader = restarted.bind(Some([1; 32])).unwrap();
    assert_eq!(reader.snapshot().unwrap(), added);
    let edited = restarted
        .edit(
            &reader,
            added.revision,
            Edit::Replace {
                old: word("Tilo", None),
                new: word("TiLo", None),
            },
        )
        .unwrap();
    assert_eq!(
        dictation
            .snapshot()
            .unwrap()
            .hints("en")
            .collect::<Vec<_>>(),
        ["TiLo"]
    );
    let removed = restarted
        .edit(&reader, edited.revision, Edit::Remove(word("tilo", None)))
        .unwrap();
    assert!(osk.snapshot().unwrap().words.is_empty());
    let added = service
        .edit(&osk, removed.revision, Edit::Add(word("Quinten", None)))
        .unwrap();
    service.edit(&osk, added.revision, Edit::Reset).unwrap();
    assert!(reader.snapshot().unwrap().words.is_empty());
    assert!(
        restarted
            .bind(Some([1; 32]))
            .unwrap()
            .snapshot()
            .unwrap()
            .words
            .is_empty()
    );
}

#[test]
fn unicode_languages_and_duplicate_intent_are_preserved() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(root.path().to_owned()).unwrap();
    let reader = service.bind(Some([2; 32])).unwrap();
    let mut snapshot = reader.snapshot().unwrap();
    for added in [
        word("Élodie", Some("FR")),
        word("E\u{301}LODIE", Some("fr")),
        word("東京", Some("ja")),
        word("İpek", Some("tr")),
        word("Tilo", None),
    ] {
        snapshot = service
            .edit(&reader, snapshot.revision, Edit::Add(added))
            .unwrap();
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
fn account_switching_revokes_old_consumers_and_anonymous_access() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(root.path().to_owned()).unwrap();
    assert!(service.bind(None).is_none());
    let alice = service.bind(Some([1; 32])).unwrap();
    service
        .edit(&alice, 0, Edit::Add(word("AlicePrivate", None)))
        .unwrap();
    let bob = service.bind(Some([2; 32])).unwrap();
    assert!(bob.snapshot().unwrap().words.is_empty());
    assert_eq!(
        alice.snapshot().unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(service.edit(&alice, 0, Edit::Reset).is_err());
    service
        .edit(&bob, 0, Edit::Add(word("BobPrivate", None)))
        .unwrap();
    let alice_again = service.bind(Some([1; 32])).unwrap();
    assert_eq!(
        alice_again
            .snapshot()
            .unwrap()
            .hints("en")
            .collect::<Vec<_>>(),
        ["AlicePrivate"]
    );
    assert!(alice.snapshot().is_err());
    service.bind(None);
    assert!(alice_again.snapshot().is_err());
    assert!(bob.snapshot().is_err());
    let different_host = Service::new(root.path().join("other-host")).unwrap();
    let foreign = different_host.bind(Some([1; 32])).unwrap();
    assert!(service.edit(&foreign, 0, Edit::Reset).is_err());
    drop(different_host);
    assert!(foreign.snapshot().is_err());
}

#[test]
fn stale_edits_cannot_clobber_another_consumer_or_resurrect_removed_words() {
    let root = tempfile::tempdir().unwrap();
    let first = Service::new(root.path().to_owned()).unwrap();
    let second = Service::new(root.path().to_owned()).unwrap();
    let a = first.bind(Some([1; 32])).unwrap();
    let b = second.bind(Some([1; 32])).unwrap();
    first.edit(&a, 0, Edit::Add(word("Tilo", None))).unwrap();
    assert_eq!(
        second.edit(&b, 0, Edit::Reset).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(b.snapshot().unwrap().contains("Tilo", "en"));
    second
        .edit(&b, 1, Edit::Remove(word("Tilo", None)))
        .unwrap();
    assert!(!a.snapshot().unwrap().contains("Tilo", "en"));
    first.edit(&a, 2, Edit::Add(word("NewWord", None))).unwrap();
    assert!(b.snapshot().unwrap().contains("NewWord", "en"));
}

#[test]
fn corrupt_storage_fails_closed_without_destroying_it_and_files_are_private() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(root.path().join("private")).unwrap();
    let reader = service.bind(Some([1; 32])).unwrap();
    service
        .edit(&reader, 0, Edit::Add(word("Tilo", None)))
        .unwrap();
    let path = service.0.path([1; 32]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    std::fs::write(&path, b"{incomplete").unwrap();
    assert!(reader.snapshot().is_err());
    assert!(service.edit(&reader, 0, Edit::Reset).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"{incomplete");
}

#[test]
fn expected_account_reader_cannot_cross_a_concurrent_switch() {
    let root = tempfile::tempdir().unwrap();
    let service = Arc::new(Service::new(root.path().to_owned()).unwrap());
    for (owner, spelling) in [([1; 32], "Alice"), ([2; 32], "Bob")] {
        let reader = service.bind(Some(owner)).unwrap();
        service
            .edit(&reader, 0, Edit::Add(word(spelling, None)))
            .unwrap();
    }
    let switcher = Arc::clone(&service);
    let switching = std::thread::spawn(move || {
        for i in 0..2_000 {
            switcher.bind(Some([if i % 2 == 0 { 1 } else { 2 }; 32]));
        }
    });
    for _ in 0..2_000 {
        if let Some(reader) = service.reader_for([1; 32])
            && let Ok(snapshot) = reader.snapshot()
        {
            assert_eq!(snapshot.hints("en").collect::<Vec<_>>(), ["Alice"]);
        }
    }
    switching.join().unwrap();
    service.bind(Some([2; 32]));
    assert!(service.reader_for([1; 32]).is_none());
}

#[cfg(feature = "watch")]
#[test]
fn another_host_edit_and_corruption_wake_live_dictionary_consumers() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(root.path().to_owned()).unwrap();
    let reader = service.bind(Some([1; 32])).unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let listener: Arc<crate::Listener> = Arc::new(move || {
        let _ = send.send(());
    });
    service.changes().listen(&listener);
    let _watch = service.watch().unwrap();
    let other = Service::new(root.path().to_owned()).unwrap();
    let other_reader = other.bind(Some([1; 32])).unwrap();
    other
        .edit(&other_reader, 0, Edit::Add(word("Tilo", None)))
        .unwrap();
    receive.recv_timeout(tilo_test_env::hang_bound()).unwrap();
    assert_eq!(
        reader.snapshot().unwrap().hints("en").collect::<Vec<_>>(),
        ["Tilo"]
    );
    while receive.try_recv().is_ok() {}
    let revision = other_reader.snapshot().unwrap().revision;
    other.edit(&other_reader, revision, Edit::Reset).unwrap();
    receive.recv_timeout(tilo_test_env::hang_bound()).unwrap();
    assert!(reader.snapshot().unwrap().words.is_empty());
    while receive.try_recv().is_ok() {}
    std::fs::write(service.0.path([1; 32]), b"{bad").unwrap();
    receive.recv_timeout(tilo_test_env::hang_bound()).unwrap();
    assert!(reader.snapshot().is_err());
}
