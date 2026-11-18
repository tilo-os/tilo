//! Every change Notes makes is one its committed surface decides. A receiver
//! (the signer, the broker, the server) knows a change only by the kind and
//! action it claims and the app's `surface.toml`; a kind missing there holds
//! the change for good, and the identity service refuses to sign a held
//! change, which shut the window's collaboration down on the first edit of
//! a note's body.

use collab_types::claim::{ChangeClaim, ClaimedAction};
use collab_types::receipt::{Receipt, WrittenDocument, check_claims};
use collab_types::surface::{KindSurface, SurfaceFile};
use notes::NotesShell;
use serde_json::{Value, json};
use ui::framework::testing::TestApp;

const SURFACE: &[u8] = include_bytes!("../surface.toml");

/// A member's replica of the part the notes are kept in.
fn part(app: &TestApp<NotesShell>) -> automerge::AutoCommit {
    app.peer_document_of(&notes::note_list::chain().part(0))
        .expect("the part is held")
}

fn call(app: &mut TestApp<NotesShell>, resource: &str, action: &str, arguments: Value) -> Value {
    app.call_on(resource, action, arguments)
        .unwrap_or_else(|error| panic!("{action} on {resource} refused: {error:?}"))
        .unwrap_or_else(|| panic!("{action} on {resource} answered nothing"))
}

/// The receipt the committed surface gives the newest change, with the
/// storage units it wrote read from the resource as it was before it, as a
/// receiver holding the resource reads them.
fn receipt_of_newest(app: &TestApp<NotesShell>) -> (String, Receipt) {
    // The notes' changes are kept in the part holding them.
    let mut doc = part(app);
    let change = doc
        .get_changes(&[])
        .last()
        .map(|change| (*change).clone())
        .expect("a change");
    let before = doc.fork_at(change.deps()).expect("the resource before it");
    let hash = change.hash().0;
    let units = collab_types::units::units_by_change(before, std::slice::from_ref(&change))
        .and_then(|mut by| by.written.remove(&hash))
        .map_or_else(|| vec![Vec::new()], |written| written.units);
    let claim =
        ChangeClaim::of_change(hash, change.message(), units).expect("the change names its action");
    // The harness runs the app under its own id; the resource is that app's.
    let (app_id, kind) = claim
        .action
        .members()
        .into_iter()
        .find_map(|member| match member {
            ClaimedAction::Verb { action, .. } => Some((action.app.clone(), action.kind.clone())),
            ClaimedAction::Edit { kind, .. } => Some((String::new(), kind.clone())),
            _ => None,
        })
        .unwrap_or_else(|| (String::new(), "notes.pages".to_owned()));
    let file = SurfaceFile::parse(SURFACE).expect("the committed surface parses");
    let surface = file
        .resource(Some(&kind))
        .unwrap_or_else(|| panic!("the committed surface does not describe `{kind}`"))
        .clone();
    let record = WrittenDocument {
        workspace: "workspace",
        app: &app_id,
        document: "record",
    };
    let receipt = check_claims(
        std::slice::from_ref(&claim),
        &record,
        Some(&[hash]),
        None,
        Some(&KindSurface(surface)),
        &|_, _| true,
    )
    .unwrap_or_else(|refusal| panic!("`{kind}` change refused: {refusal:?}"));
    (kind, receipt)
}

#[test]
fn editing_a_notes_body_is_decided_by_the_committed_surface() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    let created = call(
        &mut app,
        &notes::note_list::chain_resource(),
        "create-page",
        json!({ "title": "Links", "markdown": "[tilo OS](search for the tilo homepage)" }),
    );
    let page = created["resource"].as_str().expect("the page").to_owned();
    assert_eq!(receipt_of_newest(&app).1, Receipt::Admit);

    let note = call(&mut app, &page, "read", json!({}));
    let body = note["body"].as_str().expect("the body resource").to_owned();
    let block = note["markdown"]
        .as_str()
        .and_then(|markdown| markdown.lines().next())
        .and_then(|line| line.rsplit_once(" ^"))
        .map(|(_, id)| id.to_owned())
        .expect("the block id");
    call(
        &mut app,
        &body,
        "set-block-text",
        json!({ "block": block, "markdown": true, "text": "[tilo OS](https://tilo.so/)" }),
    );
    let (kind, receipt) = receipt_of_newest(&app);
    assert_eq!(
        kind, "notes.pages",
        "a body edit is the notebook's field edit, decided by the notebook's surface"
    );
    assert_eq!(receipt, Receipt::Admit);
}

