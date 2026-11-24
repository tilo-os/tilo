//! A task landed through the action API is listed under the workspace's tasks at once, with the
//! actions a member runs on it, both in the record's whole listing and a level at a time.

use std::collections::HashSet;

use tilo_actions::remote::{CataloguePage, CatalogueQuery};

use super::Tasks;

/// The action a member runs on a task that the listing is checked for.
const ON_A_TASK: &str = "set_priority_high";

#[test]
fn a_task_landed_through_the_api_is_listed_with_what_runs_on_it() {
    let mut tasks = Tasks::founded();
    let task = tasks.new_task("Call the landlord");

    let surveyed = tasks.opened.rows();
    let listed: Vec<String> = surveyed
        .iter()
        .map(|row| {
            format!(
                "{} | {} | {:?}",
                row.parent_resource,
                row.resource.id,
                row.action.as_ref().map(|action| &action.address.action)
            )
        })
        .collect();
    let item_action = surveyed.iter().any(|row| {
        row.parent_resource == tasks.root
            && row.resource.id == task
            && row
                .action
                .as_ref()
                .is_some_and(|action| action.address.action == ON_A_TASK)
    });
    assert!(
        item_action,
        "the landed task {task} is listed under the tasks with its priority: {listed:#?}"
    );

    // A caller lists a level at a time, as a page and an agent's ls do: the tasks are a level
    // with children, and their level lists the task.
    let mut under: Vec<Option<String>> = vec![None];
    let mut walked = Vec::new();
    let mut visited = HashSet::new();
    while let Some(parent) = under.pop() {
        if !visited.insert(parent.clone()) {
            continue;
        }
        let mut query = CatalogueQuery {
            parent_resource: parent.clone(),
            max_results: 256,
            ..CatalogueQuery::default()
        };
        loop {
            let CataloguePage {
                rows, next_cursor, ..
            } = tasks.opened.page(&query);
            for row in rows {
                if row.has_children {
                    under.push(Some(row.resource.id.clone()));
                }
                walked.push(format!(
                    "{:?} | {} | {} | {:?}",
                    parent,
                    row.parent_resource,
                    row.resource.id,
                    row.action.as_ref().map(|action| &action.address.action)
                ));
            }
            assert!(walked.len() < 5_000, "the levels end");
            if next_cursor.is_empty() {
                break;
            }
            query.cursor = next_cursor;
        }
    }
    assert!(
        walked
            .iter()
            .any(|row| row.contains(&format!("| {} | {task} |", tasks.root))
                && row.ends_with(&format!("Some({ON_A_TASK:?})"))),
        "a level at a time lists the landed task with its priority: {walked:#?}"
    );
}
