#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion"
)]

use tilo_resource::{Id, NonBlank, Replica};

use crate::model::{
    Imported, ImportedMessage, MessageKind, NewIssue, Status, Tracker, agent_assignee, tracker,
};

const AGENT: &str = "agent:claude";

/// `text` as the non-blank text an action takes.
fn nb(text: &str) -> NonBlank {
    NonBlank::parse(text).expect("the test's text says something")
}

fn create(title: &str, parent: Option<Id>, assignee: Option<&str>) -> NewIssue {
    NewIssue {
        parent,
        assignee: assignee.map(str::to_owned),
        ..NewIssue::titled(nb(title))
    }
}

fn ada() -> Replica<Tracker> {
    let mut replica = Replica::<Tracker>::new().as_author("ada");
    replica.set_now_ms(1_000);
    replica
}

fn id_of(replica: &Replica<Tracker>, title: &str) -> Id {
    replica
        .state()
        .unwrap()
        .issues
        .iter()
        .find(|(_, issue)| issue.title == title)
        .map(|(id, _)| id.clone())
        .expect("the issue is there")
}

#[test]
fn issues_are_numbered_nested_and_never_under_themselves() {
    let mut replica = ada();
    replica
        .apply(Tracker::CREATE.with(create("Ship it", None, None)))
        .unwrap();
    let parent = id_of(&replica, "Ship it");
    replica
        .apply(Tracker::CREATE.with(create("Write notes", Some(parent.clone()), None)))
        .unwrap();
    let child = id_of(&replica, "Write notes");
    let state = replica.state().unwrap();
    assert_eq!(state.issues.get(&parent).unwrap().number, 1);
    assert_eq!(state.issues.get(&child).unwrap().number, 2);
    assert_eq!(state.issues.get(&child).unwrap().author, "ada");
    assert_eq!(state.progress(&parent), (0, 1));
    assert!(
        replica
            .apply(Tracker::MOVE_UNDER.with(tracker::MoveUnder {
                issue: parent.clone(),
                parent: Some(child.clone()),
            }))
            .is_err(),
        "a parent cannot move under its own sub-issue"
    );
    assert!(
        serde_json::from_value::<tracker::Create>(serde_json::json!({"title": "  "})).is_err(),
        "a blank title is refused where the arguments are read"
    );
    // Deleting the parent lifts its sub-issue to the top level.
    replica
        .apply_confirmed(Tracker::DELETE.with(tracker::Delete { issue: parent }))
        .unwrap();
    assert_eq!(
        replica.state().unwrap().issues.get(&child).unwrap().parent,
        None
    );
}

#[test]
fn numbers_given_after_a_merge_are_past_both_sides() {
    let mut left = ada();
    let mut right = left.fork().as_author("grace");
    left.apply(Tracker::CREATE.with(create("Left", None, None)))
        .unwrap();
    right
        .apply(Tracker::CREATE.with(create("Right", None, None)))
        .unwrap();
    left.sync_with(&mut right).unwrap();
    left.apply(Tracker::CREATE.with(create("After", None, None)))
        .unwrap();
    let after = id_of(&left, "After");
    assert_eq!(left.state().unwrap().issues.get(&after).unwrap().number, 3);
}

#[test]
fn placing_on_the_board_sets_status_and_order_in_one_step() {
    let mut replica = ada();
    for title in ["One", "Two", "Three"] {
        replica
            .apply(Tracker::CREATE.with(create(title, None, None)))
            .unwrap();
    }
    let one = id_of(&replica, "One");
    let three = id_of(&replica, "Three");
    replica
        .apply(Tracker::PLACE.with(tracker::Place {
            issue: three.clone(),
            status: Status::Todo,
            before: Some(one),
        }))
        .unwrap();
    let state = replica.state().unwrap();
    let order: Vec<&str> = state
        .issues
        .values()
        .map(|issue| issue.title.as_str())
        .collect();
    assert_eq!(order, ["Three", "One", "Two"]);
    assert_eq!(state.issues.get(&three).unwrap().status, Status::Todo);
    // Down past where it was: after Two, which was third.
    replica
        .apply(Tracker::PLACE.with(tracker::Place {
            issue: three.clone(),
            status: Status::Done,
            before: None,
        }))
        .unwrap();
    let state = replica.state().unwrap();
    assert_eq!(state.issues.at(2).unwrap().0, &three);
}

