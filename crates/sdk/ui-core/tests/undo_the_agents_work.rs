//! Undoing what an agent did — across one document, and across two.
//!
//! Everything here drives the production path: real `automerge` documents with
//! the real `qdoc` schema the word processor stores, real replication by
//! fork/merge and by `State::load_incremental` (the code path a peer's CRDT
//! delta actually arrives through), the real `tilo_ui::history_source` producer, and
//! the real revert. Nothing is stubbed, and every assertion that matters is on
//! **document content** rather than on a return value — a revert that reported
//! success while leaving the text alone fails these tests.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use tilo_ui::history_source::revert::{RevertError, Slot, latest_run_by, plan_revert, revert_run};
use tilo_ui::history_source::run_undo::{StepState, refuse_run, undo_run, unreachable_steps};
use tilo_ui::history_source::runs::{AgentRun, RunStep, read_runs, record_run};
use tilo_ui::history_source::{AuthorDirectory, HistorySource, entry_id_from_hash};
use tilo_ui::permissions::UserId;
use tilo_ui::state::State;

/// The agent commits under a stable actor derived from its name — see
/// `apps/agent/src/apply.rs`, which does `ActorId::from(name.as_bytes())`. That
/// is what makes its work a findable run, and what makes `actor_handle` decode
/// the name back without any directory registration.
const AGENT: &str = "Claude";
const PERSON: &str = "alice";

const WORD_PROCESSOR: &str = "so.tilo.word-processor";
const PHOTO_EDITOR: &str = "so.tilo.photos";

fn actor_hex(handle: &str) -> String {
    ActorId::from(handle.as_bytes()).to_hex_string()
}

// ---------------------------------------------------------------------------
// The word processor's document: the real qdoc schema.
// ---------------------------------------------------------------------------

struct Report {
    doc: AutoCommit,
    c: tilo_doc::Containers,
    /// One id generator per participant, held for the participant's whole
    /// session — as the app holds one. A fresh generator per edit would restart
    /// the counter and mint block ids that collide with that participant's own
    /// earlier blocks, silently overwriting them.
    ids: std::collections::HashMap<String, tilo_doc::IdGen>,
}

impl Report {
    /// A report the person has already started writing.
    fn started_by_a_person() -> (Self, String) {
        let mut doc = AutoCommit::new();
        doc.set_actor(ActorId::from(PERSON.as_bytes()));
        let c = tilo_doc::model::init(&mut doc, &ROOT);
        tilo_resource::history::commit_stamped_at(&mut doc, 1_780_000_000);

        let mut report = Self {
            doc,
            c,
            ids: std::collections::HashMap::new(),
        };
        let intro = report.edit_as(PERSON, 1_780_000_100, |dm| {
            let id = dm.insert_block(None, "paragraph");
            dm.splice_text(&id, 0, 0, "Our findings for the quarter.");
            id
        });
        (report, intro)
    }

    /// Edit as `handle`, committing once — one change, one author, like the app.
    fn edit_as<R>(
        &mut self,
        handle: &str,
        at: i64,
        f: impl FnOnce(&mut tilo_doc::DocMut) -> R,
    ) -> R {
        self.doc.set_actor(ActorId::from(handle.as_bytes()));
        let actor = self.doc.get_actor().to_hex_string();
        let ids = self
            .ids
            .entry(actor.clone())
            .or_insert_with(|| tilo_doc::IdGen::new(actor));
        let out = {
            let mut dm = tilo_doc::DocMut {
                doc: &mut self.doc,
                c: &self.c,
                ids,
            };
            f(&mut dm)
        };
        tilo_resource::history::commit_stamped_at(&mut self.doc, at);
        out
    }

    fn text(&self) -> Vec<String> {
        tilo_doc::model::materialize(&self.doc, &self.c)
            .blocks
            .into_iter()
            .map(|b| b.text)
            .collect()
    }

    fn body(&self) -> String {
        self.text().join("\n")
    }
}

