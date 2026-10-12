#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::ReadDoc;
use automerge::transaction::Transactable;
use tilo_ui::permissions::UserId;
use tilo_ui::state::State;

#[test]
fn author_attribution_scopes_what_undo_reaches() {
    // Case `commit_action_records_no_author_by_default`.
    {
        let mut state = State::new();
        state.doc.put(&state.root, "v", 1).unwrap();
        state.commit_action(None, None);
        let last = state.peek_last_undo().expect("undo entry exists");
        assert_eq!(last.author, None);
    }

    // Case `commit_action_authored_records_user`.
    {
        let mut state = State::new();
        state.doc.put(&state.root, "v", 1).unwrap();
        state.commit_action_authored(None, None, Some(UserId("alice".into())));
        let last = state.peek_last_undo().expect("undo entry");
        assert_eq!(last.author, Some(UserId("alice".into())));
    }

    // Case `undo_for_author_skips_entries_owned_by_other_users`.
    {
        fn undo_for_author_skips_entries_owned_by_other_users() {
            let mut state = State::new();
            state.doc.put(&state.root, "v", 1).unwrap();
            state.commit_action_authored(None, None, Some(UserId("alice".into())));
            state.doc.put(&state.root, "v", 2).unwrap();
            state.commit_action_authored(None, None, Some(UserId("bob".into())));
            state.doc.put(&state.root, "v", 3).unwrap();
            state.commit_action_authored(None, None, Some(UserId("alice".into())));

            // Pops the top entry (alice's own, v=3 becomes v=2). Caret state is
            // irrelevant here, hence None/None throughout this file.
            let popped = state
                .undo_as(Some(&UserId("alice".into())), None, None, None)
                .expect("alice entry");
            assert_eq!(popped.author, Some(UserId("alice".into())));
            // Next alice undo: must skip bob's entry and pop the earlier alice entry.
            let popped = state
                .undo_as(Some(&UserId("alice".into())), None, None, None)
                .expect("alice entry again");
            assert_eq!(popped.author, Some(UserId("alice".into())));
            // No more alice entries: return None (bob's remains untouched).
            assert!(
                state
                    .undo_as(Some(&UserId("alice".into())), None, None, None)
                    .is_none()
            );
            let popped = state
                .undo_as(Some(&UserId("bob".into())), None, None, None)
                .expect("bob entry");
            assert_eq!(popped.author, Some(UserId("bob".into())));
        }
        undo_for_author_skips_entries_owned_by_other_users();
    }

    // Case `claiming_an_unattributed_entry_still_skips_another_person`.
    // Claiming an unattributed entry must not widen Ctrl+Z to a collaborator's
    // work: the fallback reaches past somebody else's action to an unclaimed one,
    // and leaves theirs standing.
    {
        let mut state = State::new();
        let me = UserId("me".into());
        let them = UserId("them".into());

        let before = state.doc.get_heads();
        state.doc.put(&state.root, "mine", 1).unwrap();
        state.record_committed_action(before, None, None);

        state.doc.put(&state.root, "theirs", 1).unwrap();
        state.commit_action_authored(None, None, Some(them));

        let popped = state
            .undo_as(Some(&me), None, None, None)
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
}

#[test]
fn keyboard_undo_and_redo_drive_button_actions() {
    // Case `ctrl_z_reverts_a_button_driven_action`.
    // The path an app's own toolbar button takes, end to end: a widget
    // dispatches its `tilo_actions` action out-of-band, the host brackets it
    // via `record_committed_action`, then Ctrl+Z is `undo_as(Some(local_author))`
    // — never the plain LIFO `undo`.
    //
    // A recorded entry carries no author (the drain has none to give), so an
    // author-scoped undo has to claim it rather than walk past.
    {
        let mut state = State::new();
        let me = UserId("someone".into());

        let before = state.doc.get_heads();
        state.doc.put(&state.root, "v", 1).unwrap();
        state.record_committed_action(before, None, None);

        assert!(
            state.undo_as(Some(&me), None, None, None).is_some(),
            "Ctrl+Z must revert a button-driven action"
        );
        assert!(
            state.doc.get(&state.root, "v").unwrap().is_none(),
            "the button's change is gone from the document"
        );
    }

    // Case `ctrl_shift_z_reapplies_a_button_driven_action`.
    // The mirror: having undone it, Ctrl+Shift+Z puts it back.
    {
        let mut state = State::new();
        let me = UserId("someone".into());

        let before = state.doc.get_heads();
        state.doc.put(&state.root, "v", 1).unwrap();
        state.record_committed_action(before, None, None);
        state.undo_as(Some(&me), None, None, None).expect("undo");

        assert!(
            state.redo_as(Some(&me), None, None, None).is_some(),
            "Ctrl+Shift+Z must re-apply the action it just reverted"
        );
        assert!(
            state.doc.get(&state.root, "v").unwrap().is_some(),
            "the button's change is back in the document"
        );
    }
}

#[test]
fn baseline_moves_past_uncommitted_and_empty_changes() {
    // Case `an_empty_commit_moves_the_baseline_past_a_change_nobody_committed_here`.
    // A change can reach a document without a local commit — a peer's ops merged
    // in, or the shared schema a `qdoc` document adopts rather than mints. The
    // baseline the *next* action brackets against has to move past it, or one
    // Ctrl+Z takes the document's own containers out along with the keystroke.
    // Here the move happens on an empty commit (a selection drag, a focus move).
    {
        let mut state = State::new();
        let me = UserId("someone".into());
        let root = state.root.clone();
        tilo_doc::model::init(&mut state.doc, &root);

        assert!(
            !state.commit_action_authored(None, None, Some(me.clone())),
            "precondition: nothing was pending, so nothing was committed"
        );

        state.doc.put(&root, "typed", "hello").unwrap();
        state.commit_action_authored(None, None, Some(me.clone()));

        state
            .undo_as(Some(&me), None, None, None)
            .expect("the keystroke is undoable");
        assert!(
            state.doc.get(&root, "typed").unwrap().is_none(),
            "the keystroke is reverted"
        );
        assert!(
            tilo_doc::model::open(&state.doc, &root).is_ok(),
            "and the schema nobody committed here is still standing"
        );
    }

    // Case `recording_an_action_that_changed_nothing_still_moves_the_baseline`.
    // The same hazard on the out-of-band route: an action that turned out to
    // change nothing still has to move the baseline, because whatever arrived
    // before it is otherwise inside the next action's bracket.
    {
        let mut state = State::new();
        let me = UserId("someone".into());
        let root = state.root.clone();
        tilo_doc::model::init(&mut state.doc, &root);

        let before = state.doc.get_heads();
        state.record_committed_action(before, None, None);

        state.doc.put(&root, "typed", "hello").unwrap();
        state.commit_action_authored(None, None, Some(me.clone()));

        state
            .undo_as(Some(&me), None, None, None)
            .expect("the keystroke is undoable");
        assert!(
            state.doc.get(&root, "typed").unwrap().is_none(),
            "the keystroke is reverted"
        );
        assert!(
            tilo_doc::model::open(&state.doc, &root).is_ok(),
            "and the schema nobody committed here is still standing"
        );
    }
}
