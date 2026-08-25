//! The live model, against the document the demo actually uses.
//!
//! Everything else in this crate's tests pins the decision to the keyword
//! vocabulary so it is reproducible. This one does the opposite on purpose:
//! it makes the real call and asserts on what came back, because the claim
//! the demo makes is about a model reading a document, and a test that
//! never calls one cannot support it.
//!
//! It is `#[ignore]`d — it needs the network and a key, and neither belongs
//! in a default `cargo test`. It is **not** allowed to pass vacuously: run
//! it without a key and it fails saying so, rather than skipping quietly.
//! Run it with:
//!
//! ```text
//! cargo test -p agent --test live_model -- --ignored --nocapture
//! ```

use std::path::Path;
use std::time::Duration;

use qos_agent::model::{Chooser, Route};
use qos_agent::plan::{Block, Instruction};

/// The demo's document: a line of prose, then notes under it.
///
/// Three things here are only visible to a reader who understands what
/// the page is about, and none of them to a split on separators:
///
/// * The prose says doors at seven; the note under it says 18:00. One of
///   them is wrong and the document does not say which.
/// * A seating capacity is not a line in a running order, however much it
///   looks like the others — it has a colon in it, so a separator split
///   keeps it as an event.
/// * The sound check is listed after the doors open.
///
/// The prose/notes contradiction is the one this test holds the model to.
/// It is a fact stated twice in the same document with two different
/// values, so noticing it needs only care, and either answer to "which is
/// right" would be invented — which makes saying so the correct move
/// rather than a judgement call the model might reasonably decline.
fn demo_blocks() -> Vec<Block> {
    let b = |kind: &str, text: &str| Block {
        id: format!("b{text:.4}"),
        kind: kind.into(),
        text: text.into(),
        attrs: Default::default(),
    };
    vec![
        b(
            "paragraph",
            "Doors open at seven and the room seats two hundred and forty.",
        ),
        b("list_item", "Doors: 18:00"),
        b("list_item", "Sound check: 18:30"),
        b("list_item", "Seating: 240"),
        b("paragraph", "Claude: make this a run sheet"),
    ]
}

/// What the room exports, surveyed off the word processor's own tree for the
/// document this test hands the model. Real addresses, so an answer naming one
/// passes the same gate a live run's would.
fn surveyed(blocks: &[qos_agent::plan::Block]) -> qos_agent::surface::Catalogue {
    let mut doc = automerge::AutoCommit::new();
    let c = qdoc::model::init(&mut doc, &automerge::ROOT);
    let mut ids = qdoc::model::seeded_id_gen(&doc, &c);
    {
        let mut dm = qdoc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        let mut after: Option<String> = None;
        for b in blocks {
            let id = dm.insert_block(after.as_deref(), &b.kind);
            dm.splice_text(&id, 0, 0, &b.text);
            after = Some(id);
        }
    }
    doc.commit();
    let wp = &qos_agent::surface::CONTRACTS[0];
    qos_agent::surface::catalogue(
        &[(wp, &doc)],
        &qos_agent::View::default(),
        &granted("Claude"),
    )
}

/// The capability an inviter issues an agent for the app it was invited to.
fn granted(name: &str) -> ui::permissions::PermissionContext {
    use ui::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};
    PermissionContext::from_capabilities(
        Actor::agent(name),
        vec![CapOverride::grant(
            Subject::agent(name),
            Scope::App("word-processor".to_string()),
            Verb::Write,
        )
        .with_id("invited")],
    )
}

fn chooser() -> Chooser {
    assert!(
        qos_agent::model::api_key().is_some(),
        "no ANTHROPIC_API_KEY in the environment or the operator's env file. \
         This test asserts on a live model and cannot prove anything without \
         one, so it fails rather than passing having done nothing."
    );
    // Generous next to the agent's own deadline: this test is asserting on
    // what the model said, not on the agent's refusal to wait for it.
    Chooser::new(true, Duration::from_secs(60))
}

