//! The coordinator of a coordinated task, on its own: the items it takes
//! up, the order it runs them in, what each item's agent is told, and the
//! bounds it keeps.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use super::*;
use tilo_agent::tools::ItemPlan;

fn granted() -> Vec<(String, String)> {
    [
        ("so.tilo.chat", "Chat"),
        ("so.tilo.sheets", "Sheets"),
        ("so.tilo.word-processor", "Word Processor"),
        ("so.tilo.messages", "Messages"),
    ]
    .into_iter()
    .map(|(id, title)| (id.to_owned(), title.to_owned()))
    .collect()
}

fn listed(items: &[(&str, &[&str], &[usize])]) -> Obligations {
    Obligations {
        items: items
            .iter()
            .map(|(text, _, _)| (*text).to_owned())
            .collect(),
        plans: items
            .iter()
            .map(|(_, apps, after)| ItemPlan {
                apps: apps.iter().map(|app| (*app).to_owned()).collect(),
                after: after.to_vec(),
                wait: None,
            })
            .collect(),
        ..Obligations::default()
    }
}

fn coordinator(items: &[(&str, &[&str], &[usize])]) -> Coordinator {
    let mut coordinator =
        Coordinator::new("t1".into(), "Post Alpha, log it, then post Bravo".into());
    assert_eq!(
        coordinator.adopt_list(listed(items), &granted(), &[]),
        items.len()
    );
    coordinator
}

fn done(coordinator: &mut Coordinator, reply: Option<&str>) {
    coordinator.reply = reply.map(str::to_owned);
    coordinator.item_ended(&Ok("Done: 1 action.".into()), None);
}

/// The items listed are taken up with their apps resolved however the
/// model wrote them, run in the order listed, and the coordinator checks
/// once before the task ends; an item's agent's plans are logged as its
/// own, under the task id with its suffix.
#[test]
fn items_run_in_order_with_their_apps_then_one_check() {
    let mut coordinator = coordinator(&[
        ("post Alpha", &["chat"], &[]),
        ("log Alpha", &["Sheets", "Word Processor", "nowhere"], &[1]),
        ("post Bravo", &["so.tilo.chat"], &[]),
    ]);
    assert_eq!(coordinator.items[0].apps, ["so.tilo.chat"]);
    assert_eq!(
        coordinator.items[1].apps,
        ["so.tilo.sheets", "so.tilo.word-processor"]
    );
    assert_eq!(coordinator.turn, None, "the list is taken");
    assert_eq!(coordinator.role().role, "coordinator");
    for at in 0..3 {
        assert_eq!(coordinator.next(), Next::Item(at));
        coordinator.start(at);
        let role = coordinator.role();
        assert_eq!((role.role, role.item), ("item", Some(at + 1)));
        assert_eq!(role.task, format!("t1/item-{}", at + 1));
        done(&mut coordinator, None);
    }
    assert_eq!(coordinator.next(), Next::Check);
    coordinator.checked = true;
    assert_eq!(coordinator.next(), Next::Finish);
    let outcome = coordinator.outcome().unwrap();
    assert!(
        outcome.ends_with("Items done: 1. post Alpha; 2. log Alpha; 3. post Bravo."),
        "{outcome}"
    );
}

/// An item's agent is told the request, its own item, the checklist with
/// where each item is and what the items that ended did, and how to end;
/// nothing of the conversations before it.
#[test]
fn an_item_s_agent_is_told_its_item_the_checklist_and_what_came_before() {
    let mut coordinator = coordinator(&[
        ("post Alpha", &["chat"], &[]),
        ("log Alpha", &["sheets"], &[]),
        ("post Bravo", &["chat"], &[]),
    ]);
    coordinator.start(0);
    done(&mut coordinator, Some("Posted."));
    coordinator.start(1);
    let note = coordinator.item_note(1);
    assert!(note.starts_with(ROLE_INTRO), "{note}");
    assert!(
        note.contains("you are the agent of item 2 only: log Alpha"),
        "{note}"
    );
    assert!(
        note.contains("The original request, for reference: Post Alpha, log it, then post Bravo"),
        "{note}"
    );
    assert!(
        note.contains("1. post Alpha (done); 2. log Alpha (yours); 3. post Bravo (to do)"),
        "{note}"
    );
    assert!(
        note.contains("- 1. post Alpha: done. It said: Posted."),
        "{note}"
    );
    assert!(note.contains("cannot: <why>"), "{note}");
}