/// A person giving work to an agent starts its run then, acting for them:
/// on a new issue and on one given to it later alike.
#[test]
fn a_task_is_a_shared_thread_two_people_take_part_in() {
    let mut ada = ada().as_invoker("ada");
    ada.apply(Tracker::CREATE.with(create("Plan the offsite", None, None)))
        .unwrap();
    let issue = id_of(&ada, "Plan the offsite");
    let mut bob = ada.fork().as_invoker("bob");
    ada.apply(Tracker::COMMENT.with(tracker::Comment {
        issue: issue.clone(),
        text: nb("Book the room"),
        origin: None,
    }))
    .unwrap();
    bob.sync_with(&mut ada).unwrap();
    bob.apply(Tracker::COMMENT.with(tracker::Comment {
        issue: issue.clone(),
        text: nb("I will write the agenda"),
        origin: None,
    }))
    .unwrap();
    ada.sync_with(&mut bob).unwrap();
    for replica in [&ada, &bob] {
        let state = replica.state().unwrap();
        let said: Vec<(String, String)> = state.issues[&issue]
            .comments
            .values()
            .map(|message| (message.author.clone(), message.text.clone()))
            .collect();
        assert_eq!(
            said,
            vec![
                ("ada".to_owned(), "Book the room".to_owned()),
                ("bob".to_owned(), "I will write the agenda".to_owned()),
            ],
            "both people read both messages, each attributed to its author"
        );
        assert!(
            state.issues[&issue]
                .comments
                .values()
                .all(|message| message.kind == MessageKind::Comment),
            "a message in the thread reads as a comment"
        );
    }
}

#[test]
fn imported_tasks_are_one_issue_whoever_imports_them_and_never_come_back() {
    let task = Imported {
        task: "t1".into(),
        title: "Old task".into(),
        instruction: "Do the old thing".into(),
        status: Status::Todo,
        assignee: Some(agent_assignee("claude").0),
        author: "ada".into(),
        created_ms: 5,
        messages: vec![ImportedMessage {
            author: AGENT.into(),
            by_agent: true,
            at_ms: 6,
            text: "Started".into(),
        }],
    };
    let import = || {
        Tracker::IMPORT.with(tracker::Import {
            tasks: vec![task.clone()],
        })
    };
    let mut left = ada();
    let mut right = left.fork().as_author("grace");
    left.apply(import()).unwrap();
    right.apply(import()).unwrap();
    left.sync_with(&mut right).unwrap();
    let state = left.state().unwrap();
    assert_eq!(state.issues.len(), 1);
    let issue = state.issues.get(&task.issue_id()).unwrap();
    assert_eq!(issue.assigned_agent(), Some(AGENT));
    assert!(issue.body.text().contains("Do the old thing"));
    assert_eq!(issue.comments.len(), 1);
    left.apply_confirmed(Tracker::DELETE.with(tracker::Delete {
        issue: task.issue_id(),
    }))
    .unwrap();
    left.apply(import()).unwrap();
    assert!(left.state().unwrap().issues.is_empty());
}

#[test]
fn a_task_syncs_to_another_device_survives_a_restart_and_merges_its_edits() {
    use crate::model::Area;

    let mut on_laptop = ada();
    on_laptop
        .apply(Tracker::CREATE.with(create("Prepare Q3", None, Some(AGENT))))
        .unwrap();
    let issue = id_of(&on_laptop, "Prepare Q3");
    on_laptop
        .apply(Tracker::PLAN.with(tracker::Plan {
            issue: issue.clone(),
            area: Some(Area {
                resource: "notes/finance/q3".into(),
                app: "so.tilo.notes".into(),
                kind: "notes.note".into(),
                label: "Q3 financials".into(),
            }),
            priority: Some(crate::model::Priority::High),
            estimate: None,
            due_ms: Some(1_800_000_000_000),
        }))
        .unwrap();
    // The person's other device joins with the tracker it holds and takes in
    // the rest: every field of the task is there, from the record alone. Each
    // device writes its own changes for the person at it, as a host does.
    let mut on_laptop = on_laptop.as_invoker("ada");
    let mut on_desktop = on_laptop.fork().as_invoker("ada");
    on_desktop.sync_with(&mut on_laptop).unwrap();
    let synced = on_desktop.state().unwrap();
    let read = synced.issues.get(&issue).unwrap();
    assert_eq!(read.title, "Prepare Q3");
    assert_eq!(read.assigned_agent(), Some(AGENT));
    assert_eq!(read.priority, Some(2));
    assert_eq!(
        read.area.as_ref().map(|area| area.label.as_str()),
        Some("Q3 financials")
    );

    // Both work on it at once, off the net: the desktop nests a subtask, the
    // agent the issue is assigned to comments on its laptop. Both edits are
    // ordinary collaborative changes and both survive the merge.
    on_desktop
        .apply(Tracker::CREATE.with(create("Gather the ledgers", Some(issue.clone()), None)))
        .unwrap();
    let mut agent = on_laptop.fork().as_agent_for(AGENT, "ada");
    agent
        .apply(Tracker::COMMENT.with(tracker::Comment {
            issue: issue.clone(),
            text: nb("The ledgers are in the shared drive"),
            origin: None,
        }))
        .unwrap();
    on_laptop.sync_with(&mut agent).unwrap();
    on_desktop.sync_with(&mut on_laptop).unwrap();
    for merged in [on_laptop.state().unwrap(), on_desktop.state().unwrap()] {
        let task = merged.issues.get(&issue).unwrap();
        assert!(
            task.comments
                .values()
                .any(|message| message.by_agent && message.text.contains("shared drive")),
            "the agent's comment is on both"
        );
        assert!(
            merged
                .issues
                .iter()
                .any(|(_, other)| other.title == "Gather the ledgers"),
            "the subtask made on the other device is on both"
        );
    }

    // Restart: the tracker is rebuilt from its own bytes, and every field the
    // person could read before is still there.
    let before = on_desktop.state().unwrap();
    let reopened = Replica::<Tracker>::load(&on_desktop.save()).unwrap();
    let after = reopened.state().unwrap();
    assert_eq!(after.issues.len(), before.issues.len());
    let reopened_issue = after.issues.get(&issue).unwrap();
    assert_eq!(reopened_issue, before.issues.get(&issue).unwrap());
    assert_eq!(reopened_issue.priority, Some(2));
}

