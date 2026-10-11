//! An agent works on a project one call at a time — each change a verb on
//! the project or the file it acts on, or a field edit at a path — and what
//! it did reads back as the editor's own sentences.

#![allow(clippy::panic, reason = "in a test a panic is the assertion")]

use code_editor::project::{Project, SourceEdit};
use code_editor::{CodeEditorShell, actions};
use ui::framework::testing::TestApp;
use ui::tilo_actions::{ActionInvoker, ActorKind};
use ui::tilo_resource::Resource as _;

fn entry_named(app: &TestApp<CodeEditorShell>, name: &str) -> String {
    app.state()
        .files
        .iter()
        .find(|(_, entry)| entry.name == name)
        .unwrap_or_else(|| panic!("an entry named {name}"))
        .0
        .to_string()
}

#[allow(
    clippy::too_many_lines,
    reason = "one agent session, then the history it left"
)]
#[test]
fn an_agent_changes_a_project_in_one_call_each_and_history_tells_what_it_did() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<CodeEditorShell>::new().unwrap();
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let kind = Project::KIND;
    let call = |app: &mut TestApp<CodeEditorShell>, resource: &str, id: &str, args| {
        app.call_by(agent.clone(), resource, id, args)
            .unwrap_or_else(|error| panic!("{id}: {error:?}"));
    };

    call(
        &mut app,
        kind,
        "new-folder",
        serde_json::json!({ "name": "src" }),
    );
    let src = entry_named(&app, "src");
    call(
        &mut app,
        kind,
        "new-file",
        serde_json::json!({ "name": "main.rs", "folder": src, "text": "fn main() {}\n" }),
    );
    let main = entry_named(&app, "main.rs");
    let typed = Project::paths()
        .buffers
        .key(main.clone())
        .perform(&SourceEdit::between(
            "fn main() {}\n",
            "fn main() { run() }\n",
        ))
        .unwrap();
    call(
        &mut app,
        kind,
        "field-edit",
        serde_json::json!({ "edit": typed }),
    );
    let item = actions::file_resource_id(&main);
    call(
        &mut app,
        &item,
        "rename",
        serde_json::json!({ "name": "app.rs" }),
    );
    call(&mut app, &item, "move-entry", serde_json::json!({}));
    let language = Project::paths()
        .files
        .item(&ui::tilo_resource::Id::from(main.as_str()))
        .language
        .set("Plain Text".to_owned());
    call(
        &mut app,
        kind,
        "field-edit",
        serde_json::json!({ "edit": language }),
    );
    assert!(
        app.call_by(agent.clone(), &item, "delete", serde_json::json!({}))
            .is_err(),
        "deleting asks for confirmation"
    );

    let state = app.state();
    assert_eq!(
        state.path_of(&main.as_str().into()).as_deref(),
        Some("app.rs")
    );
    assert_eq!(
        app.read_document(|doc| {
            ui::automerge::ReadDoc::get(doc, ui::automerge::ROOT, "buffers")
                .unwrap()
                .and_then(|(_, buffers)| {
                    ui::automerge::ReadDoc::get(doc, &buffers, main.as_str())
                        .unwrap()
                        .map(|(_, text)| ui::automerge::ReadDoc::text(doc, &text).unwrap())
                })
        })
        .as_deref(),
        Some("fn main() { run() }\n")
    );

    let english = include_str!("../locales/en-US.ftl");
    let catalogue = tilo_history::Catalogue::new("en-US", &[english]);
    let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
    let entries: Vec<_> = app
        .versions()
        .into_iter()
        .filter_map(|version| version.context)
        .filter(|entry| {
            entry
                .invoker
                .as_ref()
                .is_some_and(|invoker| invoker.actor == "agent:claude")
        })
        .collect();
    let sentences: Vec<String> = entries.iter().map(|entry| reader.sentence(entry)).collect();
    assert_eq!(
        sentences,
        [
            "Created the folder src",
            "Created main.rs",
            "Edited a file",
            "Renamed main.rs to app.rs",
            "Moved app.rs",
            "Set the language of app.rs to Plain Text",
        ]
    );
    let invoker = entries[0].invoker.as_ref().unwrap();
    assert_eq!(
        (invoker.kind, invoker.on_behalf_of.as_deref()),
        (ActorKind::Agent, Some("ada")),
        "the entry names the agent and the person it acted for"
    );
}
