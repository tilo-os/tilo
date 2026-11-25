use super::*;

tilo_test_env::scenario! {
    fn a_types_history_support_and_gaps_are_declared_not_assumed;

    phase a_type_is_classified_by_the_declarations_it_made_not_by_its_app;
    {
        // A type with a read adapter shows its versions through that adapter and restores them
        // through the store's history verbs, checked against the same read.
        let content = HistoricalSupport::of(true);
        assert_eq!(content, HistoricalSupport::Content);
        assert!(content.restorable());

        // A type that declares no read adapter is unsupported, never promised.
        let unreadable = HistoricalSupport::of(false);
        assert_eq!(unreadable, HistoricalSupport::Unsupported);
        assert!(!unreadable.restorable());
    }

    phase the_three_gaps_are_distinct_and_say_their_own_reason;
    {
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

        for gap in [unsupported, unavailable, incompatible, older] {
            assert!(!gap.reason().is_empty());
            assert_eq!(gap.to_string(), gap.reason());
        }
    }
}
