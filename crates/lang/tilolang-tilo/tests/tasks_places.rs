//! The sidebar's places: the window's own places in the order it lists them, the projects the
//! root holds under a heading that adds one of them, and what each place says when it holds
//! nothing, is all done with, or is someone's.

use tilolang_runtime::{Actor, UiNode, Value, ViewInstance};

use crate::fixtures::ShownSection;
use crate::tasks::{
    APP, call, count, create, go_to, home_list_rows, key_of, places, read, shown_count,
    shown_title, target, task, workspace,
};
use crate::tasks::{bound_key, task_page};

/// The places under each heading of the sidebar, in order.
fn sections(view: &ViewInstance) -> Vec<ShownSection> {
    crate::fixtures::sections(view.tree())
}

tilo_test_env::scenario! {
    fn the_tasks_window_has_its_places;

    phase the_sidebar_is_the_windows_places_and_the_projects_the_root_holds;
    // The sidebar the app draws: the window's own places in the order it lists them, counted where
    // the filter of a place asks for a count, and the projects the root holds under a heading that
    // adds one of them.
    {
        let (ada, root) = workspace();
        let project = call(&ada, "new_project", vec![root.clone()]);
        call(
            &ada,
            "rename",
            vec![project.clone(), Value::text("Groceries")],
        );
        let milk = call(&ada, "new_task", create(&root, "Buy oat milk"));
        call(&ada, "file", vec![milk, project]);
        let done = call(&ada, "new_task", create(&root, "Take the bins out"));
        call(&ada, "check", vec![done]);
        ada.commit();
        let view = ada.view(APP);
        let labels: Vec<String> = places(&view).into_iter().map(|(label, _)| label).collect();
        assert_eq!(
            labels,
            [
                "All issues",
                "Inbox",
                "Today",
                "Upcoming",
                "Todos",
                "Nobody yet",
                "Agent work",
                "Waiting",
                "Done",
                "Groceries"
            ],
            "the sidebar is the window's own places in the order it lists them, then the root's \
         projects: a priority is searched, never offered as a place: {labels:?}"
        );
        assert_eq!(
            count(&view, "All issues"),
            Some(2),
            "every issue counts, the one done with as well: {:?}",
            places(&view)
        );
        assert_eq!(
            count(&view, "Inbox"),
            None,
            "what waits to be sorted counts, and a count of zero says nothing: {:?}",
            places(&view)
        );
        assert_eq!(
            count(&view, "Today"),
            None,
            "and what is due says nothing while nothing is due: {:?}",
            places(&view)
        );
        assert_eq!(
            count(&view, "Upcoming"),
            None,
            "what is due later counts nothing: {:?}",
            places(&view)
        );
        assert_eq!(
            count(&view, "Todos"),
            Some(1),
            "the planned work counts what stands in it: {:?}",
            places(&view)
        );
        assert_eq!(
            count(&view, "Nobody yet"),
            Some(1),
            "and so does what nobody holds: {:?}",
            places(&view)
        );
        assert_eq!(
            count(&view, "Done"),
            None,
            "and Done counts nothing: {:?}",
            places(&view)
        );
        let groups = sections(&view);
        assert_eq!(
            groups,
            vec![("Projects".to_owned(), vec![("Groceries".to_owned(), None)]),],
            "the root's projects, each its own place, none of them counted"
        );
        // The heading's add is the action that makes another project on the root.
        let section = key_of(&view, "ui.NavSection", &|node| {
            node.props.get("title") == Some(&Value::text("Projects"))
        });
        let add = view.tree().find(&section)
            .and_then(|node| node.props.get("add"))
            .unwrap_or_else(|| panic!("the group offers an add"));
        let Value::Offered(add) = add else {
            panic!("an offered action: {add:?}");
        };
        assert_eq!(add.label, "New project");
        assert!(
            view.tree().find(&section).is_some_and(|node| node.events.contains_key("add.0")),
            "pressing it runs the action"
        );
        assert_eq!(add.icon.as_deref(), Some("folder-plus"));
    }

    phase a_project_place_shows_its_tasks_and_adds_into_it_in_the_same_window;
    // A project's place shows that project's tasks in the same window, and the entry at the
    // list's foot adds into it: choosing a place navigates in place, never away.
    {
        let (ada, root) = workspace();
        let project = call(&ada, "new_project", vec![root.clone()]);
        call(
            &ada,
            "rename",
            vec![project.clone(), Value::text("Groceries")],
        );
        let milk = call(&ada, "new_task", create(&root, "Buy oat milk"));
        call(&ada, "file", vec![milk.clone(), project.clone()]);
        call(&ada, "new_task", create(&root, "Book a haircut"));
        ada.commit();
        let mut view = ada.view(APP);
        go_to(&mut view, "Inbox");
        assert_eq!(home_list_rows(&mut view).len(), 1, "the inbox first");
        go_to(&mut view, "Groceries");
        let listed: Vec<String> = home_list_rows(&mut view)
            .into_iter()
            .map(|(_, _, of, _)| of)
            .collect();
        assert_eq!(listed, vec![target(&milk).unwrap()], "the project's tasks");
        let want = Value::text("project:Groceries");
        let nav = key_of(&view, "ui.NavList", &|node| {
            node.props.get("selected") == Some(&want)
        });
        assert!(
            view.tree().find(&nav).is_some(),
            "the window's place is the project's"
        );
        assert_eq!(
            shown_title(&view).as_deref(),
            Some("Groceries"),
            "and the window's page is titled after it"
        );
        assert_eq!(
            shown_count(&view),
            None,
            "a project's place is its own list, not a tally"
        );
        let entry = key_of(&view, "ui.TextInput", &|node| {
            node.props.get("placeholder") == Some(&Value::text("New task in Groceries"))
        });
        // The entry holds what is typed until Enter: its submit carries the title.
        view.fire_with(&entry, "submit", Some(&Value::text("Buy bread")))
            .unwrap();
        view.poll_patches().unwrap();
        let bread = task(&ada, "Buy bread");
        assert_eq!(
            target(&read(&ada, &bread, "project")),
            target(&project),
            "added into the place's project"
        );
        let listed: Vec<String> = home_list_rows(&mut view)
            .into_iter()
            .map(|(_, _, of, _)| of)
            .collect();
        assert!(
            listed.contains(&target(&bread).unwrap()),
            "the list shows it at once: {listed:?}"
        );
    }

    phase every_place_leads_to_a_list_not_to_the_nothing_chosen_page;
    // Every place of the sidebar leads to a page of its own: a list, or the list's own empty
    // state. None lands on the window of nothing chosen.
    {
        let (ada, root) = workspace();
        let project = call(&ada, "new_project", vec![root.clone()]);
        call(&ada, "rename", vec![project, Value::text("Groceries")]);
        call(&ada, "new_task", create(&root, "Book a haircut"));
        ada.commit();
        let mut view = ada.view(APP);
        let labels: Vec<String> = places(&view).into_iter().map(|(label, _)| label).collect();
        for place in labels {
            go_to(&mut view, &place);
            assert_eq!(
                shown_title(&view).as_deref(),
                Some(place.as_str()),
                "{place} is the page the window shows"
            );
            let shown = !view.tree().of_kind("ui.VirtualList").is_empty()
                || view.tree().of_kind("ui.EmptyState").into_iter().any(|node| {
                    node.props
                        .get("title")
                        .is_some_and(|title| *title != Value::text("No task selected"))
                });
            assert!(shown, "{place} lands on the nothing-chosen page");
        }
    }

    phase a_place_whose_tasks_are_all_done_says_so;
    // With every task in the place done, the place says so calmly, over the foot that folds what is
    // done away, and the places that count count nothing: what is done with leaves nothing open.
    {
        let (ada, root) = workspace();
        // A project, the place that folds what is done with under its list: the places that count
        // count what they leave open, so theirs counts nothing.
        let project = call(&ada, "new_project", vec![root.clone()]);
        call(
            &ada,
            "rename",
            vec![project.clone(), Value::text("Groceries")],
        );
        let mut made = Vec::new();
        for title in ["Buy oat milk", "Take the bins out", "Book a haircut"] {
            let task = call(&ada, "new_task", create(&root, title));
            call(&ada, "file", vec![task.clone(), project.clone()]);
            made.push(task);
        }
        ada.commit();
        for task in &made {
            call(&ada, "check", vec![task.clone()]);
        }
        ada.commit();
        let mut view = ada.view(APP);
        assert_eq!(
            count(&view, "Inbox"),
            None,
            "nothing is open, so the place says nothing: {:?}",
            places(&view)
        );
        assert_eq!(
            count(&view, "Today"),
            None,
            "and the day's place says nothing open either"
        );
        go_to(&mut view, "Groceries");
        let inline: Vec<&UiNode> = view.tree().of_kind("ui.EmptyState")
            .into_iter()
            .filter(|node| {
                matches!(
                    node.props.get("inline"),
                    Some(Value::Bool(true) | Value::Int(1))
                )
            })
            .collect();
        assert_eq!(inline.len(), 1, "one calm all-done state, above the fold");
        // The words are the window's, which the test host hands back as the phrase's id.
        let Some(Value::Text(title)) = inline[0].props.get("title") else {
            panic!("the all-done state is titled");
        };
        let title = title.clone();
        assert!(
            title == "All done" || title == "tilolang-all-done",
            "a calm all-done state, not the empty list's: got {title:?}"
        );
        assert_eq!(
            home_list_rows(&mut view).len(),
            3,
            "and what is done with folds under it"
        );
    }

    phase a_place_that_holds_nothing_says_so_rather_than_all_done;
    // A place that holds nothing says it holds nothing: nothing there waits, but nothing there is
    // done with either, so it is not that all of it is.
    {
        let (ada, root) = workspace();
        // Due today, so the place of what nobody sorted does not hold it.
        let task = call(&ada, "new_task", create(&root, "Buy oat milk"));
        call(
            &ada,
            "schedule",
            vec![task, Value::some(Value::text("2026-10-29"))],
        );
        ada.commit();
        let mut view = ada.view(APP);
        go_to(&mut view, "Inbox");
        let said: Vec<String> = view.tree().of_kind("ui.EmptyState")
            .into_iter()
            .filter_map(|node| match node.props.get("title") {
                Some(Value::Text(title)) => Some(title.clone()),
                _ => None,
            })
            .collect();
        assert!(
            said.iter().any(|title| title == "No tasks here"),
            "the place says it holds nothing: {said:?}"
        );
        assert!(
            !said
                .iter()
                .any(|title| title == "All done" || title == "tilolang-all-done"),
            "and not that everything is done: {said:?}"
        );
        assert!(
            home_list_rows(&mut view).is_empty(),
            "a place that holds nothing draws no list"
        );
    }

    phase a_shared_workspace_offers_the_place_of_what_the_person_looking_holds;
    // A workspace that names who a task belongs to offers the place of what the person looking
    // holds once someone else is in it, after the type's own places, so the place a window opens
    // on does not move; on their own, nothing is anyone's but theirs and the place would say what
    // a list of everything does. The filter is the app's, over its own assignee field and the
    // person looking; the sidebar is the app's too.
    {
        use tilolang_runtime::members::Member;

        let (ada, _root) = workspace();
        let mut view = ada.view(APP);
        view.poll_patches().unwrap();
        assert!(
            !places(&view)
                .iter()
                .any(|(label, _)| label == "Assigned to me"),
            "a workspace of one has no place of their own: {:?}",
            places(&view)
        );
        let scenario = ada
            .host
            .package(APP)
            .unwrap()
            .meta
            .scenarios
            .iter()
            .find(|scenario| scenario.name == "a shared week")
            .expect("Tasks stages a shared week")
            .action
            .clone();
        ada.host
            .runtime()
            .invoke_action(&Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")), (scenario.as_str(), vec![]))
            .unwrap();
        ada.commit();
        view.poll_patches().unwrap();
        let all = places(&view);
        assert_eq!(
            all.iter()
                .take(9)
                .map(|(label, _)| label.as_str())
                .collect::<Vec<_>>(),
            [
                "All issues",
                "Inbox",
                "Today",
                "Upcoming",
                "Todos",
                "Nobody yet",
                "Agent work",
                "Waiting",
                "Done"
            ],
            "the type's own places stand as they did, so the one a window opens on does not \
         move: {all:?}"
        );
        assert_eq!(
            all.get(9),
            Some(&("Assigned to me".to_owned(), Some(1))),
            "the place of what the person looking holds comes after the type's own places, and \
         counts the one open task the person looking holds: {all:?}"
        );
        assert!(
            all[10..].iter().all(|(label, _)| label == "Website launch"),
            "and the root's projects follow it, so the place a window opens on does not move: \
         {all:?}"
        );
        // The workspace narrows to the person looking alone: the place goes.
        ada.host
            .runtime()
            .set_members(vec![Member::person("ada", tilo_actions::policy::Role::Owner)], None);
        view.poll_patches().unwrap();
        assert!(
            !places(&view)
                .iter()
                .any(|(label, _)| label == "Assigned to me"),
            "the place goes when no one else is in the workspace: {:?}",
            places(&view)
        );
    }
}

/// The assignee's person field offers the workspace's members and its agents, by the people
/// and software a staged workspace has in it.
#[test]
fn the_assignee_offers_the_workspaces_people_and_agents() {
    let (ada, _root) = workspace();
    let scenario = ada
        .host
        .package(APP)
        .unwrap()
        .meta
        .scenarios
        .iter()
        .find(|scenario| scenario.name == "a shared week")
        .expect("Tasks stages a shared week")
        .action
        .clone();
    ada.host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            (scenario.as_str(), vec![]),
        )
        .unwrap();
    ada.commit();
    let post = task(&ada, "Write the launch post");
    let page = task_page(&ada, &post);
    let field = {
        let meta = &ada.host.package(APP).unwrap().meta;
        crate::fixtures::field_id(meta, "Task", "assignee")
    };
    let key = bound_key(page.tree(), &field).expect("the page binds its assignee");
    let node = page.tree().find(&key).expect("the bound node is drawn");
    assert_eq!(node.kind, "ui.PersonField");
    assert_eq!(
        node.props.get("people"),
        Some(&Value::List(vec![
            Value::Actor(Actor::from(tilolang_tilo::runtime::ids::MemberId::person(
                "Mia"
            ))),
            Value::Actor(Actor::from(tilolang_tilo::runtime::ids::MemberId::agent(
                "scenario:Scout"
            ))),
        ])),
        "the list offers the workspace's people and agents"
    );
    assert_eq!(
        node.props.get("me"),
        Some(&Value::Actor(Actor::from(
            tilolang_tilo::runtime::ids::MemberId::person("ada")
        ))),
        "the list leads with the person the page is shown to"
    );
}
