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
use crate::planner::Answered;
use std::time::Duration;
use tilo_agent::tools::ItemPlan;

fn granted() -> Vec<(String, String)> {
    [
        ("so.tilo.chat", "Chat"),
        ("so.tilo.sheets", "Sheets"),
        ("so.tilo.word-processor", "Word Processor"),
        ("so.tilo.messages", "Messages"),
        ("so.tilo.pdf-viewer", "PDF Viewer"),
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
        assert_eq!(coordinator.next(Instant::now()), Next::Item(at));
        coordinator.start(at);
        let role = coordinator.role();
        assert_eq!((role.role, role.item), ("item", Some(at + 1)));
        assert_eq!(role.task, format!("t1/item-{}", at + 1));
        done(&mut coordinator, None);
    }
    assert_eq!(coordinator.next(Instant::now()), Next::Check);
    coordinator.checked = true;
    assert_eq!(coordinator.next(Instant::now()), Next::Finish);
    let outcome = coordinator.outcome().unwrap();
    assert!(
        outcome.ends_with("Items done: 1. post Alpha; 2. log Alpha; 3. post Bravo."),
        "{outcome}"
    );
}

/// An item's agent is told the request, its own item, the checklist with
/// where each item is, what the items that ended did and what they read,
/// that it reads and opens what it needs itself, and that "cannot" is for
/// what cannot be done after trying; nothing of the conversations before it.
#[test]
fn an_item_s_agent_is_told_its_item_the_checklist_and_what_came_before() {
    let mut coordinator = coordinator(&[
        ("read the code", &["chat"], &[]),
        ("log the code", &["sheets"], &[]),
        ("post Bravo", &["chat"], &[]),
    ]);
    coordinator.start(0);
    coordinator.reply = Some("Read it.".into());
    let read = Running::new(
        "t1".into(),
        "read the code".into(),
        Vec::new(),
        vec![(
            "so.tilo.chat".into(),
            Answered {
                seen: "Rae: the LR-2041 code is 4321".into(),
                untrusted: false,
            },
        )],
    );
    coordinator.item_ended(&Ok("Done: 0 actions.".into()), Some(read));
    coordinator.start(1);
    let note = coordinator.item_note(1);
    assert!(note.starts_with(ROLE_INTRO), "{note}");
    assert!(
        note.contains("you are the agent of item 2 only: log the code"),
        "{note}"
    );
    assert!(
        note.contains("The original request, for reference: Post Alpha, log it, then post Bravo"),
        "{note}"
    );
    assert!(
        note.contains("1. read the code (done); 2. log the code (yours); 3. post Bravo (to do)"),
        "{note}"
    );
    assert!(
        note.contains("- 1. read the code: done. It said: Read it.")
            && note.contains("What it read and its steps answered: Rae: the LR-2041 code is 4321"),
        "what item 1 read is handed on: {note}"
    );
    assert!(
        note.contains("read the records it needs") && note.contains("open a record"),
        "{note}"
    );
    assert!(
        note.contains("Reply \"cannot: <why>\" only when, after trying"),
        "{note}"
    );
}

/// What the items before one found is handed on within a bound: the
/// oldest findings are shortened first, and every item's line stays.
#[test]
fn what_earlier_items_found_is_bounded_oldest_first() {
    let mut coordinator = coordinator(&[
        ("read A", &["chat"], &[]),
        ("read B", &["chat"], &[]),
        ("read C", &["chat"], &[]),
        ("read D", &["chat"], &[]),
        ("use them", &["sheets"], &[]),
    ]);
    for (at, word) in ["alpha", "bravo", "charlie", "delta"]
        .into_iter()
        .enumerate()
    {
        coordinator.start(at);
        let seen = format!("{word} ").repeat(1_000);
        let read = Running::new(
            "t1".into(),
            "read".into(),
            Vec::new(),
            vec![(
                "so.tilo.chat".into(),
                Answered {
                    seen: format!("{seen}{word}-end"),
                    untrusted: false,
                },
            )],
        );
        coordinator.item_ended(&Ok("Done: 0 actions.".into()), Some(read));
    }
    let results = coordinator.results(true);
    assert!(
        results.chars().count() <= 8_100,
        "{}",
        results.chars().count()
    );
    for word in ["A", "B", "C", "D"] {
        assert!(
            results.contains(&format!(". read {word}: done.")),
            "{results}"
        );
    }
    assert!(
        results.contains("delta-end"),
        "the newest finding is kept whole"
    );
    assert!(
        !results.contains("alpha-end"),
        "the oldest is shortened first"
    );
}

