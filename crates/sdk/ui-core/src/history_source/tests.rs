//! End-to-end tests for the history producer.
//!
//! These drive the production path only: real automerge documents, real tilo-doc
//! schema, the real `HistorySource`, the real `ActivityTimeline` filter, and
//! the real `State::restore_to_heads`. Nothing is stubbed — a bug in any of
//! those links fails a test here.

use crate::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::widget::EntryKind;
use automerge::transaction::{CommitOptions, Transactable};
use automerge::{ActorId, AutoCommit, ObjType, ROOT};

use super::*;
use crate::permissions::UserId;
use crate::state::State;
use crate::widget::history::HistoryAuthor;
use crate::widget::{ActivityTimeline, HistoryFilter};

/// A fixed clock so date-range assertions are deterministic.
const T0: i64 = 1_780_000_000; // 2026-06-08T…, comfortably in the Tilo era.
const HOUR: i64 = 3_600;

#[test]
fn browsing_history_never_commits_or_invents_entries() {
    // phase: browsing_history_preserves_pending_action_and_committed_entry_ids
    {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "title", "saved").unwrap();
        let saved = doc.commit().unwrap();
        let saved_id = entry_id_from_hash(&saved.to_string());
        doc.put(ROOT, "title", "still typing").unwrap();
        let pending = doc.pending_ops();

        assert_eq!(committed_heads(&doc), vec![saved]);
        assert_eq!(
            doc.pending_ops(),
            pending,
            "cache lookup must not commit an edit"
        );

        let entries = source().entries(&mut doc);
        assert_eq!(
            doc.pending_ops(),
            pending,
            "listing must not commit an edit"
        );
        assert_eq!(
            entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![saved_id]
        );
        assert_eq!(heads_for_entry(&mut doc, &saved_id), Some(vec![saved]));
        assert_eq!(
            doc.pending_ops(),
            pending,
            "navigation must not commit an edit"
        );
        assert_eq!(version_index_for_entry(&mut doc, &saved_id), Some(0));
        assert_eq!(doc.pending_ops(), pending, "lookup must not commit an edit");
        let comparison = compare_entries(&mut doc, &[saved_id]).unwrap();
        assert_eq!(
            doc.pending_ops(),
            pending,
            "comparison must not commit an edit"
        );
        let snapshot = AutoCommit::load(&comparison.after_document).unwrap();
        assert_eq!(
            snapshot.get(ROOT, "title").unwrap().unwrap().0.as_str(),
            Some("saved")
        );
        assert_eq!(
            doc.get(ROOT, "title").unwrap().unwrap().0.as_str(),
            Some("still typing")
        );

        // The app's eventual action boundary must retain its metadata and all edits.
        doc.put(ROOT, "title", "finished typing").unwrap();
        doc.commit_with(CommitOptions::default().with_message("Rename document"));
        let changes = doc.get_changes(&[]);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[1].message(), Some("Rename document"));
    }
    // phase: browsing_uncommitted_document_does_not_invent_history
    {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "title", "draft").unwrap();
        let pending = doc.pending_ops();
        assert!(committed_heads(&doc).is_empty());
        assert!(source().entries(&mut doc).is_empty());
        assert!(heads_for_entry(&mut doc, &HistoryEntryId([9; 16])).is_none());
        assert!(matches!(
            compare_entries(&mut doc, &[HistoryEntryId([9; 16])]),
            Err(CompareError::UnknownEntry(_))
        ));
        assert_eq!(doc.pending_ops(), pending);
        assert!(doc.commit().is_some());
        assert_eq!(doc.get_changes(&[]).len(), 1);
    }
    // phase: history_preview_preserves_pending_text_edits
    {
        let (mut doc, block) = two_identity_document();
        let entries = source().entries(&mut doc);
        let first = content_rows(&entries)[0].id;
        let c = tilo_doc::model::open(&doc, &ROOT).unwrap();
        let mut ids = tilo_doc::IdGen::new(doc.get_actor().to_hex_string());
        tilo_doc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        }
        .splice_text(&block, 12, 0, " typing");
        let pending = doc.pending_ops();
        assert!(pending > 0);
        let preview = materialize_entry(&mut doc, &first).unwrap().unwrap();
        assert_eq!(preview.blocks[0].text, "hello");
        assert!(
            materialize_entry(&mut doc, &HistoryEntryId([9; 16]))
                .unwrap()
                .is_none()
        );
        assert_eq!(doc.pending_ops(), pending);
        assert!(doc.commit().is_some());
    }
    // phase: an_untouched_document_has_no_history
    // An empty document produces an empty timeline rather than a phantom row.
    {
        let mut doc = AutoCommit::new();
        assert!(HistorySource::new().entries(&mut doc).is_empty());
    }
}

fn at(secs: i64) -> SystemTime {
    let secs = u64::try_from(secs).unwrap_or(0);
    UNIX_EPOCH
        .checked_add(Duration::from_secs(secs))
        .unwrap_or(UNIX_EPOCH)
}

/// A document carrying the tilo-doc schema, authored by `handle`.
fn rich_doc(handle: &str) -> (AutoCommit, tilo_doc::Containers, tilo_doc::IdGen) {
    let mut doc = AutoCommit::new();
    doc.set_actor(ActorId::from(handle.as_bytes()));
    let c = tilo_doc::model::init(&mut doc, &ROOT);
    let ids = tilo_doc::IdGen::new(doc.get_actor().to_hex_string());
    tilo_resource::history::commit_stamped_at(&mut doc, T0);
    (doc, c, ids)
}

