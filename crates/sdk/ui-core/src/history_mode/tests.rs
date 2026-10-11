use automerge::transaction::{CommitOptions, Transactable};
use automerge::{ActorId, AutoCommit, ROOT, ReadDoc};
use tilo_actions::{ActionContext, ActionFacet};

use super::*;
use crate::history_source::entry_id_from_hash;

fn resource(profile: &str, workspace: &str) -> HistoryResource {
    HistoryResource::new(
        profile,
        workspace,
        "dev.tilo.history-test",
        RecordId::parse("uuid:00000000-0000-0000-0000-000000000001").expect("record id"),
    )
}

fn value(doc: &AutoCommit, key: &str) -> Option<String> {
    doc.get(ROOT, key)
        .expect("read value")
        .and_then(|(value, _)| value.as_str().map(str::to_owned))
}

fn load(snapshot: &HistorySnapshot) -> AutoCommit {
    AutoCommit::load(snapshot.bytes()).expect("history snapshot is an Automerge document")
}

fn two_titles() -> (AutoCommit, HistoryEntryId, HistoryEntryId) {
    let mut doc = AutoCommit::new();
    doc.set_actor(ActorId::from("alice".as_bytes()));
    doc.put(ROOT, "title", "one").unwrap();
    let one = doc.commit().expect("first title commits");
    doc.put(ROOT, "title", "two").unwrap();
    let two = doc.commit().expect("second title commits");
    (
        doc,
        entry_id_from_hash(&one.to_string()),
        entry_id_from_hash(&two.to_string()),
    )
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn selected_snapshots_are_exact_isolated_and_reachable() {
    // phase: selected_snapshot_is_isolated_from_the_live_document
    {
        // Catches materializing a historical selection through the live document,
        // which would either show the latest value or advance the live frontier.
        let (mut doc, one, _) = two_titles();
        let live_heads = doc.get_heads();
        let mut history =
            HistoryController::new(resource("profile-a", "workspace-a"), &doc).unwrap();

        history.select(&doc, &[one]).unwrap();

        let historical = load(history.snapshot().expect("selected snapshot"));
        assert_eq!(value(&historical, "title").as_deref(), Some("one"));
        assert_eq!(value(&doc, "title").as_deref(), Some("two"));
        assert_eq!(doc.get_heads(), live_heads);
    }
    // phase: pending_live_edits_are_neither_committed_nor_projected_as_history
    {
        // Catches calling get_heads/entries on the live document or on a clone
        // before discarding its open transaction. Either mistake turns speculative
        // UI state into a synthetic durable history entry.
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "title", "committed").unwrap();
        let committed = doc.commit().unwrap();
        doc.put(ROOT, "title", "draft").unwrap();
        assert_eq!(doc.pending_ops(), 1);

        let mut history =
            HistoryController::new(resource("profile-a", "workspace-a"), &doc).unwrap();
        assert_eq!(history.entries().len(), 1);
        assert_eq!(
            history.entries()[0].id,
            entry_id_from_hash(&committed.to_string())
        );
        assert_eq!(doc.pending_ops(), 1);
        assert_eq!(value(&doc, "title").as_deref(), Some("draft"));

        history
            .select(&doc, &[entry_id_from_hash(&committed.to_string())])
            .unwrap();
        let review = history.review_restore().unwrap();
        assert_eq!(
            history.validate_restore(&review, &doc),
            Err(HistoryError::UncommittedLiveChanges)
        );
        assert_eq!(doc.pending_ops(), 1);
        assert_eq!(value(&doc, "title").as_deref(), Some("draft"));
    }
    // phase: selected_change_uses_its_causal_frontier_without_an_unrelated_branch
    {
        // Catches deriving versions from causal-list positions: a concurrent edit
        // ordered before the selected change would leak into its preview.
        let mut base = AutoCommit::new();
        base.set_actor(ActorId::from("seed".as_bytes()));
        base.put(ROOT, "seed", true).unwrap();
        base.commit().unwrap();

        let mut selected_branch = base.fork();
        selected_branch.set_actor(ActorId::from("alice".as_bytes()));
        selected_branch.put(ROOT, "title", "alice-only").unwrap();
        let selected_hash = selected_branch.commit().unwrap();

        let mut unrelated_branch = base.fork();
        unrelated_branch.set_actor(ActorId::from("bob".as_bytes()));
        unrelated_branch.put(ROOT, "status", "bob-only").unwrap();
        unrelated_branch.commit().unwrap();
        selected_branch.merge(&mut unrelated_branch).unwrap();

        let selected = entry_id_from_hash(&selected_hash.to_string());
        let mut history =
            HistoryController::new(resource("profile-a", "workspace-a"), &selected_branch).unwrap();
        history.select(&selected_branch, &[selected]).unwrap();

        let snapshot = history.snapshot().unwrap();
        let historical = load(snapshot);
        assert_eq!(value(&historical, "title").as_deref(), Some("alice-only"));
        assert_eq!(value(&historical, "status"), None);
        assert_eq!(snapshot.heads(), &[selected_hash]);
    }
    // phase: controller_keeps_older_entries_reachable_beyond_its_first_page
    {
        // The controller lists the newest page first; an older durable
        // version stays a request away and selectable, never gone.
        let mut doc = AutoCommit::new();
        let mut oldest = None;
        let total = super::HISTORY_PAGE + 5;
        for revision in 0..total as u64 {
            doc.put(ROOT, "revision", revision).unwrap();
            let committed = doc.commit().unwrap();
            oldest.get_or_insert(committed);
        }
        let oldest = entry_id_from_hash(&oldest.unwrap().to_string());

        let mut history =
            HistoryController::new(resource("profile-a", "workspace-a"), &doc).unwrap();
        assert_eq!(history.entries().len(), super::HISTORY_PAGE);
        assert!(history.has_earlier());
        history.show_earlier(&doc).unwrap();
        assert_eq!(history.entries().len(), total);
        assert!(!history.has_earlier());
        history.select(&doc, &[oldest]).unwrap();

        let snapshot = load(history.snapshot().unwrap());
        assert_eq!(
            snapshot.get(ROOT, "revision").unwrap().unwrap().0.as_u64(),
            Some(0)
        );
    }
    // phase: comparison_switches_between_exact_before_and_after_snapshots
    {
        // Catches implementing compare as a visual flag over one snapshot rather
        // than two exact graph frontiers.
        let (doc, _, two) = two_titles();
        let mut history =
            HistoryController::new(resource("profile-a", "workspace-a"), &doc).unwrap();
        history.select(&doc, &[two]).unwrap();

        history.select_compare_side(PreviewSide::Before).unwrap();
        assert_eq!(
            value(&load(history.snapshot().unwrap()), "title").as_deref(),
            Some("one")
        );

        history.select_compare_side(PreviewSide::After).unwrap();
        assert_eq!(
            value(&load(history.snapshot().unwrap()), "title").as_deref(),
            Some("two")
        );
    }
}