#[test]
#[ignore = "calls the live Anthropic API; needs a key and the network"]
fn the_model_drafts_a_run_sheet_that_a_split_on_separators_could_not() {
    let blocks = demo_blocks();
    let offers = surveyed(&blocks);
    let d = chooser().choose("make this a run sheet", &blocks, &offers);

    let Route::Model { why, .. } = &d.route else {
        panic!(
            "the call did not reach a model, so this proves nothing: {}",
            d.route.detail()
        );
    };
    println!("\n  why: {why}");

    assert_eq!(
        d.plan.len(),
        1,
        "one thing was asked for, so one step: {:?}",
        d.plan
    );
    let Instruction::RunSheet(draft) = &d.plan.steps[0].instruction else {
        panic!("expected a drafted run sheet, got {:?}", d.plan);
    };
    println!("  headers: {:?}", draft.headers);
    for row in &draft.rows {
        println!("  row: {row:?}");
    }
    println!("  note: {:?}\n", draft.note);

    // It passed the gate, so it is writable at all.
    assert_eq!(draft.check(), Ok(()));

    let flat = draft
        .grid()
        .iter()
        .flatten()
        .map(|c| c.to_lowercase())
        .collect::<Vec<_>>()
        .join(" | ");

    // The two events are there.
    assert!(flat.contains("18:00"), "doors are missing: {flat}");
    assert!(flat.contains("18:30"), "the sound check is missing: {flat}");

    // The judgement: a seating capacity is not a line in a running order.
    // A split on separators keeps it, because it has a colon in it.
    assert!(
        !flat.contains("240"),
        "the seating figure was carried into the run sheet as if it were an \
         event, which is exactly what splitting on separators does: {flat}"
    );

    // The document states the door time twice and disagrees with itself.
    // Neither value can be preferred without inventing something, so the
    // right move is to carry the facts through unchanged and say so — and
    // that is what is asserted, in both halves.
    let note = draft
        .note_text()
        .unwrap_or_else(|| panic!("the contradiction went unremarked: {draft:?}"))
        .to_lowercase();
    assert!(
        note.contains("seven")
            || note.contains("19:00")
            || note.contains("18:00")
            || note.contains("door"),
        "the note does not address the two door times: {note:?}"
    );
    // Said, not silently fixed: the note is a remark about the notes, so
    // the table still carries what the notes actually said.
    assert!(
        flat.contains("18:00"),
        "the door time was quietly rewritten instead of questioned: {flat}"
    );
}

#[test]
#[ignore = "calls the live Anthropic API; needs a key and the network"]
fn the_model_still_refuses_what_the_app_cannot_do() {
    // The other half of a live decision: an instruction nothing in the
    // catalogue answers must come back as a refusal, not as whichever
    // entry is closest. A model that always picks something is not
    // choosing, and the agent would carry out the wrong thing confidently.
    let blocks = demo_blocks();
    let offers = surveyed(&blocks);
    let d = chooser().choose("email this to the venue and book a taxi", &blocks, &offers);
    // Every step, not merely the first. A sentence naming two things it
    // cannot do must not come back as one operation it can: the failure
    // this guards against is the half-answer, and a plan is a place a half
    // could hide.
    assert!(
        d.plan
            .steps
            .iter()
            .all(|s| matches!(s.instruction, Instruction::Unknown(_))),
        "expected a refusal, got {:?} ({})",
        d.plan,
        d.route.detail()
    );
}

// ---------------------------------------------------------------------------
// The room the film asks about
// ---------------------------------------------------------------------------

/// The whole room, not one document: a page, a directory, a conversation, and
/// a photo editor that is **not** running.
///
/// The tests above hand the model one application, which is the shape of an
/// instruction typed into a page. The request this one is about arrives the
/// other way — somebody right-clicks a photograph and says something about it
/// — and what the model is then handed is a survey of a room: applications,
/// paths, and the operations that may be performed on each. That is a
/// different kind of input, and it is the one that came back declined.
fn film_room(directory: &Path) -> qos_agent::surface::Catalogue {
    use qos_agent::surface;

    // The page the request wants the photograph put into.
    let mut page = automerge::AutoCommit::new();
    let c = qdoc::model::init(&mut page, &automerge::ROOT);
    let mut ids = qdoc::model::seeded_id_gen(&page, &c);
    {
        let mut dm = qdoc::DocMut {
            doc: &mut page,
            c: &c,
            ids: &mut ids,
        };
        let mut after: Option<String> = None;
        for (kind, text) in [
            ("heading", "Run of show"),
            ("list_item", "Doors"),
            ("list_item", "Sound"),
            ("list_item", "Seating"),
        ] {
            let id = dm.insert_block(after.as_deref(), kind);
            dm.splice_text(&id, 0, 0, text);
            after = Some(id);
        }
    }
    page.commit();

    // The file manager, browsing the directory the photograph is in. Its
    // resources are read off the real directory by the explorer's own reader,
    // so the entries in the survey are the paths a run would really carry.
    let mut files = automerge::AutoCommit::new();
    {
        use automerge::transaction::Transactable;
        files
            .put_object(&automerge::ROOT, "files.rename_query", automerge::ObjType::Text)
            .expect("seed the rename buffer");
    }
    files.commit();

    // The conversation the request is written into: one workspace, two
    // channels, which is what the room's starter seed makes.
    let mut chat = automerge::AutoCommit::new();
    chat::doc::ChatDoc::seed_starter(&mut chat, "u_claude");
    chat.commit();

    let held: Vec<(&'static surface::AppContract, &automerge::AutoCommit)> = vec![
        (&surface::CONTRACTS[0], &page),
        (&surface::CONTRACTS[1], &files),
        (&surface::CONTRACTS[2], &chat),
    ];
    surface::catalogue(
        &held,
        &surface::View {
            directory: Some(directory.to_path_buf()),
            ..Default::default()
        },
        &granted_across(&["word-processor", "files", "chat", "photos"], "Claude"),
    )
}