/// An item's agent that says it cannot do its item is asked again only
/// when it said so before trying a step or a read, and once.
#[test]
fn a_cannot_is_read_from_the_reply() {
    assert_eq!(
        cannot_why("cannot: no PDF here").as_deref(),
        Some("no PDF here")
    );
    assert_eq!(
        cannot_why("Cannot post twice").as_deref(),
        Some("post twice")
    );
    assert_eq!(cannot_why("Posted; cannot wait"), None);
}

/// An item whose agent says it cannot be done is not done, and the items
/// that need it are not failed unrun: it goes to the coordinator, the
/// items that do not need it run meanwhile, and once the coordinator went
/// on without it the items that need it run too. The task's result says
/// it could not be done.
#[test]
fn an_item_that_cannot_be_done_goes_to_the_coordinator() {
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
    assert_eq!(coordinator.failures, [0]);
    assert_eq!(
        coordinator.next(Instant::now()),
        Next::Item(2),
        "item 2 needs item 1"
    );
    assert_eq!(
        coordinator.items[1].state,
        State::Pending,
        "not failed unrun"
    );
    coordinator.start(2);
    done(&mut coordinator, None);
    let asked = coordinator.ask_failures();
    assert_eq!(
        asked,
        "Item 1 (enter the code) could not be done: no code arrived.\n"
    );
    // The coordinator went on without it.
    coordinator.waive_asked();
    assert_eq!(coordinator.next(Instant::now()), Next::Item(1));
    assert!(
        coordinator
            .checklist(None)
            .contains("1. enter the code (not done, the items after it go on: no code arrived)")
    );
    coordinator.start(1);
    done(&mut coordinator, None);
    coordinator.checked = true;
    assert_eq!(coordinator.next(Instant::now()), Next::Finish);
    let failed = coordinator.outcome().unwrap_err();
    assert!(
        failed.starts_with("Item 1 (enter the code) could not be done: no code arrived."),
        "{failed}"
    );
    assert!(
        failed.contains("Items done: 2. log the sign-in; 3. post ready."),
        "{failed}"
    );
}

/// The coordinator's note on an item that could not be done has a fresh
/// agent do it again, once; the items that need it wait for the redo.
#[test]
fn a_failed_item_is_done_again_with_the_coordinator_s_note() {
    let mut coordinator = coordinator(&[
        ("read the PDF", &["pdf"], &[]),
        ("log its total", &["sheets"], &[1]),
    ]);
    assert_eq!(coordinator.items[0].apps, ["so.tilo.pdf-viewer"]);
    coordinator.start(0);
    done(&mut coordinator, Some("cannot: the PDF is not open"));
    coordinator.ask_failures();
    coordinator.adjust(
        Obligations {
            adjust: vec![(1, "open HM-48213 documents.pdf first".into())],
            ..Obligations::default()
        },
        &granted(),
        false,
    );
    coordinator.waive_asked();
    assert_eq!(coordinator.items[0].state, State::Pending);
    assert!(coordinator.items[0].redone && !coordinator.items[0].waived);
    assert_eq!(
        coordinator.next(Instant::now()),
        Next::Item(0),
        "the redo, before what needs it"
    );
    coordinator.start(0);
    assert_eq!(coordinator.role().task, "t1/item-1-redo");
    assert!(
        coordinator
            .item_note(0)
            .contains("Do this item again: open HM-48213 documents.pdf first.")
    );
    done(&mut coordinator, None);
    assert_eq!(coordinator.next(Instant::now()), Next::Item(1));
}