#[test]
fn the_pinned_preview_survives_refresh_until_back_to_live() {
    // phase: refresh_keeps_the_pinned_preview_and_back_to_live_clears_it
    {
        // Catches refresh silently advancing a preview, and catches a failed
        // selection destroying the last valid historical state.
        let (mut doc, one, _) = two_titles();
        let mut history =
            HistoryController::new(resource("profile-a", "workspace-a"), &doc).unwrap();
        history.select(&doc, &[one]).unwrap();
        let pinned = history.snapshot().unwrap().bytes().to_vec();

        doc.set_actor(ActorId::from("bob".as_bytes()));
        doc.put(ROOT, "remote", "arrived").unwrap();
        doc.commit().unwrap();
        history.refresh(&doc).unwrap();

        assert!(history.has_live_updates());
        assert_eq!(history.snapshot().unwrap().bytes(), pinned.as_slice());
        assert!(matches!(
            history.select(&doc, &[HistoryEntryId::from_bytes([0xff; 16])]),
            Err(HistoryError::UnknownEntry(_))
        ));
        assert_eq!(history.snapshot().unwrap().bytes(), pinned.as_slice());

        history.back_to_live();
        assert!(history.snapshot().is_none());
        assert!(!history.has_live_updates());
    }
    // phase: select_before_refresh_updates_projection_without_advancing_the_pinned_preview
    {
        // Catches select advancing the factual-projection cache key without
        // rebuilding its entries, which makes a later refresh skip the new row.
        let (mut doc, one, _) = two_titles();
        let mut history =
            HistoryController::new(resource("profile-a", "workspace-a"), &doc).unwrap();

        doc.put(ROOT, "remote", "third committed change").unwrap();
        let third = doc.commit().unwrap();
        let third = entry_id_from_hash(&third.to_string());

        history.select(&doc, &[one]).unwrap();
        let pinned = history.snapshot().unwrap().bytes().to_vec();
        assert!(history.has_live_updates());
        assert!(history.entries().iter().any(|entry| entry.id == third));

        history.refresh(&doc).unwrap();
        assert!(history.entries().iter().any(|entry| entry.id == third));
        assert_eq!(history.snapshot().unwrap().bytes(), pinned.as_slice());
        assert!(history.has_live_updates());
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn restore_review_is_scoped_to_a_preview_and_refuses_effect_receipts() {
    // phase: restore_review_is_scoped_and_invalidated_by_selection_or_live_changes
    {
        // Catches replaying a review token in another account/workspace, after a
        // selection change, or against a different current live frontier.
        let (mut doc, one, two) = two_titles();
        let original_heads = doc.get_heads();
        let mut history =
            HistoryController::new(resource("profile-a", "workspace-a"), &doc).unwrap();
        history.select(&doc, &[one]).unwrap();
        let review = history.review_restore().unwrap();
        assert_eq!(history.validate_restore(&review, &doc), Ok(()));
        assert_eq!(doc.get_heads(), original_heads, "validation is read-only");

        for mismatched in [
            resource("profile-b", "workspace-a"),
            resource("profile-a", "workspace-b"),
            HistoryResource::new(
                "profile-a",
                "workspace-a",
                "dev.tilo.other-app",
                RecordId::parse("uuid:00000000-0000-0000-0000-000000000001").unwrap(),
            ),
            HistoryResource::new(
                "profile-a",
                "workspace-a",
                "dev.tilo.history-test",
                RecordId::parse("uuid:00000000-0000-0000-0000-000000000002").unwrap(),
            ),
        ] {
            let mut other = HistoryController::new(mismatched, &doc).unwrap();
            other.select(&doc, &[one]).unwrap();
            assert_eq!(
                other.validate_restore(&review, &doc),
                Err(HistoryError::ResourceMismatch)
            );
        }

        history.select(&doc, &[two]).unwrap();
        assert_eq!(
            history.validate_restore(&review, &doc),
            Err(HistoryError::ReviewInvalidated)
        );

        history.select(&doc, &[one]).unwrap();
        let stale_review = history.review_restore().unwrap();
        doc.put(ROOT, "remote", "new live state").unwrap();
        doc.commit().unwrap();
        let changed_heads = doc.get_heads();
        assert_eq!(
            history.validate_restore(&stale_review, &doc),
            Err(HistoryError::StaleLiveHeads)
        );
        assert_eq!(doc.get_heads(), changed_heads, "validation is read-only");
    }
    // phase: restore_review_requires_a_preview_and_refuses_effect_receipts
    {
        // Catches treating durable proof of an external effect as restorable app
        // content; reverting its CRDT receipt cannot undo the external effect.
        let (mut doc, _, _) = two_titles();
        let mut history =
            HistoryController::new(resource("profile-a", "workspace-a"), &doc).unwrap();
        assert_eq!(history.review_restore(), Err(HistoryError::NoPreview));

        let context = ActionContext::for_resource("uuid:document-1", "document", "Document")
            .with_app("dev.tilo.history-test", "History test")
            .with_facet(ActionFacet::new("tilo.receipt", "effect"));
        doc.put(ROOT, "last_export_receipt", "receipt-1").unwrap();
        let receipt = doc
            .commit_with(
                CommitOptions::default()
                    .with_message(context.change_message().expect("effect context encodes")),
            )
            .unwrap();
        history.refresh(&doc).unwrap();
        history
            .select(&doc, &[entry_id_from_hash(&receipt.to_string())])
            .unwrap();

        assert_eq!(
            history.review_restore(),
            Err(HistoryError::EffectOnlySelection)
        );
    }
    // phase: newly_committed_effect_receipt_cannot_bypass_review_before_refresh
    {
        // Catches classifying a selected current-document change from stale
        // cached entries rather than the same committed graph used for preview.
        let (mut doc, one, _) = two_titles();
        let mut history =
            HistoryController::new(resource("profile-a", "workspace-a"), &doc).unwrap();
        history.select(&doc, &[one]).unwrap();
        let prior_review = history.review_restore().unwrap();

        let context = ActionContext::for_resource("uuid:document-1", "document", "Document")
            .with_app("dev.tilo.history-test", "History test")
            .with_facet(ActionFacet::new("tilo.receipt", "effect"));
        doc.put(ROOT, "last_export_receipt", "receipt-before-refresh")
            .unwrap();
        let receipt = doc
            .commit_with(
                CommitOptions::default()
                    .with_message(context.change_message().expect("effect context encodes")),
            )
            .unwrap();

        history
            .select(&doc, &[entry_id_from_hash(&receipt.to_string())])
            .unwrap();
        assert_eq!(
            history.review_restore(),
            Err(HistoryError::EffectOnlySelection)
        );
        assert_eq!(
            history.validate_restore(&prior_review, &doc),
            Err(HistoryError::ReviewInvalidated)
        );
    }
}
