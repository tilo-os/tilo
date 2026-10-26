//! The waiting stack and the ledger: one place for a decision, one for a
//! record, and no third.

use super::{
    Action, Item, Notice, Owner, Published, State, Target, Urgency, act, clear, defer, deferred,
    dismiss, items, publish, raise, seen, take, unseen, waiting,
};
use crate::icons::Icon;

fn owner(app: &str) -> Owner {
    Owner::new(app, "Store").with_icon(Icon::Download)
}

/// An item carries its owner's name as well as its icon, what it is about,
/// why, and exactly one action.
#[test]
fn an_item_says_who_asks_what_about_and_what_to_do() {
    let _serial = crate::jobs::test_lock();
    clear();
    let id = raise(
        Item::new(
            owner("so.tilo.store"),
            "Couldn’t install LibreOffice",
            Action::new("store.retry", "Try again"),
        )
        .about("LibreOffice")
        .because("480 MB needed, 210 MB free")
        .urgent(Urgency::Blocking),
    );
    let waiting = items();
    assert_eq!(waiting.len(), 1);
    let item = &waiting[0];
    assert_eq!(item.id(), id);
    assert_eq!(item.owner().name, "Store");
    assert_eq!(item.owner().icon, Icon::Download);
    assert_eq!(item.title(), "Couldn’t install LibreOffice");
    assert_eq!(
        item.detail_line(),
        "LibreOffice, 480 MB needed, 210 MB free"
    );
    assert_eq!(item.action().label, "Try again");
    assert!(item.since_ms() > 0);
}

/// The stack is ordered by what it costs to leave: blocking first, then the
/// oldest.
#[test]
fn the_stack_is_ordered_by_cost_then_age() {
    let _serial = crate::jobs::test_lock();
    clear();
    let later = raise(Item::new(
        owner("so.tilo.store"),
        "Update available",
        Action::new("store.open", "Open"),
    ));
    let blocking = raise(
        Item::new(
            owner("so.tilo.store"),
            "Printer offline",
            Action::new("print.retry", "Print again"),
        )
        .urgent(Urgency::Blocking),
    );
    let earlier = raise(Item::new(
        owner("so.tilo.store"),
        "Sign in again",
        Action::new("auth.signin", "Sign in"),
    ));
    let order: Vec<_> = items().into_iter().map(|item| item.id()).collect();
    assert_eq!(order, vec![blocking, later, earlier]);
}

/// Seeing is not dismissing: the count falls, the rows stay. Deferring keeps
/// the row out of the way and still counted.
#[test]
fn seeing_does_not_dismiss_and_deferring_still_counts() {
    let _serial = crate::jobs::test_lock();
    clear();
    let one = raise(Item::new(
        owner("so.tilo.store"),
        "Update available",
        Action::new("store.open", "Open"),
    ));
    let two = raise(Item::new(
        owner("so.tilo.remote"),
        "Session ended",
        Action::new("remote.reconnect", "Reconnect"),
    ));
    assert_eq!(unseen(), 2);
    seen();
    assert_eq!(unseen(), 0, "nothing counts once it was looked at");
    assert_eq!(items().len(), 2, "and both rows stay");
    assert_eq!(waiting(), 2);

    defer(two);
    assert_eq!(items().len(), 1, "a deferred row is out of the stack");
    assert_eq!(deferred(), 1, "and still counted");
    assert_eq!(waiting(), 1);

    dismiss(one);
    assert!(items().is_empty());
}

/// Raising the same news again does not stack rows, and makes it count
/// again: something that happened twice is news again.
#[test]
fn the_same_news_twice_is_one_row() {
    let _serial = crate::jobs::test_lock();
    clear();
    let first = raise(Item::new(
        owner("so.tilo.store"),
        "Update available",
        Action::new("store.open", "Open"),
    ));
    seen();
    assert_eq!(unseen(), 0);
    let again = raise(Item::new(
        owner("so.tilo.store"),
        "Update available",
        Action::new("store.open", "Open"),
    ));
    assert_eq!(first, again);
    assert_eq!(items().len(), 1);
    assert_eq!(unseen(), 1);
}