/// The task is a published resource of the action registry, and each change
/// says, typed, which task made it and which person it acted under.
#[test]
fn the_task_is_published_with_its_verbs_and_its_changes_carry_the_task() {
    let mut person = ada();
    person
        .apply(Tracker::CREATE.with(create("Release the notes", None, Some(AGENT))))
        .unwrap();
    let issue = id_of(&person, "Release the notes");

    // Published through the registry, with the verbs a surface or an agent
    // runs, rather than an app-private store.
    let tree = person.published().unwrap();
    assert_eq!(tree.kind, crate::model::KIND);
    let verbs: Vec<&str> = tree
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    for verb in ["create", "comment", "assign", "plan", "link"] {
        assert!(verbs.contains(&verb), "{verb} is published; got {verbs:?}");
    }

    // Every change says what it did and for whom, in fields the person reads:
    // the agent, the kind of actor, and the person it acted under.
    let mut agent = person.fork().as_agent_for(AGENT, "ada");
    agent
        .apply(Tracker::COMMENT.with(tracker::Comment {
            issue,
            text: nb("The notes are out"),
            origin: None,
        }))
        .unwrap();
    let versions = agent.versions();
    let spent = versions
        .iter()
        .rev()
        .find_map(|version| {
            let context = version.context.as_ref()?;
            context.what.as_ref()?;
            Some(context)
        })
        .expect("the agent's comment is a version with what it did");
    let invoker = spent.invoker.as_ref().expect("the change names its actor");
    assert_eq!(invoker.key(), AGENT);
    assert!(invoker.acting.is_agent());
    assert_eq!(invoker.acting.acted_for_person(), Some("ada"));
}

/// A create says what it makes: the collection it adds to, so a window says the
/// step by that collection's own noun ("Creating an issue in Issues") rather
/// than only where it happens.
#[test]
fn a_create_names_the_collection_it_makes_one_of() {
    let person = ada();
    let tree = person.published().unwrap();
    let create = tree
        .actions
        .iter()
        .find(|action| action.id == "create")
        .expect("the tracker publishes its create");
    assert_eq!(create.contract.step, Some(tilo_actions::StepKind::Create));
    assert_eq!(create.contract.creates.as_deref(), Some("issues"));
    let area = tree
        .actions
        .iter()
        .find(|action| action.id == "create-area")
        .expect("the tracker publishes its project and area create");
    assert_eq!(
        area.contract.creates.as_deref(),
        Some("areas"),
        "a create says the collection it writes"
    );
}

/// An agent renames an issue by the tracker's own verb: the title it writes
/// is the issue's, the change is the agent's, and a blank or over-long title
/// is refused before anything is written.
#[test]
fn rename_writes_the_issue_title_and_refuses_a_blank_one() {
    let mut person = ada();
    person
        .apply(Tracker::CREATE.with(create("Book the room", None, None)))
        .unwrap();
    let issue = id_of(&person, "Book the room");
    let mut agent = person.fork().as_agent_for(AGENT, "ada");
    agent
        .apply(Tracker::RENAME.with(tracker::Rename {
            issue: issue.clone(),
            title: nb("Book room four"),
        }))
        .unwrap();
    assert_eq!(
        agent.state().unwrap().issues.get(&issue).unwrap().title,
        "Book room four"
    );
    let spent = agent
        .versions()
        .iter()
        .rev()
        .find_map(|version| version.context.clone())
        .expect("the rename is a version with what it did");
    assert_eq!(
        spent.invoker.expect("the change names its actor").key(),
        AGENT
    );
    assert!(
        agent
            .apply(Tracker::RENAME.with(tracker::Rename {
                issue,
                title: NonBlank::parse(&"x".repeat(241)).unwrap(),
            }))
            .is_err(),
        "a title that runs past a line is refused"
    );
}