/// The two changes an agent makes for "add a closing line and caption the
/// figure": one new paragraph, one caption. Returns the ids of both blocks.
fn agent_writes_two_paragraphs(report: &mut Report) -> (String, String) {
    let closing = report.edit_as(AGENT, 1_780_000_200, |dm| {
        let id = dm.insert_block(None, "paragraph");
        dm.splice_text(&id, 0, 0, "Revenue grew by a fifth.");
        id
    });
    let caption = report.edit_as(AGENT, 1_780_000_300, |dm| {
        let id = dm.insert_block(None, "paragraph");
        dm.splice_text(&id, 0, 0, "Figure 1: quarterly revenue.");
        id
    });
    (closing, caption)
}

/// The run covering everything the agent authored, found the way a caller with
/// no recorded run finds it: the maximal contiguous band under the agent's
/// actor.
fn agents_run(doc: &mut AutoCommit) -> tilo_ui::history_source::revert::ChangeRun {
    latest_run_by(doc, &actor_hex(AGENT)).expect("the agent authored something")
}

fn directory() -> AuthorDirectory {
    AuthorDirectory::new()
}

// ---------------------------------------------------------------------------
// The premise: a remote author's work is not on anybody's undo stack.
// ---------------------------------------------------------------------------

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn agent_work_is_undone_through_the_document_not_the_peer_stack() {
    // Case `a_peer_cannot_reach_the_agents_work_through_the_undo_stack`.
    {
        // Alice's session, holding the document as `tilo_ui::State` — the real thing an
        // app runs on.
        let (mut report, _intro) = Report::started_by_a_person();
        let mut alice = State::from_doc(report.doc.fork());

        // The agent edits its own replica and ships a delta, exactly as
        // `apps/agent/src/session.rs` does: `save_incremental` over the wire,
        // `load_incremental` on the receiving side.
        agent_writes_two_paragraphs(&mut report);
        let delta = report.doc.save_incremental();
        alice
            .load_incremental(&delta)
            .expect("a peer's delta merges");

        assert!(
            alice.doc.get_heads() == report.doc.get_heads(),
            "the peer really has the agent's changes"
        );

        // And yet: nothing to undo. This is the whole reason the revert path exists
        // — `commit_action_authored` is the only thing that pushes an undo entry,
        // and it is only ever called for a locally handled event.
        assert!(
            alice
                .undo_for_author(&UserId(AGENT.into()), None, None)
                .is_none(),
            "a remote author's work never reaches the local undo stack"
        );
        assert!(
            alice.undo(None, None).is_none(),
            "and there is no anonymous entry standing in for it either"
        );
    }

    // Case `the_peer_can_undo_the_agents_work_by_addressing_it_in_the_document`.
    {
        let (mut report, _intro) = Report::started_by_a_person();
        let before = report.body();
        let mut alice = State::from_doc(report.doc.fork());

        agent_writes_two_paragraphs(&mut report);
        alice
            .load_incremental(&report.doc.save_incremental())
            .expect("merge");

        let run = agents_run(&mut alice.doc);
        let out = revert_run(&mut alice.doc, &run, Some(&actor_hex(AGENT)), &directory())
            .expect("the run is in this document");
        // The caller commits — that is what stamps and attributes the undo.
        alice.commit_action_authored(None, None, Some(UserId(PERSON.into())));

        assert_eq!(out.covered, 2, "both of the agent's changes were addressed");
        assert!(out.is_complete(), "nothing was held back: {:?}", out.kept);
        assert!(out.wrote, "the undo actually wrote something");

        let c = tilo_doc::model::open(&alice.doc, &ROOT).expect("schema");
        let body: Vec<String> = tilo_doc::model::materialize(&alice.doc, &c)
            .blocks
            .into_iter()
            .map(|b| b.text)
            .collect();
        assert_eq!(
            body.join("\n"),
            before,
            "the document is back to what the person had written"
        );
    }

    // Case `the_undo_is_a_new_change_and_leaves_the_agents_authorship_standing`.
    {
        let (mut report, _intro) = Report::started_by_a_person();
        agent_writes_two_paragraphs(&mut report);

        let versions_before = tilo_resource::history::versions(&mut report.doc).len();
        let run = agents_run(&mut report.doc);
        report.doc.set_actor(ActorId::from(PERSON.as_bytes()));
        revert_run(&mut report.doc, &run, Some(&actor_hex(AGENT)), &directory()).expect("run");
        tilo_resource::history::commit_stamped_at(&mut report.doc, 1_780_000_400);

        let versions = tilo_resource::history::versions(&mut report.doc);
        assert!(
            versions.len() > versions_before,
            "history grew: {} -> {}",
            versions_before,
            versions.len()
        );
        assert_eq!(
            versions.last().unwrap().actor,
            actor_hex(PERSON),
            "the undo is attributed to the person who asked for it"
        );
        assert!(
            versions.iter().any(|v| v.actor == actor_hex(AGENT)),
            "and the agent's own changes are still in the history, still its own"
        );

        // The history pane still names the agent, with no directory registration —
        // the actor round-trips through `actor_handle`.
        let rows = HistorySource::new().entries(&mut report.doc);
        assert!(
            rows.iter().any(|r| r.author.name == AGENT),
            "the timeline names the agent: {:?}",
            rows.iter()
                .map(|r| r.author.name.clone())
                .collect::<Vec<_>>()
        );
    }

    // Case `every_peer_converges_on_the_undo`.
    {
        let (mut report, _intro) = Report::started_by_a_person();
        agent_writes_two_paragraphs(&mut report);

        let mut bob = report.doc.fork();
        bob.set_actor(ActorId::from("bob".as_bytes()));

        let run = agents_run(&mut report.doc);
        report.doc.set_actor(ActorId::from(PERSON.as_bytes()));
        revert_run(&mut report.doc, &run, Some(&actor_hex(AGENT)), &directory()).expect("run");
        tilo_resource::history::commit_stamped_at(&mut report.doc, 1_780_000_400);

        bob.merge(&mut report.doc).expect("peers merge");

        let cb = tilo_doc::model::open(&bob, &ROOT).expect("schema");
        let bob_text: Vec<String> = tilo_doc::model::materialize(&bob, &cb)
            .blocks
            .into_iter()
            .map(|b| b.text)
            .collect();
        assert_eq!(
            bob_text,
            report.text(),
            "both peers see the same document after the undo"
        );
        assert_eq!(
            bob_text.join("\n"),
            "Our findings for the quarter.",
            "and what they see is the state before the agent wrote"
        );
    }
}

