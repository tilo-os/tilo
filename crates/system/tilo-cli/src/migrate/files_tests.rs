use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;

use super::files::{self, Sink};
use super::state::{Item, Journal, Place, Review};
use super::tests::test_env::{Recorded, Told, land_as};
use super::workspace::{Invocation, Workspace};
use super::{Lander, clouds, registry};

/// Call `land` as one invocation of `source`, as `start` does; the journal it leaves.
fn bring(
    place: &Place,
    source: &str,
    id: &str,
    workspace: &mut dyn Workspace,
    land: impl FnOnce(&mut Lander<'_>) -> Result<()>,
) -> Journal {
    let mut told = Told {
        yes: true,
        ..Told::default()
    };
    land_as(
        place,
        source,
        &Invocation {
            id: id.into(),
            title: "Bring files".into(),
        },
        workspace,
        &mut told,
        land,
    )
    .unwrap()
}

/// A home with a `Documents` tree of `count` files spread over folders.
fn home_with(dir: &Path, count: usize) -> PathBuf {
    let home = dir.join("home");
    for index in 0..count {
        let path = home.join(format!(
            "Documents/{}/{}/note-{index}.txt",
            index % 7,
            index % 3
        ));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("note {index}\n")).unwrap();
    }
    std::fs::create_dir_all(home.join("Projects/site")).unwrap();
    std::fs::write(home.join("Projects/site/index.html"), "<h1>hi</h1>").unwrap();
    home
}

fn chosen(source: &str, keys: &[&str], count: usize) -> Review {
    Review {
        source: source.into(),
        title: source.into(),
        handle: source.into(),
        items: keys
            .iter()
            .map(|key| Item {
                key: (*key).into(),
                kind: "folder".into(),
                title: (*key).into(),
                count,
                counts: "files".into(),
                within: None,
                chosen: true,
                note: None,
            })
            .collect(),
        unreadable: Vec::new(),
    }
}