/// An item whose agent says it cannot be done is not done; an item that
/// needs it is left undone without an agent, the others still run, and the
/// task's result says which and why.
#[test]
fn an_item_that_cannot_be_done_leaves_what_needs_it_undone() {
    let mut coordinator = coordinator(&[
        ("enter the code", &["chat"], &[]),
        ("log the sign-in", &["sheets"], &[1]),
        ("post ready", &["chat"], &[]),
    ]);
    coordinator.start(0);
    done(&mut coordinator, Some("cannot: no code arrived"));
    assert_eq!(
        coordinator.items[0].state,
        State::Failed("no code arrived".into())
    );
    assert_eq!(coordinator.next(), Next::Item(2), "item 2 needs item 1");
    assert_eq!(
        coordinator.items[1].state,
        State::Failed("item 1, which it needs, could not be done".into())
    );
    coordinator.start(2);
    done(&mut coordinator, None);
    coordinator.checked = true;
    assert_eq!(coordinator.next(), Next::Finish);
    let failed = coordinator.outcome().unwrap_err();
    assert!(
        failed.starts_with("Item 1 (enter the code) could not be done: no code arrived."),
        "{failed}"
    );
    assert!(failed.contains("Items done: 3. post ready."), "{failed}");
    assert!(
        failed.contains("Left undone: 1. enter the code (no code arrived); 2. log the sign-in"),
        "{failed}"
    );
}

/// An item that names a later item it follows waits for it; items that
/// wait on each other run in the order listed rather than never.
#[test]
fn after_holds_an_item_back_and_a_cycle_runs_in_order() {
    let mut coordinator = coordinator(&[
        ("post the summary", &["chat"], &[2]),
        ("write the summary", &["word processor"], &[]),
    ]);
    assert_eq!(coordinator.next(), Next::Item(1));
    coordinator.start(1);
    done(&mut coordinator, None);
    assert_eq!(coordinator.next(), Next::Item(0));

    let mut cycle = self::coordinator(&[("a", &[], &[2]), ("b", &[], &[1])]);
    assert_eq!(cycle.next(), Next::Item(0));
}

/// Notes on items: the running item's is a hint, a pending item starts with
/// its note, a done item is done again once with its note and what it did,
/// and a second note on it is not taken. Items added stop at the bound;
/// a pending item may be set aside; the check's items are fixes, once.
#[test]
fn notes_redo_once_items_are_bounded_and_the_check_fixes_once() {
    let mut coordinator = coordinator(&[
        ("post Alpha", &["chat"], &[]),
        ("post Bravo", &["chat"], &[]),
        ("post Charlie", &["chat"], &[]),
        ("post Delta", &["chat"], &[]),
    ]);
    coordinator.start(0);
    done(&mut coordinator, Some("Posted Alpha."));
    coordinator.start(1);
    let hint = coordinator.adjust(
        Obligations {
            adjust: vec![
                (1, "post Alfa instead".into()),
                (2, "say Bravo twice".into()),
                (3, "Charlie is now Carla".into()),
                (9, "no such item".into()),
            ],
            set_aside: vec![(4, "cancelled".into()), (2, "running, not pending".into())],
            ..Obligations::default()
        },
        &granted(),
        false,
    );
    assert_eq!(hint.as_deref(), Some("say Bravo twice"));
    assert_eq!(coordinator.items[0].state, State::Pending);
    assert!(coordinator.items[0].redone);
    assert_eq!(
        coordinator.items[0].note.as_deref(),
        Some(
            "Do this item again: post Alfa instead. What it did before: done. It said: Posted Alpha."
        )
    );
    assert_eq!(
        coordinator.items[2].note.as_deref(),
        Some("Charlie is now Carla")
    );
    assert_eq!(
        coordinator.items[3].state,
        State::SetAside("cancelled".into())
    );
    done(&mut coordinator, None);
    assert_eq!(coordinator.next(), Next::Item(0), "the redo comes first");
    coordinator.start(0);
    assert_eq!(coordinator.role().task, "t1/item-1-redo");
    done(&mut coordinator, None);
    coordinator.adjust(
        Obligations {
            adjust: vec![(1, "again".into())],
            ..Obligations::default()
        },
        &granted(),
        false,
    );
    assert_eq!(
        coordinator.items[0].state,
        State::Done,
        "redone at most once"
    );

    let echo: (&str, &[&str], &[usize]) = ("post Echo", &["chat"], &[]);
    let added = listed(&[echo; 12]);
    coordinator.adjust(added, &granted(), true);
    assert_eq!(coordinator.items.len(), MOST_OBLIGATIONS);
    assert!(coordinator.items[4].fixup && coordinator.fixed);
}