/// Two peers over one document: Alice writes "hello", Bob appends " world",
/// Alice appends "!" — each edit one commit, stamped an hour apart. Returns
/// Alice's document (with Bob's change merged in) and the block id.
fn two_identity_document() -> (AutoCommit, String) {
    let (mut alice, c, mut alice_ids) = rich_doc("alice");

    let block = {
        let mut dm = tilo_doc::DocMut {
            doc: &mut alice,
            c: &c,
            ids: &mut alice_ids,
        };
        let id = dm.insert_block(None, "paragraph");
        dm.splice_text(&id, 0, 0, "hello");
        id
    };
    tilo_resource::history::commit_stamped_at(&mut alice, T0 + HOUR);

    // Bob forks from Alice's state, as a synced peer would.
    let mut bob = alice.fork();
    bob.set_actor(ActorId::from("bob".as_bytes()));
    {
        let mut bob_ids = tilo_doc::IdGen::new(bob.get_actor().to_hex_string());
        let mut dm = tilo_doc::DocMut {
            doc: &mut bob,
            c: &c,
            ids: &mut bob_ids,
        };
        dm.splice_text(&block, 5, 0, " world");
    }
    tilo_resource::history::commit_stamped_at(&mut bob, T0 + 2 * HOUR);

    alice.merge(&mut bob).expect("peers merge");

    {
        let mut dm = tilo_doc::DocMut {
            doc: &mut alice,
            c: &c,
            ids: &mut alice_ids,
        };
        dm.splice_text(&block, 11, 0, "!");
    }
    tilo_resource::history::commit_stamped_at(&mut alice, T0 + 3 * HOUR);

    (alice, block)
}

fn source() -> HistorySource {
    let mut dir = AuthorDirectory::new();
    dir.register(
        &ActorId::from("alice".as_bytes()).to_hex_string(),
        HistoryAuthor::new("Alice", 200.0, "a11ce000"),
    );
    dir.register(
        &ActorId::from("bob".as_bytes()).to_hex_string(),
        HistoryAuthor::new("Bob", 30.0, "b0b00000"),
    );
    HistorySource::new().with_authors(dir)
}

/// The content edits, i.e. everything but the schema-init version.
fn content_rows(entries: &[HistoryEntry]) -> Vec<&HistoryEntry> {
    entries
        .iter()
        .filter(|e| e.summary != NO_VISIBLE_CHANGE)
        .collect()
}

// 1. Three edits, two identities, attributed

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn rows_attribute_and_summarise_what_actually_changed() {
    // phase: three_edits_by_two_identities_are_attributed_correctly
    // THE HEADLINE TEST. Three edits made under two different actors must come
    // back as three rows with the right names on them.
    {
        let (mut doc, _) = two_identity_document();
        let entries = source().entries(&mut doc);

        let rows = content_rows(&entries);
        assert_eq!(
            rows.len(),
            3,
            "three authored edits, three rows; got {:?}",
            entries
                .iter()
                .map(|e| (&e.author.name, &e.summary))
                .collect::<Vec<_>>()
        );
        let names: Vec<&str> = rows.iter().map(|e| e.author.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Alice", "Bob", "Alice"],
            "causal order and authorship: Alice wrote, Bob appended, Alice appended"
        );
        assert_ne!(rows[0].author.avatar_hue, rows[1].author.avatar_hue);
        assert_eq!(rows[0].author.identity_short, "a11ce000");
        assert_eq!(rows[1].author.identity_short, "b0b00000");
    }
    // phase: summaries_report_the_characters_actually_typed
    // The summary is derived from what the change actually did, not from an op
    // count. "hello" is five characters; " world" is six; "!" is one.
    {
        let (mut doc, _) = two_identity_document();
        let entries = source().entries(&mut doc);
        let rows = content_rows(&entries);

        assert_eq!(rows[0].summary, "+5 -0 chars", "\"hello\"");
        assert_eq!(rows[1].summary, "+6 -0 chars", "\" world\"");
        assert_eq!(rows[2].summary, "+1 -0 chars", "\"!\"");
        for r in &rows {
            assert_eq!(r.entry_kind, EntryKind::Text, "text edits are splices");
        }
    }
    // phase: deletions_are_counted_and_a_removed_block_is_a_tombstone
    // A deletion is reported as a deletion, and a change that only removes a block
    // is classed as a tombstone so it lands under the timeline's "Deletes" chip.
    {
        let (mut doc, c, mut ids) = rich_doc("alice");
        let (a, b) = {
            let mut dm = tilo_doc::DocMut {
                doc: &mut doc,
                c: &c,
                ids: &mut ids,
            };
            let a = dm.insert_block(None, "paragraph");
            dm.splice_text(&a, 0, 0, "keep this");
            let b = dm.insert_block(Some(&a), "paragraph");
            dm.splice_text(&b, 0, 0, "doomed");
            (a, b)
        };
        tilo_resource::history::commit_stamped_at(&mut doc, T0 + HOUR);

        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut doc,
                c: &c,
                ids: &mut ids,
            };
            dm.splice_text(&a, 4, 5, "");
        }
        tilo_resource::history::commit_stamped_at(&mut doc, T0 + 2 * HOUR);

        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut doc,
                c: &c,
                ids: &mut ids,
            };
            dm.remove_block(&b);
        }
        tilo_resource::history::commit_stamped_at(&mut doc, T0 + 3 * HOUR);

        let entries = source().entries(&mut doc);
        let rows = content_rows(&entries);
        assert_eq!(rows.len(), 3, "three authored edits: {rows:?}");
        assert_eq!(
            rows[1].summary, "+0 -5 chars",
            "deleting \" this\" is five chars"
        );
        assert_eq!(rows[1].entry_kind, EntryKind::Text);
        assert_eq!(
            rows[2].entry_kind,
            EntryKind::Delete,
            "a removed block is a delete"
        );
        assert_eq!(rows[2].summary, "-1 block");
    }
    // phase: a_plain_automerge_document_falls_back_to_patch_summaries
    // A document with no tilo-doc schema still produces real rows via the patch
    // fallback, which is what makes the producer usable over the compositor's
    // chrome document, not just tilo-doc artifacts.
    {
        let mut doc = AutoCommit::new();
        doc.set_actor(ActorId::from("alice".as_bytes()));
        let body = doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
        doc.splice_text(&body, 0, 0, "Hello").unwrap();
        tilo_resource::history::commit_stamped_at(&mut doc, T0);
        doc.splice_text(&body, 5, 0, " world").unwrap();
        tilo_resource::history::commit_stamped_at(&mut doc, T0 + HOUR);
        doc.put(&ROOT, "title", "Notes").unwrap();
        tilo_resource::history::commit_stamped_at(&mut doc, T0 + 2 * HOUR);

        let entries = source().entries(&mut doc);
        assert_eq!(entries.len(), 3, "three commits, three rows");
        assert_eq!(entries[0].summary, "+5 -0 chars");
        assert_eq!(entries[1].summary, "+6 -0 chars");
        assert_eq!(entries[1].entry_kind, EntryKind::Text);
        assert_eq!(
            entries[2].summary, "Edited title",
            "a map put names the field it edited"
        );
        assert_eq!(entries[2].entry_kind, EntryKind::Details);
        assert!(
            entries.iter().all(|e| e.author.name == "Alice"),
            "single-actor document, single author"
        );
    }
    // phase: an_unregistered_actor_is_named_honestly_in_a_produced_row
    // An actor nobody registered gets an honest label and a stable colour — never
    // a made-up name.
    {
        let mut doc = AutoCommit::new();
        // High bytes: not a decodable handle, so this is a true stranger.
        let stranger: Vec<u8> = (0u8..32).map(|i| 0x80u8.wrapping_add(i)).collect();
        doc.set_actor(ActorId::from(stranger.as_slice()));
        let body = doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
        doc.splice_text(&body, 0, 0, "who wrote this").unwrap();
        tilo_resource::history::commit_stamped_at(&mut doc, T0);

        let entries = HistorySource::new().entries(&mut doc);
        assert_eq!(entries.len(), 1);
        let a = &entries[0].author;
        assert_eq!(a.name, UNKNOWN_AUTHOR_NAME);
        assert_eq!(
            a.identity_short.len(),
            8,
            "the short actor hex disambiguates"
        );
        assert!((0.0..360.0).contains(&a.avatar_hue));
    }
    // phase: semantic_action_context_becomes_the_row_summary_and_browse_metadata
    {
        let (mut doc, c, mut ids) = rich_doc("alice");
        let mut dm = tilo_doc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        let block = dm.insert_block(None, "paragraph");
        dm.splice_text(&block, 0, 0, "launch");
        let context = tilo_actions::ActionContext::for_resource(
            "notes.note:launch",
            "notes.note",
            "Launch brief",
        )
        .with_app("org.tilo.Notes", "Notes")
        .with_purpose("Prepare the launch brief")
        .with_group("issue:42", "Launch brief", "issue");
        doc.commit_with(
            CommitOptions::default()
                .with_time(T0 + HOUR)
                .with_message(context.change_message().expect("context encodes")),
        );

        let entries = source().entries(&mut doc);
        let row = entries.last().expect("authored row");
        assert_eq!(row.summary, "Prepare the launch brief");
        assert_eq!(row.context.as_ref(), Some(&context));
        assert!(HistoryFilter::default().with_app("Notes").matches(row));
        assert!(
            HistoryFilter::default()
                .with_resource_kind("notes.note")
                .matches(row)
        );
    }
}

