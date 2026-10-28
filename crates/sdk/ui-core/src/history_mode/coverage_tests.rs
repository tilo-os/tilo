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
    // A type with a restore schema restores content.
    let content = HistoricalSupport::of(
        &scope(Some(&crate::state::owned_restore_tests::SCHEMA)),
        true,
    );
    assert_eq!(content, HistoricalSupport::Content);
    assert!(content.restorable());
    assert!(content.declares_schema());

    // A type with no schema but a read adapter reads its versions through
    // that adapter and restores them byte for byte.
    let read = HistoricalSupport::of(&scope(None), true);
    assert_eq!(read, HistoricalSupport::MetadataOnly);
    assert!(read.restorable(), "exact bytes can be brought back");
    assert!(!read.declares_schema());

    // A type that declares neither is unsupported, never promised.
    let unreadable = HistoricalSupport::of(&scope(None), false);
    assert_eq!(unreadable, HistoricalSupport::Unsupported);
    assert!(!unreadable.restorable());
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
fn a_registered_schema_is_checked_before_a_record_exists() {
    assert!(TypeHistory::check_schema(&crate::state::owned_restore_tests::SCHEMA).is_ok());
    let history = TypeHistory::of(
        "test.board",
        1,
        &scope(Some(&crate::state::owned_restore_tests::SCHEMA)),
        true,
    );
    assert_eq!(history.kind, "test.board");
    assert_eq!(history.version, 1);
    assert!(history.support.declares_schema());
}
