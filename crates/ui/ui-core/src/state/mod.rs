mod replica;
#[cfg(feature = "model")]
pub use collaboration::state::{DocumentFrontier, WritingActors, changes_beyond};
pub use replica::State;

#[cfg(feature = "model")]
use automerge::transaction::Transactable as _;
#[cfg(test)]
#[cfg(feature = "model")]
use automerge::{AutoCommit, ObjId};
#[cfg(feature = "model")]
pub(crate) use collab_types::history::HistoryMoment;
#[cfg(feature = "model")]
use collab_types::history::same_frontier;
#[cfg(feature = "model")]
pub use collab_types::history::{UndoEntry, UndoLog};
#[cfg(feature = "model")]
pub use collab_types::history::{write_patches, write_revert_patches, write_revert_patches_where};

#[cfg(feature = "model")]
/// What the toolkit does with an app's [`State`] beyond keeping it: merge committed history it was
/// served, and report its undo status in words.
pub(crate) trait StateHistory {
    /// Whether the window's document cannot take a peer's document yet: an unfinished edit, or
    /// undo history over a different frontier.
    fn defers_remote(&self) -> bool;
    /// Merge committed history when nothing local is open over it: `None` when the live
    /// document has uncommitted or unrecorded edits, else whether the merge changed it.
    fn merge_remote_committed(
        &mut self,
        incoming: tilo_ui_base::collaboration::Incoming<'_>,
    ) -> Result<Option<bool>, crate::history_mode::routes::RemoteApplyError>;
    /// What the history can do for `author` right now.
    fn undo_status_snapshot(
        &self,
        author: Option<&tilo_ui_base::permissions::MemberId>,
    ) -> tilo_ui_base::widget::undo_access::UndoStatus;
}

#[cfg(feature = "model")]
impl StateHistory for State {
    fn defers_remote(&self) -> bool {
        self.doc.pending_ops() != 0
            || !same_frontier(self.undo_log().last_heads(), &self.doc.base_heads())
    }

    fn merge_remote_committed(
        &mut self,
        incoming: tilo_ui_base::collaboration::Incoming<'_>,
    ) -> Result<Option<bool>, crate::history_mode::routes::RemoteApplyError> {
        if self.defers_remote() {
            return Ok(None);
        }
        let (doc, undo, outbound) = self.parts_mut();
        let before = doc.get_heads();
        let changed = crate::history_mode::routes::merge_committed(doc, incoming)?;
        if changed {
            undo.rebase(doc);
            // The owner that served this history holds it: the next primary delta must
            // not echo it back as this window's change.
            outbound.observe_changes_after(doc, &before);
        }
        Ok(Some(changed))
    }

    fn undo_status_snapshot(
        &self,
        author: Option<&tilo_ui_base::permissions::MemberId>,
    ) -> tilo_ui_base::widget::undo_access::UndoStatus {
        crate::undo::status_snapshot(&self.doc, self.undo_log(), author)
    }
}

#[cfg(test)]
#[cfg(feature = "model")]
mod tests {
    use super::*;
    use automerge::{ReadDoc, ScalarValue, transaction::Transactable};

    #[test]
    fn a_payload_that_stops_parsing_part_way_is_refused_whole() {
        let mut peer = AutoCommit::new();
        peer.put(ROOT, "title", "workspace").unwrap();
        peer.commit();
        let base = peer.get_heads();
        let mut state = State::new();
        state.load_incremental(&peer.save()).unwrap();
        // Two changes: a valid first one, then a second cut short.
        peer.put(ROOT, "first", "kept only whole").unwrap();
        peer.commit();
        let middle = peer.get_heads();
        peer.put(ROOT, "second", "cut short").unwrap();
        peer.commit();
        let first = peer.save_after(&base);
        let second = peer.save_after(&middle);
        let first_change = &first[..first.len() - second.len()];
        let mut torn = first_change.to_vec();
        torn.extend_from_slice(&second[..second.len() - 1]);
        let before = state.doc.get_heads();
        assert!(state.load_incremental(&torn).is_err());
        assert_eq!(
            state.doc.get_heads(),
            before,
            "not even the valid first change"
        );
        assert!(state.doc.get(ROOT, "first").unwrap().is_none());
        assert!(state.load_incremental(&first).unwrap() > 0);
        assert_eq!(state.doc.get_heads(), peer.get_heads());
    }

    tilo_test_env::scenario! {
        fn a_frontier_publishes_what_this_replica_wrote;

        phase a_frontier_publishes_only_this_replicas_changes;
        {
            let mut doc = AutoCommit::new().with_actor(automerge::ActorId::random());
            doc.put(ROOT, "mine", "this device").unwrap();
            doc.commit();
            let mine = doc.get_heads();
            let mut other = AutoCommit::load(&doc.save())
                .unwrap()
                .with_actor(automerge::ActorId::random());
            other.put(ROOT, "from_peer", "another device").unwrap();
            other.commit();
            doc.load_incremental(&other.save_after(&mine)).unwrap();
            assert_eq!(
                doc.get_heads(),
                other.get_heads(),
                "the change is held here"
            );
            // A frontier never told the delivered change arrived publishes this replica's
            // change alone: a change goes out from the replica that made it, and the snapshot
            // handshake carries the rest.
            let mut frontier = DocumentFrontier::default();
            let (delta, heads) = frontier.pending_delta(&mut doc, &WritingActors::default());
            let published = automerge::Automerge::parse_changes(&delta).unwrap();
            assert_eq!(published.len(), 1);
            assert_eq!(published[0].actor_id(), doc.get_actor());
            assert_eq!(heads, doc.get_heads());
        }

        phase a_frontier_publishes_changes_a_handle_on_this_device_made;
        {
            let device = automerge::ActorId::random();
            let mut doc = AutoCommit::new().with_actor(device.clone());
            doc.put(ROOT, "setting", "on").unwrap();
            doc.commit();
            let mut frontier = DocumentFrontier::at_document(&mut doc);
            let handled = automerge::ActorId::from(
                crate::history_source::authors::device_actor(b"alice", &device).as_slice(),
            );
            doc.set_actor(handled.clone());
            doc.put(ROOT, "named", "alice").unwrap();
            doc.commit();
            doc.set_actor(device);
            let (delta, heads) = frontier.pending_delta(&mut doc, &WritingActors::default());
            let published = automerge::Automerge::parse_changes(&delta).unwrap();
            assert_eq!(published.len(), 1);
            assert_eq!(published[0].actor_id(), &handled);
            assert_eq!(heads, doc.get_heads());
        }

        phase a_frontier_publishes_changes_written_as_an_author_writer;
        {
            let writer = automerge::ActorId::random();
            let mut doc = AutoCommit::new().with_actor(writer.clone());
            doc.put(ROOT, "setting", "on").unwrap();
            doc.commit();
            let mut frontier = DocumentFrontier::at_document(&mut doc);
            let mut own = WritingActors::default();
            let authored = crate::resource::author_writer(&writer, "alice");
            own.note(authored.clone());
            doc.set_actor(authored.clone());
            doc.put(ROOT, "named", "alice").unwrap();
            doc.commit();
            doc.set_actor(writer);
            let (delta, heads) = frontier.pending_delta(&mut doc, &own);
            let published = automerge::Automerge::parse_changes(&delta).unwrap();
            assert_eq!(published.len(), 1);
            assert_eq!(published[0].actor_id(), &authored);
            assert_eq!(heads, doc.get_heads());
        }
    }