/// The step line a person reads says what each of the tracker's main actions
/// does there — it edits, makes, deletes or writes — never merely that work
/// happens in Tasks.
#[test]
fn the_main_actions_declare_the_kind_of_step_they_take() {
    use tilo_actions::StepKind;

    let person = ada();
    let tree = person.published().unwrap();
    let kind_of = |id: &str| {
        tree.actions
            .iter()
            .find(|action| action.id == id)
            .unwrap_or_else(|| panic!("the tracker publishes {id}"))
            .contract
            .step
    };
    assert_eq!(kind_of("create"), Some(StepKind::Create));
    assert_eq!(kind_of("rename"), Some(StepKind::Edit));
    assert_eq!(kind_of("comment"), Some(StepKind::Write));
    assert_eq!(kind_of("delete"), Some(StepKind::Delete));
}

#[test]
fn a_project_holds_the_tasks_that_work_towards_it_and_the_people_involved() {
    use crate::model::{Kind, area_goal};

    let mut person = ada();
    person
        .apply(Tracker::CREATE_AREA.with(tracker::CreateArea {
            name: nb("Launch"),
            kind: Some(Kind::Project),
        }))
        .unwrap();
    let project = person
        .state()
        .unwrap()
        .areas
        .iter()
        .next()
        .map(|(id, _)| id.clone())
        .expect("the project is there");
    let read = person.state().unwrap();
    let made = read.area(&project).expect("the project");
    assert_eq!(made.name, "Launch");
    assert_eq!(made.kind, Kind::Project);
    assert!(
        serde_json::from_value::<tracker::CreateArea>(serde_json::json!({"name": "  "})).is_err(),
        "a project with no name is refused where the arguments are read"
    );

    // Two tasks work towards it, one of them as a sub-task of the other, and
    // a third works towards nothing: the project holds the two, at any depth
    // through their sub-tasks.
    for title in ["Write the notes", "Draw the slides", "Book a holiday"] {
        person
            .apply(Tracker::CREATE.with(create(title, None, None)))
            .unwrap();
    }
    let notes = id_of(&person, "Write the notes");
    person
        .apply(Tracker::CREATE.with(create("Read them back", Some(notes.clone()), None)))
        .unwrap();
    let read_back = id_of(&person, "Read them back");
    for issue in [&notes, &read_back] {
        person
            .apply(Tracker::PLAN.with(tracker::Plan {
                issue: issue.clone(),
                area: Some(area_goal(&project, "Launch")),
                priority: None,
                due_ms: None,
                estimate: None,
            }))
            .unwrap();
    }
    let held: Vec<String> = person
        .state()
        .unwrap()
        .in_area(&project)
        .map(|(_, issue)| issue.title.clone())
        .collect();
    assert_eq!(held, ["Write the notes", "Read them back"]);

    // The task reads in the project and in the tracker's list alike: the
    // same issue, named as working towards the project.
    let listed = person.state().unwrap().issues.get(&notes).cloned();
    let listed = listed.expect("the task is in the tracker's list");
    let listed = listed.area.as_ref().map(|area| area.label.as_str());
    assert_eq!(listed, Some("Launch"));
}