/// The four grants the film issues: leave to act in each of these
/// applications, and — for the one that is not running — leave to open it.
fn granted_across(apps: &[&str], name: &str) -> ui::permissions::PermissionContext {
    use ui::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};
    PermissionContext::from_capabilities(
        Actor::agent(name),
        apps.iter()
            .map(|app| {
                CapOverride::grant(
                    Subject::agent(name),
                    Scope::App((*app).to_string()),
                    Verb::Write,
                )
                .with_id(format!("invited-{app}"))
            })
            .collect(),
    )
}

/// A directory shaped like the one the film works in: a photograph and the
/// document it is going into.
fn a_photograph_and_a_page() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    // A real PNG rather than a stub, because the explorer reports what it
    // reads and a survey built on a lie proves nothing about the survey.
    let png = photos::pixels::encode_png(
        &photos::pixels::Rgba::new(320, 200, vec![0u8; 4 * 320 * 200])
            .expect("a rectangle of pixels"),
    )
    .expect("encode a photograph");
    std::fs::write(dir.path().join("harbour.png"), png).expect("write the photograph");
    std::fs::write(dir.path().join("Run of show.md"), b"# Run of show\n")
        .expect("write the page");
    dir
}

/// **The film's request, answered rather than declined.**
///
/// "Make this square and put it in the open document", said about a
/// photograph, with the photo editor not running. Three things have to happen
/// — open it, crop it, place the result — and only the first can be named
/// before the open, so the answer is an open plus a remainder.
///
/// What this is really holding is the step before that: the call has to come
/// back with an answer at all. The same request declined on the way in, and
/// the fallback that catches a decline could only say "open the photo
/// editor", which opened a window and reported the work finished. So the
/// assertion is on the route first and the plan second, and neither is
/// allowed to pass by way of the keyword vocabulary.
#[test]
#[ignore = "calls the live Anthropic API; needs a key and the network"]
fn the_request_the_film_turns_on_is_answered() {
    let dir = a_photograph_and_a_page();
    let room = film_room(dir.path());
    let photograph = format!("files.entry:{}", dir.path().join("harbour.png").display());
    // What the compositor hands the agent when somebody right-clicks a thing
    // and says something about it: the room narrowed to that thing, plus the
    // opens, because opening an application performs nothing to any resource.
    let scoped = room.scoped_to(&photograph);
    assert!(
        scoped.offers.iter().any(|o| o.id().starts_with("photos.app#")),
        "the photo editor has to be openable from here, or the request \
         cannot be answered at all: {:?}",
        scoped.offers.iter().map(|o| o.id()).collect::<Vec<_>>()
    );

    let d = chooser().choose(
        "make this square and put it in the open document",
        &[],
        &scoped,
    );

    let Route::Model { model, why, asked } = &d.route else {
        panic!(
            "the call did not reach an answer, so the run would fall to the \
             keyword vocabulary — which cannot express a request that crosses \
             two applications: {}",
            d.route.detail()
        );
    };
    println!("\n  answered by: {model}\n  why: {why}\n  asked: {asked:?}");

    // The open is named. It is the one step of the three that a survey taken
    // before it could possibly hold.
    let opens = d.plan.steps.iter().any(|s| {
        matches!(&s.instruction, Instruction::Perform { id, .. } if id.starts_with("photos.app#"))
    });
    assert!(opens, "nothing in the plan opens the photo editor: {:?}", d.plan);

    // And the rest of the sentence is kept rather than dropped — as a step
    // whose operation cannot be named yet, or as the remainder. Dropping it
    // is the failure this whole path exists to prevent: a window opens, the
    // last part never happens, and the run reports itself as finished.
    let carries_the_rest = d.plan.len() > 1
        || d.plan
            .steps
            .iter()
            .any(|s| matches!(s.instruction, Instruction::Unknown(_)));
    assert!(
        carries_the_rest,
        "the plan opens the editor and stops, so cropping and placing were \
         dropped on the floor: {:?}",
        d.plan
    );
}