    tilo_test_env::scenario! {
        fn undo_restores_text_marks_and_list_order_exactly;

        phase undo_restores_multi_span_and_multi_character_text_edits_exactly;
        {
            let mut state = State::new();
            let text = state.doc.put_object(ROOT, "text", ObjType::Text).unwrap();
            state.commit_action(None, None);
            for next in [
                "first thought",
                "second thought",
                "a second, longer thought",
                "",
            ] {
                state.doc.update_text(&text, next).unwrap();
                state.commit_action(None, None);
            }
            for expected in [
                "a second, longer thought",
                "second thought",
                "first thought",
                "",
            ] {
                state.undo(None, None).unwrap();
                assert_eq!(state.doc.text(&text).unwrap(), expected);
            }
            for expected in [
                "first thought",
                "second thought",
                "a second, longer thought",
                "",
            ] {
                state.redo(None, None).unwrap();
                assert_eq!(state.doc.text(&text).unwrap(), expected);
            }
        }

        phase undo_unformatted_text_between_formatted_neighbors_does_not_inherit_marks;
        {
            use automerge::marks::{ExpandMark, Mark};
            let mut state = State::new();
            let text = state.doc.put_object(ROOT, "text", ObjType::Text).unwrap();
            state.doc.splice_text(&text, 0, 0, "aXb").unwrap();
            state
                .doc
                .mark(
                    &text,
                    Mark::new("bold".into(), true, 0, 1),
                    ExpandMark::After,
                )
                .unwrap();
            state
                .doc
                .mark(
                    &text,
                    Mark::new("bold".into(), true, 2, 3),
                    ExpandMark::After,
                )
                .unwrap();
            state.commit_action(None, None);
            let before = state.doc.marks(&text).unwrap();
            assert_eq!(
                before
                    .iter()
                    .map(|mark| (mark.start, mark.end))
                    .collect::<Vec<_>>(),
                [(0, 1), (2, 3)]
            );
            state.doc.splice_text(&text, 1, 1, "").unwrap();
            state.commit_action(None, None);
            let after = state.doc.marks(&text).unwrap();
            assert_eq!(state.doc.text(&text).unwrap(), "ab");
            for _ in 0..2 {
                state.undo(None, None).unwrap();
                assert_eq!(state.doc.text(&text).unwrap(), "aXb");
                assert_eq!(
                    state.doc.marks(&text).unwrap(),
                    before,
                    "restored X must remain unformatted"
                );
                state.redo(None, None).unwrap();
                assert_eq!(state.doc.text(&text).unwrap(), "ab");
                assert_eq!(state.doc.marks(&text).unwrap(), after);
            }
        }

        phase grouped_list_reorder_undo_redo_preserves_objects_marks_and_order;
        {
            fn insert(doc: &mut AutoCommit, list: &ObjId, index: usize, label: &str) {
                let block = doc.insert_object(list, index, ObjType::Map).unwrap();
                doc.put(&block, "id", label).unwrap();
                let text = doc.put_object(&block, "text", ObjType::Text).unwrap();
                doc.splice_text(&text, 0, 0, label).unwrap();
                doc.mark(
                    &text,
                    automerge::marks::Mark::new("bold".into(), true, 0, label.len()),
                    automerge::marks::ExpandMark::After,
                )
                .unwrap();
            }
            fn rows(
                doc: &AutoCommit,
                list: &ObjId,
            ) -> Vec<(String, String, Vec<automerge::marks::Mark>)> {
                (0..doc.length(list))
                    .map(|index| {
                        let block = doc.get(list, index).unwrap().unwrap().1;
                        let id = doc
                            .get(&block, "id")
                            .unwrap()
                            .unwrap()
                            .0
                            .as_str()
                            .unwrap()
                            .to_string();
                        let text = doc.get(&block, "text").unwrap().unwrap().1;
                        (id, doc.text(&text).unwrap(), doc.marks(&text).unwrap())
                    })
                    .collect()
            }
            let mut state = State::new();
            let list = state.doc.put_object(ROOT, "blocks", ObjType::List).unwrap();
            for (index, label) in [
                "parent-a", "child-a", "task-a", "parent-b", "child-b", "parent-c",
            ]
            .iter()
            .enumerate()
            {
                insert(&mut state.doc, &list, index, label);
            }
            state.commit_action(None, None);
            let before = rows(&state.doc, &list);
            let base = state.doc.get_heads();
            let mut peer = state.doc.fork_at(&base).unwrap();
            insert(&mut peer, &list, 2, "peer-child");
            peer.commit();
            let peer_row = rows(&peer, &list)[2].clone();
            state.doc.delete(&list, 3).unwrap();
            insert(&mut state.doc, &list, 0, "parent-b");
            state.doc.delete(&list, 4).unwrap();
            insert(&mut state.doc, &list, 1, "child-b");
            state.commit_action(None, None);
            let after = rows(&state.doc, &list);
            assert_eq!(
                after.iter().map(|row| row.0.as_str()).collect::<Vec<_>>(),
                [
                    "parent-b", "child-b", "parent-a", "child-a", "task-a", "parent-c"
                ]
            );
            for cycle in 0..3 {
                state.undo(None, None).unwrap();
                assert_eq!(rows(&state.doc, &list), before, "undo cycle {cycle}");
                state.redo(None, None).unwrap();
                assert_eq!(rows(&state.doc, &list), after, "redo cycle {cycle}");
            }
            state.load_incremental(&peer.save_after(&base)).unwrap();
            let mut before_with_peer = before;
            before_with_peer.insert(2, peer_row.clone());
            let mut after_with_peer = after;
            after_with_peer.insert(4, peer_row);
            assert_eq!(rows(&state.doc, &list), after_with_peer);
            state.undo(None, None).unwrap();
            assert_eq!(rows(&state.doc, &list), before_with_peer);
            state.redo(None, None).unwrap();
            assert_eq!(rows(&state.doc, &list), after_with_peer);
        }
    }

