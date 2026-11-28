//! Backups as it ships: what the system's History says each change of how many snapshots a
//! backup keeps did, in English and in German.

use tilolang_runtime::{Actor, Value};

use crate::fixtures::Pane;

const SOURCE: &str = include_str!("../../../../apps/tl/backups/app.tl");

fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// Each step of a retention count says which class it changed and the count it left, read
/// from the policy as the change left it, with the class by its label in the reader's language.
#[test]
fn history_says_how_many_snapshots_of_each_class_a_change_keeps() {
    let app = Pane::install_with_ids(
        &me(),
        SOURCE,
        include_str!("../../../../apps/tl/backups/app.ids"),
    )
    .rooted("so.tilo.backups.Backups");
    for (action, which) in [
        ("keep_more", "hourly"),
        ("keep_fewer", "daily"),
        ("keep_more", "weekly"),
        ("keep_fewer", "monthly"),
    ] {
        app.call(action, vec![app.root_value(), Value::text(which)]);
    }
    let german = include_str!("../../../../apps/tl/backups/locales/de-DE.ftl");
    let english = [
        ("Changed Hourly of", "to 25"),
        ("Changed Daily of", "to 6"),
        ("Changed Weekly of", "to 5"),
        ("Changed Monthly of", "to 11"),
    ];
    let in_german = [
        ("Stündlich von", "auf 25 geändert"),
        ("Täglich von", "auf 6 geändert"),
        ("Wöchentlich von", "auf 5 geändert"),
        ("Monatlich von", "auf 11 geändert"),
    ];
    let policy = match app.host.runtime().read(app.root(), "policy").unwrap() {
        Value::Ref(policy) => (*policy.target).clone(),
        other => panic!("the policy is a record of its own: {other:?}"),
    };
    for (locale, words, wanted) in [("en-US", "", english), ("de-DE", german, in_german)] {
        let mut said = app.history(&policy, locale, words);
        let said = said.split_off(said.len().saturating_sub(wanted.len()));
        assert_eq!(said.len(), wanted.len(), "{said:?}");
        for (said, (class, count)) in said.iter().zip(wanted) {
            assert!(
                said.starts_with(class) && said.ends_with(count),
                "{class:?} … {count:?}: {said:?}"
            );
        }
    }
}
