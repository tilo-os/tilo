//! App-facing reviewed source capture and native import integration tests.
use super::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::continuity::{
        BookmarkImport, HistoryImport, ImportActions, ImportBatch, ImportBundle,
    };
    use crate::imported_folders::NodeKind;
    use rusqlite::Connection;
    use std::fs::File;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn granted(directory: &tempfile::TempDir) -> GrantedProfile {
        GrantedProfile::from_directory(File::open(directory.path()).unwrap()).unwrap()
    }

    fn assert_native_import(batch: &ImportBatch) {
        let mut library =
            ui::tilo_resource::Replica::<crate::personal_bookmarks::PersonalBookmarks>::new();
        let mut personal = ui::tilo_resource::Replica::<crate::history::BrowsingHistory>::new();
        library.apply(batch.bookmark_action().unwrap()).unwrap();
        personal.apply(batch.history_action().unwrap()).unwrap();
        library.apply(batch.bookmark_action().unwrap()).unwrap();
        personal.apply(batch.history_action().unwrap()).unwrap();
        if !batch.bookmarks.is_empty() {
            let projected = library.state().unwrap().bookmarks();
            assert_eq!(projected.entries().len(), batch.bookmarks.len());
        }
        let projected = personal.state().unwrap().entries();
        assert_eq!(projected.len(), batch.history.len());
        for row in &batch.history {
            let actual = projected
                .iter()
                .find(|actual| actual.url == row.url)
                .unwrap();
            assert_eq!(actual.visit_count, row.visit_count);
            assert_eq!(actual.last_visit, row.last_visit);
            assert_eq!(actual.title, row.title);
        }
    }

    fn source_bytes(directory: &tempfile::TempDir, name: &str) -> Vec<(String, Vec<u8>, u32)> {
        [
            name.to_string(),
            format!("{name}-wal"),
            format!("{name}-shm"),
        ]
        .into_iter()
        .filter_map(|name| {
            let path = directory.path().join(&name);
            let bytes = std::fs::read(&path).ok()?;
            Some((
                name,
                bytes,
                std::fs::metadata(path).unwrap().permissions().mode(),
            ))
        })
        .collect()
    }

    #[test]
    fn chromium_bookmarks_use_the_granted_file_and_skip_non_web_pages() {
        let source = tempfile::tempdir().unwrap();
        let json = br#"{"roots":{"bookmark_bar":{"type":"folder","children":[{"type":"folder","children":[{"type":"url","url":"https://example.org/","name":"Example"}]},{"type":"url","url":"https://EXAMPLE.org","name":"Duplicate"},{"type":"url","url":"file:///secret","name":"Local"}]}}}"#;
        std::fs::write(source.path().join("Bookmarks"), json).unwrap();
        let before = source_bytes(&source, "Bookmarks");
        let batch = parse_chromium_bookmarks(&granted(&source)).unwrap();
        assert_native_import(&batch);
        assert_eq!(
            batch.bookmarks,
            vec![BookmarkImport {
                url: "https://example.org/".into(),
                title: "Example".into()
            }]
        );
        assert_eq!(before, source_bytes(&source, "Bookmarks"));
        assert_eq!(
            batch.source,
            parse_chromium_bookmarks(&granted(&source)).unwrap().source
        );
    }

    #[test]
    fn chromium_complete_bundle_preserves_roots_nested_order_and_repeated_url_placements() {
        let source = tempfile::tempdir().unwrap();
        let json = br#"{"roots":{"bookmark_bar":{"id":"1","name":"Bookmarks bar","type":"folder","children":[{"id":"2","name":"Work","type":"folder","children":[{"id":"3","type":"url","url":"https://example.org/","name":"Work example"}]},{"id":"4","name":"Personal","type":"folder","children":[{"id":"5","type":"url","url":"https://example.org/","name":"Personal example"}]}]},"other":{"id":"6","name":"Other bookmarks","type":"folder","children":[]}}}"#;
        std::fs::write(source.path().join("Bookmarks"), json).unwrap();
        let before = source_bytes(&source, "Bookmarks");
        let bundle = parse_chromium_bookmarks_bundle(&granted(&source)).unwrap();
        let tree = bundle.imported_tree.as_ref().unwrap();
        assert_eq!(tree.nodes.len(), 6);
        assert_eq!(
            tree.nodes
                .iter()
                .filter(|n| matches!(n.kind, NodeKind::Bookmark { .. }))
                .count(),
            2
        );
        let work = tree.nodes.iter().find(|n| n.id == "chromium:2").unwrap();
        let personal = tree.nodes.iter().find(|n| n.id == "chromium:4").unwrap();
        assert_eq!(work.parent.as_deref(), Some("root:bookmark_bar:chromium:1"));
        assert_eq!((work.position, personal.position), (0, 1));
        assert_eq!(
            tree.nodes
                .iter()
                .find(|n| n.id == "chromium:5")
                .unwrap()
                .title,
            "Personal example"
        );
        let encoded = serde_json::to_string(&bundle).unwrap();
        assert_eq!(ImportBundle::decode(&encoded).unwrap(), bundle);
        let mut library =
            ui::tilo_resource::Replica::<crate::personal_bookmarks::PersonalBookmarks>::new();
        library.apply(bundle.bookmark_action().unwrap()).unwrap();
        assert_eq!(
            library.state().unwrap().imported_trees(),
            vec![tree.clone()]
        );
        assert_eq!(before, source_bytes(&source, "Bookmarks"));
        assert_eq!(
            parse_chromium_bookmarks_bundle(&granted(&source)).unwrap(),
            bundle
        );
    }

    #[test]
    fn firefox_wal_complete_tree_preserves_folder_parents_position_and_duplicate_placements() {
        let source = tempfile::tempdir().unwrap();
        let writer = Connection::open(source.path().join("places.sqlite")).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE moz_places(id INTEGER PRIMARY KEY,url TEXT,title TEXT,last_visit_date INTEGER,visit_count INTEGER); CREATE TABLE moz_bookmarks(id INTEGER PRIMARY KEY,fk INTEGER,title TEXT,type INTEGER,parent INTEGER,position INTEGER); INSERT INTO moz_places VALUES(1,'https://example.org/','Web title',9000000,3); INSERT INTO moz_bookmarks VALUES(1,NULL,'Root',2,0,0),(2,NULL,'Toolbar',2,1,0),(3,NULL,'Nested',2,2,0),(4,1,'Nested example',1,3,0),(5,1,'Toolbar example',1,2,1),(6,NULL,'',3,2,2);").unwrap();
        let before = source_bytes(&source, "places.sqlite");
        assert!(before.iter().any(|row| row.0 == "places.sqlite-wal"));
        let bundle = parse_firefox_places_bundle(&granted(&source)).unwrap();
        let tree = bundle.imported_tree.as_ref().unwrap();
        assert_eq!(tree.nodes.len(), 6);
        let nested = tree.nodes.iter().find(|n| n.id == "firefox:4").unwrap();
        let toolbar = tree.nodes.iter().find(|n| n.id == "firefox:5").unwrap();
        assert_eq!(nested.parent.as_deref(), Some("firefox:3"));
        assert_eq!(toolbar.parent.as_deref(), Some("firefox:2"));
        assert_eq!(toolbar.position, 1);
        assert_eq!(nested.kind, toolbar.kind);
        assert_eq!(nested.title, "Nested example");
        assert!(matches!(
            tree.nodes
                .iter()
                .find(|n| n.id == "firefox:6")
                .unwrap()
                .kind,
            NodeKind::Separator
        ));
        let mut library =
            ui::tilo_resource::Replica::<crate::personal_bookmarks::PersonalBookmarks>::new();
        library.apply(bundle.bookmark_action().unwrap()).unwrap();
        assert_eq!(
            library.state().unwrap().imported_trees(),
            vec![tree.clone()]
        );
        assert!(
            ui::tilo_resource::Replica::<crate::history::BrowsingHistory>::new()
                .apply(bundle.history_action().unwrap())
                .is_ok()
        );
        assert_eq!(before, source_bytes(&source, "places.sqlite"));
        assert_eq!(
            parse_firefox_places_bundle(&granted(&source)).unwrap(),
            bundle
        );
    }

    #[test]
    fn chromium_wal_history_is_read_consistently_without_source_changes() {
        let source = tempfile::tempdir().unwrap();
        let writer = Connection::open(source.path().join("History")).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE urls(url TEXT,title TEXT,last_visit_time INTEGER,visit_count INTEGER);").unwrap();
        writer
            .execute(
                "INSERT INTO urls VALUES (?1,?2,?3,?4)",
                (
                    "https://example.org/",
                    "Committed WAL page",
                    (CHROMIUM_EPOCH_MICROS + 42_000_000) as i64,
                    7,
                ),
            )
            .unwrap();
        let before = source_bytes(&source, "History");
        assert!(before.iter().any(|row| row.0 == "History-wal"));
        let batch = parse_chromium_history(&granted(&source)).unwrap();
        assert_native_import(&batch);
        assert_eq!(
            batch.history,
            vec![HistoryImport {
                url: "https://example.org/".into(),
                title: "Committed WAL page".into(),
                last_visit: 42,
                visit_count: 7
            }]
        );
        assert_eq!(before, source_bytes(&source, "History"));
        assert_eq!(
            batch.source,
            parse_chromium_history(&granted(&source)).unwrap().source
        );
    }

    #[test]
    fn firefox_wal_bookmarks_and_history_preserve_actual_counts_and_times() {
        let source = tempfile::tempdir().unwrap();
        let writer = Connection::open(source.path().join("places.sqlite")).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE moz_places(id INTEGER PRIMARY KEY,url TEXT,title TEXT,last_visit_date INTEGER,visit_count INTEGER); CREATE TABLE moz_bookmarks(id INTEGER PRIMARY KEY,fk INTEGER,title TEXT,type INTEGER);").unwrap();
        writer.execute_batch("INSERT INTO moz_places VALUES(1,'https://example.org/','History title',9000000,3); INSERT INTO moz_bookmarks VALUES(1,1,'Bookmark title',1); INSERT INTO moz_places VALUES(2,'https://EXAMPLE.org','New history title',10000000,2); INSERT INTO moz_bookmarks VALUES(2,2,'Duplicate bookmark',1);").unwrap();
        let before = source_bytes(&source, "places.sqlite");
        let batch = parse_firefox_places(&granted(&source)).unwrap();
        assert_native_import(&batch);
        assert_eq!(
            batch.bookmarks,
            vec![BookmarkImport {
                url: "https://example.org/".into(),
                title: "Bookmark title".into()
            }]
        );
        assert_eq!(
            batch.history,
            vec![HistoryImport {
                url: "https://example.org/".into(),
                title: "New history title".into(),
                last_visit: 10,
                visit_count: 5
            }]
        );
        assert_eq!(before, source_bytes(&source, "places.sqlite"));
    }

    #[test]
    fn firefox_bookmark_selection_never_queries_missing_history_metadata() {
        let source = tempfile::tempdir().unwrap();
        let writer = Connection::open(source.path().join("places.sqlite")).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE moz_places(id INTEGER PRIMARY KEY,url TEXT,title TEXT); CREATE TABLE moz_bookmarks(id INTEGER PRIMARY KEY,parent INTEGER,position INTEGER,fk INTEGER,title TEXT,type INTEGER); INSERT INTO moz_places VALUES(1,'https://example.org/','Page title'); INSERT INTO moz_bookmarks VALUES(1,0,0,NULL,'Research',2); INSERT INTO moz_bookmarks VALUES(2,1,0,1,'My bookmark',1);").unwrap();
        let before = source_bytes(&source, "places.sqlite");
        let bundle = parse_firefox_places_selected_bundle(&granted(&source), true, false).unwrap();
        assert!(bundle.chunks.iter().all(|chunk| chunk.history.is_empty()));
        let mut personal =
            ui::tilo_resource::Replica::<crate::personal_bookmarks::PersonalBookmarks>::new();
        personal
            .apply(
                crate::personal_bookmarks::PersonalBookmarks::IMPORT_BOOKMARKS
                    .with(serde_json::to_string(&bundle).unwrap()),
            )
            .unwrap();
        let state = personal.state().unwrap();
        assert_eq!(state.bookmarks().entries().len(), 1);
        assert_eq!(state.imported_trees().first().unwrap().nodes.len(), 2);
        assert!(parse_firefox_places_selected_bundle(&granted(&source), false, true).is_err());
        assert!(parse_firefox_places_bundle(&granted(&source)).is_err());
        assert_eq!(before, source_bytes(&source, "places.sqlite"));
    }

    #[test]
    fn firefox_history_selection_never_queries_missing_bookmark_tables() {
        let source = tempfile::tempdir().unwrap();
        let writer = Connection::open(source.path().join("places.sqlite")).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE moz_places(id INTEGER PRIMARY KEY,url TEXT,title TEXT,last_visit_date INTEGER,visit_count INTEGER); INSERT INTO moz_places VALUES(1,'https://example.org/','A visited page',19000000,4);").unwrap();
        let before = source_bytes(&source, "places.sqlite");
        let bundle = parse_firefox_places_selected_bundle(&granted(&source), false, true).unwrap();
        assert!(bundle.imported_tree.is_none());
        assert!(bundle.chunks.iter().all(|chunk| chunk.bookmarks.is_empty()));
        let mut personal = ui::tilo_resource::Replica::<crate::history::BrowsingHistory>::new();
        personal.apply(bundle.history_action().unwrap()).unwrap();
        let rows = personal.state().unwrap().entries();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows.first().unwrap().last_visit, 19);
        assert_eq!(rows.first().unwrap().visit_count, 4);
        assert!(parse_firefox_places_selected_bundle(&granted(&source), true, false).is_err());
        assert!(parse_firefox_places_selected_bundle(&granted(&source), false, false).is_err());
        assert!(parse_firefox_places_bundle(&granted(&source)).is_err());
        assert_eq!(before, source_bytes(&source, "places.sqlite"));
    }

    #[test]
    fn complete_history_bundle_preserves_every_row_beyond_single_batch_limit() {
        let source = tempfile::tempdir().unwrap();
        let mut writer = Connection::open(source.path().join("History")).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE urls(url TEXT,title TEXT,last_visit_time INTEGER,visit_count INTEGER);").unwrap();
        let transaction = writer.transaction().unwrap();
        {
            let mut insert = transaction
                .prepare("INSERT INTO urls VALUES (?1,?2,?3,?4)")
                .unwrap();
            for index in 0..10_001u64 {
                insert
                    .execute((
                        format!("https://example.org/page/{index}"),
                        format!("Page {index}"),
                        (CHROMIUM_EPOCH_MICROS + index * 1_000_000) as i64,
                        3,
                    ))
                    .unwrap();
            }
        }
        transaction.commit().unwrap();
        let before = source_bytes(&source, "History");
        assert!(parse_chromium_history(&granted(&source)).is_err());
        let bundle = parse_chromium_history_bundle(&granted(&source)).unwrap();
        assert_eq!(
            bundle
                .chunks
                .iter()
                .map(|chunk| chunk.history.len())
                .sum::<usize>(),
            10_001
        );
        assert!(
            bundle
                .chunks
                .iter()
                .all(|chunk| chunk.history.len() <= 1000)
        );
        bundle.validate().unwrap();
        assert_eq!(
            bundle,
            parse_chromium_history_bundle(&granted(&source)).unwrap()
        );
        let mut personal = ui::tilo_resource::Replica::<crate::history::BrowsingHistory>::new();
        personal.apply(bundle.history_action().unwrap()).unwrap();
        assert_eq!(personal.state().unwrap().imported.len(), 10_001);
        personal.apply(bundle.history_action().unwrap()).unwrap();
        assert_eq!(personal.state().unwrap().imported.len(), 10_001);
        assert_eq!(before, source_bytes(&source, "History"));
    }

    #[test]
    fn links_corruption_and_oversized_sources_are_refused() {
        let source = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("Bookmarks"), b"{}").unwrap();
        symlink(
            elsewhere.path().join("Bookmarks"),
            source.path().join("Bookmarks"),
        )
        .unwrap();
        assert!(parse_chromium_bookmarks(&granted(&source)).is_err());
        std::fs::write(source.path().join("History"), b"not a database").unwrap();
        assert!(parse_chromium_history(&granted(&source)).is_err());
        let oversized = File::create(source.path().join("places.sqlite")).unwrap();
        oversized.set_len(MAX_DATABASE_BYTES + 1).unwrap();
        assert!(parse_firefox_places(&granted(&source)).is_err());
        assert_eq!(
            std::fs::read(elsewhere.path().join("Bookmarks")).unwrap(),
            b"{}"
        );
    }
}