    #[test]
    fn adoption_refused_at_an_exhausted_revision_keeps_the_live_document() {
        let mut state = State::new();
        state.doc.put(automerge::ROOT, "body", "first").unwrap();
        state.commit_action(None, None);
        state.parts_mut().1.set_revision(u64::MAX);
        assert!(!state.adopt_doc(AutoCommit::new()));
        assert_eq!(
            state
                .doc
                .get(automerge::ROOT, "body")
                .unwrap()
                .unwrap()
                .0
                .as_str(),
            Some("first")
        );
    }

    tilo_test_env::scenario! {
        fn frontiers_and_rejected_deltas_survive_merges_and_retries;

        phase explicit_frontier_is_independent_of_automerge_save_cursor;
        {
            let mut doc = AutoCommit::new();
            let mut frontier = DocumentFrontier::at_document(&mut doc);
            doc.put(automerge::ROOT, "body", "draft").unwrap();
            let _snapshot = doc.save();

            let delta = frontier.take_delta(&mut doc, &WritingActors::default());
            assert!(
                !delta.is_empty(),
                "a full snapshot must not consume this route's delta"
            );
            assert!(
                frontier
                    .take_delta(&mut doc, &WritingActors::default())
                    .is_empty()
            );
        }

        phase rebased_frontier_does_not_echo_inbound_changes;
        {
            let mut source = AutoCommit::new();
            source.put(automerge::ROOT, "remote", true).unwrap();
            source.commit();
            let bytes = source.save();

            let mut target = AutoCommit::new();
            let mut frontier = DocumentFrontier::at_document(&mut target);
            let mut incoming = AutoCommit::load(&bytes).unwrap();
            target.merge(&mut incoming).unwrap();
            frontier.rebase(&mut target);
            assert!(
                frontier
                    .take_delta(&mut target, &WritingActors::default())
                    .is_empty()
            );

            target.put(automerge::ROOT, "local", true).unwrap();
            assert!(
                !frontier
                    .take_delta(&mut target, &WritingActors::default())
                    .is_empty()
            );
        }

        phase pending_delta_is_retryable_until_transport_accepts_it;
        {
            let mut doc = AutoCommit::new();
            let mut frontier = DocumentFrontier::at_document(&mut doc);
            doc.put(automerge::ROOT, "body", "retry me").unwrap();

            let (first, heads) = frontier.pending_delta(&mut doc, &WritingActors::default());
            let (retry, _) = frontier.pending_delta(&mut doc, &WritingActors::default());
            assert_eq!(retry, first);

            frontier.advance(heads);
            assert!(
                frontier
                    .pending_delta(&mut doc, &WritingActors::default())
                    .0
                    .is_empty()
            );
        }

        phase a_rejected_local_delta_survives_a_concurrent_merge;
        // The local edit was not accepted outbound when another device's state
        // arrived, whichever way it arrives: as an incremental delta or as a
        // whole snapshot. It stays pending for retry, and what arrived is not
        // echoed back to the device that sent it.
        {
            for (as_snapshot, incoming) in [(false, "the inbound change"), (true, "the snapshot's changes")]
            {
                let mut state = State::new();
                state.doc.put(ROOT, "local", true).unwrap();
                state.commit_action(None, None);
                let (rejected, _) = state.pending_outbound_delta();
                assert!(!rejected.is_empty());

                let mut peer = AutoCommit::new();
                peer.put(ROOT, "remote", true).unwrap();
                peer.commit();
                if as_snapshot {
                    state.merge_snapshot(&peer.save()).unwrap();
                } else {
                    state.load_incremental(&peer.save_after(&[])).unwrap();
                }

                let (retry, accepted_heads) = state.pending_outbound_delta();
                assert!(
                    !retry.is_empty(),
                    "the rejected local edit must remain pending"
                );
                let mut retry_receiver = AutoCommit::new();
                retry_receiver.load_incremental(&retry).unwrap();
                assert_eq!(
                    retry_receiver.get(ROOT, "local").unwrap().unwrap().0,
                    true.into()
                );
                assert!(
                    retry_receiver.get(ROOT, "remote").unwrap().is_none(),
                    "{incoming} must not be echoed back"
                );

                state.advance_outbound(accepted_heads);
                assert!(state.pending_outbound_delta().0.is_empty());
            }
        }
    }

