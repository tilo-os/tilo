use super::*;
use ui_core::search::index_publication::IndexWeight;

#[test]
fn an_anchored_entry_is_kept_by_its_resource_with_its_short_fields() {
    let entry = SearchEntry::new("Groceries", "milk, eggs")
        .anchored("note:7")
        .subtitled("Notes")
        .field("errands", tilo_resource::SearchWeight::Metadata)
        .field("hidden", tilo_resource::SearchWeight::Unranked);
    let indexed = index_entry(&entry).expect("an anchored entry opens");
    assert_eq!(indexed.key, "note:7");
    assert_eq!(indexed.resource, "note:7");
    assert_eq!(indexed.action, None);
    assert_eq!(indexed.subtitle.as_deref(), Some("Notes"));
    assert_eq!(
        indexed.fields,
        [
            ("milk, eggs".to_owned(), IndexWeight::Body),
            ("errands".to_owned(), IndexWeight::Metadata),
        ],
        "unranked text never enters the index"
    );
    assert!(!indexed.secret);
}

#[test]
fn a_secret_entry_carries_its_metadata_but_not_its_text() {
    let entry = SearchEntry::new("GitHub", "never indexed")
        .anchored("passwords.item:gh")
        .field("github.com", tilo_resource::SearchWeight::Identity)
        .secret_bearing();
    let indexed = index_entry(&entry).expect("kept");
    assert!(indexed.secret);
    assert_eq!(
        indexed.fields,
        [("github.com".to_owned(), IndexWeight::Identity)]
    );
}

#[test]
fn an_entry_invoking_an_action_is_keyed_by_it_and_one_opening_nothing_is_dropped() {
    let entry = SearchEntry::new("Marker", "")
        .invoking(tilo_actions::ActionAddress::new("timeline", "seek"));
    let indexed = index_entry(&entry).expect("an action opens it");
    assert_eq!(indexed.resource, "timeline");
    assert_eq!(indexed.key, "timeline\u{1f}seek");
    assert_eq!(
        indexed.action.map(|action| action.action).as_deref(),
        Some("seek")
    );
    assert!(index_entry(&SearchEntry::new("Loose", "")).is_none());
}