fn files_under(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut walking = vec![root.to_path_buf()];
    while let Some(dir) = walking.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if entry.file_type().unwrap().is_dir() {
                walking.push(entry.path());
            } else {
                found.push(
                    entry
                        .path()
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    found.sort();
    found
}

#[test]
fn a_chosen_folder_is_copied_once_resumes_and_its_undo_takes_back_only_what_it_wrote() {
    let dir = tempfile::tempdir().unwrap();
    let home = home_with(dir.path(), 500);
    let place = Place::at(&dir.path().join("state"));
    let folder = dir.path().join("Acme");
    std::fs::create_dir_all(&folder).unwrap();
    let local = files::Local { home: home.clone() };

    // The review lists the folders of the home; Documents and a folder chosen by its path
    // come over.
    let mut review = files::review(&home, None);
    let documents = review
        .items
        .iter()
        .find(|item| item.key == "Documents")
        .unwrap();
    assert_eq!(documents.count, 500, "{review:#?}");
    let mut keys = vec![
        "Documents".to_owned(),
        format!("{}", home.join("Projects/site").display()),
    ];
    files::add_paths(&home, &mut review, &mut keys).unwrap();
    review.choose(&keys, true, true).unwrap();
    assert_eq!(review.chosen().len(), 2);

    let sink = Sink::Folder(folder.clone());
    let first = bring(
        &place,
        "home",
        "first",
        &mut Recorded::default(),
        |lander| files::land(&local, &review, &sink, &place, lander),
    );
    assert_eq!(first.brought.get("files"), Some(&501));
    let copied = files_under(&folder);
    assert_eq!(copied.len(), 501);
    assert!(copied.contains(&"site/index.html".to_owned()), "{copied:?}");
    assert_eq!(
        std::fs::read_to_string(folder.join("Documents/3/1/note-10.txt")).unwrap(),
        "note 10\n"
    );

    // Starting again finds everything there; a file lost on the way comes again.
    std::fs::remove_file(folder.join("Documents/3/1/note-10.txt")).unwrap();
    let second = bring(
        &place,
        "home",
        "second",
        &mut Recorded::default(),
        |lander| files::land(&local, &review, &sink, &place, lander),
    );
    assert_eq!(second.brought.get("files"), Some(&1));
    assert_eq!(second.already_there, 500);

    // Undo of the first: what it wrote goes, a file the person changed since stays, and what
    // the second brought stays.
    std::fs::write(folder.join("Documents/0/0/note-0.txt"), "changed since").unwrap();
    std::fs::write(folder.join("mine.txt"), "the person's own").unwrap();
    let removed = files::undo_written(&place, "home", "first").unwrap();
    assert_eq!(removed, Some(499));
    let left = files_under(&folder);
    assert_eq!(
        left,
        [
            "Documents/0/0/note-0.txt",
            "Documents/3/1/note-10.txt",
            "mine.txt"
        ]
    );
    assert!(
        !folder.join("site").exists(),
        "a folder the undo emptied goes"
    );
    assert_eq!(files::undo_written(&place, "home", "first").unwrap(), None);
    assert_eq!(crate::folders::size(0), "0 B");
    assert_eq!(crate::folders::size(1_500_000), "1.5 MB");
}

#[test]
fn without_a_folder_here_the_files_go_up_and_into_the_tree_in_one_step() {
    let dir = tempfile::tempdir().unwrap();
    let home = home_with(dir.path(), 12);
    let place = Place::at(&dir.path().join("state"));
    let local = files::Local { home };
    let review = chosen("home", &["Documents"], 12);
    let mut workspace = Recorded::default();
    let journal = bring(&place, "home", "first", &mut workspace, |lander| {
        files::land(
            &local,
            &review,
            &Sink::Upload(String::new()),
            &place,
            lander,
        )
    });
    assert_eq!(journal.brought.get("files"), Some(&12));
    let opens: Vec<&Value> = workspace
        .actions
        .iter()
        .filter(|(_, action, _)| action == "files#open")
        .map(|(_, _, args)| args)
        .collect();
    assert_eq!(opens.len(), 1, "one step puts the folder in the tree");
    let list: Vec<Value> = serde_json::from_slice(workspace.puts.last().unwrap()).unwrap();
    assert_eq!(list.len(), 12);
    assert!(
        list.iter()
            .any(|file| file["dir"] == "Documents/3/1" && file["name"] == "note-10.txt"),
        "{list:?}"
    );
    assert_eq!(
        opens[0]["files"],
        format!(
            "tilo.content:{}",
            blake3::hash(workspace.puts.last().unwrap()).to_hex()
        )
    );
}

#[test]
fn a_webdav_drive_comes_over_through_the_provider_files_mounts_it_with() {
    use cloud_source_dav::fake::{FakeDav, PASSWORD, USER};
    let server = FakeDav::nextcloud();
    for (path, body) in [
        ("Photos/2026/beach.jpg", "jpeg"),
        ("Photos/2026/hill.jpg", "jpeg too"),
        ("Notes/todo.md", "- [ ] move"),
    ] {
        server.put(path, body.as_bytes());
    }
    let mut account = accounts::Account::new("nc", accounts::cloud_types::NEXTCLOUD);
    account.config = server.account_config();
    account.config["use_files"] = true.into();
    let source = clouds::open_with(
        &account,
        Arc::new(cloud_source::StaticCredential(
            cloud_source::Credential::Basic {
                username: USER.into(),
                password: zeroize::Zeroizing::new(PASSWORD.into()),
            },
        )),
    )
    .unwrap();
    let named = registry::named("nextcloud").unwrap();
    let drive = clouds::Drive::of(source, named).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let place = Place::at(&dir.path().join("state"));
    let folder = dir.path().join("Acme/Nextcloud");
    let review = chosen("nextcloud", &["Photos"], 2);
    let journal = bring(
        &place,
        "nextcloud",
        "first",
        &mut Recorded::default(),
        |lander| {
            files::land(
                &drive,
                &review,
                &Sink::Folder(folder.clone()),
                &place,
                lander,
            )
        },
    );
    assert_eq!(journal.brought.get("files"), Some(&2));
    assert_eq!(
        files_under(&folder),
        ["Photos/2026/beach.jpg", "Photos/2026/hill.jpg"]
    );
    assert_eq!(
        std::fs::read_to_string(folder.join("Photos/2026/hill.jpg")).unwrap(),
        "jpeg too"
    );
}

#[test]
fn a_google_doc_comes_over_as_the_office_file_drive_exports() {
    use cloud_source_gdrive::fake::{FakeDrive, SOURCE_TOKEN};
    let server = FakeDrive::start();
    let root = server.root_id();
    server.put_file(&root, "budget.csv", b"a,b\n1,2\n");
    server.put_native(&root, "Plan", "application/vnd.google-apps.document");
    let mut account = accounts::Account::new("drive", accounts::cloud_types::GOOGLE_DRIVE);
    account.config = serde_json::json!({
        accounts::cloud_types::MOUNT_CONFIG_KEY: server.config(),
    });
    let source = clouds::open_with(
        &account,
        Arc::new(cloud_source::StaticCredential(
            cloud_source::Credential::Bearer {
                token: zeroize::Zeroizing::new(SOURCE_TOKEN.into()),
                expires: None,
            },
        )),
    )
    .unwrap();
    let drive = clouds::Drive::of(source, registry::named("gdrive").unwrap()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let place = Place::at(&dir.path().join("state"));
    let folder = dir.path().join("Acme/Google Drive");
    let review = chosen("gdrive", &["budget.csv", "Plan.docx"], 1);
    bring(
        &place,
        "gdrive",
        "first",
        &mut Recorded::default(),
        |lander| {
            files::land(
                &drive,
                &review,
                &Sink::Folder(folder.clone()),
                &place,
                lander,
            )
        },
    );
    assert_eq!(files_under(&folder), ["Plan.docx", "budget.csv"]);
    assert_eq!(
        std::fs::read(folder.join("budget.csv")).unwrap(),
        b"a,b\n1,2\n"
    );
}
