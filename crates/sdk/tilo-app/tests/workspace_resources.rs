//! A document every app of a workspace opens beside its own: the workspace's
//! emoji, kept by Chat and read and added to from any app's window, the
//! same record on each, decided by the workspace's grants on Chat's resource.

use tilo_app::prelude::*;
use tilo_app::tilo_workspace_emoji::{self as emoji, WorkspaceEmoji, workspace_emoji as verbs};

/// The workspace both apps' windows are in.
const WORKSPACE: &str = "6f0c3a52-8e1b-4d6a-9c2f-3b7e5d1a4c90";

/// A note in a shared workspace.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Note {
    pub text: String,
}

#[resource(
    kind = "test.note",
    app = "so.tilo.test-jottings",
    catalogue = "tests/locales"
)]
impl Note {
    /// Write the note.
    #[action]
    pub fn write(&mut self, text: String) {
        self.text = text;
    }
}

/// The app that keeps the workspace's emoji.
struct ChatApp;

impl ResourceApp for ChatApp {
    type Resource = Note;
    const ID: &'static str = emoji::OWNER;
    const TITLE: &'static str = "Chat";

    fn view(_cx: &mut ViewCx<Note>) -> Element {
        Box::new(Column::new())
    }
}

/// The kind of the workspace's agents, which Assistant keeps.
const AGENTS: &str = tilo_app::tilo_workspace_agent::KIND;

/// The app that keeps the workspace's emoji and opens it with its window, as Chat does.
struct KeepingApp;

impl ResourceApp for KeepingApp {
    type Resource = Note;
    const ID: &'static str = emoji::OWNER;
    const TITLE: &'static str = "Chat";

    fn view(_cx: &mut ViewCx<Note>) -> Element {
        Box::new(Column::new())
    }

    fn resources(resources: &mut Resources<'_>) {
        resources.shared_workspace::<WorkspaceEmoji>();
    }
}

/// The app that keeps the workspace's agents and opens them with its window, as Assistant does.
struct KeepingAgentsApp;

impl ResourceApp for KeepingAgentsApp {
    type Resource = Note;
    const ID: &'static str = tilo_app::tilo_workspace_agent::OWNER;
    const TITLE: &'static str = "Assistant";

    fn view(_cx: &mut ViewCx<Note>) -> Element {
        Box::new(Column::new())
    }

    fn resources(resources: &mut Resources<'_>) {
        resources.shared_workspace::<tilo_app::tilo_workspace_agent::WorkspaceAgents>();
    }
}

/// Another app of the workspace, drawing the workspace's emoji by name on
/// [`LINES`] lines, as every line a note lays out reads them.
struct NotesApp;

thread_local! {
    static LINES: std::cell::Cell<usize> = const { std::cell::Cell::new(1) };
}

impl ResourceApp for NotesApp {
    type Resource = Note;
    const ID: &'static str = "so.tilo.test-jottings";
    const TITLE: &'static str = "Notes";

    fn view(_cx: &mut ViewCx<Note>) -> Element {
        let shown = || -> Element {
            Box::new(Text::dynamic(|| {
                let names: Vec<String> = tilo_app::workspace_emoji::emoji()
                    .iter()
                    .map(|emoji| format!(":{}:", emoji.name))
                    .collect();
                if names.is_empty() {
                    "no emoji".to_owned()
                } else {
                    names.join(" ")
                }
            }))
        };
        let lines = LINES.with(std::cell::Cell::get);
        Box::new((0..lines).fold(Column::new(), |column, _| column.push(shown())))
    }
}

/// A picture as the record keeps one: a small PNG.
fn picture() -> String {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend_from_slice(&[0; 64]);
    emoji::EmojiImage(png).to_base64()
}

fn add(name: &str) -> verbs::AddEmoji {
    verbs::AddEmoji {
        name: name.into(),
        image: picture(),
    }
}

