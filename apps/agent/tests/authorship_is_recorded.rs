//! What the agent writes says so, in the document, an hour later.
//!
//! Presence answers "where is it right now" and expires with the session.
//! Authorship is the other half: which sentences came from the agent rather
//! than from the people in the room. That has to survive both of them closing
//! the page, so it lives in the document as a mark on the text — not on the
//! presence channel, and not as a fact about the replica that happens to be
//! open.
//!
//! The guard in the third test is the one worth stating: a document nobody
//! attributed must stay unattributed. Marking prose the agent did not write
//! would make the mark worthless in exactly the frame where it matters.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::AutoCommit;
use tilo_agent::surface::WORD_PROCESSOR;
use word_processor::actions::{RegionKind, RichTextAction};
use word_processor::framework::Document;

const AGENT: &str = "Claude";
const PERSON: &str = "quinten";

/// The invitation the agent joined with: write on the word processor.
fn invited() -> ui::permissions::PermissionContext {
    use ui::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};
    PermissionContext::from_capabilities(
        Actor::agent(AGENT),
        vec![CapOverride::grant(
            Subject::agent(AGENT),
            Scope::App(WORD_PROCESSOR.app_id().to_owned()),
            Verb::Write,
        )],
    )
}

/// A page with one paragraph somebody typed, and the id of that paragraph.
fn a_page_somebody_wrote() -> (Vec<u8>, String) {
    let mut doc = AutoCommit::new();
    doc.set_actor(automerge::ActorId::from(PERSON.as_bytes()));
    let c = tilo_doc::model::init(&mut doc, &automerge::ROOT);
    let mut ids = tilo_doc::model::seeded_id_gen(&doc, &c);
    let block = {
        let mut dm = tilo_doc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        let p = dm.insert_block(None, "paragraph");
        dm.splice_text(&p, 0, 0, "Doors at seven.");
        p
    };
    doc.commit();
    (doc.save(), block)
}

/// Every author run the document records for `block`, oldest position first.
fn runs_on(bytes: &[u8], block: &str) -> Vec<(usize, usize, String)> {
    let doc = AutoCommit::load(bytes).expect("reload the document");
    let c = tilo_doc::model::open(&doc, &automerge::ROOT).expect("open the schema");
    let marks = tilo_doc::model::marks(&doc, &c, block);
    tilo_doc::author_runs(&marks)
}

/// Each case seeds its own fresh page, so no edit leaks between them.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each case keeps its original fixture and assertions"
)]
fn the_agents_own_words_and_only_those_carry_its_name_in_the_document() {
    // Case `what_the_agent_wrote_carries_its_name_in_the_document`.
    {
        let (bytes, block) = a_page_somebody_wrote();
        let mut editor =
            tilo_agent::apply::Editor::from_snapshot(&WORD_PROCESSOR, &bytes, AGENT, invited())
                .expect("seed the replica");

        let note = " The bullet says 18:00.";
        editor
            .apply::<Document>(
                &[word_processor::model::action(&RichTextAction::SpliceText {
                    region: RegionKind::Body,
                    block: block.clone(),
                    at: "Doors at seven.".chars().count(),
                    del: 0,
                    ins: note.to_string(),
                })],
            )
            .expect("the agent's edit applies");

        let saved = editor.full();
        let runs = runs_on(&saved, &block);

        assert_eq!(
            runs.len(),
            1,
            "the agent's sentence should be one authored run, got {runs:?}"
        );
        let (start, end, author) = &runs[0];
        assert_eq!(author, AGENT, "the run must name the agent that wrote it");
        assert_eq!(
            *start,
            "Doors at seven.".chars().count(),
            "attribution must start where the agent's text starts"
        );
        assert_eq!(
            *end,
            start + note.chars().count(),
            "attribution must cover exactly what the agent added"
        );

        // And it is durable rather than a fact about the live replica: a reader
        // that only ever sees the saved bytes reads the same answer.
        let text = {
            let doc = AutoCommit::load(&saved).expect("reload");
            let c = tilo_doc::model::open(&doc, &automerge::ROOT).expect("open");
            tilo_doc::model::block_text(&doc, &c, &block).unwrap_or_default()
        };
        let authored: String = text.chars().skip(*start).take(end - start).collect();
        assert_eq!(
            authored, note,
            "the marked range must be the agent's own sentence"
        );
    }

    // Case `the_sentence_the_person_typed_is_left_alone`.
    {
        let (bytes, block) = a_page_somebody_wrote();
        let mut editor =
            tilo_agent::apply::Editor::from_snapshot(&WORD_PROCESSOR, &bytes, AGENT, invited())
                .expect("seed the replica");
        editor
            .apply::<Document>(
                &[word_processor::model::action(&RichTextAction::SpliceText {
                    region: RegionKind::Body,
                    block: block.clone(),
                    at: "Doors at seven.".chars().count(),
                    del: 0,
                    ins: " The bullet says 18:00.".to_string(),
                })],
            )
            .expect("the agent's edit applies");

        let runs = runs_on(&editor.full(), &block);
        let typed = 0.."Doors at seven.".chars().count();
        for (start, end, author) in &runs {
            assert!(
                *start >= typed.end,
                "{author} was credited with text at {start}..{end}, inside the \
                 sentence a person typed"
            );
        }
    }

    // Case `a_page_nobody_attributed_stays_unattributed`.
    {
        // The over-marking guard. The agent touching a document must not turn
        // prose that was already there into prose it claims.
        let (bytes, block) = a_page_somebody_wrote();
        assert!(
            runs_on(&bytes, &block).is_empty(),
            "an untouched page must carry no authorship"
        );

        let mut editor =
            tilo_agent::apply::Editor::from_snapshot(&WORD_PROCESSOR, &bytes, AGENT, invited())
                .expect("seed the replica");
        // An edit that removes text authors nothing.
        editor
            .apply::<Document>(
                &[word_processor::model::action(&RichTextAction::SpliceText {
                    region: RegionKind::Body,
                    block: block.clone(),
                    at: 0,
                    del: 6,
                    ins: String::new(),
                })],
            )
            .expect("the agent's deletion applies");

        assert!(
            runs_on(&editor.full(), &block).is_empty(),
            "deleting somebody else's words authors none of the rest"
        );
    }
}
