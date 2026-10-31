use std::os::unix::ffi::OsStrExt as _;

use tilo_service::{Answer, Questions};

use super::*;
use crate::reply::reply;

fn destination_options(directory: bool) -> HashMap<String, OwnedValue> {
    let choices = vec![(
        tilo_dialogs::portal::WRITABLE_DIRECTORY_CHOICE.to_owned(),
        "Import here".to_owned(),
        Vec::<(String, String)>::new(),
        "true".to_owned(),
    )];
    HashMap::from([
        ("directory".into(), OwnedValue::from(directory)),
        (
            "choices".into(),
            OwnedValue::try_from(Value::new(choices)).unwrap(),
        ),
    ])
}

fn caller() -> PortalCaller {
    PortalCaller::new("org.example")
}

/// The same, running in the session's sandbox in `room`.
fn sandboxed(room: &str) -> PortalCaller {
    PortalCaller {
        sandboxed: true,
        workspace: Some(room.into()),
        ..caller()
    }
}

/// `pick` with the person answering `uris` (declining on `None`); the
/// question they saw, and the reply.
async fn picked(question: FileChooserQuestion, uris: Option<Vec<String>>) -> (Question, Reply) {
    picked_by(&caller(), &tilo_documents::Documents::new(), question, uris).await
}

/// [`picked`] for `caller`, whose chosen documents `documents` keeps.
async fn picked_by(
    caller: &PortalCaller,
    documents: &tilo_documents::Documents,
    question: FileChooserQuestion,
    uris: Option<Vec<String>>,
) -> (Question, Reply) {
    let questions = Questions::new();
    let portal = crate::tests::portal_with(&questions, documents.clone());
    let path = "/org/freedesktop/portal/desktop/request/1_2/t";
    let respond = async {
        tokio::task::yield_now().await;
        let asked = questions.pending().pop().expect("the chooser was asked");
        match uris {
            Some(uris) => assert!(questions.answer(
                &asked.handle,
                Answer {
                    uris,
                    ..Answer::default()
                }
            )),
            None => assert!(questions.decline(&asked.handle)),
        }
        asked
    };
    let (outcome, asked) = tokio::join!(pick(&portal, caller, path, question), respond);
    (asked, reply(outcome))
}

fn uris_of(results: &Results) -> Vec<String> {
    Vec::<String>::try_from(results["uris"].try_clone().unwrap()).unwrap()
}

#[test]
fn writable_directory_requires_explicit_single_directory_request() {
    assert!(!writable_directory_requested(&HashMap::new()));
    assert!(!writable_directory_requested(&destination_options(false)));
    let mut options = destination_options(true);
    assert!(writable_directory_requested(&options));
    options.insert("multiple".into(), OwnedValue::from(true));
    assert!(!writable_directory_requested(&options));
    options.remove("multiple");
    options.insert("choices".into(), OwnedValue::from(false));
    assert!(!writable_directory_requested(&options));
}

#[tokio::test]
async fn directory_consent_exports_only_the_chosen_folder() {
    let root = tempfile::tempdir().unwrap();
    let selected = root.path().join("import # destination");
    std::fs::create_dir(&selected).unwrap();
    let sibling = root.path().join("not selected");
    std::fs::create_dir(&sibling).unwrap();
    for (write, selection) in [
        (false, Some(vec![file_uri(&selected).unwrap()])),
        (true, Some(vec![file_uri(&selected).unwrap()])),
        (true, None),
    ] {
        let mut options = destination_options(true);
        if !write {
            options.remove("choices");
        }
        let question = decode_open_options("Open folder".into(), &options);
        assert_eq!(question.writable_directory, write);
        let cancelled = selection.is_none();
        let (asked, (code, result)) = picked(question, selection).await;
        assert_eq!(asked.kind, FILE_CHOOSER_QUESTION);
        let details: FileChooserQuestion = serde_json::from_value(asked.details).unwrap();
        assert!(details.directory && !details.save && !details.multiple);
        assert_eq!(details.writable_directory, write);
        if cancelled {
            assert_eq!(code, 1);
            assert!(result.is_empty());
        } else {
            assert_eq!(code, 0);
            let uris = uris_of(&result);
            assert_eq!(uris, vec![file_uri(&selected).unwrap()]);
            assert!(!uris.contains(&file_uri(&sibling).unwrap()));
            assert_eq!(option::<bool>(&result, "writable"), Some(write));
            assert_eq!(result.contains_key("choices"), write);
        }
    }
}

