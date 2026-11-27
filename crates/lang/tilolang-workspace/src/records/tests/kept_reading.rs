//! A record's reading kept open between asks ([`super::super::Kept`]), as the record provider
//! keeps one per reader: told which documents another record's write moved, it answers what a
//! record opened fresh answers, under ten thousand tasks.

use std::rc::Rc;

use collab_types::store::MovedRecord;
use tilo_actions::ActionAddress;
use tilo_actions::remote::CatalogueQuery;

use super::{APP, Opened, PERSON, ResourceStore, Tasks, built_in, member};

#[test]
fn a_kept_reading_told_what_moved_answers_as_a_fresh_one() {
    let mut tasks = Tasks::founded();
    for start in (0..10_000).step_by(500) {
        let titles: Vec<String> = (start..start + 500)
            .map(|at| format!("Task {at}: answer the landlord about the boiler"))
            .collect();
        tasks.new_tasks(&titles);
    }
    let packages = built_in(APP).unwrap().unwrap();
    let port = tasks.opened.host().store().port();
    let open = || {
        Opened::open(
            ResourceStore::new(Rc::clone(&port)),
            &packages,
            member(tilo_actions::MemberId::person(PERSON)),
            None,
        )
        .unwrap()
    };
    let under_tasks = CatalogueQuery {
        parent_resource: Some(tasks.root.clone()),
        max_results: 40,
        ..CatalogueQuery::default()
    };
    let kept = open();
    let first = kept.page(&under_tasks);
    assert_eq!(first.rows.len(), 40, "a page of 40 rows under the tasks");
    assert!(!first.next_cursor.is_empty(), "and more after it");

    // Another record writes a task; the kept reading is told the documents it moved, as the
    // workspace tells each write.
    let watch = tasks.opened.host().store().watch();
    let made = tasks.new_task("Written after the ask");
    let moved: Vec<MovedRecord> = watch
        .take()
        .docs
        .into_iter()
        .map(MovedRecord::whole)
        .collect();
    assert!(!moved.is_empty(), "the write moved a document");
    kept.host().store().moved_records(&moved);

    let fresh = open();
    assert_eq!(
        kept.page(&under_tasks),
        fresh.page(&under_tasks),
        "the kept reading pages as a fresh one"
    );
    let action = first
        .rows
        .iter()
        .find_map(|row| row.action.as_ref())
        .unwrap()
        .address
        .action
        .clone();
    let exact = CatalogueQuery {
        target: Some(ActionAddress::new(made, action)),
        ..CatalogueQuery::default()
    };
    let found = kept.page(&exact);
    assert_eq!(
        found.rows.len(),
        1,
        "the task written since is offered: {found:?}"
    );
    assert_eq!(found, fresh.page(&exact));
}
