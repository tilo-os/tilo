//! A disk Windows ran from, for proofs and tests: laid out as Windows lays out a system drive,
//! with one person's home holding what a person keeps there.

use std::path::Path;

/// The bookmark Chrome kept on the disk.
pub const BOOKMARK: &str = "https://example.org/kept";
/// The page Chrome's history holds.
pub const VISITED: &str = "https://example.org/visited";

/// A disk Windows ran from: Ada's home with her documents, pictures, a `OneDrive` folder, an
/// Outlook archive, and Chrome's default profile; and the system's own accounts beside hers.
///
/// # Panics
/// When the disk cannot be written.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that cannot be written is a broken test setup"
)]
pub fn write_windows_disk(root: &Path) {
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