#[tokio::test]
async fn a_writable_destination_refuses_files_and_several_folders() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("file");
    std::fs::write(&file, b"unchanged").unwrap();
    let folder = file_uri(directory.path()).unwrap();
    for uris in [vec![file_uri(&file).unwrap()], vec![folder.clone(), folder]] {
        let question = FileChooserQuestion {
            directory: true,
            writable_directory: true,
            ..FileChooserQuestion::default()
        };
        let (_, (code, result)) = picked(question, Some(uris)).await;
        assert_eq!(code, 2);
        assert!(result.is_empty());
    }
    assert_eq!(std::fs::read(file).unwrap(), b"unchanged");
}

#[tokio::test]
async fn open_hands_out_no_write_access_and_save_does() {
    for (save, write) in [(false, false), (true, true)] {
        let question = FileChooserQuestion {
            title: "t".into(),
            save,
            ..FileChooserQuestion::default()
        };
        let (_, (code, result)) =
            picked(question, Some(vec!["file:///tmp/target.txt".into()])).await;
        assert_eq!(code, 0);
        assert_eq!(uris_of(&result), ["file:///tmp/target.txt"]);
        assert_eq!(
            option::<bool>(&result, "writable"),
            Some(write),
            "save={save}: an Open must not hand out write access, and a Save must"
        );
    }
}

#[tokio::test]
async fn a_sandboxed_app_is_handed_a_store_path_that_holds_what_was_picked() {
    let folder = tempfile::tempdir().unwrap();
    let chosen = folder.path().join("notes.txt");
    std::fs::write(&chosen, b"what the person picked").unwrap();
    let documents = tilo_documents::Documents::new();
    let caller = sandboxed("studio");

    let (_, (code, results)) = picked_by(
        &caller,
        &documents,
        FileChooserQuestion::default(),
        Some(vec![file_uri(&chosen).unwrap()]),
    )
    .await;
    assert_eq!(code, 0);
    let uris = uris_of(&results);
    assert_eq!(uris.len(), 1);
    let exported = path_of_uri(&uris[0]).expect("an absolute path");
    assert!(
        exported.starts_with(tilo_documents::SANDBOX_DIR),
        "the app is handed a path in the store it has bound, not {}",
        exported.display()
    );
    assert_eq!(exported.file_name(), Some("notes.txt".as_ref()));
    assert_eq!(option::<bool>(&results, "writable"), Some(false));

    // What the app opens is the file the person picked: the export names
    // that document of that app's, and reading it reads the file.
    let key = crate::doc_store::key_of(&caller).expect("a sandboxed caller in a room");
    let target = tilo_documents::host::resolve(&documents, &key, &exported)
        .expect("the export is a document of the app");
    assert_eq!(
        std::fs::read(target.host_path()).unwrap(),
        b"what the person picked"
    );
}

/// The store `documents` served as a filesystem, where this machine can
/// mount one: the same table the portal granted through, so a file chosen
/// for an app is a file in that app's view.
fn mounted_store(
    documents: &tilo_documents::Documents,
) -> Option<(tempfile::TempDir, tilo_documents::mount::Mounted)> {
    let at = tempfile::tempdir().unwrap();
    match tilo_documents::mount::mount(documents.clone(), &at.path().join("documents")) {
        Ok(mounted) => Some((at, mounted)),
        Err(error) => {
            tilo_test_env::capability_unavailable(
                tilo_test_env::Capability::Fuse,
                format_args!("mounting the documents failed: {error}"),
            );
            None
        }
    }
}