// 2. Restore

#[test]
fn restore_appends_an_undoable_entry_or_does_nothing() {
    // phase: restore_returns_earlier_content_and_appends_a_new_entry
    // Restore to an earlier version must bring the earlier content back — and
    // must do it by *appending* to history, never by rewriting it.
    {
        let (doc, block) = two_identity_document();
        let mut state = State::from_doc(doc);

        let before = source().entries(&mut state.doc);
        let rows = content_rows(&before);
        assert_eq!(rows.len(), 3);
        let v1 = rows[0].id;
        let ids_before: Vec<HistoryEntryId> = before.iter().map(|e| e.id).collect();

        let containers = tilo_doc::model::open(&state.doc, &ROOT).unwrap();
        let text_now = tilo_doc::model::materialize(&state.doc, &containers).blocks[0]
            .text
            .clone();
        assert_eq!(text_now, "hello world!", "precondition: latest content");

        let target = heads_for_entry(&mut state.doc, &v1).expect("row addresses a version");
        let changed = state.restore_to_heads(
            &target,
            None,
            Some("restore".into()),
            Some(UserId("alice".into())),
        );
        assert!(
            changed,
            "restoring to a different version must change the document"
        );

        let containers = tilo_doc::model::open(&state.doc, &ROOT).unwrap();
        let restored = tilo_doc::model::materialize(&state.doc, &containers).blocks[0]
            .text
            .clone();
        assert_eq!(restored, "hello", "content matches the version restored to");

        let after = source().entries(&mut state.doc);
        assert_eq!(
            after.len(),
            before.len() + 1,
            "the restore is a new entry, not a rewind"
        );
        for id in &ids_before {
            assert!(
                after.iter().any(|e| e.id == *id),
                "every pre-restore entry is still in the timeline: history was rewritten"
            );
        }
        let newest = after.last().unwrap();
        assert!(
            !ids_before.contains(&newest.id),
            "the newest entry is genuinely new"
        );
        assert_eq!(
            newest.summary, "+0 -7 chars",
            "the restore removed \" world!\""
        );
        assert_eq!(
            &block,
            &tilo_doc::model::materialize(&state.doc, &containers).blocks[0].id
        );
    }
    // phase: a_restore_is_undoable
    // The restore lands on the undo stack, so a user who scrubbed by mistake can
    // Ctrl+Z out of it.
    {
        let (doc, _) = two_identity_document();
        let mut state = State::from_doc(doc);
        let rows_ids: Vec<HistoryEntryId> = content_rows(&source().entries(&mut state.doc))
            .iter()
            .map(|e| e.id)
            .collect();
        let target = heads_for_entry(&mut state.doc, &rows_ids[0]).unwrap();

        state.restore_to_heads(&target, None, None, Some(UserId("alice".into())));
        let text = |s: &State| {
            let c = tilo_doc::model::open(&s.doc, &ROOT).unwrap();
            tilo_doc::model::materialize(&s.doc, &c).blocks[0]
                .text
                .clone()
        };
        assert_eq!(text(&state), "hello");

        state
            .undo(None, None)
            .expect("the restore is on the undo stack");
        assert_eq!(text(&state), "hello world!", "undo puts the restore back");
    }
    // phase: restoring_to_the_current_version_does_nothing
    // Restoring to the state the document is already in is a no-op — it must not
    // litter the timeline with empty entries.
    {
        let (doc, _) = two_identity_document();
        let mut state = State::from_doc(doc);
        let entries = source().entries(&mut state.doc);
        let latest = entries.last().unwrap().id;
        let target = heads_for_entry(&mut state.doc, &latest).unwrap();

        assert!(!state.restore_to_heads(&target, None, None, None));
        assert_eq!(
            source().entries(&mut state.doc).len(),
            entries.len(),
            "a no-op restore adds no entry"
        );
    }
    // phase: an_unknown_entry_id_resolves_to_no_version
    // A row id from a different document addresses nothing here.
    {
        let (mut doc, _) = two_identity_document();
        assert_eq!(heads_for_entry(&mut doc, &HistoryEntryId([0xab; 16])), None);
    }
}