// ---------------------------------------------------------------------------
// Somebody built on it.
// ---------------------------------------------------------------------------

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn undo_keeps_later_human_edits_and_is_idempotent() {
    // Case `a_paragraph_the_person_edited_afterwards_is_kept_and_reported`.
    {
        let (mut report, _intro) = Report::started_by_a_person();
        let (_closing, caption) = agent_writes_two_paragraphs(&mut report);

        // The person rewrites the agent's caption — building on its work.
        report.edit_as(PERSON, 1_780_000_350, |dm| {
            dm.splice_text(&caption, 0, 0, "Revised. ");
        });

        let run = agents_run(&mut report.doc);
        let mut dir = directory();
        dir.register(
            &actor_hex(PERSON),
            tilo_ui::widget::history::HistoryAuthor::new("Alice", 200.0, "a11ce000"),
        );

        // The dry run says what will happen before anything is written.
        let planned =
            plan_revert(&mut report.doc, &run, Some(&actor_hex(AGENT)), &dir).expect("run");
        assert!(
            !planned.is_complete(),
            "the plan already knows this will be partial"
        );

        report.doc.set_actor(ActorId::from(PERSON.as_bytes()));
        let out = revert_run(&mut report.doc, &run, Some(&actor_hex(AGENT)), &dir).expect("run");
        tilo_resource::history::commit_stamped_at(&mut report.doc, 1_780_000_400);

        assert!(
            !out.is_complete(),
            "a revert that kept something must not read as complete"
        );
        assert_eq!(
            out.blocked_by()
                .iter()
                .map(|a| a.name.clone())
                .collect::<Vec<_>>(),
            vec!["Alice".to_string()],
            "and it names who held it back"
        );

        // The load-bearing assertion: her words are still there.
        let body = report.body();
        assert!(
            body.contains("Revised. Figure 1: quarterly revenue."),
            "the paragraph she edited was left exactly as it is: {body:?}"
        );
        assert!(
            !body.contains("Revenue grew by a fifth."),
            "the paragraph nobody touched was undone: {body:?}"
        );
        assert!(
            body.contains("Our findings for the quarter."),
            "and her own original paragraph survived: {body:?}"
        );
    }

    // Case `a_persons_edit_elsewhere_does_not_block_the_undo`.
    {
        // Adding a paragraph next to the agent's is not building on it. If a shared
        // parent container counted as a conflict, nothing would ever be undoable in
        // a document somebody is still working in.
        let (mut report, intro) = Report::started_by_a_person();
        agent_writes_two_paragraphs(&mut report);
        report.edit_as(PERSON, 1_780_000_350, |dm| {
            dm.splice_text(&intro, 0, 0, "Draft. ");
            let id = dm.insert_block(None, "paragraph");
            dm.splice_text(&id, 0, 0, "My own note.");
        });

        let run = agents_run(&mut report.doc);
        report.doc.set_actor(ActorId::from(PERSON.as_bytes()));
        let out =
            revert_run(&mut report.doc, &run, Some(&actor_hex(AGENT)), &directory()).expect("run");
        tilo_resource::history::commit_stamped_at(&mut report.doc, 1_780_000_400);

        assert!(
            out.is_complete(),
            "her separate work is not a reason to hold back: {:?}",
            out.kept
        );
        let body = report.body();
        assert!(
            body.contains("Draft. Our findings for the quarter."),
            "her edit to her own paragraph survived: {body:?}"
        );
        assert!(
            body.contains("My own note."),
            "her paragraph survived: {body:?}"
        );
        assert!(!body.contains("Revenue grew by a fifth."), "{body:?}");
        assert!(!body.contains("Figure 1"), "{body:?}");
    }

    // Case `a_field_somebody_overwrote_afterwards_is_kept`.
    {
        let (_room, mut photo, _report, run) = one_instruction_two_apps();

        photo.set_actor(ActorId::from(PERSON.as_bytes()));
        let (_, img) = photo.get(ROOT, "image").unwrap().unwrap();
        photo.put(&img, "crop_w", 1600).unwrap();
        tilo_resource::history::commit_stamped_at(&mut photo, 1_780_000_450);

        let step = run
            .step_for(PHOTO_EDITOR, "file:/photos/figure-1.png")
            .unwrap();
        let out =
            revert_run(&mut photo, &step.run, Some(&actor_hex(AGENT)), &directory()).expect("run");
        tilo_resource::history::commit_stamped_at(&mut photo, 1_780_000_500);

        assert!(
            !out.is_complete(),
            "the overwritten field held part of it back"
        );
        assert!(
            out.kept
                .iter()
                .any(|k| matches!(&k.slot, Slot::Field(_, k2) if k2 == "crop_w")),
            "and says which field: {:?}",
            out.kept
        );
        assert_eq!(
            crop(&photo),
            (1600, 3000),
            "her width stands; the height nobody touched is back"
        );
    }

    // Case `undoing_a_run_twice_does_not_undo_it_twice_over`.
    {
        let (mut report, _intro) = Report::started_by_a_person();
        let before = report.body();
        agent_writes_two_paragraphs(&mut report);
        let run = agents_run(&mut report.doc);

        report.doc.set_actor(ActorId::from(PERSON.as_bytes()));
        revert_run(&mut report.doc, &run, Some(&actor_hex(AGENT)), &directory()).expect("run");
        tilo_resource::history::commit_stamped_at(&mut report.doc, 1_780_000_400);
        let once = report.body();

        let again =
            revert_run(&mut report.doc, &run, Some(&actor_hex(AGENT)), &directory()).expect("run");
        tilo_resource::history::commit_stamped_at(&mut report.doc, 1_780_000_500);

        assert_eq!(report.body(), once, "the second undo changed nothing");
        assert_eq!(once, before, "and the first one did the whole job");
        assert!(
            !again.wrote,
            "a run whose effect is already gone reports that it wrote nothing"
        );
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn runs_span_applications_and_are_refused_whole_when_unresolvable() {
    // Case `a_run_from_another_document_is_refused_rather_than_guessed_at`.
    {
        let (mut report, _intro) = Report::started_by_a_person();
        agent_writes_two_paragraphs(&mut report);
        let run = agents_run(&mut report.doc);

        // A different document entirely — the ids name nothing here.
        let (mut other, _) = Report::started_by_a_person();
        assert_eq!(
            revert_run(&mut other.doc, &run, Some(&actor_hex(AGENT)), &directory()),
            Err(RevertError::UnknownRun),
            "an id from another artifact is refused, not applied to whatever matches"
        );
    }

    // Case `one_instruction_across_two_applications_is_one_undo`.
    {
        let (_room, mut photo, mut report, run) = one_instruction_two_apps();

        assert_eq!(crop(&photo), (1200, 900), "the agent did crop it");
        assert!(report.body().contains("Figure 1"), "and did place it");
        assert_eq!(
            run.apps(),
            vec![PHOTO_EDITOR, WORD_PROCESSOR],
            "the run knows both applications"
        );

        // Each app undoes its own document; nothing holds both.
        photo.set_actor(ActorId::from(PERSON.as_bytes()));
        report.doc.set_actor(ActorId::from(PERSON.as_bytes()));
        let mut open: Vec<(String, String, &mut AutoCommit)> = vec![
            (
                PHOTO_EDITOR.into(),
                "file:/photos/figure-1.png".into(),
                &mut photo,
            ),
            (
                WORD_PROCESSOR.into(),
                "file:/reports/q3.qdoc".into(),
                &mut report.doc,
            ),
        ];
        let report_out = undo_run(&run, Some(&actor_hex(AGENT)), &directory(), &mut open);
        drop(open);
        tilo_resource::history::commit_stamped_at(&mut photo, 1_780_000_500);
        tilo_resource::history::commit_stamped_at(&mut report.doc, 1_780_000_500);

        assert!(
            report_out.is_complete(),
            "both halves came back clean: {report_out:?}"
        );
        assert_eq!(
            crop(&photo),
            (4000, 3000),
            "the crop is undone in the photo editor's own document"
        );
        assert!(
            !report.body().contains("Figure 1"),
            "and the placement is undone in the word processor's: {:?}",
            report.body()
        );
        assert!(
            report.body().contains("Our findings for the quarter."),
            "without touching what the person wrote"
        );
    }

    // Case `a_run_is_refused_whole_when_one_application_is_not_open`.
    {
        let (_room, photo, mut report, run) = one_instruction_two_apps();
        let untouched = report.body();
        let photo_before = crop(&photo);

        let mut open: Vec<(String, String, &mut AutoCommit)> = vec![(
            WORD_PROCESSOR.into(),
            "file:/reports/q3.qdoc".into(),
            &mut report.doc,
        )];
        let out = undo_run(&run, Some(&actor_hex(AGENT)), &directory(), &mut open);
        drop(open);

        assert!(out.is_refusal(), "nothing was attempted: {out:?}");
        assert!(!out.is_complete(), "and it must not read as done");
        assert_eq!(
            out.unreachable_apps(),
            vec![PHOTO_EDITOR],
            "the refusal names the application that was missing"
        );
        assert_eq!(
            report.body(),
            untouched,
            "the reachable document was left alone rather than half-undone"
        );
        assert_eq!(crop(&photo), photo_before, "and so was the other one");
    }

    // Case `a_pre_flight_names_every_missing_application_before_anything_is_written`.
    {
        let (_room, _photo, _report, run) = one_instruction_two_apps();
        let missing = unreachable_steps(&run, |_| false);
        assert_eq!(missing.len(), 2, "nothing is open");
        let refused = refuse_run(&run, &missing);
        assert!(refused.is_refusal());
        assert_eq!(
            refused.unreachable_apps(),
            vec![PHOTO_EDITOR, WORD_PROCESSOR]
        );

        // With one open, the other is still named — and the report distinguishes
        // "could not" from "chose not to".
        let missing = unreachable_steps(&run, |s| s.app == WORD_PROCESSOR);
        let refused = refuse_run(&run, &missing);
        assert_eq!(refused.unreachable_apps(), vec![PHOTO_EDITOR]);
        assert!(
            refused
                .steps
                .iter()
                .any(|s| s.app == WORD_PROCESSOR && s.state == StepState::NotAttempted),
            "the reachable one is reported as untouched, not as undone: {refused:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Two applications, one instruction.
// ---------------------------------------------------------------------------

/// The photo editor's document: a crop stored as fields on an image record.
/// Different app, different schema, different CRDT — which is the point.
fn photo_started_by_a_person() -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(ActorId::from(PERSON.as_bytes()));
    let img = doc.put_object(ROOT, "image", ObjType::Map).unwrap();
    doc.put(&img, "crop_w", 4000).unwrap();
    doc.put(&img, "crop_h", 3000).unwrap();
    tilo_resource::history::commit_stamped_at(&mut doc, 1_780_000_000);
    doc
}

fn crop(doc: &AutoCommit) -> (i64, i64) {
    let (_, img) = doc.get(ROOT, "image").unwrap().unwrap();
    let read = |k: &str| match doc.get(&img, k) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Int(n) => *n,
            ScalarValue::Uint(n) => i64::try_from(*n).unwrap_or(-1),
            _ => -1,
        },
        _ => -1,
    };
    (read("crop_w"), read("crop_h"))
}

fn agent_crops(doc: &mut AutoCommit) {
    doc.set_actor(ActorId::from(AGENT.as_bytes()));
    let (_, img) = doc.get(ROOT, "image").unwrap().unwrap();
    doc.put(&img, "crop_w", 1200).unwrap();
    doc.put(&img, "crop_h", 900).unwrap();
    tilo_resource::history::commit_stamped_at(doc, 1_780_000_200);
}

/// The room document — the compositor's, the only one that sees both apps.
fn room() -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(ActorId::from("compositor".as_bytes()));
    doc.put(ROOT, "workspaces", "seed").unwrap();
    tilo_resource::history::commit_stamped_at(&mut doc, 1_780_000_000);
    doc
}

/// "Crop the photograph and put it in the report" — one instruction, two apps.
fn one_instruction_two_apps() -> (AutoCommit, AutoCommit, Report, AgentRun) {
    let mut photo = photo_started_by_a_person();
    let (mut report, _intro) = Report::started_by_a_person();

    agent_crops(&mut photo);
    report.edit_as(AGENT, 1_780_000_300, |dm| {
        let id = dm.insert_block(None, "paragraph");
        dm.splice_text(&id, 0, 0, "Figure 1: the cropped photograph.");
        id
    });

    let photo_run = latest_run_by(&mut photo, &actor_hex(AGENT)).expect("the agent cropped");
    let report_run = agents_run(&mut report.doc);

    let mut room = room();
    let steps = vec![
        RunStep {
            app: PHOTO_EDITOR.into(),
            record: "file:/photos/figure-1.png".into(),
            run: photo_run,
            summary: "Cropped to 1200x900".into(),
        },
        RunStep {
            app: WORD_PROCESSOR.into(),
            record: "file:/reports/q3.qdoc".into(),
            run: report_run,
            summary: "Placed the figure".into(),
        },
    ];
    record_run(
        &mut room,
        "Crop the photograph and put it in the report",
        Some(&actor_hex(AGENT)),
        &steps,
    )
    .expect("a run with steps is recorded");
    tilo_resource::history::commit_stamped_at(&mut room, 1_780_000_400);

    let run = read_runs(&room).pop().expect("the run reads back");
    (room, photo, report, run)
}

#[test]
fn run_records_replicate_without_polluting_history() {
    // Case `the_recorded_run_replicates_and_names_the_same_work_on_another_peer`.
    {
        let (mut room, mut photo, mut report, _run) = one_instruction_two_apps();

        let mut peer_room = room.fork();
        peer_room.set_actor(ActorId::from("bob".as_bytes()));
        peer_room.merge(&mut room).expect("rooms merge");

        let there = read_runs(&peer_room);
        assert_eq!(there.len(), 1, "the run crossed to the peer");
        let run = &there[0];
        assert_eq!(run.label, "Crop the photograph and put it in the report");
        assert_eq!(run.steps.len(), 2);
        assert_eq!(run.by.as_deref(), Some(actor_hex(AGENT).as_str()));

        // And the ids it carries name the same changes in the app documents, so the
        // peer can undo work it only ever received.
        photo.set_actor(ActorId::from("bob".as_bytes()));
        report.doc.set_actor(ActorId::from("bob".as_bytes()));
        let mut open: Vec<(String, String, &mut AutoCommit)> = vec![
            (
                PHOTO_EDITOR.into(),
                "file:/photos/figure-1.png".into(),
                &mut photo,
            ),
            (
                WORD_PROCESSOR.into(),
                "file:/reports/q3.qdoc".into(),
                &mut report.doc,
            ),
        ];
        let out = undo_run(run, Some(&actor_hex(AGENT)), &directory(), &mut open);
        drop(open);
        assert!(out.is_complete(), "the peer undid it too: {out:?}");
        assert_eq!(crop(&photo), (4000, 3000));
    }

    // Case `the_run_record_does_not_become_a_row_in_the_history`.
    {
        // The band is already on screen as the run; a "1 field" row under it would
        // be noise, and would invite undoing the bookkeeping instead of the work.
        let (mut room, _photo, _report, _run) = one_instruction_two_apps();
        let rows = HistorySource::new().entries(&mut room);
        // One row: the room's own seed change. The run record is not a row.
        assert_eq!(
            rows.len(),
            1,
            "the producer dropped the bookkeeping change rather than showing it: {:?}",
            rows.iter().map(|r| r.summary.clone()).collect::<Vec<_>>()
        );
        assert!(
            rows.iter()
                .all(|r| r.summary != tilo_ui::history_source::NO_VISIBLE_CHANGE),
            "and left no empty row behind: {:?}",
            rows.iter().map(|r| r.summary.clone()).collect::<Vec<_>>()
        );
    }

    // Case `a_run_with_no_steps_is_not_recorded`.
    {
        // An instruction that changed nothing has nothing to undo, and a record
        // claiming otherwise would put an affordance on screen that can only fail.
        let mut r = room();
        assert!(record_run(&mut r, "did nothing", None, &[]).is_none());
        r.commit();
        assert!(read_runs(&r).is_empty());
    }

    // Case `the_schema_genesis_is_nobodys_run`.
    {
        // Every qdoc document opens with a change written under a fixed actor that
        // no person is. Addressing that actor must find nothing: inverting the
        // genesis would take the document's own containers out with it.
        let (mut report, _intro) = Report::started_by_a_person();
        agent_writes_two_paragraphs(&mut report);

        let versions = tilo_resource::history::versions(&mut report.doc);
        let genesis = versions
            .iter()
            .find(|v| tilo_doc::model::is_schema_genesis(v))
            .expect("the document carries the shared schema genesis")
            .clone();

        assert!(
            latest_run_by(&mut report.doc, &genesis.actor).is_none(),
            "the genesis is not a run anybody can undo"
        );
        let rows = HistorySource::new().entries(&mut report.doc);
        assert!(
            rows.iter()
                .all(|r| r.id != entry_id_from_hash(&genesis.hash.to_string())),
            "and it is not offered as a revision either"
        );
    }

    // Case `entry_ids_are_stable_across_peers`.
    {
        // The whole cross-peer story rests on this: a run recorded on one machine
        // must name the same changes on another.
        let (mut report, _intro) = Report::started_by_a_person();
        agent_writes_two_paragraphs(&mut report);
        let mut peer = report.doc.fork();

        let here: Vec<_> = tilo_resource::history::versions(&mut report.doc)
            .into_iter()
            .map(|v| entry_id_from_hash(&v.hash.to_string()))
            .collect();
        let there: Vec<_> = tilo_resource::history::versions(&mut peer)
            .into_iter()
            .map(|v| entry_id_from_hash(&v.hash.to_string()))
            .collect();
        assert_eq!(here, there, "content addresses, so both peers agree");
    }
}

// ---------------------------------------------------------------------------
// Undoing from an identity alone — what an author chip beside the text has.
// ---------------------------------------------------------------------------

#[test]
fn author_chips_find_and_bound_an_authors_run() {
    // Case `an_author_chip_has_everything_it_needs_to_undo_that_author`.
    {
        // A chip reading "Claude" next to a line knows one thing: whose line it is.
        // That is enough — `latest_run_by` turns an identity into the band, and the
        // band is what gets inverted. No band has to have been named, and nothing
        // has to have been recorded in advance.
        let (mut report, _intro) = Report::started_by_a_person();
        let before = report.body();
        agent_writes_two_paragraphs(&mut report);

        // Someone else works in between, so the agent's band is genuinely bounded
        // rather than simply being "everything at the end".
        report.edit_as(PERSON, 1_780_000_350, |dm| {
            let id = dm.insert_block(None, "paragraph");
            dm.splice_text(&id, 0, 0, "My own note.");
            id
        });

        let run = latest_run_by(&mut report.doc, &actor_hex(AGENT)).expect("the chip's author");
        report.doc.set_actor(ActorId::from(PERSON.as_bytes()));
        let out =
            revert_run(&mut report.doc, &run, Some(&actor_hex(AGENT)), &directory()).expect("run");
        tilo_resource::history::commit_stamped_at(&mut report.doc, 1_780_000_400);

        assert_eq!(
            out.covered, 2,
            "the band stopped at the agent's own changes"
        );
        assert!(out.is_complete(), "{:?}", out.kept);
        let body = report.body();
        assert!(
            body.contains("My own note."),
            "the work that came after is untouched: {body:?}"
        );
        assert!(body.contains(&before), "and what came before: {body:?}");
        assert!(!body.contains("Revenue grew by a fifth."), "{body:?}");
        assert!(!body.contains("Figure 1"), "{body:?}");
    }

    // Case `a_run_found_by_author_stops_at_somebody_elses_change`.
    {
        // The band must never swallow a collaborator's work just because it sits
        // between two of the agent's changes.
        let (mut report, _intro) = Report::started_by_a_person();
        report.edit_as(AGENT, 1_780_000_200, |dm| {
            let id = dm.insert_block(None, "paragraph");
            dm.splice_text(&id, 0, 0, "An early agent line.");
            id
        });
        report.edit_as(PERSON, 1_780_000_250, |dm| {
            let id = dm.insert_block(None, "paragraph");
            dm.splice_text(&id, 0, 0, "Her line in between.");
            id
        });
        report.edit_as(AGENT, 1_780_000_300, |dm| {
            let id = dm.insert_block(None, "paragraph");
            dm.splice_text(&id, 0, 0, "A later agent line.");
            id
        });

        let run = latest_run_by(&mut report.doc, &actor_hex(AGENT)).expect("agent");
        report.doc.set_actor(ActorId::from(PERSON.as_bytes()));
        let out =
            revert_run(&mut report.doc, &run, Some(&actor_hex(AGENT)), &directory()).expect("run");
        tilo_resource::history::commit_stamped_at(&mut report.doc, 1_780_000_400);

        assert_eq!(
            out.covered, 1,
            "the run is the most recent contiguous band, not everything the agent ever did"
        );
        let body = report.body();
        assert!(body.contains("Her line in between."), "{body:?}");
        assert!(
            body.contains("An early agent line."),
            "the earlier band is a separate thing and was left alone: {body:?}"
        );
        assert!(!body.contains("A later agent line."), "{body:?}");
    }
}