    tilo_test_env::scenario! {
        fn the_history_is_a_lifo_carrying_focus_and_context;

        phase history_keeps_every_step_in_the_document;
        // The history is the document's: no window evicts old steps, and the
        // newest stays undoable whatever came before it.
        {
            let mut state = State::new();
            let steps = 600usize;
            for index in 0..steps {
                state
                    .doc
                    .put(
                        &automerge::ROOT,
                        "k",
                        ScalarValue::Int(i64::try_from(index).unwrap()),
                    )
                    .unwrap();
                state.commit_action(Some(index), None);
            }
            assert_eq!(state.undo_depth(), steps);
            let undone = state.undo(None, None).expect("the newest step is undoable");
            assert_eq!(undone.focused_node_idx, steps.checked_sub(1));
            assert_eq!(
                state
                    .doc
                    .get(&automerge::ROOT, "k")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_i64(),
                i64::try_from(steps).unwrap().checked_sub(2)
            );
        }

        phase redo_entry_carries_current_focus_and_metadata;
        // A single `commit_action` → undo → redo round-trip must preserve the
        // focus index and widget metadata captured on the stacks — that's
        // what restores the caret in `EditorApp` after Ctrl+Shift+Z.
        {
            let mut state = State::new();

            state
                .doc
                .put(&automerge::ROOT, "k", ScalarValue::Str("v".into()))
                .unwrap();
            state.commit_action(Some(7), Some("meta1".into()));

            // Simulate a second mutation whose current caret/focus should be the one we
            // expect redo to rehydrate.
            state
                .doc
                .put(&automerge::ROOT, "k", ScalarValue::Str("v2".into()))
                .unwrap();

            // Undo with the current state (Some(3) / "meta_current"), as `App::commit_action`
            // passes it.
            let popped = state
                .undo(Some(3), Some("meta_current".into()))
                .expect("undo must pop the seeded entry");
            assert_eq!(popped.focused_node_idx, Some(7));
            assert_eq!(popped.widget_metadata, Some("meta1".into()));

            // Redo — the test-only callers here don't need to bump the stack, so pass
            // None/None. The returned entry must reflect the state that was current at
            // the moment `undo(...)` fired.
            let redone = state
                .redo(None, None)
                .expect("redo must pop the redo-stack entry");
            assert_eq!(redone.focused_node_idx, Some(3));
            assert_eq!(redone.widget_metadata, Some("meta_current".into()));

            // Sanity: the doc itself is back at the post-second-mutation state so
            // renderers stay in sync.
            let (val, _) = state.doc.get(&automerge::ROOT, "k").unwrap().unwrap();
            assert_eq!(val.as_str(), Some("v2"));
        }

        phase native_commit_context_is_stamped_without_changing_undo;
        {
            let mut state = State::new();
            state.doc.put(&ROOT, "body", "hello").unwrap();
            let context =
                tilo_actions::ActionContext::for_resource("document:1", "document", "Draft")
                    .with_app("org.tilo.Writer", "Writer");
            assert!(state.commit_action_authored_with_context(
                None,
                None,
                Some(MemberId::person("ada")),
                Some(context.clone()),
            ));
            let changes = state.doc.get_changes(&[]);
            let change = changes.last().expect("native change");
            let decoded = change
                .message()
                .and_then(|message| tilo_actions::ActionContext::from_change_message(message))
                .map(|mut context| {
                    assert!(context.gesture.is_some(), "the change joined an undo step");
                    context.gesture = None;
                    context
                });
            assert_eq!(
                decoded,
                Some(context.with_invoker(tilo_actions::ActionInvoker::person("ada")))
            );
            assert!(state.peek_last_undo().is_some());
        }

        phase single_user_undo_redo_roundtrip;
        // Single-user undo/redo round-trip on a map value returns to the exact
        // pre/post values.
        {
            let mut state = State::new();
            state
                .doc
                .put(&ROOT, "k", ScalarValue::Str("a".into()))
                .unwrap();
            state.commit_action(None, None);
            state
                .doc
                .put(&ROOT, "k", ScalarValue::Str("b".into()))
                .unwrap();
            state.commit_action(None, None);

            let val = |s: &State| {
                s.doc
                    .get(&ROOT, "k")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str()
                    .unwrap()
                    .to_string()
            };
            assert_eq!(val(&state), "b");
            state.undo(None, None).unwrap();
            assert_eq!(val(&state), "a", "undo reverts to pre-action value");
            state.redo(None, None).unwrap();
            assert_eq!(val(&state), "b", "redo restores the action");
        }

        phase multi_action_undo_order_is_lifo;
        // Multiple actions undo in LIFO order, each reverting only its own change.
        {
            let mut state = State::new();
            state.doc.put(&ROOT, "a", 1).unwrap();
            state.commit_action(None, None);
            state.doc.put(&ROOT, "b", 2).unwrap();
            state.commit_action(None, None);
            state.doc.put(&ROOT, "c", 3).unwrap();
            state.commit_action(None, None);

            let has = |s: &State, k: &str| s.doc.get(&ROOT, k).unwrap().is_some();
            assert!(has(&state, "a") && has(&state, "b") && has(&state, "c"));

            state.undo(None, None).unwrap(); // reverts "c"
            assert!(has(&state, "a") && has(&state, "b") && !has(&state, "c"));
            state.undo(None, None).unwrap(); // reverts "b"
            assert!(has(&state, "a") && !has(&state, "b") && !has(&state, "c"));
            state.undo(None, None).unwrap(); // reverts "a"
            assert!(!has(&state, "a") && !has(&state, "b") && !has(&state, "c"));
        }
    }

    use automerge::{ObjType, ROOT};
    use tilo_ui_base::permissions::MemberId;

    fn text_body(state: &State, obj: &ObjId) -> String {
        state.doc.text(obj).unwrap_or_default()
    }

