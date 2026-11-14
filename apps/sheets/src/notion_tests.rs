use std::io::Write as _;

use super::*;

fn localized() {
    ui::i18n::I18nManager::init("en-US");
}

fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }
    buffer.into_inner()
}

tilo_test_env::scenario! {
    fn a_notion_export_reads_as_tables;

    phase an_export_archive_gives_one_table_per_view;
    {
        localized();
        let bytes = archive(&[
            (
                "Tasks 1a2b3c4d5e_all.csv",
                b"Name,Done\nShip,yes\n" as &[u8],
            ),
            ("Notes ef60718293.md", b"# Notes\n" as &[u8]),
        ]);
        let tables = tables(&bytes).unwrap();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].name, "Tasks");
        assert_eq!(tables[0].csv, "Name,Done\nShip,yes\n");
    }

    phase a_csv_the_export_does_not_mark_keeps_its_whole_name;
    {
        localized();
        let bytes = archive(&[("Q3 budget.csv", b"a,b\n1,2\n" as &[u8])]);
        let tables = tables(&bytes).unwrap();
        assert_eq!(tables[0].name, "Q3 budget");
    }

    phase a_database_listing_gives_its_rows_columns_from_the_properties;
    {
        localized();
        let listing = serde_json::json!({
            "results": [
                {"type": "child_database", "child_database": {
                    "title": "Projects",
                    "rows": [
                        {"properties": {
                            "Name": {"type": "title", "title": [{"plain_text": "Launch"}]},
                            "Count": {"type": "number", "number": 3},
                            "Done": {"type": "checkbox", "checkbox": true},
                        }},
                        {"properties": {
                            "Name": {"type": "title", "title": [{"plain_text": "Follow-up"}]},
                            "Done": {"type": "checkbox", "checkbox": false},
                        }},
                    ],
                }},
            ],
        });
        let bytes = serde_json::to_vec(&listing).unwrap();
        let tables = tables(&bytes).unwrap();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].name, "Projects");
        let csv = &tables[0].csv;
        assert_eq!(csv.lines().count(), 3, "a header and two rows: {csv}");
        let header: Vec<&str> = csv.lines().next().unwrap().split(',').collect();
        assert_eq!(header.len(), 3, "one column per property: {csv}");
        for name in ["Name", "Count", "Done"] {
            assert!(header.contains(&name), "{name} in {csv}");
        }
        assert!(csv.contains("Launch"), "{csv}");
        assert!(csv.contains('3'), "{csv}");
        assert!(csv.contains("Follow-up"), "{csv}");
    }

    phase a_databases_dates_and_selections_arrive_as_their_text;
    {
        localized();
        let listing = serde_json::json!({
            "results": [
                {"type": "child_database", "child_database": {
                    "title": "Shipments",
                    "rows": [
                        {"properties": {
                            "When": {"type": "date", "date": {"start": "2026-11-01"}},
                            "State": {"type": "select", "select": {"name": "Shipped"}},
                            "Tags": {"type": "multi_select", "multi_select": [{"name": "a"}, {"name": "b"}]},
                        }},
                    ],
                }},
            ],
        });
        let bytes = serde_json::to_vec(&listing).unwrap();
        let tables = tables(&bytes).unwrap();
        let csv = &tables[0].csv;
        assert!(csv.contains("2026-11-01"), "{csv}");
        assert!(csv.contains("Shipped"), "{csv}");
        assert!(csv.contains("\"a, b\""), "{csv}");
    }

    phase a_file_with_no_table_is_refused;
    {
        localized();
        let empty = archive(&[("Page 1a2b.md", b"# Page\n" as &[u8])]);
        assert!(tables(&empty).is_err());
        assert!(tables(b"not an export").is_err());
        let listing = serde_json::json!({"results": [{"type": "paragraph"}]});
        assert!(tables(&serde_json::to_vec(&listing).unwrap()).is_err());
    }

    phase a_quoted_field_keeps_its_quotes_and_line_breaks;
    {
        assert_eq!(csv_field_of("plain"), "plain");
        assert_eq!(csv_field_of("a,b"), "\"a,b\"");
        assert_eq!(csv_field_of("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field_of("two\nlines"), "\"two\nlines\"");
    }
}

// On its own, not a phase of the scenario above: a table block has no title, so this reads the
// default table name from the catalogue on a thread where no test installed an i18n manager, and
// a phase before it would leave the one it installed.
#[test]
fn a_listing_hands_over_its_table_block_as_rows() {
    let listing = serde_json::json!({
        "results": [
            {"type": "table", "table": {"table_width": 3}, "children": [
                {"type": "table_row", "table_row": {"cells": [
                    [{"plain_text": "Name"}],
                    [{"plain_text": "Owner"}],
                    [{"plain_text": "Due"}],
                ]}},
                {"type": "table_row", "table_row": {"cells": [
                    [{"plain_text": "Ship it"}],
                    [{"plain_text": "Alex, Sam"}],
                    [{"plain_text": "2026-11-01"}],
                ]}},
            ]},
        ],
    });
    let bytes = serde_json::to_vec(&listing).unwrap();
    let tables = tables(&bytes).unwrap();
    assert_eq!(tables.len(), 1);
    assert_eq!(
        tables[0].csv,
        "Name,Owner,Due\nShip it,\"Alex, Sam\",2026-11-01\n"
    );
}
