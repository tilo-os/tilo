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

/// `pick` with the person answering `uris` (declining on `None`); the
/// question they saw, and the reply.
async fn picked(question: FileChooserQuestion, uris: Option<Vec<String>>) -> (Question, Reply) {
    let questions = Questions::new();
    let portal = crate::tests::portal(&questions);
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
    let caller = caller();
    let (outcome, asked) = tokio::join!(pick(&portal, &caller, path, question), respond);
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
    let (code, results) = reply(paths_to_open_response(
        Some(vec![PathBuf::from("/tmp/a.txt")]),
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