#[tokio::test]
async fn what_a_sandboxed_app_is_handed_is_the_picked_file_in_its_own_view() {
    let documents = tilo_documents::Documents::new();
    let Some((_at, mounted)) = mounted_store(&documents) else {
        return;
    };
    let folder = tempfile::tempdir().unwrap();
    let chosen = folder.path().join("report.pdf");
    std::fs::write(&chosen, b"%PDF-1.7 what the person picked").unwrap();
    let caller = sandboxed("studio");

    let (_, (code, results)) = picked_by(
        &caller,
        &documents,
        FileChooserQuestion::default(),
        Some(vec![file_uri(&chosen).unwrap()]),
    )
    .await;
    assert_eq!(code, 0);
    let exported = path_of_uri(&uris_of(&results)[0]).expect("an absolute path");

    // The launcher binds the app's view of the store at the sandbox
    // directory, so the path the chooser handed it names a file there.
    let key = crate::doc_store::key_of(&caller).expect("a sandboxed caller in a room");
    let within = exported
        .strip_prefix(tilo_documents::SANDBOX_DIR)
        .expect("a path inside the store the app has bound");
    let handed = tilo_documents::app_view(mounted.path(), &key).join(within);
    assert_eq!(
        std::fs::read(&handed).unwrap(),
        b"%PDF-1.7 what the person picked"
    );

    // And it is that app's own view: another app's is not handed it.
    let other = tilo_documents::AppKey::new("studio", "org.example.other").unwrap();
    let siblings = std::fs::read_dir(tilo_documents::app_view(mounted.path(), &other))
        .expect("another app's view answers");
    assert_eq!(siblings.count(), 0, "no documents before it chose any");
}

#[tokio::test]
async fn a_host_app_is_handed_the_plain_path() {
    let folder = tempfile::tempdir().unwrap();
    let chosen = folder.path().join("notes.txt");
    std::fs::write(&chosen, b"x").unwrap();
    // A host app, and a sandboxed one whose room the session does not
    // know: neither has a view of the store, so both are handed the path.
    let unknown_room = PortalCaller {
        sandboxed: true,
        ..caller()
    };
    for caller in [caller(), unknown_room] {
        let (_, (code, results)) = picked_by(
            &caller,
            &tilo_documents::Documents::new(),
            FileChooserQuestion::default(),
            Some(vec![file_uri(&chosen).unwrap()]),
        )
        .await;
        assert_eq!(code, 0);
        assert_eq!(uris_of(&results), [file_uri(&chosen).unwrap()]);
    }
}