// 3. Filters, through the real widget

#[test]
fn contributor_and_date_filters_select_and_intersect() {
    // phase: filtering_by_contributor_hides_the_other_authors_rows
    // Filtering by contributor must actually filter. Asserted through
    // `ActivityTimeline`, not `HistoryFilter::matches` in isolation.
    {
        let (mut doc, _) = two_identity_document();
        let entries = source().entries(&mut doc);

        let mut timeline = ActivityTimeline::with_entries(entries.clone());
        let unfiltered = timeline.visible_len();
        assert_eq!(unfiltered, entries.len(), "no filter shows everything");

        timeline.set_filter(HistoryFilter::default().with_author("Bob"));
        assert_eq!(timeline.visible_len(), 1, "Bob made exactly one edit");
        assert_eq!(timeline.visible_entry(0).unwrap().author.name, "Bob");

        timeline.set_filter(HistoryFilter::default().with_author("Alice"));
        assert_eq!(
            timeline.visible_len(),
            entries.len() - 1,
            "everything Bob did not write is Alice's"
        );
        assert!(
            (0..timeline.visible_len())
                .all(|i| timeline.visible_entry(i).unwrap().author.name == "Alice"),
            "no Bob rows leak through the Alice filter"
        );
    }
    // phase: filtering_by_date_range_selects_only_edits_inside_the_window
    // Filtering by date range must actually filter. The producer's timestamps are
    // the ones the commits carry, so a window that brackets one edit shows one row.
    {
        let (mut doc, _) = two_identity_document();
        let entries = source().entries(&mut doc);

        let mut timeline = ActivityTimeline::with_entries(entries.clone());
        // Bob's edit is stamped at T0 + 2h.
        timeline.set_filter(HistoryFilter {
            time_range: Some((at(T0 + 2 * HOUR - 60), at(T0 + 2 * HOUR + 60))),
            ..Default::default()
        });
        assert_eq!(timeline.visible_len(), 1, "one edit falls in the window");
        assert_eq!(timeline.visible_entry(0).unwrap().author.name, "Bob");

        timeline.set_filter(HistoryFilter {
            time_range: Some((at(T0 - 10 * HOUR), at(T0 - 9 * HOUR))),
            ..Default::default()
        });
        assert_eq!(timeline.visible_len(), 0, "nothing happened then");

        // The full span holds everything.
        timeline.set_filter(HistoryFilter {
            time_range: Some((at(T0 - HOUR), at(T0 + 10 * HOUR))),
            ..Default::default()
        });
        assert_eq!(timeline.visible_len(), entries.len());
    }
    // phase: contributor_and_date_filters_intersect
    // The two filters compose: contributor AND window.
    {
        let (mut doc, _) = two_identity_document();
        let entries = source().entries(&mut doc);
        let mut timeline = ActivityTimeline::with_entries(entries);

        timeline.set_filter(HistoryFilter {
            authors: vec!["Alice".into()],
            time_range: Some((at(T0 + 2 * HOUR - 60), at(T0 + 2 * HOUR + 60))),
            ..Default::default()
        });
        assert_eq!(
            timeline.visible_len(),
            0,
            "Alice made no edit in the hour Bob did"
        );
    }
}

// Timestamps + authorship edge cases

