use super::{HistoryEntry, HistoryEntryId, read, wire};

fn hex(byte: u8) -> String {
    wire::entry_hex(&[byte; 16])
}

fn entry(listed: u8, change: u8, name: &str, at_ms: i64) -> wire::Entry {
    wire::Entry {
        id: hex(listed),
        change: Some(hex(change)),
        author: wire::Author {
            name: name.into(),
            rgb: [10, 20, 30],
            identity: "ab12".into(),
            relation: wire::Relation::Person,
            ..wire::Author::default()
        },
        at_ms,
        summary: format!("{name} edited the note"),
        app: Some("Notes".into()),
        ..wire::Entry::default()
    }
}

tilo_test_env::scenario! {
    fn a_windows_history_names_authors_as_the_workspaces_history_does;

    phase rows_take_the_name_the_feed_gave_their_change;
    {
        let feed = wire::Feed {
            entries: vec![entry(2, 12, "Ada", 2_000), entry(1, 11, "Ben", 1_000)],
            groups: vec![wire::Group {
                id: "group".into(),
                label: "Draft".into(),
                start: hex(1),
                end: Some(hex(2)),
                named_by: Some("Ada".into()),
            }],
            total: 3,
            ..wire::Feed::default()
        };
        let own = |byte: u8, summary: &str| HistoryEntry {
            id: HistoryEntryId::from_bytes([byte; 16]),
            author: crate::widget::HistoryAuthor::new("Device", 0.0, "cd34"),
            entry_kind: crate::widget::EntryKind::Text,
            artifact_kind: collab_types::ArtifactKind::File,
            timestamp: crate::time::SystemTime::UNIX_EPOCH,
            summary: summary.into(),
            text_delta: None,
            diff_payload: None,
            redaction: None,
            context: None,
            target: None,
        };
        let mut entries = vec![own(11, "Renamed the note"), own(12, "Added a line"), own(13, "Kept")];
        let mut named = Vec::new();
        read(&feed).apply(&mut entries, &mut named);
        assert_eq!(entries[1].author.name, "Ada", "named under its id in the window's own resource");
        assert_eq!(entries[1].author.rgb(), (10, 20, 30));
        assert_eq!(entries[0].author.name, "Ben");
        assert_eq!(
            entries[1].summary, "Added a line",
            "what a change did is what the document says"
        );
        assert_eq!(
            entries[2].author.name, "Device",
            "a change the feed did not name keeps its own author"
        );
        assert_eq!(named.len(), 1);
        assert_eq!(named[0].start, HistoryEntryId::from_bytes([11; 16]));
        assert_eq!(named[0].end, Some(HistoryEntryId::from_bytes([12; 16])));
    }
}

/// Software names whom it acted for.
#[test]
fn software_names_whom_it_acted_for() {
    crate::i18n::I18nManager::init("en-US");
    let mut served = entry(1, 1, "Planner", 0);
    served.author.relation = wire::Relation::Software;
    served.author.on_behalf_of = Some("Ben".into());
    let author = super::author(&served.author);
    assert_eq!(author.name, "Planner");
    assert_eq!(
        crate::history_source::sentence::acted_for(&author).as_deref(),
        Some("for Ben")
    );
}