/// Who is involved in a project is the project's own to keep, and a document
/// it holds is a link one of its tasks carries.
#[test]
fn a_projects_people_carry_their_role_and_its_documents_are_its_tasks_links() {
    use crate::model::area_goal;

    let mut person = ada();
    person
        .apply(Tracker::CREATE_AREA.with(tracker::CreateArea {
            name: nb("Launch"),
            kind: None,
        }))
        .unwrap();
    let project = person
        .state()
        .unwrap()
        .areas
        .iter()
        .map(|(id, _)| id.clone())
        .next()
        .expect("the project");
    person
        .apply(Tracker::CREATE.with(create("Write the notes", None, None)))
        .unwrap();
    let notes = id_of(&person, "Write the notes");
    person
        .apply(Tracker::PLAN.with(tracker::Plan {
            issue: notes.clone(),
            area: Some(area_goal(&project, "Launch")),
            priority: None,
            due_ms: None,
            estimate: None,
        }))
        .unwrap();

    // A document it holds is a link the task carries, so it is read on the
    // project's page without being stored a second time.
    person
        .apply(Tracker::LINK.with(tracker::Link {
            issue: notes,
            app: "so.tilo.notes".parse().unwrap(),
            kind: "notes.note".into(),
            resource: "notes/launch".parse().unwrap(),
            label: "The launch plan".into(),
            record: None,
        }))
        .unwrap();

    // Who is involved is the project's own to keep, with the role each has.
    for (name, role) in [("ada", "writes the notes"), ("grace", "draws")] {
        person
            .apply(Tracker::INVOLVE.with(tracker::Involve {
                area: project.clone(),
                name: nb(name),
                role: Some(role.into()),
            }))
            .unwrap();
    }
    let read = person.state().unwrap();
    let people: Vec<(String, String)> = read
        .area(&project)
        .expect("the project")
        .people
        .values()
        .map(|person| (person.name.clone(), person.role.clone()))
        .collect();
    assert_eq!(
        people,
        [
            ("ada".to_owned(), "writes the notes".to_owned()),
            ("grace".to_owned(), "draws".to_owned())
        ]
    );

    // Taking someone out of the project leaves the project's other people.
    let grace = read
        .area(&project)
        .expect("the project")
        .people
        .iter()
        .find(|(_, person)| person.name == "grace")
        .map(|(id, _)| id.clone())
        .expect("grace is involved");
    person
        .apply(Tracker::REMOVE_PERSON.with(tracker::RemovePerson {
            area: project.clone(),
            person: grace,
        }))
        .unwrap();
    assert_eq!(
        person.state().unwrap().area(&project).unwrap().people.len(),
        1
    );
    assert!(
        person
            .apply(Tracker::REMOVE_PERSON.with(tracker::RemovePerson {
                area: Id::from_raw("gone"),
                person: Id::from_raw("gone"),
            }))
            .is_err(),
        "a project that is gone takes nobody out"
    );
}

/// A project and an area differ only in what they say about themselves: the
/// kind is what a person reads, and a project is one by default.
#[test]
fn an_area_is_a_project_that_goes_on() {
    use crate::model::Kind;

    let mut person = ada();
    person
        .apply(Tracker::CREATE_AREA.with(tracker::CreateArea {
            name: nb("Support"),
            kind: Some(Kind::Area),
        }))
        .unwrap();
    person
        .apply(Tracker::CREATE_AREA.with(tracker::CreateArea {
            name: nb("Launch"),
            kind: None,
        }))
        .unwrap();
    let read = person.state().unwrap();
    let kinds: Vec<Kind> = read.areas.values().map(|area| area.kind).collect();
    assert_eq!(kinds, [Kind::Area, Kind::Project]);

    let support = read
        .areas
        .iter()
        .find(|(_, area)| area.name == "Support")
        .map(|(id, _)| id.clone())
        .expect("the area");
    person
        .apply(Tracker::RENAME_AREA.with(tracker::RenameArea {
            area: support.clone(),
            name: nb("Customer support"),
        }))
        .unwrap();
    let read = person.state().unwrap();
    assert_eq!(read.area(&support).unwrap().name, "Customer support");
    assert_eq!(
        read.area(&support).unwrap().kind,
        Kind::Area,
        "the kind stays"
    );
    assert!(
        serde_json::from_value::<tracker::RenameArea>(
            serde_json::json!({"area": support.to_string(), "name": " "})
        )
        .is_err(),
        "a project keeps a name"
    );
}

/// The project and who is involved in it are the tracker's document, as the
/// tasks are: they reach the person's other device and survive a restart.
#[test]
fn a_projects_tasks_and_people_reach_the_other_device_and_a_restart() {
    use crate::model::{Kind, area_goal};

    let mut on_laptop = ada();
    on_laptop
        .apply(Tracker::CREATE_AREA.with(tracker::CreateArea {
            name: nb("Launch"),
            kind: Some(Kind::Project),
        }))
        .unwrap();
    let project = on_laptop
        .state()
        .unwrap()
        .areas
        .iter()
        .map(|(id, _)| id.clone())
        .next()
        .expect("the project");
    on_laptop
        .apply(Tracker::INVOLVE.with(tracker::Involve {
            area: project.clone(),
            name: nb("ada"),
            role: Some("writes".into()),
        }))
        .unwrap();
    on_laptop
        .apply(Tracker::CREATE.with(create("Write the notes", None, None)))
        .unwrap();
    let notes = id_of(&on_laptop, "Write the notes");
    on_laptop
        .apply(Tracker::PLAN.with(tracker::Plan {
            issue: notes,
            area: Some(area_goal(&project, "Launch")),
            priority: None,
            due_ms: None,
            estimate: None,
        }))
        .unwrap();

    let mut on_laptop = on_laptop.as_invoker("ada");
    let mut on_desktop = on_laptop.fork().as_invoker("ada");
    on_desktop.sync_with(&mut on_laptop).unwrap();
    for merged in [on_laptop.state().unwrap(), on_desktop.state().unwrap()] {
        let area = merged.area(&project).expect("the project on both");
        assert_eq!(area.name, "Launch");
        assert_eq!(area.people.len(), 1);
        assert_eq!(merged.in_area(&project).count(), 1);
    }

    let reopened = Replica::<Tracker>::load(&on_desktop.save()).unwrap();
    let after = reopened.state().unwrap();
    let area = after.area(&project).expect("the project after a restart");
    assert_eq!(area.name, "Launch");
    assert_eq!(area.kind, Kind::Project);
    assert_eq!(
        area.people
            .values()
            .next()
            .map(|person| person.name.as_str()),
        Some("ada")
    );
    assert_eq!(after.in_area(&project).count(), 1);
}