#[test]
fn entries_are_stamped_capped_and_stably_addressed() {
    // phase: stamped_commits_carry_their_wall_clock_into_the_row
    // A stamped commit produces a real timestamp, so date filters have
    // something to compare against.
    {
        let (mut doc, _) = two_identity_document();
        let entries = source().entries(&mut doc);
        let rows = content_rows(&entries);
        assert_eq!(rows[0].timestamp, at(T0 + HOUR));
        assert_eq!(rows[1].timestamp, at(T0 + 2 * HOUR));
        assert_eq!(rows[2].timestamp, at(T0 + 3 * HOUR));
    }
    // phase: state_commit_action_stamps_the_change
    // `State::commit_action` — the path every `ui` app takes — stamps its changes.
    {
        let mut state = State::new();
        let body = state.doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
        state.doc.splice_text(&body, 0, 0, "typed").unwrap();
        state.commit_action(None, None);

        let entries = HistorySource::new().entries(&mut state.doc);
        assert_eq!(entries.len(), 1);
        let age = crate::time::wall()
            .duration_since(entries[0].timestamp)
            .expect("timestamp is in the past, not 1970-adjacent nonsense");
        assert!(
            age < Duration::from_secs(600),
            "a just-committed action must be stamped with now, got {age:?} ago"
        );
    }
    // phase: the_entry_cap_keeps_the_most_recent_versions
    // The window cap keeps the newest rows, not the oldest.
    {
        let (mut doc, c, mut ids) = rich_doc("alice");
        let p = {
            let mut dm = tilo_doc::DocMut {
                doc: &mut doc,
                c: &c,
                ids: &mut ids,
            };
            dm.insert_block(None, "paragraph")
        };
        tilo_resource::history::commit_stamped_at(&mut doc, T0);
        for i in 0..6 {
            {
                let mut dm = tilo_doc::DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                dm.splice_text(&p, i, 0, "x");
            }
            let i = i64::try_from(i).unwrap_or(i64::MAX);
            tilo_resource::history::commit_stamped_at(&mut doc, T0 + (i + 1) * HOUR);
        }
        let all = source().entries(&mut doc);
        let capped = source().with_max_entries(3).entries(&mut doc);
        assert_eq!(capped.len(), 3);
        assert_eq!(
            capped.iter().map(|e| e.id).collect::<Vec<_>>(),
            all[all.len() - 3..]
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>(),
            "the cap takes the tail of history, not the head"
        );
        // The first capped row still describes a delta, since the window opens
        // against the snapshot just before it.
        assert_eq!(capped[0].summary, "+1 -0 chars");
    }
    // phase: entry_ids_are_stable_across_peers
    // Entry ids are the change's own content address, so two peers building a
    // timeline over the same history agree on which row is which.
    {
        let (mut alice, _) = two_identity_document();
        let bytes = alice.save();
        let mut elsewhere = AutoCommit::load(&bytes).unwrap();

        let a: Vec<HistoryEntryId> = source().entries(&mut alice).iter().map(|e| e.id).collect();
        let b: Vec<HistoryEntryId> = source()
            .entries(&mut elsewhere)
            .iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(a, b, "same history, same row ids");
    }
    // phase: heads_address_a_merged_history_correctly
    // `prefix_heads` has to handle a merged, non-linear change graph, and
    // materialising through it must reproduce the content the timeline claims.
    {
        let (mut doc, _) = two_identity_document();
        let entries = source().entries(&mut doc);
        let rows = content_rows(&entries);

        for (row, expected) in rows.iter().zip(["hello", "hello world", "hello world!"]) {
            let idx = version_index_for_entry(&mut doc, &row.id).expect("row addresses a version");
            let snap = tilo_doc::snapshot::at(&mut doc, idx).expect("version materialises");
            assert_eq!(
                snap.blocks[0].text, expected,
                "version {idx} should read {expected:?}"
            );
            let heads = tilo_resource::history::prefix_heads(&mut doc, idx);
            assert!(!heads.is_empty(), "every version has at least one head");
        }
    }
}

#[test]
fn unschematised_changes_are_named_by_what_they_edited() {
    fn summaries(edit: impl FnOnce(&mut AutoCommit)) -> Vec<String> {
        let mut doc = AutoCommit::new();
        let settings = doc.put_object(ROOT, "settings", ObjType::Map).unwrap();
        doc.put(&settings, "theme", "dark").unwrap();
        doc.put(ROOT, "title", "draft").unwrap();
        doc.commit();
        edit(&mut doc);
        doc.commit();
        HistorySource::new()
            .entries(&mut doc)
            .into_iter()
            .map(|entry| entry.summary)
            .collect()
    }
    let lookup = |key: &str, path: &str, more: usize| {
        crate::i18n::I18nManager::format(key, &[("path", path.into()), ("more", more.into())])
    };

    let one = summaries(|doc| doc.put(ROOT, "title", "final").unwrap());
    assert_eq!(
        one.last(),
        Some(&lookup("history-edited-field", "title", 0))
    );
    assert_eq!(one.last().map(String::as_str), Some("Edited title"));

    let nested = summaries(|doc| {
        let (_, settings) = doc.get(ROOT, "settings").unwrap().unwrap();
        doc.put(&settings, "theme", "light").unwrap();
    });
    assert_eq!(
        nested.last(),
        Some(&lookup("history-edited-field", "settings.theme", 0))
    );

    let several = summaries(|doc| {
        doc.put(ROOT, "title", "final").unwrap();
        doc.put(ROOT, "subtitle", "new").unwrap();
    });
    let last = several.last().unwrap();
    assert!(
        *last == lookup("history-edited-field", "title", 1)
            || *last == lookup("history-edited-field", "subtitle", 1),
        "{last}"
    );
    assert!(last.ends_with("and 1 more"), "{last}");

    let removed = summaries(|doc| doc.delete(ROOT, "title").unwrap());
    assert_eq!(
        removed.last(),
        Some(&lookup("history-removed-field", "title", 0))
    );
}

#[test]
fn a_feed_counts_edited_fields_and_never_shows_their_keys() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "desktop_counter_v1", 1_u64).unwrap();
    doc.commit();
    doc.put(ROOT, "desktop_counter_v1", 2_u64).unwrap();
    doc.put(ROOT, "contents_owners_v1", "peer").unwrap();
    doc.commit();
    doc.delete(ROOT, "contents_owners_v1").unwrap();
    doc.commit();
    let summaries = HistorySource::new()
        .without_field_paths()
        .entries(&mut doc)
        .into_iter()
        .map(|entry| entry.summary)
        .collect::<Vec<_>>();
    assert_eq!(
        summaries,
        ["Edited 1 field", "Edited 2 fields", "Removed 1 field"]
    );
    assert!(summaries.iter().all(|summary| !summary.contains("_v1")));
}

#[test]
fn an_action_context_names_its_change_before_any_field_fallback() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "title", "draft").unwrap();
    doc.commit();
    doc.put(ROOT, "title", "final").unwrap();
    let context = tilo_actions::ActionContext::for_resource("r", "doc", "Report").with_described(
        tilo_actions::Described::new(tilo_actions::Did::verb(
            tilo_actions::ActionKey::new("org.tilo.Reports", "doc", "finalize"),
            [],
        )),
    );
    doc.commit_with(CommitOptions::default().with_message(context.change_message().unwrap()));
    let entries = HistorySource::new().entries(&mut doc);
    let last = entries.last().unwrap();
    assert_eq!(
        last.summary,
        crate::history_source::sentence::sentence(&context)
    );
    assert_eq!(last.summary, "Finalize in Report");
    assert!(!last.summary.contains("title"), "{}", last.summary);
}

