use ui::state::State;
use ui::permissions::UserId;
use automerge::transaction::Transactable;
use automerge::ReadDoc;

#[test]
fn commit_action_records_no_author_by_default() {
    let mut state = State::new();
    state.doc.put(&state.root, "v", 1).unwrap();
    state.commit_action(None, None);
    let last = state.peek_last_undo().expect("undo entry exists");
    assert_eq!(last.author, None);
}

#[test]
fn commit_action_authored_records_user() {
    let mut state = State::new();
    state.doc.put(&state.root, "v", 1).unwrap();
    state.commit_action_authored(None, None, Some(UserId("alice".into())));
    let last = state.peek_last_undo().expect("undo entry");
    assert_eq!(last.author, Some(UserId("alice".into())));
}

#[test]
fn undo_for_author_skips_entries_owned_by_other_users() {
    let mut state = State::new();
    // Alice's change
    state.doc.put(&state.root, "v", 1).unwrap();
    state.commit_action_authored(None, None, Some(UserId("alice".into())));
    // Bob's change
    state.doc.put(&state.root, "v", 2).unwrap();
    state.commit_action_authored(None, None, Some(UserId("bob".into())));
    // Alice's change
    state.doc.put(&state.root, "v", 3).unwrap();
    state.commit_action_authored(None, None, Some(UserId("alice".into())));

    // Alice's undo should pop the top entry (alice's own, v=3 becomes v=2).
    // This test doesn't care about caret state, so pass None/None.
    let popped = state
        .undo_for_author(&UserId("alice".into()), None, None)
        .expect("alice entry");
    assert_eq!(popped.author, Some(UserId("alice".into())));
    // Next alice undo: must skip bob's entry and pop the earlier alice entry.
    let popped = state
        .undo_for_author(&UserId("alice".into()), None, None)
        .expect("alice entry again");
    assert_eq!(popped.author, Some(UserId("alice".into())));
    // No more alice entries: return None (bob's remains untouched).
    assert!(state
        .undo_for_author(&UserId("alice".into()), None, None)
        .is_none());
    // Bob can still undo his entry.
    let popped = state
        .undo_for_author(&UserId("bob".into()), None, None)
        .expect("bob entry");
    assert_eq!(popped.author, Some(UserId("bob".into())));
}

/// The path an app's own toolbar button takes, end to end.
///
/// A widget dispatches its `qos_actions` action out-of-band and reports the
/// heads it captured beforehand; the host drains
/// `Widget::take_committed_action` once per frame and calls
/// `record_committed_action` to bracket it. Then the user presses Ctrl+Z,
/// which is `undo_as(Some(local_author))` — never the plain LIFO `undo` that
/// the app suites call.
///
/// A recorded entry carries no author (the drain has none to give), so this is
/// the case an author-scoped undo has to claim rather than walk past. Nothing
/// else can ever reach one.
#[test]
fn ctrl_z_reverts_a_button_driven_action() {
    let mut state = State::new();
    let me = UserId("someone".into());

    let before = state.doc.get_heads();
    state.doc.put(&state.root, "v", 1).unwrap();
    state.record_committed_action(before, None, None);

    assert!(
        state.undo_as(Some(&me), None, None).is_some(),
        "Ctrl+Z must revert a button-driven action"
    );
    assert!(
        state.doc.get(&state.root, "v").unwrap().is_none(),
        "the button's change is gone from the document"
    );
}

/// The mirror: having undone it, Ctrl+Shift+Z puts it back.
#[test]
fn ctrl_shift_z_reapplies_a_button_driven_action() {
    let mut state = State::new();
    let me = UserId("someone".into());

    let before = state.doc.get_heads();
    state.doc.put(&state.root, "v", 1).unwrap();
    state.record_committed_action(before, None, None);
    state.undo_as(Some(&me), None, None).expect("undo");

    assert!(
        state.redo_as(Some(&me), None, None).is_some(),
        "Ctrl+Shift+Z must re-apply the action it just reverted"
    );
    assert!(
        state.doc.get(&state.root, "v").unwrap().is_some(),
        "the button's change is back in the document"
    );
}

/// Claiming an unattributed entry must not widen Ctrl+Z to a collaborator's
/// work: the fallback reaches past somebody else's action to an unclaimed one,
/// and leaves theirs standing.
#[test]
fn claiming_an_unattributed_entry_still_skips_another_person() {
    let mut state = State::new();
    let me = UserId("me".into());
    let them = UserId("them".into());

    let before = state.doc.get_heads();
    state.doc.put(&state.root, "mine", 1).unwrap();
    state.record_committed_action(before, None, None);

    state.doc.put(&state.root, "theirs", 1).unwrap();
    state.commit_action_authored(None, None, Some(them.clone()));

    let popped = state
        .undo_as(Some(&me), None, None)
        .expect("my unattributed action is still reachable under theirs");
    assert_eq!(
        popped.author, None,
        "the entry claimed was the unattributed one"
    );
    assert!(
        state.doc.get(&state.root, "mine").unwrap().is_none(),
        "my own action is reverted"
    );
    assert!(
        state.doc.get(&state.root, "theirs").unwrap().is_some(),
        "their action survives my undo"
    );
}
