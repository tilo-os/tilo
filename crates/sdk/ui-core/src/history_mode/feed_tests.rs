use super::*;

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
    fn a_windows_history_lists_what_the_workspaces_history_serves;

    phase rows_carry_the_change_in_the_windows_resource_and_the_name_the_feed_gave;
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
        let listed = listed(&feed);
        let ids: Vec<HistoryEntryId> = listed.entries.iter().map(|entry| entry.id).collect();
        assert_eq!(
            ids,
            [HistoryEntryId::from_bytes([11; 16]), HistoryEntryId::from_bytes([12; 16])],
            "oldest first, each under its id in the window's own resource"
        );
        assert_eq!(listed.entries[1].author.name, "Ada");
        assert_eq!(listed.entries[1].author.rgb(), (10, 20, 30));
        assert_eq!(listed.entries[1].summary, "Ada edited the note");
        assert_eq!(listed.named.len(), 1);
        assert_eq!(listed.named[0].start, HistoryEntryId::from_bytes([11; 16]));
        assert_eq!(listed.named[0].end, Some(HistoryEntryId::from_bytes([12; 16])));
        assert!(listed.has_earlier, "the feed holds one change more than it listed");
    }
}
