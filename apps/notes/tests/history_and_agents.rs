//! An agent works on the notebook one call at a time — each change a verb on
//! the note it acts on or a field edit at a path — and what it did reads back
//! as the app's own sentences, naming the notes as they were called.

use notes::{Notebook, NotesShell, PageId};
use tilo_document::RichTextAction;
use ui::framework::testing::TestApp;
use ui::tilo_actions::{ActionInvoker, ActorKind};

fn note_titled(app: &TestApp<NotesShell>, title: &str) -> PageId {
    crate::framework_support::shown(app)
        .pages()
        .iter()
        .find(|page| page.title == title)
        .unwrap_or_else(|| panic!("a note titled {title}"))
        .id
}

#[test]
fn an_agent_changes_notes_in_one_call_each_and_history_tells_what_it_did() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let kind = <Notebook as ui::tilo_resource::Resource>::KIND;
    let call = |app: &mut TestApp<NotesShell>, resource: &str, id: &str, args| {
        app.call_by(agent.clone(), resource, id, args)
            .unwrap_or_else(|error| panic!("{id}: {error:?}"));
    };

    call(
        &mut app,
        kind,
        "new-note",
        serde_json::json!({ "title": "Plan" }),
    );
    let plan = note_titled(&app, "Plan");
    let item = notes::actions::page_resource_id(plan);
    let body = Notebook::paths()
        .pages
        .item(&notes::notebook::page_id(plan))
        .body
        .perform(&RichTextAction::AppendParagraph {
            text: "Ship it".into(),
        })
        .unwrap();
    call(
        &mut app,
        kind,
        "field-edit",
        serde_json::json!({ "edit": body }),
    );
    call(
        &mut app,
        &item,
        "new-child",
        serde_json::json!({ "title": "Step one" }),
    );
    call(
        &mut app,
        &item,
        "tag",
        serde_json::json!({ "name": "Release" }),
    );
    call(
        &mut app,
        &item,
        "rename",
        serde_json::json!({ "title": "Roadmap" }),
    );
    call(&mut app, &item, "duplicate", serde_json::json!({}));
    let copy = note_titled(&app, "Roadmap copy");
    // Undo takes a deleted note back, so an agent's delete runs at once
    // like a person's.
    app.call_by(
        agent.clone(),
        &notes::actions::page_resource_id(copy),
        "delete",
        serde_json::json!({}),
    )
    .expect("an undoable delete asks nothing");

    let shown = crate::framework_support::shown(&app);
    assert_eq!(shown.children_of(Some(plan)).len(), 1);
    assert_eq!(shown.tags_for_page(plan)[0].name, "Release");
    assert_eq!(
        app.read_document(|doc| notes::read::page_block_snapshots(doc, plan))
            .into_iter()
            .map(|(_, text)| text)
            .collect::<Vec<_>>(),
        ["Ship it"]
    );

    let entries: Vec<_> = app
        .history()
        .into_iter()
        .filter(|entry| {
            entry
                .invoker
                .as_ref()
                .is_some_and(|invoker| invoker.actor == "agent:claude")
        })
        .collect();
    let sentences = app.sentences(&entries);
    assert_eq!(
        sentences,
        [
            "Created Plan",
            "Edited Plan",
            "Created Step one in Plan",
            "Tagged Plan with Release",
            "Renamed Plan to Roadmap",
            "Duplicated Roadmap",
            "Deleted Roadmap copy",
        ]
    );
    let invoker = entries[0].invoker.as_ref().unwrap();
    assert_eq!(
        (invoker.kind, invoker.on_behalf_of.as_deref()),
        (ActorKind::Agent, Some("ada")),
        "the entry names the agent and the person it acted for"
    );
}

/// A note another app's effect asks for someone (Tasks writing an issue's
/// note) is theirs: `create-page` answers the note's resource, which the
/// asking app links, makes one step in the notebook's history by them, and
/// their undo takes it back while the person's own notes stay.
#[test]
fn a_note_made_for_someone_through_another_app_is_one_step_of_theirs() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    app.call("new-note", serde_json::json!({ "title": "Groceries" }))
        .unwrap();
    let kind = <Notebook as ui::tilo_resource::Resource>::KIND;
    let before = app.versions().len();
    let agent = ActionInvoker::actor("agent:scribe")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");

    let made = app
        .call_by(
            agent.clone(),
            kind,
            "create-page",
            serde_json::json!({ "title": "#1 Plan the launch" }),
        )
        .unwrap();
    let note = note_titled(&app, "#1 Plan the launch");
    let resource = notes::actions::page_resource_id(note);
    assert_eq!(made.resources.first(), Some(&resource), "{made:?}");
    assert_eq!(
        ui::tilo_resource::items::item_kind(&resource).as_deref(),
        Some("notes.notebook.pages")
    );

    let versions = app.versions();
    assert_eq!(versions.len(), before + 1, "one step: {versions:?}");
    let by = versions
        .last()
        .and_then(|version| version.context.as_ref())
        .and_then(|context| context.invoker.as_ref())
        .expect("the step names who made it");
    assert_eq!(by.actor, "agent:scribe");
    assert_eq!(by.on_behalf_of.as_deref(), Some("ada"));

    app.call_by(agent, kind, "undo", serde_json::json!({}))
        .unwrap();
    let titles: Vec<String> = crate::framework_support::shown(&app)
        .pages()
        .iter()
        .map(|page| page.title.clone())
        .collect();
    assert_eq!(titles, ["Groceries"]);
}
