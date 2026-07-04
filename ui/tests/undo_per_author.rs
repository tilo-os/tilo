use ui::state::State;
use ui::permissions::UserId;
use automerge::transaction::Transactable;

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
