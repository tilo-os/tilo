//! A Windows system left on a disk, read with nothing of it running: the person's folders, the
//! folders Windows keeps elsewhere, and a browser's bookmarks and history reach the workspace as
//! a running computer's do, and what Windows locks to its own sign-in is said, not guessed at.

use std::path::Path;

use super::Source as _;
use super::disk::Disk;
use super::files::Sink;
use super::registry;
use super::state::Place;
use super::tests::test_env::{Recorded, Told, land_as, with_cx};
use super::writer::Invocation;

/// A disk Windows ran from: Ada's home with her documents, pictures, a `OneDrive` folder, an
/// Outlook archive, and Chrome's default profile; and the system's own accounts beside hers.
fn windows_disk(root: &Path) {
    let home = root.join("Users/ada");
    let write = |path: &str, bytes: &[u8]| {
        let path = home.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    };
    std::fs::create_dir_all(root.join("Windows/System32")).unwrap();
    std::fs::create_dir_all(root.join("Users/Public/Documents")).unwrap();
    std::fs::create_dir_all(root.join("Users/Default/AppData")).unwrap();
    write("NTUSER.DAT", b"hive");
    write("Documents/Taxes/2025.pdf", b"taxes");
    write("Pictures/cat.jpg", b"cat");
    write("Desktop/todo.txt", b"todo");
    write("OneDrive/notes.txt", b"notes");
    write("Documents/Outlook Files/archive.pst", b"pst");
    write("AppData/Roaming/Microsoft/Windows/Start Menu/x.lnk", b"lnk");
    let chrome = "AppData/Local/Google/Chrome/User Data/Default";
    write(
        &format!("{chrome}/Bookmarks"),
        br#"{"roots":{"bookmark_bar":{"id":"1","type":"folder","name":"Bar","children":[{"id":"2","type":"url","url":"https://example.org/kept","name":"Kept"}]}}}"#,
    );
    let history = home.join(chrome).join("History");
    let db = rusqlite::Connection::open(&history).unwrap();
    db.execute_batch(
        "CREATE TABLE urls(id INTEGER PRIMARY KEY, url TEXT, title TEXT, visit_count INTEGER, \
         last_visit_time INTEGER); \
         INSERT INTO urls VALUES(1, 'https://example.org/visited', 'Visited', 3, 13300000000000000);",
    )
    .unwrap();
    drop(db);
    // Chrome on Windows seals what it saved with the account's own key.
    write(&format!("{chrome}/Login Data"), b"sealed by Windows");
}

/// One run of what `review` chose, as `start` makes it, its files copied into `into`: what the
/// workspace was asked, and the journal the run left.
fn landed(
    place: &Place,
    here: &Path,
    review: &super::state::Review,
    into: &Path,
) -> (Recorded, super::state::Journal) {
    let row = registry::named("disk").unwrap();
    let mut workspace = Recorded::default();
    let invocation = Invocation {
        id: "run-disk".into(),
        title: Disk.summary(row, review),
    };
    let journal = land_as(
        place,
        row.key,
        &invocation,
        &mut workspace,
        &mut Told::default(),
        |lander| {
            with_cx(place, here, |cx| {
                Disk.land(
                    cx,
                    row,
                    review,
                    Some(&Sink::Folder(into.to_path_buf())),
                    lander,
                )
            })
        },
    )
    .unwrap();
    (workspace, journal)
}

/// Chrome's bookmarks and history reached Browser, one import each, and no password was saved.
fn browser_data_came_over(workspace: &Recorded) {
    let called = |action: &str| -> Vec<String> {
        workspace
            .of(action)
            .into_iter()
            .map(|args| args["bundle"].as_str().unwrap_or_default().to_owned())
            .collect()
    };
    let bookmarks = called("browser/browser.personal-bookmarks#import-bookmarks");
    assert!(
        bookmarks.len() == 1 && bookmarks[0].contains("https://example.org/kept"),
        "{bookmarks:?}"
    );
    let history = called("browser/browser.browsing-history#import-history-bundle");
    assert!(
        history.len() == 1 && history[0].contains("https://example.org/visited"),
        "{history:?}"
    );
    assert!(
        workspace
            .of("passwords/passwords.vault#save-login")
            .is_empty()
    );
}