    tilo_test_env::scenario! {
        fn undo_and_redo_keep_concurrent_remote_edits;

        phase undo_preserves_concurrent_remote_edit;
        // A committed local action, then a *concurrent remote change* merged in,
        // then `undo()`. The remote change MUST survive and only the local
        // action is reverted.
        {
            let mut state = State::new();
            let text = state.doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
            state.doc.splice_text(&text, 0, 0, "Hello").unwrap();
            state.commit_action(None, None);

            let before_local = state.doc.get_heads();

            state.doc.splice_text(&text, 5, 0, " World").unwrap();
            state.commit_action(None, None);

            let mut remote = state.doc.fork_at(&before_local).unwrap();
            let rtext = remote.get(&ROOT, "body").unwrap().unwrap().1;
            remote.splice_text(&rtext, 0, 0, "X").unwrap();
            remote.commit();
            let delta = remote.save_after(&before_local);
            state.load_incremental(&delta).unwrap();
            assert_eq!(
                text_body(&state, &text),
                "XHello World",
                "precondition merge"
            );

            state.undo(None, None).expect("undo pops the local action");

            assert_eq!(
                text_body(&state, &text),
                "XHello",
                "remote prepend survives; only local append reverted"
            );
        }

        phase redo_after_concurrent_merge_keeps_both;
        // Redoing the undone local action must reinstate it while *still* keeping
        // the concurrently-merged remote edit.
        {
            let mut state = State::new();
            let text = state.doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
            state.doc.splice_text(&text, 0, 0, "Hello").unwrap();
            state.commit_action(None, None);
            let before_local = state.doc.get_heads();
            state.doc.splice_text(&text, 5, 0, " World").unwrap();
            state.commit_action(None, None);

            let mut remote = state.doc.fork_at(&before_local).unwrap();
            let rtext = remote.get(&ROOT, "body").unwrap().unwrap().1;
            remote.splice_text(&rtext, 0, 0, "X").unwrap();
            remote.commit();
            let delta = remote.save_after(&before_local);
            state.load_incremental(&delta).unwrap();

            state.undo(None, None).unwrap();
            assert_eq!(text_body(&state, &text), "XHello");
            state.redo(None, None).unwrap();
            assert_eq!(
                text_body(&state, &text),
                "XHello World",
                "redo reinstates local append; remote prepend still present"
            );
        }

        phase undo_text_delete_with_concurrent_prepend;
        // A local text *deletion* undone after a concurrent remote prepend must
        // re-insert the deleted text at the correctly-shifted position.
        {
            let mut state = State::new();
            let text = state.doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
            state.doc.splice_text(&text, 0, 0, "Hello World").unwrap();
            state.commit_action(None, None);
            let before_local = state.doc.get_heads();

            state.doc.splice_text(&text, 5, 6, "").unwrap();
            state.commit_action(None, None);

            let mut remote = state.doc.fork_at(&before_local).unwrap();
            let rtext = remote.get(&ROOT, "body").unwrap().unwrap().1;
            remote.splice_text(&rtext, 0, 0, "X").unwrap();
            remote.commit();
            let delta = remote.save_after(&before_local);
            state.load_incremental(&delta).unwrap();
            assert_eq!(text_body(&state, &text), "XHello");

            state.undo(None, None).unwrap();
            assert_eq!(
                text_body(&state, &text),
                "XHello World",
                "deleted text restored at shifted position; remote survives"
            );
        }
    }

    tilo_test_env::scenario! {
        fn author_scoped_undo_and_redo_touch_only_that_authors_actions;

        phase undo_as_an_author_reverts_only_that_authors_op;
        // `undo_as` is the entry point `App`'s Ctrl+Z takes. With an author it
        // reverts only the requested author's most-recent action as a new
        // forward change — walking past a member's action to the local user's
        // own, not plain LIFO — leaving other authors' committed ops intact in
        // the live document, not just unpopped from the stack.
        {
            let mut state = State::new();
            let alice = MemberId::person("alice");
            let bob = MemberId::person("bob");
            // Alice sets a=1.
            state.doc.put(&ROOT, "a", 1).unwrap();
            state.commit_action_authored(None, None, Some(alice.clone()));
            // Bob sets b=1.
            state.doc.put(&ROOT, "b", 1).unwrap();
            state.commit_action_authored(None, None, Some(bob));

            // Alice undoes — only "a" is reverted; Bob's "b" stays in the doc.
            let popped = state
                .undo_as(Some(&alice), None, None)
                .expect("alice has an action to undo");
            assert_eq!(
                popped.author,
                Some(alice.clone()),
                "alice undid her own action"
            );
            assert!(
                state.doc.get(&ROOT, "a").unwrap().is_none(),
                "alice's own change is reverted in the live doc"
            );
            assert!(
                state.doc.get(&ROOT, "b").unwrap().is_some(),
                "bob's change survives in the live doc, not just on the stack"
            );
        }

        phase undo_as_without_an_author_is_plain_lifo;
        // With no identity at all — an app running outside any session — `undo_as`
        // degrades to plain LIFO rather than refusing to undo.
        {
            let mut state = State::new();
            state.doc.put(&ROOT, "a", 1).unwrap();
            state.commit_action(None, None);
            state.doc.put(&ROOT, "b", 1).unwrap();
            state.commit_action(None, None);

            state
                .undo_as(None, None, None)
                .expect("undo pops the newest");
            assert!(state.doc.get(&ROOT, "b").unwrap().is_none());
            assert!(state.doc.get(&ROOT, "a").unwrap().is_some());
        }

        phase redo_as_an_author_only_reapplies_that_authors_undo;
        // Redo is the mirror: Bob's undone action must not be re-applied by
        // Alice's Ctrl+Y.
        {
            let mut state = State::new();
            let alice = MemberId::person("alice");
            let bob = MemberId::person("bob");

            state.doc.put(&ROOT, "a", 1).unwrap();
            state.commit_action_authored(None, None, Some(alice.clone()));
            state.doc.put(&ROOT, "b", 1).unwrap();
            state.commit_action_authored(None, None, Some(bob.clone()));

            state.undo_as(Some(&bob), None, None).unwrap();
            assert!(state.doc.get(&ROOT, "b").unwrap().is_none());

            // Alice hits Ctrl+Y. There is nothing of hers to redo, and Bob's undone
            // action must stay undone.
            assert!(
                state.redo_as(Some(&alice), None, None).is_none(),
                "alice has nothing to redo"
            );
            assert!(
                state.doc.get(&ROOT, "b").unwrap().is_none(),
                "alice's redo must not resurrect bob's undone action"
            );

            state
                .redo_as(Some(&bob), None, None)
                .expect("bob redoes his own");
            assert!(state.doc.get(&ROOT, "b").unwrap().is_some());
        }

        phase can_undo_predicts_exactly_what_undo_as_does;
        // The availability a control renders from must be the availability the
        // act has, scoped to the author as the shortcut is. Asserted by *doing*
        // the undo and checking the query predicted it — not by reading the
        // stack length, which is what would let the two drift. Without author
        // scoping the control would light up and then do nothing — or, worse,
        // revert someone else's work.
        {
            let mut state = State::new();
            let a = alice();

            assert!(
                !state.can_undo_as(Some(&a)),
                "a fresh document has no history"
            );
            assert!(!state.can_redo_as(Some(&a)));
            assert!(
                state.undo_as(Some(&a), None, None).is_none(),
                "and undo agrees"
            );

            type_action(&mut state, "body", "hello", &a);
            assert!(state.can_undo_as(Some(&a)));
            assert!(!state.can_redo_as(Some(&a)));
            assert!(
                !state.can_undo_as(Some(&bob())),
                "Bob has performed no action here"
            );
            assert!(
                state.undo_as(Some(&bob()), None, None).is_none(),
                "and undo declines for Bob — the query must match"
            );
            assert!(state.can_undo_as(None));

            assert!(
                state.undo_as(Some(&a), None, None).is_some(),
                "can_undo promised an undo; it has to be there"
            );
            assert!(!state.can_undo_as(Some(&a)), "the stack is empty again");
            assert!(state.can_redo_as(Some(&a)), "and the action is redoable");

            assert!(state.redo_as(Some(&a), None, None).is_some());
            assert!(state.can_undo_as(Some(&a)));
            assert!(!state.can_redo_as(Some(&a)));
        }
    }