#[test]
fn a_records_genesis_can_be_left_out_of_a_feed_of_what_people_did() {
    use crate::record::{RecordCheckout, RecordId, RecordKind, RecordOwners, genesis_actor};
    let cache = crate::portable_storage::PortableStorage::new("profile", "device").unwrap();
    let store = RecordOwners::portable(cache).open("test", "room").unwrap();
    let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("document"));
    store.create_record(&checkout).unwrap();
    let mut doc = store.read(&checkout.id, AutoCommit::clone).unwrap();
    let genesis = genesis_actor(&doc).expect("a record names its genesis actor");
    assert!(
        tilo_resource::history::versions(&mut doc)
            .iter()
            .any(|v| v.actor == genesis.to_hex_string() && v.time_ms == 0),
        "the genesis is made by the record's derived actor at time zero"
    );
    let kept = HistorySource::new().entries(&mut doc);
    assert_eq!(
        kept.len(),
        1,
        "a document's history keeps the version it was created as"
    );
    assert!(kept[0].is_genesis(), "and says it is the genesis");
    assert_eq!(kept[0].summary, "Created");
    assert!(
        !kept[0].is_revertible(),
        "creation is restored to, not reverted"
    );
    assert!(
        HistorySource::new()
            .without_record_genesis()
            .entries(&mut doc)
            .is_empty()
    );

    doc.put(ROOT, "title", "Notes").unwrap();
    tilo_resource::history::commit_stamped_at(&mut doc, T0);
    let entries = HistorySource::new()
        .without_record_genesis()
        .entries(&mut doc);
    assert_eq!(entries.len(), 1, "only the edit someone made");
    assert_eq!(entries[0].timestamp, at(T0));
    assert!(!entries[0].is_genesis());
    assert_eq!(
        entries[0].author.identity_short,
        short_id(&doc.get_actor().to_hex_string())
    );
    assert_eq!(genesis_actor(&AutoCommit::new()), None);
}

#[test]
fn a_package_primary_documents_genesis_is_left_out_of_a_feed_too() {
    use crate::record::{RecordCheckout, RecordKind, RecordOwners, genesis_actor};
    let cache = crate::portable_storage::PortableStorage::new("profile", "device").unwrap();
    let store = RecordOwners::portable(cache).open("test", "room").unwrap();
    let checkout = RecordCheckout::package_primary(RecordKind::new("chat"));
    store
        .reserve_record(&checkout)
        .unwrap()
        .initialize(|doc| {
            doc.put(ROOT, "categories", "general")
                .map_err(|error| error.to_string())?;
            Ok(())
        })
        .unwrap();
    let mut doc = store.read(&checkout.id, AutoCommit::clone).unwrap();
    assert_eq!(
        genesis_actor(&doc),
        None,
        "a primary document records no identity"
    );
    let kept = HistorySource::new().entries(&mut doc);
    assert_eq!(
        kept.len(),
        1,
        "its own history keeps the version it was created as"
    );
    assert!(kept[0].is_genesis());
    assert!(
        HistorySource::new()
            .without_record_genesis()
            .entries(&mut doc)
            .is_empty(),
        "nobody made the root change at time zero"
    );

    doc.put(ROOT, "categories", "general, voice").unwrap();
    doc.commit();
    assert!(
        !HistorySource::new().entries(&mut doc)[1].is_genesis(),
        "an edit at time zero is not the genesis"
    );
    assert_eq!(
        HistorySource::new()
            .without_record_genesis()
            .entries(&mut doc)
            .len(),
        1,
        "an edit without a time is still someone's"
    );

    let mut first = AutoCommit::new();
    first.put(ROOT, "text", "typed").unwrap();
    first.commit();
    assert_eq!(
        HistorySource::new()
            .without_record_genesis()
            .entries(&mut first)
            .len(),
        1,
        "a document's first edit is not a genesis unless its store made it"
    );
}

#[test]
fn rows_tell_the_readers_changes_from_a_collaborators_and_an_agents() {
    use crate::widget::history::AuthorRelation;
    use tilo_actions::{ActionContext, ActionInvoker, ActorKind};
    let mut doc = AutoCommit::new();
    let step = |doc: &mut AutoCommit, key: &str, invoker: ActionInvoker| {
        doc.put(ROOT, key, "x").unwrap();
        let context = ActionContext::for_resource("r", "doc", "Report")
            .with_described(tilo_actions::Described::new(tilo_actions::Did::verb(
                tilo_actions::ActionKey::new("org.tilo.Reports", "doc", "edit"),
                [],
            )))
            .with_invoker(invoker);
        doc.commit_with(CommitOptions::default().with_message(context.change_message().unwrap()));
    };
    step(&mut doc, "mine", ActionInvoker::actor("quinten"));
    step(&mut doc, "theirs", ActionInvoker::actor("ada"));
    step(
        &mut doc,
        "agents",
        ActionInvoker::actor("agent:claude")
            .with_kind(ActorKind::Agent)
            .acting_for("quinten"),
    );
    let mut authors = AuthorDirectory::new();
    authors.set_reader("quinten");
    let entries = HistorySource::new().with_authors(authors).entries(&mut doc);
    let rows: Vec<(String, AuthorRelation)> = entries
        .iter()
        .rev()
        .take(3)
        .map(|entry| (entry.author.name.clone(), entry.author.relation))
        .collect();
    assert_eq!(
        rows,
        [
            (
                "claude · Agent, for you".to_owned(),
                AuthorRelation::Software
            ),
            ("ada".to_owned(), AuthorRelation::Person),
            ("You".to_owned(), AuthorRelation::Reader),
        ]
    );
}