/// What the committed surface decides of every change made since `before`,
/// with the kind each claims (the resource's own when it names none).
fn receipts_since(
    doc: &automerge::AutoCommit,
    before: &[automerge::ChangeHash],
) -> Vec<(String, Result<Receipt, String>)> {
    let mut doc = doc.clone();
    let changes: Vec<automerge::Change> = doc.get_changes(before);
    let file = SurfaceFile::parse(SURFACE).expect("the committed surface parses");
    changes
        .iter()
        .map(|change| {
            let before = doc.fork_at(change.deps()).expect("the resource before it");
            let hash = change.hash().0;
            let units = collab_types::units::units_by_change(before, std::slice::from_ref(change))
                .and_then(|mut by| by.written.remove(&hash))
                .map_or_else(|| vec![Vec::new()], |written| written.units);
            let claim = match ChangeClaim::of_change(hash, change.message(), units) {
                Ok(claim) => claim,
                Err(error) => return (String::new(), Err(format!("unnamed: {error}"))),
            };
            let kind = claim
                .action
                .members()
                .into_iter()
                .find_map(|member| match member {
                    ClaimedAction::Verb { action, .. } => Some(action.kind.clone()),
                    ClaimedAction::Edit { kind, .. } => Some(kind.clone()),
                    _ => None,
                })
                .filter(|kind| !kind.is_empty())
                .unwrap_or_else(|| "notes.pages".to_owned());
            let Some(surface) = file.resource(Some(&kind)) else {
                return (
                    kind.clone(),
                    Err(format!("the surface does not describe `{kind}`")),
                );
            };
            let record = WrittenDocument {
                workspace: "workspace",
                app: "so.tilo.notes",
                document: "record",
            };
            let receipt = check_claims(
                std::slice::from_ref(&claim),
                &record,
                Some(&[hash]),
                None,
                Some(&KindSurface(surface.clone())),
                &|_, _| true,
            )
            .map_err(|refusal| refusal.to_string());
            (kind, receipt)
        })
        .collect()
}

/// Typing into a note's body edits it through the body's own dispatcher,
/// not an action invocation. Its changes must claim the notebook's field
/// edit too: a change claiming the body's kind is refused by the identity
/// service, which then turns the window's collaboration off.
#[test]
fn typing_into_a_notes_body_is_decided_by_the_committed_surface() {
    use crate::component_support::*;
    let f = NotebookFixture::rich();
    f.write_page(f.child, b"Alpha");
    let checkout = notebook(&f);
    let block = f
        .store
        .read(&f.part(), |doc| {
            notes::read::page_block_snapshots(doc, f.child)[0].0.clone()
        })
        .unwrap();
    let before = f
        .store
        .read(&f.part(), |doc| doc.clone().get_heads())
        .unwrap();
    let mut live = driver(&f, 81, &checkout);
    open_note(&mut live, 81, f.child, Some(&block));
    let rect = semantic_rect(&live, &format!("notes.block:{block}"));
    live.click_at(rect.x0 + 8.0, rect.center().y);
    live.key("End");
    live.type_text(" Beta");
    live.relayout();
    close_driver(&mut live);
    drop(live);
    f.store.try_flush().unwrap();
    let receipts = f
        .store
        .read(&f.part(), |doc| receipts_since(doc, &before))
        .unwrap();
    assert!(!receipts.is_empty(), "typing changed the resource");
    for (kind, receipt) in receipts {
        assert_eq!(
            kind, "notes.pages",
            "typed body edits claim the notebook's field edit"
        );
        assert_eq!(receipt, Ok(Receipt::Admit));
    }
}

/// Renaming a note from its title applies the notebook's `rename` verb from
/// the window, not through an invocation. The verb's claim must name Notes
/// itself: a claim naming an app guessed from the action type
/// (`org.tilo.Notes`) is one the resource's app does not publish, and the
/// identity service refused it and turned the window's collaboration off.
///
/// Each keystroke in the title is one rename, so the page tree, the window's
/// bar and its tab follow the note as it is named. Each is a report of one
/// call of the published action, and a report admits one call: every one of
/// them needs, and gets, its own admitted receipt.
#[test]
fn renaming_a_note_from_its_title_is_decided_by_the_committed_surface() {
    let (mut app, page) = crate::framework_support::with_note("Welcome", "Hello");
    app.harness().relayout_at((1200, 800)).unwrap();
    let before = part(&app).get_heads();
    let title = app
        .surface_mut::<notes::NotesApp>()
        .main_title_center_for_test();
    // The title's edit is its control's published view action: entered and
    // typed through the window, as a person does.
    app.harness().click_at(title.x, title.y);
    app.harness().type_text("Road");
    app.harness().key("Enter");
    let named = crate::framework_support::notes_doc(&app, notes::read::list_pages)
        .into_iter()
        .find(|shown| shown.id == page)
        .map(|shown| shown.title);
    assert_eq!(
        named.as_deref(),
        Some("Road"),
        "what is typed in the title names the note"
    );
    let mut doc = part(&app);
    let verbs: Vec<String> = doc
        .get_changes(&before)
        .iter()
        .filter_map(|change| {
            ChangeClaim::of_change(change.hash().0, change.message(), Vec::new()).ok()
        })
        .flat_map(|claim| {
            claim
                .action
                .members()
                .into_iter()
                .filter_map(|member| match member {
                    ClaimedAction::Verb { action, .. } => Some(action.to_string()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .collect();
    // One rename per keystroke: the page tree, the window's bar and its tab
    // follow the note as it is named. Each is a report of one call of a
    // published action, so each needs its own receipt.
    assert_eq!(
        verbs, ["so.tilo.notes/notes.pages/rename"; 4],
        "each title edit renamed the note as Notes' own verb"
    );
    let receipts = receipts_since(&doc, &before);
    assert_eq!(receipts.len(), 4, "one change per keystroke, each decided");
    for (kind, receipt) in receipts {
        assert_eq!(receipt, Ok(Receipt::Admit), "a `{kind}` change was refused");
    }
}