#[tokio::test]
async fn a_sandboxed_save_names_a_writable_path_with_the_typed_name() {
    let folder = tempfile::tempdir().unwrap();
    let chosen = folder.path().join("quarterly report.txt");
    let documents = tilo_documents::Documents::new();
    let caller = sandboxed("studio");
    let question = FileChooserQuestion {
        title: "Save".into(),
        save: true,
        current_name: Some("quarterly report.txt".into()),
        ..FileChooserQuestion::default()
    };

    let (_, (code, results)) = picked_by(
        &caller,
        &documents,
        question,
        Some(vec![file_uri(&chosen).unwrap()]),
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(option::<bool>(&results, "writable"), Some(true));
    let uri = uris_of(&results)[0].clone();
    let exported = path_of_uri(&uri).expect("an absolute path");
    assert!(exported.starts_with(tilo_documents::SANDBOX_DIR));
    assert_eq!(
        exported.file_name(),
        Some("quarterly report.txt".as_ref()),
        "the name, and so the extension, the person typed"
    );

    // Writing where the app was told to write writes the file the person
    // named.
    let key = crate::doc_store::key_of(&caller).expect("a sandboxed caller in a room");
    let target = tilo_documents::host::resolve(&documents, &key, &exported)
        .expect("the export is a document of the app");
    std::fs::write(target.host_path(), b"saved").unwrap();
    assert_eq!(std::fs::read(&chosen).unwrap(), b"saved");
}

#[tokio::test]
async fn only_local_files_and_only_as_many_as_asked_are_accepted() {
    for uris in [
        vec!["https://example.com/a.txt".to_owned()],
        vec!["file:///tmp/a".to_owned(), "file:///tmp/b".to_owned()],
    ] {
        let (_, (code, _)) = picked(FileChooserQuestion::default(), Some(uris)).await;
        assert_eq!(code, 2);
    }
    let (_, (code, _)) = picked(FileChooserQuestion::default(), Some(Vec::new())).await;
    assert_eq!(code, 1, "nothing chosen is a cancel");
}

#[test]
fn open_options_decode_selection_mode_independently() {
    let plain = decode_open_options("Open".into(), &HashMap::new());
    assert!(!plain.directory && !plain.multiple);
    let options = HashMap::from([
        ("directory".to_owned(), OwnedValue::from(true)),
        ("multiple".to_owned(), OwnedValue::from(true)),
        (
            "accept_label".to_owned(),
            OwnedValue::try_from(Value::new("Attach folder".to_string())).unwrap(),
        ),
    ]);
    let decoded = decode_open_options("Choose".into(), &options);
    assert!(decoded.directory && decoded.multiple);
    assert_eq!(decoded.accept_label.as_deref(), Some("Attach folder"));
    assert_eq!(
        FileChooserQuestion::default()
            .parent_window("  ")
            .parent_window,
        None
    );
}

#[test]
fn chosen_paths_map_to_portal_responses_and_encoded_uris() {
    let direct = |path: &str| Export {
        app_path: PathBuf::from(path),
        host_path: PathBuf::from(path),
    };
    let (code, results) = reply(paths_to_open_response(
        Some(vec![direct("/tmp/a.txt")]),
        false,
    ));
    assert_eq!(code, 0);
    assert_eq!(uris_of(&results), ["file:///tmp/a.txt"]);
    assert_eq!(option::<bool>(&results, "writable"), Some(false));
    assert_eq!(reply(paths_to_open_response(None, false)).0, 1);
    assert_eq!(reply(paths_to_open_response(Some(vec![]), false)).0, 1);

    let path = PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/a #\xff.txt"));
    assert_eq!(
        file_uri(&path).as_deref(),
        Some("file:///tmp/a%20%23%FF.txt")
    );
    assert_eq!(
        path_of_uri("file:///tmp/a%20%23%FF.txt").as_deref(),
        Some(path.as_path())
    );
    assert!(file_uri(std::path::Path::new("relative.txt")).is_none());
}

#[test]
fn picker_filters_reduce_globs_and_keep_mime_types() {
    let f = PickerFilter::from_pairs("Images", &[(0, "*.PNG".into()), (0, "*.jpg".into())]);
    assert_eq!(f.extensions, vec!["png", "jpg"]);
    assert!(f.mimetypes.is_empty());
    assert!(!f.is_catch_all());
    let f = PickerFilter::from_pairs("Images", &[(1, "image/PNG".into())]);
    assert_eq!(f.mimetypes, vec!["image/png"]);
    // `*.p?g` cannot be reduced to one extension: matching nothing would
    // hide files the app would have accepted, so the filter opens up.
    let f = PickerFilter::from_pairs("Odd", &[(0, "*.p?g".into()), (0, "*.png".into())]);
    assert!(f.is_catch_all(), "{f:?}");
    assert!(PickerFilter::from_pairs("All", &[(0, "*".into())]).is_catch_all());
    assert!(PickerFilter::from_pairs("All", &[(0, "*.*".into())]).is_catch_all());
}