/// An item that names a later item it follows waits for it; items that
/// wait on each other run in the order listed rather than never.
#[test]
fn after_holds_an_item_back_and_a_cycle_runs_in_order() {
    let mut coordinator = coordinator(&[
        ("post the summary", &["chat"], &[2]),
        ("write the summary", &["word processor"], &[]),
    ]);
    assert_eq!(coordinator.next(Instant::now()), Next::Item(1));
    coordinator.start(1);
    done(&mut coordinator, None);
    assert_eq!(coordinator.next(Instant::now()), Next::Item(0));

    let mut cycle = self::coordinator(&[("a", &[], &[2]), ("b", &[], &[1])]);
    assert_eq!(cycle.next(Instant::now()), Next::Item(0));
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
    assert_eq!(
        coordinator.next(Instant::now()),
        Next::Item(0),
        "the redo comes first"
    );
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

    let texts: Vec<String> = (1..=12)
        .map(|number| format!("post Echo {number}"))
        .collect();
    let echoes: Vec<(&str, &[&str], &[usize])> = texts
        .iter()
        .map(|text| (text.as_str(), &["chat"][..], &[][..]))
        .collect();
    coordinator.adjust(listed(&echoes), &granted(), true);
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
/// renumbered and need it; a message others sent that it waits for before
/// it began ends it as it begins, and the items after it are told what
/// arrived.
#[test]
fn a_plan_s_own_wait_becomes_an_item_and_an_arrival_is_kept_for_it() {
    let wait = Wait {
        apps: vec!["so.tilo.messages".into()],
        contains: Some("code".into()),
        within: Duration::from_secs(60),
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
    assert_eq!(waits.after, [1]);
    assert_eq!(coordinator.items[2].after, [1, 2]);
    assert_eq!(coordinator.items[3].after, [3, 2], "renumbered");

    let unrelated = arrival("so.tilo.messages", "Your parcel is on its way");
    let code = arrival("so.tilo.messages", "Your sign-in code is 4321");
    assert!(!coordinator.awaited(&unrelated));
    assert!(coordinator.awaited(&code));
    assert!(!coordinator.awaited(&arrival("so.tilo.chat", "the code is 4321")));
    coordinator.remember(vec![unrelated, code.clone()]);
    coordinator.start(0);
    done(&mut coordinator, None);
    assert_eq!(
        coordinator.next(Instant::now()),
        Next::Item(2),
        "the wait ended as it began"
    );
    assert_eq!(coordinator.items[1].state, State::Done);
    assert!(coordinator.awaited(&code), "what ended a wait is not news");
    let told = coordinator.item_note(2);
    assert!(
        told.contains(
            "- 2. wait for \"code\": arrived: in messages from Bank: Your sign-in code is 4321"
        ),
        "{told}"
    );
    assert_eq!(coordinator.arrivals.len(), 1, "taken once");
}

fn arrival(app: &str, line: &str) -> Arrival {
    Arrival {
        route: DocumentRoute {
            app_id: app.into(),
            cluster_id: "room".into(),
            record: "r".into(),
        },
        title: None,
        from: Some("Bank".into()),
        lines: vec![line.into()],
    }
}

fn waiting_for(contains: &str) -> Obligations {
    let mut declared = listed(&[
        ("wait for the code", &[], &[]),
        ("enter the code", &["chat"], &[1]),
        ("log the request", &["sheets"], &[]),
    ]);
    declared.plans[0].wait = Some(tilo_agent::tools::Wait {
        app: Some("messages".into()),
        contains: Some(contains.into()),
        seconds: 60,
    });
    declared
}

/// A wait blocks only the items that need it: the others run while it is
/// held, and once only it and what needs it are left the task holds until
/// a message answers it, which lets the items that need it run.
#[test]
fn a_held_wait_blocks_only_the_items_that_need_it() {
    let mut coordinator = Coordinator::new("t".into(), "r".into());
    coordinator.adopt_list(waiting_for("PY-7731 code"), &granted(), &[]);
    let now = Instant::now();
    assert_eq!(
        coordinator.next(now),
        Next::Item(2),
        "the log runs meanwhile"
    );
    assert_eq!(coordinator.items[0].state, State::Waiting);
    assert_eq!(
        coordinator.held_until(),
        Some(now + Duration::from_secs(60))
    );
    assert_eq!(
        coordinator.held_activity().as_deref(),
        Some("Waiting for a message in messages containing \"PY-7731 code\"")
    );
    coordinator.start(2);
    done(&mut coordinator, None);
    assert_eq!(coordinator.next(now), Next::Hold);
    assert!(
        coordinator
            .checklist(None)
            .starts_with("1. wait for the code (waiting)")
    );

    coordinator.remember(vec![arrival("so.tilo.messages", "Parcel on its way")]);
    assert!(!coordinator.take_arrivals());
    coordinator.remember(vec![arrival(
        "so.tilo.messages",
        "Confirmation code for new payee PY-7731: 617284",
    )]);
    assert!(coordinator.take_arrivals());
    assert_eq!(coordinator.items[0].state, State::Done);
    assert_eq!(coordinator.next(now), Next::Item(1));
}

/// A held wait that runs out waits again while the task may; after the
/// last it could not be done, and goes to the coordinator like an item.
#[test]
fn a_held_wait_that_runs_out_waits_again_then_goes_to_the_coordinator() {
    let mut coordinator = Coordinator::new("t".into(), "r".into());
    coordinator.adopt_list(waiting_for("code"), &granted(), &[]);
    let mut now = Instant::now();
    coordinator.next(now);
    for _ in 1..MOST_WAITS {
        now += Duration::from_secs(61);
        assert!(!coordinator.run_out(now));
        assert_eq!(coordinator.items[0].state, State::Waiting);
    }
    now += Duration::from_secs(61);
    assert!(coordinator.run_out(now));
    assert!(matches!(coordinator.items[0].state, State::Failed(_)));
    assert_eq!(coordinator.failures, [0]);
    assert_eq!(
        coordinator.items[1].state,
        State::Pending,
        "not failed unrun"
    );
}

/// A coordinator's turn that lists items already listed, in other words
/// or again as they were, adds none of them; a genuinely new item is added,
/// and one that repeats an item not done has that item done again.
#[test]
fn items_listed_again_are_not_added_twice() {
    let mut coordinator = coordinator(&[
        (
            "Read Lena’s WhatsApp; capture claim and ID email details",
            &["chat"],
            &[],
        ),
        (
            "Check case, documents, rules; calculate approval and pay-by date",
            &["sheets"],
            &[1],
        ),
        ("Enter LR-2041 code", &["sheets"], &[]),
        ("Post PY-7731 confirmed in claims-ops", &["chat"], &[]),
    ]);
    coordinator.start(0);
    done(&mut coordinator, None);
    coordinator.start(3);
    done(&mut coordinator, Some("cannot: no channel"));
    let again = listed(&[
        (
            "Read WhatsApp conversation and capture ID email details",
            &["chat"],
            &[],
        ),
        (
            "Check case, documents, rules; calculate approval and pay-by",
            &["sheets"],
            &[],
        ),
        ("Enter LR-2042 code", &["sheets"], &[]),
        ("Post PY-7731 confirmed in claims-ops", &["chat"], &[]),
        ("Draft the claim email to Lena", &["mail"], &[]),
    ]);
    coordinator.adjust(again, &granted(), false);
    let texts: Vec<&str> = coordinator
        .items
        .iter()
        .map(|item| item.text.as_str())
        .collect();
    assert_eq!(
        texts,
        [
            "Read Lena’s WhatsApp; capture claim and ID email details",
            "Check case, documents, rules; calculate approval and pay-by date",
            "Enter LR-2041 code",
            "Post PY-7731 confirmed in claims-ops",
            "Enter LR-2042 code",
            "Draft the claim email to Lena",
        ]
    );
    assert_eq!(coordinator.items[3].state, State::Pending, "done again");
    assert!(coordinator.items[3].redone);
}

/// An item repeats one listed when their words but joining ones match,
/// each covering most of the other, and it has no number the listed one
/// lacks.
#[test]
fn which_items_are_one() {
    assert!(same_item("post Alpha ready", "Post alpha ready."));
    assert!(same_item(
        "Wait for renewed ID card email if not yet received",
        "Wait for renewed ID card email if not received"
    ));
    assert!(same_item(
        "Enter 617284 and confirm PY-7731",
        "Enter code and confirm PY-7731 status"
    ));
    assert!(
        !same_item("set the frame size", "set the frame size to 1920x1080"),
        "a fix with the value to set is new"
    );
    assert!(!same_item("post Alpha ready", "post Bravo ready"));
    assert!(!same_item("Enter LR-2041 code", "Enter LR-2042 code"));
    assert!(!same_item(
        "post Alpha ready",
        "post Alpha ready in Chat and log it in Sheets"
    ));
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
