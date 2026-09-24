//! End-to-end tests for the history producer.
//!
//! These drive the production path only: real automerge documents, real qdoc
//! schema, the real `HistorySource`, the real `ActivityTimeline` filter, and
//! the real `State::restore_to_heads`. Nothing is stubbed — a bug in any of
//! those links fails a test here.

use crate::time::{Duration, SystemTime, UNIX_EPOCH};

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{ActorId, AutoCommit, ObjType, ROOT};
use collab_types::OpClass;

use super::*;
use crate::permissions::UserId;
use crate::state::State;
use crate::widget::history::HistoryAuthor;
use crate::widget::{ActivityTimeline, HistoryFilter};

/// A fixed clock so date-range assertions are deterministic.
const T0: i64 = 1_780_000_000; // 2026-06-08T…, comfortably in the Tilo era.
const HOUR: i64 = 3_600;

#[test]
fn browsing_history_preserves_pending_action_and_committed_entry_ids() {
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

#[test]
fn browsing_uncommitted_document_does_not_invent_history() {
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

#[test]
fn history_preview_preserves_pending_text_edits() {
    let (mut doc, block) = two_identity_document();
    let entries = source().entries(&mut doc);
    let first = content_rows(&entries)[0].id;
    let c = qdoc::model::open(&doc, &ROOT).unwrap();
    let mut ids = qdoc::IdGen::new(doc.get_actor().to_hex_string());
    qdoc::DocMut {
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

fn at(secs: i64) -> SystemTime {
    let secs = u64::try_from(secs).unwrap_or(0);
    UNIX_EPOCH
        .checked_add(Duration::from_secs(secs))
        .unwrap_or(UNIX_EPOCH)
}

/// A document carrying the qdoc schema, authored by `handle`.
fn qdoc_doc(handle: &str) -> (AutoCommit, qdoc::Containers, qdoc::IdGen) {
    let mut doc = AutoCommit::new();
    doc.set_actor(ActorId::from(handle.as_bytes()));
    let c = qdoc::model::init(&mut doc, &ROOT);
    let ids = qdoc::IdGen::new(doc.get_actor().to_hex_string());
    qdoc::history::commit_stamped_at(&mut doc, T0);
    (doc, c, ids)
}

/// Two peers over one document: Alice writes "hello", Bob appends " world",
/// Alice appends "!" — each edit one commit, stamped an hour apart. Returns
/// Alice's document (with Bob's change merged in) and the block id.
fn two_identity_document() -> (AutoCommit, String) {
    let (mut alice, c, mut alice_ids) = qdoc_doc("alice");

    let block = {
        let mut dm = qdoc::DocMut {
            doc: &mut alice,
            c: &c,
            ids: &mut alice_ids,
        };
        let id = dm.insert_block(None, "paragraph");
        dm.splice_text(&id, 0, 0, "hello");
        id
    };
    qdoc::history::commit_stamped_at(&mut alice, T0 + HOUR);

    // Bob forks from Alice's state, as a synced peer would.
    let mut bob = alice.fork();
    bob.set_actor(ActorId::from("bob".as_bytes()));
    {
        let mut bob_ids = qdoc::IdGen::new(bob.get_actor().to_hex_string());
        let mut dm = qdoc::DocMut {
            doc: &mut bob,
            c: &c,
            ids: &mut bob_ids,
        };
        dm.splice_text(&block, 5, 0, " world");
    }
    qdoc::history::commit_stamped_at(&mut bob, T0 + 2 * HOUR);

    alice.merge(&mut bob).expect("peers merge");

    {
        let mut dm = qdoc::DocMut {
            doc: &mut alice,
            c: &c,
            ids: &mut alice_ids,
        };
        dm.splice_text(&block, 11, 0, "!");
    }
    qdoc::history::commit_stamped_at(&mut alice, T0 + 3 * HOUR);

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

/// THE HEADLINE TEST. Three edits made under two different actors must come
/// back as three rows with the right names on them.
#[test]
fn three_edits_by_two_identities_are_attributed_correctly() {
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

/// The summary is derived from what the change actually did, not from an op
/// count. "hello" is five characters; " world" is six; "!" is one.
#[test]
fn summaries_report_the_characters_actually_typed() {
    let (mut doc, _) = two_identity_document();
    let entries = source().entries(&mut doc);
    let rows = content_rows(&entries);

    assert_eq!(rows[0].summary, "+5 -0 chars", "\"hello\"");
    assert_eq!(rows[1].summary, "+6 -0 chars", "\" world\"");
    assert_eq!(rows[2].summary, "+1 -0 chars", "\"!\"");
    for r in &rows {
        assert_eq!(r.op_class, OpClass::Splice, "text edits are splices");
    }
}

/// A deletion is reported as a deletion, and a change that only removes a block
/// is classed as a tombstone so it lands under the timeline's "Deletes" chip.
#[test]
fn deletions_are_counted_and_a_removed_block_is_a_tombstone() {
    let (mut doc, c, mut ids) = qdoc_doc("alice");
    let (a, b) = {
        let mut dm = qdoc::DocMut {
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
    qdoc::history::commit_stamped_at(&mut doc, T0 + HOUR);

    {
        let mut dm = qdoc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        dm.splice_text(&a, 4, 5, "");
    }
    qdoc::history::commit_stamped_at(&mut doc, T0 + 2 * HOUR);

    {
        let mut dm = qdoc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        dm.remove_block(&b);
    }
    qdoc::history::commit_stamped_at(&mut doc, T0 + 3 * HOUR);

    let entries = source().entries(&mut doc);
    let rows = content_rows(&entries);
    assert_eq!(rows.len(), 3, "three authored edits: {rows:?}");
    assert_eq!(
        rows[1].summary, "+0 -5 chars",
        "deleting \" this\" is five chars"
    );
    assert_eq!(rows[1].op_class, OpClass::Splice);
    assert_eq!(
        rows[2].op_class,
        OpClass::Tombstone,
        "a removed block is a delete"
    );
    assert_eq!(rows[2].summary, "-1 block");
}

/// A document with no qdoc schema still produces real rows via the patch
/// fallback, which is what makes the producer usable over the compositor's
/// chrome document, not just qdoc artifacts.
#[test]
fn a_plain_automerge_document_falls_back_to_patch_summaries() {
    let mut doc = AutoCommit::new();
    doc.set_actor(ActorId::from("alice".as_bytes()));
    let body = doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
    doc.splice_text(&body, 0, 0, "Hello").unwrap();
    qdoc::history::commit_stamped_at(&mut doc, T0);
    doc.splice_text(&body, 5, 0, " world").unwrap();
    qdoc::history::commit_stamped_at(&mut doc, T0 + HOUR);
    doc.put(&ROOT, "title", "Notes").unwrap();
    qdoc::history::commit_stamped_at(&mut doc, T0 + 2 * HOUR);

    let entries = source().entries(&mut doc);
    assert_eq!(entries.len(), 3, "three commits, three rows");
    assert_eq!(entries[0].summary, "+5 -0 chars");
    assert_eq!(entries[1].summary, "+6 -0 chars");
    assert_eq!(entries[1].op_class, OpClass::Splice);
    assert_eq!(entries[2].summary, "1 field", "a map put is metadata");
    assert_eq!(entries[2].op_class, OpClass::EditMeta);
    assert!(
        entries.iter().all(|e| e.author.name == "Alice"),
        "single-actor document, single author"
    );
}

// 2. Restore

/// Restore to an earlier version must bring the earlier content back — and
/// must do it by *appending* to history, never by rewriting it.
#[test]
fn restore_returns_earlier_content_and_appends_a_new_entry() {
    let (doc, block) = two_identity_document();
    let mut state = State::from_doc(doc);

    let before = source().entries(&mut state.doc);
    let rows = content_rows(&before);
    assert_eq!(rows.len(), 3);
    let v1 = rows[0].id;
    let ids_before: Vec<HistoryEntryId> = before.iter().map(|e| e.id).collect();

    let containers = qdoc::model::open(&state.doc, &ROOT).unwrap();
    let text_now = qdoc::model::materialize(&state.doc, &containers).blocks[0]
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

    let containers = qdoc::model::open(&state.doc, &ROOT).unwrap();
    let restored = qdoc::model::materialize(&state.doc, &containers).blocks[0]
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
        &qdoc::model::materialize(&state.doc, &containers).blocks[0].id
    );
}

/// The restore lands on the undo stack, so a user who scrubbed by mistake can
/// Ctrl+Z out of it.
#[test]
fn a_restore_is_undoable() {
    let (doc, _) = two_identity_document();
    let mut state = State::from_doc(doc);
    let rows_ids: Vec<HistoryEntryId> = content_rows(&source().entries(&mut state.doc))
        .iter()
        .map(|e| e.id)
        .collect();
    let target = heads_for_entry(&mut state.doc, &rows_ids[0]).unwrap();

    state.restore_to_heads(&target, None, None, Some(UserId("alice".into())));
    let text = |s: &State| {
        let c = qdoc::model::open(&s.doc, &ROOT).unwrap();
        qdoc::model::materialize(&s.doc, &c).blocks[0].text.clone()
    };
    assert_eq!(text(&state), "hello");

    state
        .undo(None, None)
        .expect("the restore is on the undo stack");
    assert_eq!(text(&state), "hello world!", "undo puts the restore back");
}

/// Restoring to the state the document is already in is a no-op — it must not
/// litter the timeline with empty entries.
#[test]
fn restoring_to_the_current_version_does_nothing() {
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

/// A row id from a different document addresses nothing here.
#[test]
fn an_unknown_entry_id_resolves_to_no_version() {
    let (mut doc, _) = two_identity_document();
    assert_eq!(heads_for_entry(&mut doc, &HistoryEntryId([0xab; 16])), None);
}

// 3. Filters, through the real widget

/// Filtering by contributor must actually filter. Asserted through
/// `ActivityTimeline`, not `HistoryFilter::matches` in isolation.
#[test]
fn filtering_by_contributor_hides_the_other_authors_rows() {
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

/// Filtering by date range must actually filter. The producer's timestamps are
/// the ones the commits carry, so a window that brackets one edit shows one row.
#[test]
fn filtering_by_date_range_selects_only_edits_inside_the_window() {
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

/// The two filters compose: contributor AND window.
#[test]
fn contributor_and_date_filters_intersect() {
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

// Timestamps + authorship edge cases

/// A stamped commit produces a real timestamp, so date filters have
/// something to compare against.
#[test]
fn stamped_commits_carry_their_wall_clock_into_the_row() {
    let (mut doc, _) = two_identity_document();
    let entries = source().entries(&mut doc);
    let rows = content_rows(&entries);
    assert_eq!(rows[0].timestamp, at(T0 + HOUR));
    assert_eq!(rows[1].timestamp, at(T0 + 2 * HOUR));
    assert_eq!(rows[2].timestamp, at(T0 + 3 * HOUR));
}

/// `State::commit_action` — the path every `ui` app takes — stamps its changes.
#[test]
fn state_commit_action_stamps_the_change() {
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

/// An actor nobody registered gets an honest label and a stable colour — never
/// a made-up name.
#[test]
fn an_unregistered_actor_is_named_honestly_in_a_produced_row() {
    let mut doc = AutoCommit::new();
    // High bytes: not a decodable handle, so this is a true stranger.
    let stranger: Vec<u8> = (0u8..32).map(|i| 0x80u8.wrapping_add(i)).collect();
    doc.set_actor(ActorId::from(stranger.as_slice()));
    let body = doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
    doc.splice_text(&body, 0, 0, "who wrote this").unwrap();
    qdoc::history::commit_stamped_at(&mut doc, T0);

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

/// An empty document produces an empty timeline rather than a phantom row.
#[test]
fn an_untouched_document_has_no_history() {
    let mut doc = AutoCommit::new();
    assert!(HistorySource::new().entries(&mut doc).is_empty());
}

/// The window cap keeps the newest rows, not the oldest.
#[test]
fn the_entry_cap_keeps_the_most_recent_versions() {
    let (mut doc, c, mut ids) = qdoc_doc("alice");
    let p = {
        let mut dm = qdoc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        dm.insert_block(None, "paragraph")
    };
    qdoc::history::commit_stamped_at(&mut doc, T0);
    for i in 0..6 {
        {
            let mut dm = qdoc::DocMut {
                doc: &mut doc,
                c: &c,
                ids: &mut ids,
            };
            dm.splice_text(&p, i, 0, "x");
        }
        let i = i64::try_from(i).unwrap_or(i64::MAX);
        qdoc::history::commit_stamped_at(&mut doc, T0 + (i + 1) * HOUR);
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

/// Entry ids are the change's own content address, so two peers building a
/// timeline over the same history agree on which row is which.
#[test]
fn entry_ids_are_stable_across_peers() {
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

#[test]
fn semantic_action_context_becomes_the_row_summary_and_browse_metadata() {
    let (mut doc, c, mut ids) = qdoc_doc("alice");
    let mut dm = qdoc::DocMut {
        doc: &mut doc,
        c: &c,
        ids: &mut ids,
    };
    let block = dm.insert_block(None, "paragraph");
    dm.splice_text(&block, 0, 0, "launch");
    let context = tilo_actions::ActionContext::for_resource(
        "Edit",
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

/// `prefix_heads` has to handle a merged, non-linear change graph, and
/// materialising through it must reproduce the content the timeline claims.
#[test]
fn heads_address_a_merged_history_correctly() {
    let (mut doc, _) = two_identity_document();
    let entries = source().entries(&mut doc);
    let rows = content_rows(&entries);

    for (row, expected) in rows.iter().zip(["hello", "hello world", "hello world!"]) {
        let idx = version_index_for_entry(&mut doc, &row.id).expect("row addresses a version");
        let snap = qdoc::history::materialize_at(&mut doc, idx).expect("version materialises");
        assert_eq!(
            snap.blocks[0].text, expected,
            "version {idx} should read {expected:?}"
        );
        let heads = qdoc::history::prefix_heads(&mut doc, idx);
        assert!(!heads.is_empty(), "every version has at least one head");
    }
}