#[test]
fn a_windows_disk_brings_its_folders_and_browser_data_over_offline() {
    let dir = tempfile::tempdir().unwrap();
    let disk = dir.path().join("disk");
    windows_disk(&disk);
    let place = Place::at(&dir.path().join("state"));
    let row = registry::named("disk").unwrap();
    let here = dir.path().join("here");
    std::fs::create_dir_all(&here).unwrap();

    // The top of the system is named; its one person's home is read.
    let mut review = with_cx(&place, &here, |cx| {
        Disk.review(cx, row, Some(disk.to_str().unwrap())).unwrap()
    });
    assert_eq!(review.handle, disk.join("Users/ada").display().to_string());
    let title = |key: &str| {
        review
            .items
            .iter()
            .find(|item| item.key == key)
            .map(|item| (item.title.clone(), item.chosen))
    };
    let at = |path: &str| disk.join("Users/ada").join(path).display().to_string();
    assert_eq!(title(&at("Documents")), Some(("Documents".into(), true)));
    assert_eq!(title(&at("Pictures")), Some(("Pictures".into(), true)));
    assert_eq!(
        title(&at("Documents/Outlook Files")),
        Some(("Outlook archives".into(), false))
    );
    assert_eq!(
        title(&at("OneDrive")),
        Some(("OneDrive (OneDrive)".into(), false)),
        "listed once, as what Windows keeps it as: {review:#?}"
    );
    assert!(
        !review.items.iter().any(|item| item.key.contains("AppData")),
        "the system's own folders are not the person's: {review:#?}"
    );
    assert!(
        review
            .unreadable
            .iter()
            .any(|why| why.contains("Windows keeps Google Chrome's saved passwords locked")),
        "{:?}",
        review.unreadable
    );
    let chrome = "browsers:google-chrome/Default";
    assert!(review.items.iter().any(|item| item.key == chrome));

    // The person takes the familiar folders, OneDrive and Chrome's bookmarks and history.
    review
        .choose(
            &[
                at("OneDrive"),
                format!("{chrome}/bookmarks"),
                format!("{chrome}/history"),
            ],
            true,
            false,
        )
        .unwrap();
    place.keep_review(&review).unwrap();
    assert!(
        Disk.acts(&review),
        "browser data is landed by Browser's actions"
    );

    let into = dir.path().join("workspace");
    std::fs::create_dir_all(&into).unwrap();
    let (workspace, journal) = landed(&place, &here, &review, &into);

    for (path, bytes) in [
        ("Documents/Taxes/2025.pdf", &b"taxes"[..]),
        ("Pictures/cat.jpg", b"cat"),
        ("Desktop/todo.txt", b"todo"),
        ("OneDrive/notes.txt", b"notes"),
    ] {
        assert_eq!(
            std::fs::read(into.join(path)).unwrap_or_default(),
            bytes,
            "{path} came over"
        );
    }
    assert!(
        !into.join("Outlook Files").exists(),
        "what was not chosen stays"
    );
    browser_data_came_over(&workspace);
    // Five files (the archive inside Documents with it) and one import each of bookmarks and
    // history.
    let landed = with_cx(&place, &here, |cx| Disk.landed(cx, row, &journal).unwrap());
    assert_eq!(landed, 7, "{journal:#?}");
}

#[test]
fn a_disk_with_several_peoples_homes_asks_which() {
    let dir = tempfile::tempdir().unwrap();
    let disk = dir.path().join("disk");
    windows_disk(&disk);
    std::fs::create_dir_all(disk.join("Users/bea/Documents")).unwrap();
    let place = Place::at(&dir.path().join("state"));
    let row = registry::named("disk").unwrap();
    let refused = with_cx(&place, dir.path(), |cx| {
        Disk.review(cx, row, Some(disk.to_str().unwrap()))
            .unwrap_err()
            .to_string()
    });
    assert!(
        refused.contains("Users/ada") && refused.contains("Users/bea"),
        "{refused}"
    );
    // Named by their home, Bea's is read, and kept for the steps after.
    let bea = disk.join("Users/bea");
    with_cx(&place, dir.path(), |cx| {
        Disk.review(cx, row, Some(bea.to_str().unwrap())).unwrap();
        assert_eq!(
            Disk.review(cx, row, None).unwrap().handle,
            bea.display().to_string()
        );
    });
}
