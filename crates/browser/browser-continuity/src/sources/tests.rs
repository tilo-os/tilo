use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

fn granted(path: &std::path::Path) -> GrantedProfile {
    GrantedProfile::from_directory(File::open(path).unwrap()).unwrap()
}

#[test]
fn held_grant_is_not_redirected_by_replacing_the_directory_path() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("profile");
    let retained = root.path().join("retained");
    std::fs::create_dir(&original).unwrap();
    let bookmarks = br#"{"roots":{"bar":{"id":"1","type":"folder","children":[{"id":"2","type":"url","url":"https://EXAMPLE.org","name":"Retained"}]}}}"#;
    std::fs::write(original.join("Bookmarks"), bookmarks).unwrap();
    let profile = granted(&original);
    std::fs::rename(&original, &retained).unwrap();
    std::fs::create_dir(&original).unwrap();
    std::fs::write(original.join("Bookmarks"), b"not the granted source").unwrap();

    let bundle = parse_chromium_bookmarks_bundle(&profile).unwrap();
    bundle.validate().unwrap();
    assert_eq!(bundle.chunks.len(), 1);
    assert_eq!(
        bundle.chunks[0].bookmarks,
        vec![BookmarkImport {
            url: "https://example.org/".into(),
            title: "Retained".into(),
        }]
    );
    let tree = bundle.imported_tree.as_ref().unwrap();
    assert_eq!(tree.nodes.len(), 2);
    assert_eq!(tree.nodes[1].parent.as_deref(), Some("root:bar:chromium:1"));
    assert_eq!(
        std::fs::read(retained.join("Bookmarks")).unwrap(),
        bookmarks
    );
    assert_eq!(parse_chromium_bookmarks_bundle(&profile).unwrap(), bundle);
}

#[test]
fn source_links_corrupt_databases_and_uncontrolled_namespaces_are_refused() {
    let source = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let target = elsewhere.path().join("Bookmarks");
    std::fs::write(&target, b"{}").unwrap();
    symlink(&target, source.path().join("Bookmarks")).unwrap();
    let profile = granted(source.path());
    assert!(parse_chromium_bookmarks(&profile).is_err());
    std::fs::write(source.path().join("History"), b"not a database").unwrap();
    assert!(parse_chromium_history(&profile).is_err());
    let oversized = File::create(source.path().join("places.sqlite")).unwrap();
    oversized
        .set_len(MAX_DATABASE_BYTES.saturating_add(1))
        .unwrap();
    assert!(parse_firefox_places(&profile).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"{}");
    std::fs::set_permissions(source.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(GrantedProfile::from_directory(File::open(source.path()).unwrap()).is_err());
    std::fs::set_permissions(source.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn selected_firefox_wal_capture_never_reads_unselected_tables_or_changes_source() {
    let source = tempfile::tempdir().unwrap();
    let path = source.path().join("places.sqlite");
    let writer = Connection::open(&path).unwrap();
    writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE moz_places(id INTEGER PRIMARY KEY,url TEXT,title TEXT,last_visit_date INTEGER,visit_count INTEGER); INSERT INTO moz_places VALUES(1,'https://EXAMPLE.org','Visited',19000000,4);").unwrap();
    let db_before = std::fs::read(&path).unwrap();
    let wal_path = source.path().join("places.sqlite-wal");
    let wal_before = std::fs::read(&wal_path).unwrap();
    let profile = granted(source.path());
    let bundle = parse_firefox_places_selected_bundle(&profile, false, true).unwrap();
    bundle.validate().unwrap();
    assert!(bundle.imported_tree.is_none());
    assert_eq!(bundle.chunks.len(), 1);
    assert!(bundle.chunks[0].bookmarks.is_empty());
    assert_eq!(
        bundle.chunks[0].history,
        vec![HistoryImport {
            url: "https://example.org/".into(),
            title: "Visited".into(),
            last_visit: 19,
            visit_count: 4,
        }]
    );
    assert!(parse_firefox_places_selected_bundle(&profile, true, false).is_err());
    assert!(parse_firefox_places_selected_bundle(&profile, false, false).is_err());
    assert_eq!(
        parse_firefox_places_selected_bundle(&profile, false, true).unwrap(),
        bundle
    );
    assert_eq!(std::fs::read(&path).unwrap(), db_before);
    assert_eq!(std::fs::read(&wal_path).unwrap(), wal_before);
}
