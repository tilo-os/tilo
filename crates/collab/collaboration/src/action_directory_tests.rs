use super::*;
use tilo_actions::{ActionSpec, OperationKind, ResourceSpec};

fn action(id: &str, label: &str, kind: OperationKind) -> ActionSpec {
    ActionSpec::operation(id, label, kind, "test.directory", &0usize).unwrap()
}

/// An app with two lists, each with a view and an edit, and a child item.
fn resources(items: usize) -> Vec<ResourceSpec> {
    let list = |id: &str, label: &str| {
        let mut list = ResourceSpec::new(id, "test.list", label)
            .with_action(action("view", "Show", OperationKind::View).reads_document())
            .with_action(action("add", "Add item", OperationKind::Effect));
        for item in 0..items {
            list = list.with_child(
                ResourceSpec::new(format!("{id}/{item}"), "test.item", format!("Item {item}"))
                    .with_action(action("view", "Show", OperationKind::View).reads_document()),
            );
        }
        list
    };
    vec![list("todos", "Todos"), list("done", "Done")]
}

fn published(items: usize) -> PublishedDirectory {
    let resources = resources(items);
    let mut directory = PublishedDirectory::default();
    directory.publish(&resources, &tilo_actions::door::survey(&resources));
    directory
}

fn query() -> ActionDirectoryQuery {
    ActionDirectoryQuery {
        peer_id: 1,
        revision: String::new(),
        cursor: Vec::new(),
        needle: String::new(),
        parent_resource: String::new(),
        max_results: 64,
        max_bytes: 64 * 1024,
        max_work: 512,
        max_depth: 4,
        resource_name: String::new(),
    }
}

#[test]
fn nothing_published_is_unavailable() {
    let directory = PublishedDirectory::default();
    let page = directory.page(&query());
    assert!(page.revision.is_empty() && page.rows.is_empty() && !page.exhausted);
    assert_eq!(
        directory.resolve("", "todos", "add"),
        ActionDirectoryResolution::Unavailable
    );
}

#[test]
fn levels_list_resources_then_their_actions_and_children() {
    let directory = published(2);
    let top = directory.page(&query());
    assert!(top.exhausted);
    let names: Vec<_> = top.rows.iter().map(|row| row.resource.as_str()).collect();
    assert_eq!(names, ["todos", "done"]);
    assert!(
        top.rows
            .iter()
            .all(|row| row.has_children && row.action.is_none())
    );

    let level = directory.page(&ActionDirectoryQuery {
        parent_resource: "todos".into(),
        ..query()
    });
    let rows: Vec<_> = level
        .rows
        .iter()
        .map(|row| (row.resource.as_str(), row.action.as_deref()))
        .collect();
    assert_eq!(
        rows,
        [
            ("todos", Some("view")),
            ("todos", Some("add")),
            ("todos/0", None),
            ("todos/1", None)
        ]
    );
}

#[test]
fn a_level_continues_from_its_cursor_and_a_moved_directory_refuses_it() {
    let mut directory = published(5);
    let first = directory.page(&ActionDirectoryQuery {
        parent_resource: "todos".into(),
        max_results: 3,
        ..query()
    });
    assert_eq!(first.rows.len(), 3);
    assert!(!first.exhausted && !first.next_cursor.is_empty());
    let rest = directory.page(&ActionDirectoryQuery {
        parent_resource: "todos".into(),
        cursor: first.next_cursor.clone(),
        ..query()
    });
    assert!(rest.exhausted);
    assert_eq!(rest.rows.len(), 4, "two actions and five children in all");
    assert_eq!(rest.rows[0].resource, "todos/1");

    // Another request's cursor is not this one's.
    let other = directory.page(&ActionDirectoryQuery {
        parent_resource: "done".into(),
        cursor: first.next_cursor.clone(),
        ..query()
    });
    assert!(other.stale_cursor);

    // The same publication keeps the revision; a different one moves it.
    let revision = first.revision.clone();
    let same = resources(5);
    directory.publish(&same, &tilo_actions::door::survey(&same));
    assert_eq!(directory.revision(), Some(revision.as_str()));
    let fewer = resources(1);
    directory.publish(&fewer, &tilo_actions::door::survey(&fewer));
    assert_ne!(directory.revision(), Some(revision.as_str()));
    let stale = directory.page(&ActionDirectoryQuery {
        parent_resource: "todos".into(),
        cursor: first.next_cursor,
        ..query()
    });
    assert!(stale.stale_cursor && stale.rows.is_empty());
}

#[test]
fn a_name_finds_a_resource_by_kind_or_label() {
    let directory = published(3);
    let page = directory.page(&ActionDirectoryQuery {
        resource_name: "TODOS".into(),
        ..query()
    });
    assert!(page.exhausted);
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].resource, "todos");
    let items = directory.page(&ActionDirectoryQuery {
        resource_name: "test.item".into(),
        parent_resource: "done".into(),
        ..query()
    });
    assert_eq!(items.rows.len(), 3);
    assert!(items.rows.iter().all(|row| row.parent_resource == "done"));
}

#[test]
fn a_search_finds_actions_in_tree_order_and_resolves_them() {
    let directory = published(1);
    let page = directory.page(&ActionDirectoryQuery {
        needle: "add".into(),
        ..query()
    });
    assert!(page.exhausted);
    let found: Vec<_> = page
        .rows
        .iter()
        .map(|row| (row.resource.as_str(), row.action.as_deref()))
        .collect();
    assert_eq!(found, [("todos", Some("add")), ("done", Some("add"))]);
    assert!(page.rows.iter().all(|row| row.descriptor.is_some()));

    let ActionDirectoryResolution::Found { row, revision } =
        directory.resolve(&page.revision, "todos/0", "view")
    else {
        panic!("the item's view resolves");
    };
    assert_eq!(row.action.as_deref(), Some("view"));
    assert_eq!(revision.as_deref(), Some(page.revision.as_str()));
    assert_eq!(
        directory.resolve("v1:gone", "todos", "add"),
        ActionDirectoryResolution::StaleRevision
    );
    assert_eq!(
        directory.resolve("", "todos", "remove"),
        ActionDirectoryResolution::NotFound
    );
}

#[test]
fn a_row_that_does_not_fit_is_where_the_next_page_starts() {
    let directory = published(0);
    let whole = directory.page(&query());
    let first = action_directory_row_wire_bytes(&whole.rows[0]);
    let page = directory.page(&ActionDirectoryQuery {
        max_bytes: u32::try_from(PAGE_WIRE_BYTES + whole.revision.len() + first).unwrap(),
        ..query()
    });
    assert_eq!(page.rows.len(), 1);
    assert!(!page.exhausted && !page.next_cursor.is_empty());
    let rest = directory.page(&ActionDirectoryQuery {
        cursor: page.next_cursor,
        ..query()
    });
    assert_eq!(rest.rows.len(), 1);
    assert_eq!(rest.rows[0].resource, "done");
}