/// At most twelve items are taken up from a list.
#[test]
fn a_list_keeps_at_most_twelve_items() {
    let post: (&str, &[&str], &[usize]) = ("post", &["chat"], &[]);
    let many = vec![post; 13];
    let mut coordinator = Coordinator::new("t".into(), "r".into());
    let declared = listed(&many);
    assert_eq!(
        coordinator.adopt_list(declared, &granted(), &[]),
        MOST_OBLIGATIONS
    );
}

/// A wait the first plan set for itself, rather than on an item, becomes an
/// item after those its steps were for, and the items after it are
/// renumbered; a message others sent that it waits for is kept until it
/// begins, and the items after it are told what arrived.
#[test]
fn a_plan_s_own_wait_becomes_an_item_and_an_arrival_is_kept_for_it() {
    let wait = Wait {
        apps: vec!["so.tilo.messages".into()],
        contains: Some("code".into()),
        within: std::time::Duration::from_secs(60),
        rest: Some("enter it".into()),
    };
    let steps = [
        Step {
            app: "so.tilo.chat".into(),
            record: None,
            action: StepAction::Open,
            carries: None,
            serves: Some(1),
        },
        Step {
            app: String::new(),
            record: None,
            action: StepAction::Wait(wait),
            carries: None,
            serves: None,
        },
    ];
    let mut coordinator = Coordinator::new("t".into(), "r".into());
    let declared = listed(&[
        ("ask for the code", &["chat"], &[]),
        ("enter the code", &["chat"], &[1]),
        ("log it", &["sheets"], &[2]),
    ]);
    assert_eq!(coordinator.adopt_list(declared, &granted(), &steps), 4);
    let waits = &coordinator.items[1];
    assert_eq!(waits.text, "wait for \"code\"");
    assert_eq!(waits.wait.as_ref().and_then(|wait| wait.rest.clone()), None);
    assert_eq!(coordinator.items[2].after, [1]);
    assert_eq!(coordinator.items[3].after, [3], "renumbered");

    let route = |app: &str| DocumentRoute {
        app_id: app.into(),
        cluster_id: "room".into(),
        record: "r".into(),
    };
    let arrival = |app: &str, line: &str| Arrival {
        route: route(app),
        title: None,
        from: Some("Bank".into()),
        lines: vec![line.into()],
    };
    let unrelated = arrival("so.tilo.messages", "Your parcel is on its way");
    let code = arrival("so.tilo.messages", "Your sign-in code is 4321");
    assert!(!coordinator.awaited(&unrelated));
    assert!(coordinator.awaited(&code));
    assert!(!coordinator.awaited(&arrival("so.tilo.chat", "the code is 4321")));
    coordinator.remember(&[unrelated, code]);
    let wanted = coordinator.items[1].wait.clone().unwrap();
    let taken = coordinator.take_arrival(&wanted).unwrap();
    coordinator.start(1);
    coordinator.wait_ended(Some(arrived_text(&[taken])));
    assert_eq!(coordinator.items[1].state, State::Done);
    let told = coordinator.item_note(2);
    assert!(
        told.contains(
            "- 2. wait for \"code\": arrived: in messages from Bank: Your sign-in code is 4321"
        ),
        "{told}"
    );
    assert!(coordinator.take_arrival(&wanted).is_none(), "taken once");
}

/// The plan log's list of items says where each is, why, and what it did.
#[test]
fn the_items_are_logged_with_their_state_and_result() {
    let mut coordinator = coordinator(&[("post Alpha", &["chat"], &[]), ("post Bravo", &[], &[1])]);
    coordinator.start(0);
    done(&mut coordinator, Some("Posted."));
    let logged = coordinator.logged();
    assert_eq!(logged[0]["state"], "done");
    assert_eq!(logged[0]["apps"], serde_json::json!(["so.tilo.chat"]));
    assert_eq!(logged[0]["result"], "done. It said: Posted.");
    assert_eq!(logged[1]["state"], "open");
    assert_eq!(logged[1]["after"], serde_json::json!([1]));
}