fn names(record: &WorkspaceResource<WorkspaceEmoji>) -> Vec<String> {
    record
        .get()
        .names()
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[test]
fn every_app_of_the_workspace_reads_and_adds_to_the_one_record_its_owner_keeps() {
    let mut chat = TestApp::<ChatApp>::in_workspace(WORKSPACE).unwrap();
    let mut notes = TestApp::<NotesApp>::in_workspace(WORKSPACE).unwrap();
    let kept = chat
        .shared_workspace::<WorkspaceEmoji>(emoji::OWNER)
        .unwrap();
    let in_notes = notes
        .shared_workspace::<WorkspaceEmoji>(emoji::OWNER)
        .unwrap();
    let route = kept.record().route().unwrap();
    assert_eq!(route, in_notes.record().route().unwrap(), "one record");
    assert_eq!(route.app_id, emoji::OWNER, "on its owner's route");

    // Notes adds an emoji; Chat's copy takes it in, and Notes' window draws
    // it for its name.
    assert!(in_notes.may("add-emoji"));
    in_notes
        .invoke(&WorkspaceEmoji::ADD_EMOJI.with(add("party")))
        .unwrap();
    kept.record()
        .merge_peer_document(in_notes.record().document().unwrap())
        .unwrap();
    assert_eq!(names(&kept), ["party"]);
    notes.harness().relayout();
    let drawn = notes.harness().inspection_json().unwrap();
    assert!(drawn.contains(":party:"), "{drawn}");

    // Both apps' edits converge on the one record.
    kept.invoke(&WorkspaceEmoji::ADD_EMOJI.with(add("ship_it")))
        .unwrap();
    in_notes
        .record()
        .merge_peer_document(kept.record().document().unwrap())
        .unwrap();
    // The window follows what arrived in its refresh: its lines draw it.
    notes.harness().woken();
    let drawn = notes.harness().inspection_json().unwrap();
    assert!(drawn.contains(":party: :ship_it:"), "{drawn}");
    assert_eq!(names(&in_notes), ["party", "ship_it"]);

    // The person takes back their own.
    in_notes
        .invoke(&WorkspaceEmoji::REMOVE_EMOJI.with(verbs::RemoveEmoji {
            name: "party".into(),
        }))
        .unwrap();
    assert_eq!(names(&in_notes), ["ship_it"]);
}

/// The actions `app`'s window offers its member on `resource`, as a caller from outside is
/// listed them: what the workspace's grants do not give the member is not among them.
fn offered<A: ResourceApp>(app: &mut TestApp<A>, resource: &str) -> Vec<String> {
    let mut actions: Vec<String> = app
        .actions()
        .unwrap_or_default()
        .into_iter()
        .filter(|action| action.address.resource.as_str() == resource)
        .map(|action| action.address.action)
        .collect();
    actions.sort();
    actions
}

/// The arguments that add the emoji `party`.
fn add_party() -> serde_json::Value {
    serde_json::json!({ "name": "party", "image": picture() })
}

/// What its keeper's window holds of the workspace's emoji is offered to whoever reaches that
/// window from outside (an agent, a script, another device), beside the app's own resource:
/// its read, and every change the workspace's grants give the caller. No other app's window
/// offers any of it, and the same grants decide what a member does with them from any app.
#[test]
fn the_keepers_window_offers_the_workspaces_emoji_to_callers_from_outside() {
    let mut chat = TestApp::<KeepingApp>::in_workspace(WORKSPACE).unwrap();
    let actions = offered(&mut chat, emoji::KIND);
    for expected in ["add-emoji", "read", "remove-any-emoji", "remove-emoji"] {
        assert!(
            actions.iter().any(|action| action == expected),
            "{expected} in {actions:?}"
        );
    }
    chat.call_on(emoji::KIND, "add-emoji", add_party()).unwrap();
    let kept = chat
        .shared_workspace::<WorkspaceEmoji>(emoji::OWNER)
        .unwrap();
    assert_eq!(names(&kept), ["party"]);
    let read = chat
        .call_on(emoji::KIND, "read", serde_json::json!({}))
        .unwrap()
        .expect("a read answers what is held");
    assert!(read.to_string().contains("party"), "{read}");

    // A member the workspace lets only read is offered the read and no change, and a change
    // asked for all the same is refused and writes nothing.
    let mut viewer = TestApp::<KeepingApp>::with_role(tilo_app::Role::Viewer).unwrap();
    let actions = offered(&mut viewer, emoji::KIND);
    assert!(actions.iter().any(|action| action == "read"), "{actions:?}");
    for change in ["add-emoji", "remove-emoji", "remove-any-emoji"] {
        assert!(
            !actions.iter().any(|action| action == change),
            "{change} in {actions:?}"
        );
    }
    assert!(
        viewer
            .call_on(emoji::KIND, "add-emoji", add_party())
            .is_err()
    );
    let unchanged = viewer
        .shared_workspace::<WorkspaceEmoji>(emoji::OWNER)
        .unwrap();
    assert!(
        names(&unchanged).is_empty(),
        "a refused change writes nothing"
    );

    // An editor adds one; taking back someone else's is moderation, which is not theirs.
    let mut editor = TestApp::<KeepingApp>::with_role(tilo_app::Role::Editor).unwrap();
    let actions = offered(&mut editor, emoji::KIND);
    assert!(
        actions.iter().any(|action| action == "add-emoji"),
        "{actions:?}"
    );
    assert!(
        !actions.iter().any(|action| action == "remove-any-emoji"),
        "{actions:?}"
    );
    editor
        .call_on(emoji::KIND, "add-emoji", add_party())
        .unwrap();
    assert!(
        editor
            .call_on(
                emoji::KIND,
                "remove-any-emoji",
                serde_json::json!({ "name": "party" })
            )
            .is_err()
    );
    let added = editor
        .shared_workspace::<WorkspaceEmoji>(emoji::OWNER)
        .unwrap();
    assert_eq!(names(&added), ["party"]);

    // Another app's window opens the same emoji and offers nothing of them; no app keeps
    // them but their owner.
    let mut notes = TestApp::<NotesApp>::in_workspace(WORKSPACE).unwrap();
    assert!(
        notes
            .shared_workspace::<WorkspaceEmoji>(emoji::OWNER)
            .is_some()
    );
    assert_eq!(offered(&mut notes, emoji::KIND), Vec::<String>::new());
    assert!(
        notes
            .shared_workspace::<WorkspaceEmoji>(NotesApp::ID)
            .is_none()
    );

    // The workspace's grants on the owner's resource decide from any app: a viewer adds none
    // from Notes either, and an editor there adds and removes their own, not someone else's.
    let mut notes = TestApp::<NotesApp>::with_role(tilo_app::Role::Viewer).unwrap();
    let record = notes
        .shared_workspace::<WorkspaceEmoji>(emoji::OWNER)
        .unwrap();
    assert!(!record.may("add-emoji"), "a viewer adds none");
    assert!(
        record
            .invoke(&WorkspaceEmoji::ADD_EMOJI.with(add("party")))
            .is_err()
    );
    assert!(names(&record).is_empty());
    let mut notes = TestApp::<NotesApp>::with_role(tilo_app::Role::Editor).unwrap();
    let record = notes
        .shared_workspace::<WorkspaceEmoji>(emoji::OWNER)
        .unwrap();
    assert!(record.may("add-emoji") && record.may("remove-emoji"));
    assert!(
        !record.may("remove-any-emoji"),
        "removing another's is moderation"
    );
}

/// So are the workspace's agents, which Assistant keeps, whoever made one: the workspace's grants
/// alone decide who changes them. An owner makes one and reads it back; an editor names, chooses
/// the model of, limits and retires an agent another member made, through the record's own
/// actions as any caller reaches them; a viewer reads them and is offered none of those changes,
/// and one asked for all the same is refused.
#[test]
#[allow(clippy::too_many_lines, reason = "one flow, read top to bottom")]
fn the_workspaces_agents_are_offered_to_callers_and_changed_as_the_grants_say() {
    use tilo_app::tilo_workspace_agent::{self as workspace_agent, WorkspaceAgents};
    const CHANGES: [&str; 4] = [
        "rename-agent",
        "set-agent-model",
        "set-agent-spend-limit",
        "retire-agent",
    ];

    let mut assistant = TestApp::<KeepingAgentsApp>::in_workspace(WORKSPACE).unwrap();
    let actions = offered(&mut assistant, AGENTS);
    for expected in ["create-agent", "read"] {
        assert!(
            actions.iter().any(|action| action == expected),
            "{expected} in {actions:?}"
        );
    }
    assistant
        .call_on(
            AGENTS,
            "create-agent",
            serde_json::json!({ "name": "Scout" }),
        )
        .unwrap();
    let read = assistant
        .call_on(AGENTS, "read", serde_json::json!({}))
        .unwrap()
        .expect("a read answers what is held");
    assert!(read.to_string().contains("Scout"), "{read}");

    let mut editor = TestApp::<KeepingAgentsApp>::with_role(tilo_app::Role::Editor).unwrap();
    let agents = editor
        .shared_workspace::<WorkspaceAgents>(workspace_agent::OWNER)
        .unwrap();
    // Grace made it: the record says so, and nothing more follows from it.
    agents
        .invoke_as(
            &tilo_actions::ActionInvoker::person("grace"),
            &WorkspaceAgents::CREATE_AGENT.with(workspace_agent::workspace_agents::CreateAgent {
                avatar: None,
                name: tilo_app::tilo_resource::NonBlank::parse("Scout").expect("a name"),
                id: Some("scout".to_owned()),
                model: None,
                mcp_client: None,
            }),
        )
        .unwrap();
    assert_eq!(agents.get().agents[0].acts_for, "grace");
    // What the window offers on the agents and on each of them.
    let on_agents = |app: &mut TestApp<KeepingAgentsApp>| -> Vec<String> {
        app.actions()
            .unwrap_or_default()
            .into_iter()
            .filter(|action| action.address.resource.as_str().starts_with(AGENTS))
            .map(|action| action.address.action)
            .collect()
    };
    let offered_to_editor = on_agents(&mut editor);
    for change in CHANGES {
        assert!(
            offered_to_editor.iter().any(|action| action == change),
            "{change} in {offered_to_editor:?}"
        );
    }
    editor
        .call_on(
            AGENTS,
            "rename-agent",
            serde_json::json!({ "agent": "scout", "name": "Researcher" }),
        )
        .unwrap();
    editor
        .call_on(
            AGENTS,
            "set-agent-model",
            serde_json::json!({ "agent": "scout", "model": "anthropic/sonnet" }),
        )
        .unwrap();
    editor
        .call_on(
            AGENTS,
            "set-agent-spend-limit",
            serde_json::json!({ "agent": "scout", "limit_micros": 5_000_000 }),
        )
        .unwrap();
    let changed = agents.get();
    let scout = &changed.agents[0];
    assert_eq!(scout.name, "Researcher");
    assert_eq!(scout.model.as_deref(), Some("anthropic/sonnet"));
    assert_eq!(scout.spend_limit_micros, Some(5_000_000));
    editor
        .call_on(
            AGENTS,
            "retire-agent",
            serde_json::json!({ "agent": "scout" }),
        )
        .unwrap();
    assert!(!agents.get().agents[0].is_active(), "the editor retired it");

    // A viewer reads the agents and changes none of them, nor makes one.
    let mut viewer = TestApp::<KeepingAgentsApp>::with_role(tilo_app::Role::Viewer).unwrap();
    assert_eq!(
        offered(&mut viewer, AGENTS),
        ["read"],
        "a viewer reads the agents and makes none"
    );
    assert!(
        viewer
            .call_on(
                AGENTS,
                "create-agent",
                serde_json::json!({ "name": "Not theirs" })
            )
            .is_err()
    );
    let offered_to_viewer = on_agents(&mut viewer);
    for change in CHANGES {
        assert!(
            !offered_to_viewer.iter().any(|action| action == change),
            "{change} in {offered_to_viewer:?}"
        );
    }
    assert!(
        viewer
            .call_on(
                AGENTS,
                "rename-agent",
                serde_json::json!({ "agent": "scout", "name": "Mine" }),
            )
            .is_err()
    );
}

/// The keeping app's view of one of its documents where no window is open, as a workspace's
/// server holds it for whoever reaches the workspace from outside.
fn attached<A: ResourceApp<Resource = Note>>(
    app: &TestApp<A>,
) -> Result<tilo_app::framework::AttachedView, String> {
    let definition = tilo_app::framework::definition::<A>();
    tilo_app::framework::AttachedView::open(
        definition,
        tilo_app::framework::testing::TEST_WORKSPACE,
        tilo_app::resource::DocumentCheckout::new(
            app.record().clone(),
            definition.resource_type.clone(),
        ),
        app.read_document(|doc| doc.clone().save()),
        &MemberId::person("ada"),
        tilo_app::permissions::PermissionContext::permissive(),
        false,
    )
}

/// The actions `view` offers its member on `resource`, of everything it publishes to them.
fn offered_by(view: &tilo_app::framework::AttachedView, resource: &str) -> Vec<String> {
    tilo_actions::api::survey(&view.resources())
        .into_iter()
        .filter(|offered| offered.address.resource.as_str() == resource)
        .map(|offered| offered.address.action)
        .collect()
}

/// Everything Chat keeps for a workspace, by id: its emoji.
fn kept_by_chat() -> Vec<tilo_app::resource::DocumentId> {
    [emoji::KIND]
        .into_iter()
        .filter_map(|kind| {
            tilo_app::resource::DocumentId::parse(&tilo_app::resource::workspace::document_id(kind))
        })
        .collect()
}

/// Where no window is open, the keeping app's view opens what it keeps for the workspace only
/// once whoever holds the view said what the workspace holds of it: founded earlier, emoji the
/// workspace already has would get a second beginning. Told that the workspace holds none, it
/// offers them and sends nothing until something is written; handed what the workspace holds,
/// it opens that and sends none of it back.
#[test]
fn a_view_with_no_window_holds_the_workspaces_emoji_once_told_what_the_workspace_holds() {
    use tilo_app::collaboration::DocumentMutation;
    use tilo_app::resource::WorkspaceAnswer;

    let app = TestApp::<KeepingApp>::new().unwrap();
    let mut first = attached(&app).unwrap();
    assert_eq!(
        offered_by(&first, emoji::KIND),
        Vec::<String>::new(),
        "nothing opens before the view is told"
    );
    for kept in kept_by_chat() {
        first.workspace_answered(kept, WorkspaceAnswer::HoldsNone);
    }
    for expected in ["add-emoji", "read"] {
        let offered = offered_by(&first, emoji::KIND);
        assert!(
            offered.iter().any(|action| action == expected),
            "{expected} of {} in {offered:?}",
            emoji::KIND
        );
    }
    assert!(
        first.take_resources_beside_changes().unwrap().is_empty(),
        "what was only founded is nobody's change"
    );

    let agents_app = TestApp::<KeepingAgentsApp>::new().unwrap();
    let mut agents_view = attached(&agents_app).unwrap();
    assert_eq!(
        offered_by(&agents_view, AGENTS),
        Vec::<String>::new(),
        "nothing opens before the view is told"
    );
    let agents_doc =
        tilo_app::resource::DocumentId::parse(&tilo_app::resource::workspace::document_id(AGENTS))
            .unwrap();
    agents_view.workspace_answered(agents_doc, WorkspaceAnswer::HoldsNone);
    for expected in ["create-agent", "read"] {
        let offered = offered_by(&agents_view, AGENTS);
        assert!(
            offered.iter().any(|action| action == expected),
            "{expected} of {AGENTS} in {offered:?}"
        );
    }

    let add = tilo_actions::ActionAddress::new(emoji::KIND, "add-emoji");
    let party = add_party();
    first
        .invoke(&add, party.as_object().unwrap(), None)
        .unwrap();
    let mut sent = first.take_resources_beside_changes().unwrap();
    assert_eq!(sent.len(), 1, "the emoji, and nothing else it keeps");
    let handed = sent.remove(0);
    let whole = handed.mutation();
    let emoji_id = handed.document;
    assert!(
        matches!(whole, DocumentMutation::Snapshot(_)),
        "the first change goes out with the document it founded"
    );
    assert!(
        first.take_resources_beside_changes().unwrap().is_empty(),
        "nothing is sent twice"
    );

    // A second view is handed what the workspace now holds.
    let mut second = attached(&app).unwrap();
    second.merge_resource(&emoji_id, &whole).unwrap();
    for kept in kept_by_chat() {
        if kept != emoji_id {
            second.workspace_answered(kept, WorkspaceAnswer::HoldsNone);
        }
    }
    assert!(
        offered_by(&second, emoji::KIND)
            .iter()
            .any(|action| action == "read"),
        "what was handed over is offered"
    );
    let held = second
        .read_resource(&emoji_id, |doc| {
            <WorkspaceEmoji as tilo_app::tilo_resource::Resource>::read(doc)
                .map(|emoji| {
                    emoji
                        .names()
                        .into_iter()
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
        .unwrap_or_default();
    assert_eq!(held, ["party"], "it holds what the first view wrote");
    assert!(
        second.take_resources_beside_changes().unwrap().is_empty(),
        "what was handed over is not sent back"
    );
}

/// The first emoji added in a fresh workspace, from a refresh that runs while
/// something reads the record's store (as a browser page's frame can):
/// the action waits for the store instead of failing, and is applied, in
/// the order asked for, once the store is free.
#[test]
fn an_action_asked_for_while_the_store_is_in_use_waits_for_it() {
    use std::cell::RefCell;
    use std::rc::Rc;

    let mut chat = TestApp::<ChatApp>::in_workspace(WORKSPACE).unwrap();
    let kept = chat
        .shared_workspace::<WorkspaceEmoji>(emoji::OWNER)
        .unwrap();
    assert!(names(&kept).is_empty(), "a fresh workspace has none yet");
    let settled = Rc::new(RefCell::new(Vec::new()));
    kept.record()
        .read(|_| {
            // Asked for at once, the store in use keeps it for the next
            // refresh: input is never dropped.
            kept.invoke(&WorkspaceEmoji::ADD_EMOJI.with(add("first")))
                .expect("kept for the store to let go");
            for name in ["party", "ship_it"] {
                let settled = Rc::clone(&settled);
                kept.invoke_then(&WorkspaceEmoji::ADD_EMOJI.with(add(name)), move |added| {
                    settled.borrow_mut().push((name, added.is_ok()));
                });
            }
            // Refreshes while the store stays in use keep them waiting.
            let _ = kept.revision();
            assert!(settled.borrow().is_empty());
        })
        .unwrap();
    // The window's next refresh applies them, in order.
    assert_eq!(
        names(&kept),
        ["first", "party", "ship_it"],
        "the edit asked for in the pass landed too"
    );
    assert_eq!(*settled.borrow(), [("party", true), ("ship_it", true)]);

    // One the record refuses settles as refused, without waiting.
    kept.invoke_then(&WorkspaceEmoji::ADD_EMOJI.with(add("party")), {
        let settled = Rc::clone(&settled);
        move |added| settled.borrow_mut().push(("again", added.is_ok()))
    });
    assert_eq!(settled.borrow().last(), Some(&("again", false)));
}

/// The record is synced in the window's refresh, once a frame: the lines read what it followed.
#[test]
fn a_frame_syncs_the_workspaces_emoji_once_however_many_lines_draw_them() {
    let syncs_in_a_frame_of = |lines: usize| -> usize {
        LINES.with(|cell| cell.set(lines));
        let mut notes = TestApp::<NotesApp>::in_workspace(WORKSPACE).unwrap();
        let record = notes
            .shared_workspace::<WorkspaceEmoji>(emoji::OWNER)
            .unwrap();
        record
            .invoke(&WorkspaceEmoji::ADD_EMOJI.with(add("party")))
            .unwrap();
        notes.harness().relayout();
        let drawn = notes.harness().inspection_json().unwrap();
        assert_eq!(drawn.matches(":party:").count(), lines, "{drawn}");
        let ((), samples) =
            tilo_app::tilo_resource::profiling::observe(|| notes.harness().relayout());
        samples
            .iter()
            .filter(|sample| sample.phase == "framework.live_sync")
            .map(|sample| sample.calls)
            .sum()
    };
    let (few, many) = (syncs_in_a_frame_of(10), syncs_in_a_frame_of(200));
    assert_eq!(few, many, "syncs grow with the lines drawn");
    assert!(many <= 2, "{many} syncs in one frame");
}

/// Another app of the workspace, whose view shows the workspace's agents in a part of its
/// own, beside what the person opened and typed in the same window: a pane, and a field.
struct ReadingApp;

thread_local! {
    /// How often [`ReadingApp`]'s view, its pane and its part of the agents were built.
    static BUILT: std::cell::Cell<(usize, usize, usize)> =
        const { std::cell::Cell::new((0, 0, 0)) };
    /// What the person typed into [`ReadingApp`]'s field, as its view holds it.
    static TYPED: std::cell::RefCell<Option<LocalState<String>>> =
        const { std::cell::RefCell::new(None) };
}

fn count_built(count: impl FnOnce(&mut (usize, usize, usize))) {
    BUILT.with(|built| {
        let mut now = built.get();
        count(&mut now);
        built.set(now);
    });
}

impl ResourceApp for ReadingApp {
    type Resource = Note;
    const ID: &'static str = "so.tilo.test-jottings";
    const TITLE: &'static str = "Notes";

    fn view(cx: &mut ViewCx<Note>) -> Element {
        count_built(|built| built.0 = built.0.saturating_add(1));
        let typed = cx.local(String::new());
        TYPED.with(|held| *held.borrow_mut() = Some(typed.clone()));
        let open = cx.local(false);
        let pane = cx.switch(&open, |_cx, open| {
            count_built(|built| built.1 = built.1.saturating_add(1));
            Box::new(Text::new(if *open {
                "the pane is open"
            } else {
                "the pane is shut"
            }))
        });
        // The agents are read where they are shown: in the part that shows them.
        let always = cx.local(());
        let agents = cx.switch(&always, |cx, ()| {
            count_built(|built| built.2 = built.2.saturating_add(1));
            let shown = match cx
                .shared_workspace::<tilo_app::tilo_workspace_agent::WorkspaceAgents>(
                    tilo_app::tilo_workspace_agent::OWNER,
                ) {
                Some(agents) => {
                    let names: Vec<String> = agents
                        .get()
                        .active()
                        .map(|agent| agent.name.clone())
                        .collect();
                    if names.is_empty() {
                        "no agent works here".to_owned()
                    } else {
                        names.join(", ")
                    }
                }
                None => "the agents are not here".to_owned(),
            };
            Box::new(Text::new(&shown))
        });
        let opened = open.clone();
        Box::new(
            Column::new()
                .push(Box::new(TextInput::from_state(&typed).with_id("typed")))
                .push(Box::new(
                    Button::new("open-pane", "Open the pane").on_open(move || opened.set(true)),
                ))
                .push(pane)
                .push(agents),
        )
    }
}

/// Whether `app`'s window says `text`: as a name, a value or a description a screen reader
/// reads, or in what it draws.
fn shows<A: ResourceApp>(app: &mut TestApp<A>, text: &str) -> bool {
    let read = app.harness().a11y_nodes().iter().any(|(_, node)| {
        [node.label(), node.value(), node.description()]
            .into_iter()
            .flatten()
            .any(|said| said.contains(text))
    });
    read || app
        .harness()
        .inspection_json()
        .is_ok_and(|drawn| drawn.contains(text))
}

/// A window whose view read a document of its workspace as not here shows it once the
/// workspace delivers it, with no reload: the part that asked for it is built again, it
/// alone, and what the person had open and had typed in the same window stays.
#[test]
fn a_part_that_read_a_workspaces_document_as_not_here_shows_it_when_it_arrives() {
    use tilo_app::tilo_workspace_agent::{
        self as workspace_agent, WorkspaceAgents, workspace_agents,
    };

    let mut app =
        TestApp::<ReadingApp>::awaiting_the_workspace(WORKSPACE, workspace_agent::OWNER).unwrap();
    let agents_id =
        tilo_app::resource::DocumentId::parse(&tilo_app::resource::workspace::document_id(AGENTS))
            .unwrap();
    assert!(
        shows(&mut app, "the agents are not here"),
        "on their way, the agents are not read as none"
    );
    assert!(
        app.wanted_resources().contains(&agents_id),
        "the workspace is asked for them"
    );

    // The person opens a pane and types in the same window.
    app.harness().click_named("open-pane").unwrap();
    app.harness().click_named("typed").unwrap();
    app.harness().type_text("half a thought");
    app.frame();
    let typed = || {
        TYPED
            .with(|held| held.borrow().as_ref().map(LocalState::get))
            .unwrap_or_default()
    };
    assert!(shows(&mut app, "the pane is open"));
    assert_eq!(typed(), "half a thought");
    let before = BUILT.with(std::cell::Cell::get);

    // The workspace delivers its agents: one works there.
    app.workspace_delivers::<WorkspaceAgents>(workspace_agent::OWNER, |agents| {
        agents
            .invoke(
                &WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
                    avatar: None,
                    name: tilo_app::tilo_resource::NonBlank::parse("Scout").expect("a name"),
                    id: None,
                    model: None,
                    mcp_client: None,
                }),
            )
            .map_err(|error| error.to_string())
    })
    .unwrap();
    // The window's own frames take it up: nobody opens it again, and nothing is forced.
    for _ in 0..4 {
        app.frame();
    }
    assert!(
        shows(&mut app, "Scout"),
        "the part shows the agents that arrived, with no reload"
    );
    assert!(!shows(&mut app, "the agents are not here"));
    let after = BUILT.with(std::cell::Cell::get);
    assert_eq!(after.0, before.0, "the view was not built again");
    assert_eq!(after.1, before.1, "nor the pane beside the part");
    assert_eq!(
        after.2,
        before.2.saturating_add(1),
        "the part that asked was built again, once"
    );
    assert!(
        shows(&mut app, "the pane is open"),
        "what the person had open stays open"
    );
    assert_eq!(typed(), "half a thought", "what they had typed stays");
}