/// An agent's change is the agent's, on behalf of its person: by the
/// invoker its entry records, and by its own key when it writes as itself.
/// The row names both and carries them as data.
#[test]
fn an_agents_change_reads_as_the_agent_on_behalf_of_its_person() {
    use crate::widget::history::{AuthorRelation, HistoryAttribution};
    use tilo_actions::{ActionContext, ActionInvoker, ActorKind};
    // Through the action surface: the host stamps the authenticated agent
    // and the person it acted for on the change.
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "title", "x").unwrap();
    let context = ActionContext::for_resource("r", "doc", "Report")
        .with_described(tilo_actions::Described::new(tilo_actions::Did::verb(
            tilo_actions::ActionKey::new("org.tilo.Reports", "doc", "edit"),
            [],
        )))
        .with_invoker(
            ActionInvoker::actor("agent:claude")
                .with_kind(ActorKind::Agent)
                .acting_for("ada"),
        );
    doc.commit_with(CommitOptions::default().with_message(context.change_message().unwrap()));
    let mut authors = AuthorDirectory::new();
    authors.set_reader("quinten");
    let entries = HistorySource::new().with_authors(authors).entries(&mut doc);
    let author = &entries.last().unwrap().author;
    assert_eq!(author.name, "claude · Agent, for ada");
    assert_eq!(author.relation, AuthorRelation::Software);
    assert_eq!(
        author.attribution,
        Some(HistoryAttribution {
            actor: "agent:claude".into(),
            kind: ActorKind::Agent,
            actor_name: "claude".into(),
            on_behalf_of: Some("ada".into()),
            on_behalf_of_name: Some("ada".into()),
        })
    );

    // As itself: a delegated agent signs with its own device key, which the
    // directory knows as the agent of its person, in its own colour.
    let device = [7_u8; 32];
    let person = hex::encode([9_u8; 32]);
    let mut doc = AutoCommit::new();
    doc.set_actor(ActorId::from(device.as_slice()));
    doc.put(ROOT, "title", "y").unwrap();
    doc.commit();
    let mut authors = AuthorDirectory::new();
    authors.set_reader("quinten");
    authors.register_person(&person, HistoryAuthor::new("Ada", 0.0, "ada00000"));
    authors.register_agent(
        &hex::encode(device),
        HistoryAgent {
            principal: "agent:scribe".into(),
            label: "Scribe".into(),
            rgb: (10, 20, 30),
            person: person.clone(),
        },
    );
    let entries = HistorySource::new()
        .with_authors(authors.clone())
        .entries(&mut doc);
    let author = &entries.last().unwrap().author;
    assert_eq!(author.name, "Scribe · Agent, for Ada");
    assert_eq!(author.relation, AuthorRelation::Software);
    assert_eq!(author.rgb(), (10, 20, 30), "the agent's presence colour");
    assert_eq!(
        author.attribution,
        Some(HistoryAttribution {
            actor: "agent:scribe".into(),
            kind: ActorKind::Agent,
            actor_name: "Scribe".into(),
            on_behalf_of: Some(person.clone()),
            on_behalf_of_name: Some("Ada".into()),
        })
    );
    // The person reading it sees it was done for them.
    authors.set_reader(person);
    let entries = HistorySource::new().with_authors(authors).entries(&mut doc);
    assert_eq!(
        entries.last().unwrap().author.name,
        "Scribe · Agent, for you"
    );
}

// N. A long-lived reader reads each change once and says what a fresh read says

/// What `entries_since` says and how many changes it read doing so.
fn read_since(
    source: &HistorySource,
    doc: &mut AutoCommit,
    log: &mut HistoryLog,
) -> (Vec<HistoryEntry>, usize) {
    let before = log.changes_read();
    let entries = source.entries_since(doc, log);
    (entries, log.changes_read() - before)
}

/// `entries_since` agrees with a fresh `entries`, and so do the heads the
/// log names for every row and the hash and dependencies it keeps for them.
fn assert_matches_a_fresh_read(
    source: &HistorySource,
    doc: &mut AutoCommit,
    log: &HistoryLog,
    since: &[HistoryEntry],
    step: &str,
) {
    let fresh = source.entries(doc);
    assert_eq!(since, fresh.as_slice(), "{step}: rows");
    for entry in &fresh {
        assert_eq!(
            log.heads_for_entry(&entry.id),
            heads_for_entry(doc, &entry.id),
            "{step}: heads of {:?}",
            entry.summary
        );
        let (hash, deps) = log.change(&entry.id).expect("a listed change is logged");
        let change = doc.get_change_by_hash(&hash).expect("the logged hash");
        assert_eq!(entry_id_from_hash(&hash.to_string()), entry.id, "{step}");
        assert_eq!(deps, change.deps(), "{step}: deps of {:?}", entry.summary);
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one growing document, one labelled phase per kind of change appended"
)]
fn entries_since_reads_only_appended_changes_and_matches_entries() {
    // A window of four, so appending slides it past changes read earlier.
    let source = source().with_max_entries(4);
    let mut log = HistoryLog::default();

    // phase: a tilo-doc document, edited by one person a change at a time
    let (mut doc, c, mut ids) = rich_doc("alice");
    let (rows, read) = read_since(&source, &mut doc, &mut log);
    assert_eq!(
        read,
        doc.get_changes(&[]).len().min(4),
        "the first read reads the window"
    );
    assert_matches_a_fresh_read(&source, &mut doc, &log, &rows, "genesis");
    let block = {
        let mut dm = tilo_doc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        dm.insert_block(None, "paragraph")
    };
    tilo_resource::history::commit_stamped_at(&mut doc, T0 + HOUR);
    let mut len = 0;
    for ((n, word), hours) in ["one", " two", " three", " four", " five", " six"]
        .into_iter()
        .enumerate()
        .zip(2_i64..)
    {
        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut doc,
                c: &c,
                ids: &mut ids,
            };
            dm.splice_text(&block, len, 0, word);
        }
        len += word.chars().count();
        tilo_resource::history::commit_stamped_at(&mut doc, T0 + HOUR * hours);
        let (rows, read) = read_since(&source, &mut doc, &mut log);
        assert_eq!(
            read,
            if n == 0 { 2 } else { 1 },
            "edit {n}: only what was appended"
        );
        assert_matches_a_fresh_read(&source, &mut doc, &log, &rows, &format!("edit {n}"));
    }

    // phase: a peer's concurrent edits, merged in
    let mut bob = doc.fork();
    bob.set_actor(ActorId::from("bob".as_bytes()));
    {
        let mut bob_ids = tilo_doc::IdGen::new(bob.get_actor().to_hex_string());
        let mut dm = tilo_doc::DocMut {
            doc: &mut bob,
            c: &c,
            ids: &mut bob_ids,
        };
        dm.splice_text(&block, 0, 0, "Bob: ");
    }
    tilo_resource::history::commit_stamped_at(&mut bob, T0 + 20 * HOUR);
    {
        let mut dm = tilo_doc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        dm.splice_text(&block, 0, 0, "Alice: ");
    }
    tilo_resource::history::commit_stamped_at(&mut doc, T0 + 21 * HOUR);
    doc.merge(&mut bob).expect("peers merge");
    let (rows, read) = read_since(&source, &mut doc, &mut log);
    assert_eq!(read, 2, "both sides of the merge, once each");
    assert_matches_a_fresh_read(&source, &mut doc, &log, &rows, "merge");

    // phase: a change that only names a version is read but listed by no one
    let start = rows.first().expect("rows").id;
    groups::name_range(&mut doc, &start, None, "Draft");
    tilo_resource::history::commit_stamped_at(&mut doc, T0 + 22 * HOUR);
    let (rows, read) = read_since(&source, &mut doc, &mut log);
    assert_eq!(read, 1, "the naming change");
    assert_matches_a_fresh_read(&source, &mut doc, &log, &rows, "named range");

    // phase: nothing new reads nothing
    let (rows, read) = read_since(&source, &mut doc, &mut log);
    assert_eq!(read, 0, "an unchanged document");
    assert_matches_a_fresh_read(&source, &mut doc, &log, &rows, "unchanged");

    // phase: another document is read from scratch
    let (mut other, _block) = two_identity_document();
    let (rows, read) = read_since(&source, &mut other, &mut log);
    let changes = other.get_changes(&[]).len();
    assert_eq!(read, changes.min(4), "the other document's window");
    assert_matches_a_fresh_read(&source, &mut other, &log, &rows, "another document");

    // phase: a change of locale reads the window again, in its words
    crate::i18n::I18nManager::set_pseudo_locale(crate::i18n::PseudoLocale::Expanded);
    let (rows, read) = read_since(&source, &mut other, &mut log);
    let fresh = source.entries(&mut other);
    crate::i18n::I18nManager::set_pseudo_locale(crate::i18n::PseudoLocale::Off);
    assert_eq!(read, changes.min(4), "a new locale reads again");
    assert_eq!(rows, fresh, "rows in the new locale");

    // phase: an unschematised document whose window slides one change at a time
    let mut plain = AutoCommit::new();
    plain.set_actor(ActorId::from("alice".as_bytes()));
    let mut log = HistoryLog::default();
    for n in 0..8_i64 {
        plain.put(ROOT, format!("field-{}", n % 3), n).unwrap();
        if n % 4 == 3 {
            plain.delete(ROOT, "field-0").unwrap();
        }
        tilo_resource::history::commit_stamped_at(&mut plain, T0 + n * HOUR);
        let (rows, read) = read_since(&source, &mut plain, &mut log);
        assert_eq!(read, 1, "plain edit {n}");
        assert_matches_a_fresh_read(&source, &mut plain, &log, &rows, &format!("plain {n}"));
    }
}

