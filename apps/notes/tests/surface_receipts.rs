//! Every change Notes makes is one its committed surface decides. A receiver
//! (the signer, the broker, the relay) knows a change only by the kind and
//! action it claims and the app's `surface.toml`; a kind missing there holds
//! the change for good, and the identity service refuses to sign a held
//! change, which shut the window's collaboration down on the first edit of
//! a note's body.

use collab_types::claim::{ChangeClaim, ClaimedAction};
use collab_types::receipt::{Receipt, Record, check_claims};
use collab_types::surface::{KindSurface, SurfaceFile};
use notes::NotesShell;
use serde_json::{Value, json};
use ui::framework::testing::TestApp;

const SURFACE: &[u8] = include_bytes!("../surface.toml");

fn call(app: &mut TestApp<NotesShell>, resource: &str, action: &str, arguments: Value) -> Value {
    app.call_on(resource, action, arguments)
        .unwrap_or_else(|error| panic!("{action} on {resource} refused: {error:?}"))
        .unwrap_or_else(|| panic!("{action} on {resource} answered nothing"))
}

/// The receipt the committed surface gives the newest change, with the
/// storage units it wrote read from the record as it was before it, as a
/// receiver holding the record reads them.
fn receipt_of_newest(app: &TestApp<NotesShell>) -> (String, Receipt) {
    let mut doc = app.peer_document();
    let change = doc
        .get_changes(&[])
        .last()
        .map(|change| (*change).clone())
        .expect("a change");
    let before = doc.fork_at(change.deps()).expect("the record before it");
    let hash = change.hash().0;
    let units = collab_types::units::units_by_change(before, std::slice::from_ref(&change))
        .remove(&hash)
        .unwrap_or_else(|| vec![Vec::new()]);
    let claim =
        ChangeClaim::of_change(hash, change.message(), units).expect("the change names its action");
    // The harness runs the app under its own id; the record is that app's.
    let (app_id, kind) = claim
        .action
        .members()
        .into_iter()
        .find_map(|member| match member {
            ClaimedAction::Verb { action, .. } => Some((action.app.clone(), action.kind.clone())),
            ClaimedAction::Edit { kind, .. } => Some((String::new(), kind.clone())),
            _ => None,
        })
        .expect("the change claims a kind");
    let file = SurfaceFile::parse(SURFACE).expect("the committed surface parses");
    let surface = file
        .resource(Some(&kind))
        .unwrap_or_else(|| panic!("the committed surface does not describe `{kind}`"))
        .clone();
    let record = Record {
        room: "room",
        app: &app_id,
        record: "record",
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
        "notes.notebook",
        "create-page",
        json!({ "title": "Links", "markdown": "[tilo OS](search for the tilo homepage)" }),
    );
    let page = created["resource"].as_str().expect("the page").to_owned();
    assert_eq!(receipt_of_newest(&app).1, Receipt::Admit);

    let note = call(&mut app, "notes.notebook", "read", json!({ "item": page }));
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
        kind, "notes.notebook",
        "a body edit is the notebook's field edit, decided by the notebook's surface"
    );
    assert_eq!(receipt, Receipt::Admit);
}