/// The project and the people in it are published as anything of the record
/// is: the tracker offers a place to make one, and each project is a resource
/// of its own with the actions a person takes on it.
#[test]
fn the_project_is_published_with_its_verbs_and_a_resource_of_its_own() {
    use crate::model::{Kind, area_resource};

    let mut person = ada();
    person
        .apply(Tracker::CREATE_AREA.with(tracker::CreateArea {
            name: nb("Launch"),
            kind: Some(Kind::Project),
        }))
        .unwrap();
    let project = person
        .state()
        .unwrap()
        .areas
        .iter()
        .map(|(id, _)| id.clone())
        .next()
        .expect("the project");

    let tree = person.published().unwrap();
    let verbs: Vec<&str> = tree
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    for verb in ["create-area", "rename-area", "involve", "remove-person"] {
        assert!(verbs.contains(&verb), "{verb} is published; got {verbs:?}");
    }
    let children: Vec<&str> = tree
        .children
        .iter()
        .map(|child| child.id.as_str())
        .collect();
    let address = area_resource(&project);
    assert!(
        children.contains(&address.as_str()),
        "the project is a resource of its own: {children:?}"
    );
}

#[test]
fn an_issue_keeps_the_page_it_was_written_from() {
    let mut replica = ada();
    replica
        .apply(Tracker::CREATE.with(NewIssue {
            url: Some("  https://issues.example/ENG-1 ".to_owned()),
            ..create("Close the books", None, None)
        }))
        .unwrap();
    let id = id_of(&replica, "Close the books");
    let state = replica.state().unwrap();
    assert_eq!(
        state.issues.get(&id).unwrap().url.as_deref(),
        Some("https://issues.example/ENG-1")
    );
}