/// A source that passes over the changes that only move a view: one
/// operation putting the root `view` key.
fn only_moves_the_view(change: &automerge::Change) -> bool {
    use automerge::legacy::{Key, ObjectId};
    change.len() == 1
        && change.decode().operations.first().is_some_and(|op| {
            op.obj == ObjectId::Root && matches!(&op.key, Key::Map(key) if key == "view")
        })
}

#[test]
fn passed_over_changes_make_no_row_and_take_no_place_in_the_window() {
    let viewer = source()
        .with_max_entries(3)
        .passing_over(only_moves_the_view);
    let mut log = HistoryLog::default();
    let mut doc = AutoCommit::new();
    let mut hours = 0_i64..;
    let mut commit = |doc: &mut AutoCommit| {
        let hour = hours.next().unwrap_or_default();
        tilo_resource::history::commit_stamped_at(doc, T0 + HOUR * hour);
    };
    for title in ["one", "two", "three"] {
        doc.put(ROOT, "title", title).unwrap();
        commit(&mut doc);
    }
    let (rows, _) = read_since(&viewer, &mut doc, &mut log);
    assert_eq!(rows.len(), 3, "the window holds the three edits");
    assert_matches_a_fresh_read(&viewer, &mut doc, &log, &rows, "edits");

    // phase: moving the view, many times, reads each change once and moves
    // no row
    let revision = log.rows_revision();
    for view in 0..10_u64 {
        doc.put(ROOT, "view", view).unwrap();
        commit(&mut doc);
        let (after, read) = read_since(&viewer, &mut doc, &mut log);
        assert_eq!(read, 1, "view {view}: its own change");
        assert_eq!(after, rows, "view {view}: the rows are as they were");
        assert_eq!(log.rows_revision(), revision, "view {view}: rows unmoved");
        assert_matches_a_fresh_read(&viewer, &mut doc, &log, &after, "view");
    }

    // phase: an edit after them is a row, read at heads that include them,
    // and slides the window by one place, not by the views
    doc.put(ROOT, "title", "four").unwrap();
    commit(&mut doc);
    let (rows, read) = read_since(&viewer, &mut doc, &mut log);
    assert_eq!(read, 1);
    assert_ne!(log.rows_revision(), revision, "a row moved");
    assert_eq!(rows.len(), 3, "the window still holds three edits");
    assert_matches_a_fresh_read(&viewer, &mut doc, &log, &rows, "edit after views");

    // phase: a change that moves the view and edits is no view change
    doc.put(ROOT, "view", 99_u64).unwrap();
    doc.put(ROOT, "title", "five").unwrap();
    commit(&mut doc);
    let (rows, _) = read_since(&viewer, &mut doc, &mut log);
    assert_matches_a_fresh_read(&viewer, &mut doc, &log, &rows, "view and edit");
    let unfiltered = source().with_max_entries(3).entries(&mut doc);
    assert_eq!(
        rows.last().map(|row| row.id),
        unfiltered.last().map(|row| row.id),
        "it is a row"
    );

    // phase: a log read from scratch passes over the same changes
    let mut scratch = HistoryLog::default();
    let (fresh, _) = read_since(&viewer, &mut doc, &mut scratch);
    assert_eq!(fresh, rows);
}