    fn alice() -> tilo_ui_base::permissions::MemberId {
        MemberId::person("alice")
    }

    fn bob() -> tilo_ui_base::permissions::MemberId {
        MemberId::person("bob")
    }

    fn type_action(
        state: &mut State,
        key: &str,
        text: &str,
        author: &tilo_ui_base::permissions::MemberId,
    ) {
        let obj = state
            .doc
            .put_object(&automerge::ROOT, key, automerge::ObjType::Text)
            .unwrap();
        state.doc.splice_text(&obj, 0, 0, text).unwrap();
        assert!(
            state.commit_action_authored_with_context(
                None,
                None,
                Some(author.clone()),
                Some(typed()),
            ),
            "the edit must actually have committed"
        );
    }

    fn typed() -> tilo_actions::ActionContext {
        tilo_actions::ActionContext::for_resource("note", "note", "Note").with_described(
            tilo_actions::Described::new(tilo_actions::Did::verb(
                tilo_actions::ActionKey::new("org.tilo.Notes", "note", "type"),
                [],
            )),
        )
    }

    tilo_test_env::scenario! {
        fn undo_status_and_revision_describe_and_bound_every_stack_change;

        phase undo_status_describes_the_action_on_the_stack;
        // The status a widget receives describes the step that is on the
        // stack as its action record says it, as the history pane does.
        {
            let mut state = State::new();
            let a = alice();

            let empty = crate::undo::state_status(&state, Some(&a));
            assert_eq!(empty, tilo_ui_base::widget::undo_access::UndoStatus::empty());

            type_action(&mut state, "body", "hello", &a);
            let s = crate::undo::state_status(&state, Some(&a));
            assert!(s.can_undo && !s.can_redo);
            assert_eq!(
                s.undo_summary,
                Some(crate::history_source::sentence::sentence(&typed())),
                "the summary is the step's own sentence"
            );
            assert_eq!(s.redo_summary, None, "nothing has been undone yet");
        }

        phase redo_summary_describes_the_action_restored_not_the_reversal;
        // Redo's summary must read as the action being put *back*, not as the
        // reversal that is physically on the redo stack — otherwise "Redo" is
        // described to the user as deleting their text.
        {
            let mut state = State::new();
            let a = alice();
            type_action(&mut state, "body", "hello", &a);
            state.undo_as(Some(&a), None, None).unwrap();

            let s = crate::undo::state_status(&state, Some(&a));
            assert!(!s.can_undo && s.can_redo);
            assert_eq!(
                s.redo_summary,
                Some(crate::history_source::sentence::sentence(&typed())),
                "redo names the step it puts back"
            );
        }

        phase the_revision_moves_on_every_stack_change;
        // The status is only ever recomputed when the revision moves, so the
        // revision has to move on every stack change — a miss leaves a control
        // rendering a history that is no longer there.
        {
            let mut state = State::new();
            let a = alice();

            let start = state.history_revision();
            type_action(&mut state, "body", "hi", &a);
            let after_commit = state.history_revision();
            assert_ne!(after_commit, start, "committing an action moves it");

            state.undo_as(Some(&a), None, None).unwrap();
            let after_undo = state.history_revision();
            assert_ne!(after_undo, after_commit, "undo moves it");

            state.redo_as(Some(&a), None, None).unwrap();
            let after_redo = state.history_revision();
            assert_ne!(after_redo, after_undo, "redo moves it");

            // An event that changes nothing must NOT move it, or the host re-derives the
            // summaries every frame — and each derivation closes the open transaction.
            assert!(!state.commit_action_authored(None, None, Some(a.clone())));
            assert_eq!(
                state.history_revision(),
                after_redo,
                "an empty commit is not a history change"
            );

            let mut anon = State::new();
            anon.doc.put(&automerge::ROOT, "k", 1).unwrap();
            anon.commit_action(None, None);
            let anon_committed = anon.history_revision();
            anon.undo(None, None).unwrap();
            let anon_undone = anon.history_revision();
            assert_ne!(anon_undone, anon_committed, "plain undo moves the revision");
            anon.redo(None, None).unwrap();
            assert_ne!(
                anon.history_revision(),
                anon_undone,
                "plain redo moves the revision"
            );

            // Swapping the document resets the stacks; the revision must still move, so a
            // status cached against the old document is not reused.
            let mut other = automerge::AutoCommit::new();
            other.put(&automerge::ROOT, "k", 1).unwrap();
            state.adopt_doc(other);
            assert_ne!(
                state.history_revision(),
                after_redo,
                "adopting a document is a history change"
            );
            assert!(
                !state.can_undo_as(Some(&a)),
                "the new document has no history"
            );
        }

        phase exhausted_history_revision_refuses_transitions_without_mutating;
        {
            let mut state = State::new();
            let author = alice();
            type_action(&mut state, "body", "before exhaustion", &author);
            let heads_before_undo = state.doc.get_heads();
            state.parts_mut().1.set_revision(u64::MAX);

            assert!(
                state.undo_as(Some(&author), None, None).is_none(),
                "undo must fail rather than reuse an old revision"
            );
            assert_eq!(state.doc.get_heads(), heads_before_undo);
            assert!(state.can_undo_as(Some(&author)), "the entry was not popped");

            state.doc.put(&automerge::ROOT, "later", 1).unwrap();
            let heads_before_commit = state.doc.get_heads();
            assert!(
                !state.commit_action_authored(None, None, Some(author)),
                "committing at exhaustion must fail closed"
            );
            assert_eq!(state.doc.get_heads(), heads_before_commit);
            assert_eq!(state.history_revision(), u64::MAX);

            let replacement = automerge::AutoCommit::new();
            assert!(!state.adopt_doc(replacement));
            assert_eq!(state.doc.get_heads(), heads_before_commit);
        }

        phase a_bound_history_revision_limit_refuses_undo_redo_and_restore;
        {
            let mut doc = AutoCommit::new();
            let mut history = UndoLog::new(&mut doc);
            let before = doc.get_heads();
            doc.put(ROOT, "value", 1).unwrap();
            history.record_committed_action_in(&mut doc, before.clone(), None, None, alice(), 11);
            history.undo_in(&mut doc, &alice(), 11, None, None).unwrap();
            history.set_revision(u64::MAX);
            let heads = doc.get_heads();
            assert!(
                history
                    .redo_in(&mut doc, &alice(), 11, None, None)
                    .is_none()
            );
            assert!(!history.restore_to_heads_in(&mut doc, &before, None, None, alice(), 11));
            assert_eq!(doc.get_heads(), heads);
            assert_eq!(
                history
                    .next_redo_in(&doc, &alice(), 11)
                    .unwrap()
                    .instance_id,
                Some(11)
            );
        }

        phase a_restore_is_visible_to_a_control;
        // A restore from the history scrubber lands on the undo stack, so a
        // control must report it as undoable.
        {
            let mut state = State::new();
            let a = alice();
            type_action(&mut state, "body", "hello", &a);
            let version = state.doc.get_heads();
            type_action(&mut state, "other", "more", &a);

            assert!(state.restore_to_heads(&version, None, None, Some(a.clone())));
            assert!(
                state.can_undo_as(Some(&a)),
                "the restore itself is undoable"
            );
        }
    }