/// Acting takes the row away and reports the one verb it waited on.
#[test]
fn acting_reports_the_verb_and_takes_the_row() {
    let _serial = crate::jobs::test_lock();
    clear();
    let item = raise(Item::new(
        owner("so.tilo.store"),
        "Update available",
        Action::new("store.open", "Open"),
    ));
    let acted = take(item).expect("waited on a verb");
    assert_eq!(acted.action.verb, "store.open");
    assert_eq!(acted.owner.app, "so.tilo.store");
    assert!(items().is_empty());
    assert_eq!(act(item), None, "an item is acted on once");
}

/// An app can take back what it raised: the thing it was about was answered
/// somewhere else.
#[test]
fn an_app_takes_back_what_it_raised() {
    let _serial = crate::jobs::test_lock();
    clear();
    let _ = raise(
        Item::new(
            owner("so.tilo.store"),
            "Couldn’t install LibreOffice",
            Action::new("store.retry", "Try again"),
        )
        .subject(Target::Job(7)),
    );
    let _ = raise(Item::new(
        owner("so.tilo.store"),
        "Update available",
        Action::new("store.open", "Open"),
    ));
    super::dismissed(Target::Job(7));
    let waiting = items();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].title(), "Update available");
}

/// An outcome that needs a person waits; one that does not is recorded. The
/// same call decides, so no app has to choose a channel.
#[test]
fn publish_sends_a_decision_to_the_stack_and_a_record_to_the_ledger() {
    let _serial = crate::jobs::test_lock();
    clear();
    attention_ledger_clear();
    let waiting = publish(Notice::needs(
        owner("so.tilo.print-centre"),
        "The printer ran out of paper",
        Action::new("print.resume", "Resume"),
    ));
    assert!(matches!(waiting, Published::Waiting(_)));
    let recorded = publish(Notice::did(
        owner("so.tilo.print-centre"),
        "Printed 3 pages",
    ));
    assert!(matches!(recorded, Published::Recorded(_)));
    assert_eq!(items().len(), 1, "only the decision waits");
    assert_eq!(super::entries().len(), 1, "the record is kept");
}

fn attention_ledger_clear() {
    super::clear_ledger();
}

/// An item cannot be built without an action: a row that cannot act is not a
/// row. The type is the rule, so there is nothing to remember.
#[test]
fn an_item_without_an_action_does_not_compile() {
    let _serial = crate::jobs::test_lock();
    // This is the compile-time half; the test pins that the only constructors
    // take an action.
    let item = Item::new(owner("so.tilo.store"), "x", Action::new("v", "Do it"));
    assert_eq!(item.action().verb, "v");
    assert_eq!(item.state(), State::Unseen);
}

/// A window's bar can show the count without opening anything.
#[test]
fn the_bar_can_say_how_many_are_unseen() {
    let _serial = crate::jobs::test_lock();
    clear();
    assert_eq!(super::pane::pill(), None);
    let _ = raise(Item::new(
        owner("so.tilo.store"),
        "Update available",
        Action::new("store.open", "Open"),
    ));
    assert_eq!(super::pane::pill().as_deref(), Some("1 waiting"));
    let _ = raise(Item::new(
        owner("so.tilo.store"),
        "A second thing",
        Action::new("store.open", "Open"),
    ));
    assert_eq!(super::pane::pill().as_deref(), Some("2 waiting"));
}

/// The stack and the Jobs surface are asked for by name: a host resolves the
/// request once.
#[test]
fn the_surface_can_be_asked_for_from_anywhere() {
    let _serial = crate::jobs::test_lock();
    let _ = super::take_opening();
    super::open();
    assert!(super::take_opening());
    assert!(!super::take_opening(), "a request is answered once");
    let _ = crate::jobs::take_opening();
    crate::jobs::open();
    assert!(crate::jobs::take_opening());
}
