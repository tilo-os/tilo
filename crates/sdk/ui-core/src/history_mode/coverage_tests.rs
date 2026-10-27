use super::*;
use crate::history_mode::HistoryPresentation;

fn scope(schema: Option<&'static HistoryRestoreSchema>) -> HistoryScopeDefinition {
    HistoryScopeDefinition {
        key: "document",
        title: "Record",
        presentation: HistoryPresentation::External {
            reason: "checks the declaration only",
        },
        content_schema: schema,
    }
}

#[test]
fn a_type_is_classified_by_the_declarations_it_made_not_by_its_app() {
    let text = TextRendering::from_declaration(true, true);
    let structured = TextRendering::from_declaration(true, false);
    let none = TextRendering::from_declaration(false, false);
    assert_eq!(text, TextRendering::Text);
    assert_eq!(structured, TextRendering::Structured);
    assert_eq!(none, TextRendering::None);

    // A type with a restore schema restores content and, when it renders
    // text, diffs as text.
    let content = HistoricalSupport::of(
        &scope(Some(&crate::state::owned_restore_tests::SCHEMA)),
        text,
    );
    assert_eq!(content, HistoricalSupport::Content { text });
    assert!(content.restorable());
    assert!(content.declares_schema());
    assert!(content.diffs_as_text());

    // A type with no schema but a read adapter is metadata only: its
    // versions restore exact bytes, and nothing is diffed as text unless it
    // declared a text rendering.
    let binary = HistoricalSupport::of(&scope(None), structured);
    assert_eq!(binary, HistoricalSupport::MetadataOnly { text: structured });
    assert!(binary.restorable(), "exact bytes can be brought back");
    assert!(!binary.declares_schema());
    assert!(!binary.diffs_as_text(), "a binary file has no text diff");
    let rich = HistoricalSupport::of(&scope(None), text);
    assert!(rich.diffs_as_text(), "a declared text rendering is diffed");

    // A type that declares neither is unsupported, never promised.
    let unreadable = HistoricalSupport::of(&scope(None), none);
    assert_eq!(unreadable, HistoricalSupport::Unsupported { text: none });
    assert!(!unreadable.restorable());
    assert!(!unreadable.diffs_as_text());
}

#[test]
fn the_three_gaps_are_distinct_and_say_their_own_reason() {
    let unsupported = HistoricalGap::unsupported("this file has no versions");
    let unavailable = HistoricalGap::unavailable("that version is not in this record");
    let incompatible = HistoricalGap::of_version(3, 2).expect("a newer storage version");

    assert_ne!(unsupported, unavailable);
    assert_ne!(unsupported, incompatible);
    assert_ne!(unavailable, incompatible);
    assert!(matches!(unsupported, HistoricalGap::Unsupported(_)));
    assert!(matches!(unavailable, HistoricalGap::Unavailable(_)));
    assert_eq!(
        incompatible,
        HistoricalGap::SchemaIncompatible {
            stored: 3,
            expected: 2,
            reason: "written by a newer version of the app (storage version 3, this app reads 2)"
                .into(),
        }
    );
    let older = HistoricalGap::of_version(1, 2).expect("a migration is owed");
    assert!(
        older.reason().contains("brought up to version 2"),
        "{older}"
    );
    assert_eq!(HistoricalGap::of_version(2, 2), None);

    // Every gap says something a person can read, and says it once.
    for gap in [unsupported, unavailable, incompatible, older] {
        assert!(!gap.reason().is_empty());
        assert_eq!(gap.to_string(), gap.reason());
    }
}

#[test]
fn a_binary_version_names_the_exact_bytes_it_holds() {
    let data = b"not text at all\x00\x01".to_vec();
    let hash = *blake3::hash(&data).as_bytes();
    let version = BinaryVersion {
        hash: &hash,
        len: u64::try_from(data.len()).unwrap(),
    };
    let address = version.address();
    // The address is the hash and the exact length of those bytes.
    assert_eq!(
        address,
        format!(
            "blake3:{}:{}",
            blake3::Hash::from_bytes(hash).to_hex(),
            data.len()
        )
    );
    // The address is the bytes themselves: fetching it returns the exact
    // content the version holds, which is what a restore takes back.
    let hex = address
        .strip_prefix("blake3:")
        .and_then(|rest| rest.split_once(':'))
        .expect("a content address names its hash and length");
    assert_eq!(
        blake3::Hash::from_hex(hex.0).unwrap(),
        blake3::Hash::from_bytes(hash)
    );
    assert_eq!(hex.1, data.len().to_string());
}

#[test]
fn a_registered_schema_is_checked_before_a_record_exists() {
    assert!(TypeHistory::check_schema(&crate::state::owned_restore_tests::SCHEMA).is_ok());
    let history = TypeHistory::of(
        "test.board",
        1,
        &scope(Some(&crate::state::owned_restore_tests::SCHEMA)),
        TextRendering::Text,
    );
    assert_eq!(history.kind, "test.board");
    assert_eq!(history.version, 1);
    assert!(history.support.declares_schema());
}