    tilo_test_env::scenario! {
        fn undo_in_formatted_text_restores_marks_and_keeps_concurrent_edits;

        phase undo_of_one_mark_over_several_texts_clears_it_from_each;
        {
            use automerge::marks::{ExpandMark, Mark};
            let mut state = State::new();
            let texts: Vec<ObjId> = ["Background", "Method", "Results"]
                .into_iter()
                .map(|word| {
                    let text = state.doc.put_object(ROOT, word, ObjType::Text).unwrap();
                    state.doc.splice_text(&text, 0, 0, word).unwrap();
                    text
                })
                .collect();
            state.commit_action(None, None);
            for text in &texts {
                let end = state.doc.length(text);
                state
                    .doc
                    .mark(
                        text,
                        Mark::new("bold".into(), true, 0, end),
                        ExpandMark::After,
                    )
                    .unwrap();
            }
            state.commit_action(None, None);
            state.undo(None, None).unwrap();
            for text in &texts {
                assert_eq!(state.doc.marks(text).unwrap(), [], "{text}");
            }
        }

        phase undo_of_several_splices_in_formatted_text_restores_text_and_marks;
        {
            use automerge::marks::{ExpandMark, Mark};
            let mut state = State::new();
            let text = state.doc.put_object(ROOT, "text", ObjType::Text).unwrap();
            state
                .doc
                .splice_text(&text, 0, 0, "Project Falcon remains")
                .unwrap();
            state
                .doc
                .mark(
                    &text,
                    Mark::new("bold".into(), true, 0, 14),
                    ExpandMark::After,
                )
                .unwrap();
            state.commit_action(None, None);
            let before = state.doc.marks(&text).unwrap();
            state.doc.splice_text(&text, 9, 0, "Osprey").unwrap();
            state.doc.splice_text(&text, 8, 1, "").unwrap();
            state.doc.splice_text(&text, 14, 5, "").unwrap();
            state.commit_action(None, None);
            assert_eq!(state.doc.text(&text).unwrap(), "Project Osprey remains");
            let after = state.doc.marks(&text).unwrap();
            for _ in 0..2 {
                state.undo(None, None).unwrap();
                assert_eq!(state.doc.text(&text).unwrap(), "Project Falcon remains");
                assert_eq!(state.doc.marks(&text).unwrap(), before);
                state.redo(None, None).unwrap();
                assert_eq!(state.doc.text(&text).unwrap(), "Project Osprey remains");
                assert_eq!(state.doc.marks(&text).unwrap(), after);
            }
        }

        phase undo_of_formatted_text_keeps_a_concurrent_edit_in_it;
        {
            use automerge::marks::{ExpandMark, Mark};
            let mut state = State::new();
            let text = state.doc.put_object(ROOT, "text", ObjType::Text).unwrap();
            state
                .doc
                .splice_text(&text, 0, 0, "Project Falcon remains")
                .unwrap();
            state
                .doc
                .mark(
                    &text,
                    Mark::new("bold".into(), true, 0, 14),
                    ExpandMark::After,
                )
                .unwrap();
            state.commit_action(None, None);
            let before_local = state.doc.get_heads();
            state.doc.splice_text(&text, 9, 0, "Osprey").unwrap();
            state.doc.splice_text(&text, 8, 1, "").unwrap();
            state.doc.splice_text(&text, 14, 5, "").unwrap();
            state.commit_action(None, None);
            let mut peer = state.doc.fork_at(&before_local).unwrap();
            peer.splice_text(&text, 0, 0, "New: ").unwrap();
            peer.splice_text(&text, 27, 0, " today").unwrap();
            peer.commit();
            state
                .load_incremental(&peer.save_after(&before_local))
                .unwrap();
            assert_eq!(
                state.doc.text(&text).unwrap(),
                "New: Project Osprey remains today"
            );
            state.undo(None, None).unwrap();
            assert_eq!(
                state.doc.text(&text).unwrap(),
                "New: Project Falcon remains today"
            );
            let bold: Vec<_> = state
                .doc
                .marks(&text)
                .unwrap()
                .into_iter()
                .map(|mark| (mark.name.to_string(), mark.start, mark.end))
                .collect();
            assert_eq!(bold, [("bold".to_owned(), 5, 19)]);
        }

        phase revert_beside_a_concurrent_edit_keeps_wide_characters_whole_in_every_encoding;
        {
            use automerge::TextEncoding;
            for encoding in [
                TextEncoding::UnicodeCodePoint,
                TextEncoding::Utf8CodeUnit,
                TextEncoding::Utf16CodeUnit,
                TextEncoding::GraphemeCluster,
            ] {
                let width = |s: &str| match encoding {
                    TextEncoding::Utf8CodeUnit => s.len(),
                    TextEncoding::Utf16CodeUnit => s.encode_utf16().count(),
                    TextEncoding::UnicodeCodePoint | TextEncoding::GraphemeCluster => s.chars().count(),
                };
                let mut doc = AutoCommit::new_with_encoding(encoding);
                let text = doc.put_object(ROOT, "text", ObjType::Text).unwrap();
                doc.splice_text(&text, 0, 0, "a🦀b").unwrap();
                let before = doc.get_heads();
                doc.splice_text(&text, width("a🦀"), 0, "XY").unwrap();
                doc.splice_text(&text, 0, 1, "").unwrap();
                let after = doc.get_heads();
                assert_eq!(doc.text(&text).unwrap(), "🦀XYb", "{encoding:?}");
                let mut peer = doc.fork_at(&before).unwrap();
                peer.splice_text(&text, width("a🦀b"), 0, "!").unwrap();
                peer.commit();
                doc.load_incremental(&peer.save_after(&before)).unwrap();
                assert_eq!(doc.text(&text).unwrap(), "🦀XYb!", "{encoding:?}");
                write_revert_patches(&mut doc, &after, &before);
                doc.commit();
                assert_eq!(doc.text(&text).unwrap(), "a🦀b!", "{encoding:?}");
            }
        }

        phase undo_keeps_a_created_paragraph_a_member_wrote_in;
        {
            fn paragraph(state: &mut State, blocks: &ObjId, id: &str) -> ObjId {
                let block = state.doc.put_object(blocks, id, ObjType::Map).unwrap();
                state.doc.put(&block, "order", id).unwrap();
                state.doc.put_object(&block, "text", ObjType::Text).unwrap()
            }
            fn texts(state: &State, blocks: &ObjId) -> Vec<(String, String)> {
                state
                    .doc
                    .keys(blocks)
                    .map(|id| {
                        let block = state.doc.get(blocks, &id).unwrap().unwrap().1;
                        let text = state.doc.get(&block, "text").unwrap().unwrap().1;
                        (id, state.doc.text(&text).unwrap())
                    })
                    .collect()
            }
            let mut state = State::new();
            let blocks = state.doc.put_object(ROOT, "blocks", ObjType::Map).unwrap();
            let first = paragraph(&mut state, &blocks, "a");
            state.commit_action(None, None);
            state.doc.splice_text(&first, 0, 0, "hello").unwrap();
            state.commit_action(None, None);
            paragraph(&mut state, &blocks, "b");
            state.commit_action(None, None);
            let seen = state.doc.get_heads();
            let mut peer = state.doc.fork_at(&seen).unwrap();
            let block = peer.get(&blocks, "b").unwrap().unwrap().1;
            let text = peer.get(&block, "text").unwrap().unwrap().1;
            peer.splice_text(&text, 0, 0, "their words").unwrap();
            peer.commit();
            state.load_incremental(&peer.save_after(&seen)).unwrap();

            state.undo(None, None).unwrap();
            state.undo(None, None).unwrap();
            assert_eq!(
                texts(&state, &blocks),
                [
                    ("a".to_owned(), String::new()),
                    ("b".to_owned(), "their words".to_owned())
                ]
            );
            // Redo puts back only what the undo took.
            state.redo(None, None).unwrap();
            state.redo(None, None).unwrap();
            assert_eq!(
                texts(&state, &blocks),
                [
                    ("a".to_owned(), "hello".to_owned()),
                    ("b".to_owned(), "their words".to_owned())
                ]
            );
        }
    }
}