/// The issue's body as a reader of the record gets it: the markdown in the
/// record's JSON, the same an agent's `read` answers.
fn body_read(replica: &Replica<Tracker>, issue: &Id) -> String {
    let json = replica.state().unwrap().read_document(None).unwrap();
    json["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == issue.as_str())
        .map(|entry| entry["value"]["body"].as_str().unwrap().to_owned())
        .unwrap()
}

#[test]
fn a_body_reads_back_as_the_markdown_it_was_written_as() {
    let markdown = "# Plan\n\nLine one\n\n**bold** and a [link](https://example.com) with `code`\n\n- one\n- two\n\n```\nfn main() {}\n```";
    let mut replica = ada();
    replica
        .apply(Tracker::CREATE.with(NewIssue {
            body: Some(markdown.to_owned()),
            ..create("Rich", None, None)
        }))
        .unwrap();
    let issue = id_of(&replica, "Rich");
    let read = body_read(&replica, &issue);
    for kept in [
        "# Plan",
        "**bold**",
        "[link](https://example.com)",
        "`code`",
        "- one",
        "fn main() {}",
    ] {
        assert!(read.contains(kept), "{kept:?} is lost from {read:?}");
    }
    // What is read writes back the same text.
    replica
        .apply(Tracker::CREATE.with(NewIssue {
            body: Some(read.clone()),
            ..create("Again", None, None)
        }))
        .unwrap();
    let again = id_of(&replica, "Again");
    assert_eq!(body_read(&replica, &again), read);

    // Replacing the body through the field edit an agent uses keeps it too.
    let edit = serde_json::json!({
        "path": [{"field": "issues"}, {"item": issue.as_str()}, {"field": "body"}],
        "op": "replace",
        "text": "Now *this* and [that](https://example.org/x)",
    });
    let edit = tilo_resource::Edit::<Tracker>::from_json(edit).unwrap();
    replica
        .apply(tilo_resource::Invoke::edit(&edit).unwrap())
        .unwrap();
    assert_eq!(
        body_read(&replica, &issue),
        "Now *this* and [that](https://example.org/x)"
    );
}

#[test]
fn every_tracker_action_publishes_what_it_reads() {
    let mut replica = ada();
    replica
        .apply(Tracker::CREATE.with(create("One", None, None)))
        .unwrap();
    let unstated =
        tilo_resource::schema_check::unstated_arguments::<Tracker>(&replica.state().unwrap());
    assert!(unstated.is_empty(), "{unstated:#?}");
}

fn new_project(replica: &mut Replica<Tracker>, name: &str) -> Id {
    replica
        .apply(Tracker::CREATE_AREA.with(tracker::CreateArea {
            name: nb(name),
            kind: None,
        }))
        .unwrap();
    replica
        .state()
        .unwrap()
        .areas
        .iter()
        .find(|(_, area)| area.name == name)
        .map(|(id, _)| id.clone())
        .unwrap()
}

fn project_of(replica: &Replica<Tracker>, issue: &Id) -> Option<String> {
    replica
        .state()
        .unwrap()
        .issues
        .get(issue)
        .unwrap()
        .area
        .as_ref()
        .map(|area| area.resource.clone())
}

#[test]
fn an_issue_is_created_in_a_project_moves_between_projects_and_outlives_one() {
    let mut replica = ada();
    let launch = new_project(&mut replica, "Launch");
    let hiring = new_project(&mut replica, "Hiring");
    replica
        .apply(Tracker::CREATE.with(NewIssue {
            project: Some(launch.clone()),
            ..create("Write the post", None, None)
        }))
        .unwrap();
    let issue = id_of(&replica, "Write the post");
    assert_eq!(
        project_of(&replica, &issue).as_deref(),
        Some(launch.as_str())
    );
    assert_eq!(replica.state().unwrap().in_area(&launch).count(), 1);
    assert!(
        replica
            .apply(Tracker::CREATE.with(NewIssue {
                project: Some(Id::from_raw("nowhere")),
                ..create("Lost", None, None)
            }))
            .is_err(),
        "a project the tracker does not have is refused"
    );

    replica
        .apply(Tracker::SET_PROJECT.with(tracker::SetProject {
            issue: issue.clone(),
            project: Some(hiring.clone()),
        }))
        .unwrap();
    assert_eq!(
        project_of(&replica, &issue).as_deref(),
        Some(hiring.as_str())
    );

    replica
        .apply_confirmed(Tracker::DELETE_AREA.with(tracker::DeleteArea {
            area: hiring.clone(),
        }))
        .unwrap();
    let state = replica.state().unwrap();
    assert!(state.area(&hiring).is_none());
    assert_eq!(
        project_of(&replica, &issue),
        None,
        "its issue stays, in no project"
    );
}

fn origin(author: &str, created_ms: i64) -> tilo_resource::Origin {
    tilo_resource::Origin {
        author: author.to_owned(),
        handle: Some("jane".to_owned()),
        source: "Jira".to_owned(),
        url: Some("https://tracker.example.com/acme/issues/12".to_owned()),
        created_ms: Some(created_ms),
        updated_ms: Some(created_ms.saturating_add(1_000)),
    }
}

#[test]
fn a_moved_issue_shows_its_original_author_and_date_and_keeps_who_moved_it() {
    let mut person = ada();
    let mut mover = person
        .fork()
        .as_agent_for("agent:grant-a8a42b74", "ada")
        .named("Company mover");
    mover.set_now_ms(1_900_000_000_000);
    let written = 1_741_000_000_000;
    mover
        .apply(Tracker::CREATE.with(NewIssue {
            origin: Some(origin("Jane Doe", written)),
            ..create("Old issue", None, None)
        }))
        .unwrap();
    let issue_id = id_of(&mover, "Old issue");
    mover
        .apply(Tracker::COMMENT.with(tracker::Comment {
            issue: issue_id.clone(),
            text: nb("An **old** remark"),
            origin: Some(origin("Sam Roe", written.saturating_add(5_000))),
        }))
        .unwrap();
    mover
        .apply(Tracker::COMMENT.with(tracker::Comment {
            issue: issue_id.clone(),
            text: nb("Moved from the old tracker"),
            origin: None,
        }))
        .unwrap();
    let state = mover.state().unwrap();
    let issue = state.issues.get(&issue_id).unwrap();
    assert_eq!(issue.shown_author(), "Jane Doe");
    assert_eq!(issue.shown_created_ms(), written);
    assert_eq!(
        issue.url.as_deref(),
        Some("https://tracker.example.com/acme/issues/12")
    );
    // The record still says who moved it, and when.
    assert_eq!(issue.author, "agent:grant-a8a42b74");
    assert_eq!(issue.author_name, "Company mover");
    assert_eq!(issue.created_ms, 1_900_000_000_000);
    let comments: Vec<_> = issue.comments.values().collect();
    assert_eq!(comments[0].shown_author(), "Sam Roe");
    assert_eq!(comments[0].shown_at_ms(), written.saturating_add(5_000));
    assert_eq!(comments[0].author, "agent:grant-a8a42b74");
    // The agent's own message is by its name, never its key.
    assert_eq!(comments[1].shown_author(), "Company mover");

    assert!(
        mover
            .apply(Tracker::CREATE.with(NewIssue {
                origin: Some(tilo_resource::Origin {
                    author: " ".into(),
                    ..origin("x", 1)
                }),
                ..create("Nobody's", None, None)
            }))
            .is_err(),
        "an origin names its author"
    );
}

#[test]
fn an_outsider_is_assigned_by_name_until_they_join() {
    let mut replica = ada();
    replica
        .apply(Tracker::CREATE.with(create("Theirs", None, Some("outside:Jane Doe"))))
        .unwrap();
    let issue = id_of(&replica, "Theirs");
    assert_eq!(
        replica
            .state()
            .unwrap()
            .issues
            .get(&issue)
            .unwrap()
            .assigned_outsider(),
        Some("Jane Doe")
    );
    // When they join, the issue is theirs by their id.
    replica
        .apply(Tracker::ASSIGN.with(tracker::Assign {
            issue: issue.clone(),
            assignee: Some("person:jane".into()),
        }))
        .unwrap();
    assert_eq!(
        replica
            .state()
            .unwrap()
            .issues
            .get(&issue)
            .unwrap()
            .assigned_person(),
        Some("jane")
    );
    assert!(
        replica
            .apply(Tracker::ASSIGN.with(tracker::Assign {
                issue,
                assignee: Some("Jane".into()),
            }))
            .is_err(),
        "an assignee says what it is"
    );
}

#[test]
fn priority_is_named_estimate_is_kept_and_files_attach() {
    use crate::model::Priority;
    let mut replica = ada();
    replica
        .apply(Tracker::CREATE.with(NewIssue {
            priority: Some(Priority::Urgent),
            estimate: Some(3),
            labels: Some(vec!["bug".into(), " ".into()]),
            ..create("Sized", None, None)
        }))
        .unwrap();
    let issue = id_of(&replica, "Sized");
    let read = |replica: &Replica<Tracker>| {
        replica
            .state()
            .unwrap()
            .issues
            .get(&issue)
            .cloned()
            .unwrap()
    };
    assert_eq!(read(&replica).priority, Some(1));
    assert_eq!(read(&replica).estimate, Some(3));
    assert_eq!(read(&replica).labels.len(), 1);
    // Named in the call an agent writes, from the published schema.
    let plan = serde_json::json!({ "issue": issue.as_str(), "priority": "low", "estimate": 8 });
    replica
        .apply(Tracker::PLAN.with(serde_json::from_value::<tracker::Plan>(plan).unwrap()))
        .unwrap();
    assert_eq!(Priority::of_level(read(&replica).priority), Priority::Low);
    assert_eq!(read(&replica).estimate, Some(8));
    replica
        .apply(Tracker::PLAN.with(tracker::Plan {
            issue: issue.clone(),
            area: None,
            priority: Some(Priority::None),
            estimate: None,
            due_ms: None,
        }))
        .unwrap();
    assert_eq!(
        read(&replica).priority,
        None,
        "`none` takes the priority off"
    );
    assert!(
        serde_json::from_value::<tracker::Plan>(
            serde_json::json!({ "issue": issue.as_str(), "priority": 2 })
        )
        .is_err(),
        "a priority is a name"
    );

    replica
        .apply(Tracker::ATTACH.with(tracker::Attach {
            issue: issue.clone(),
            name: nb("trace.log"),
            bytes: tilo_resource::Bytes::to_keep(b"line one\n".to_vec()),
            media_type: Some("text/plain".into()),
        }))
        .unwrap();
    let attached = read(&replica);
    let (file_id, file) = attached.attachments.iter().next().unwrap();
    assert_eq!(file.name, "trace.log");
    assert_eq!(file.author, "ada");
    assert_ne!(file.bytes, tilo_resource::Bytes::default());
    replica
        .apply(Tracker::DETACH.with(tracker::Detach {
            issue: issue.clone(),
            attachment: file_id.clone(),
        }))
        .unwrap();
    assert!(read(&replica).attachments.is_empty());
}
