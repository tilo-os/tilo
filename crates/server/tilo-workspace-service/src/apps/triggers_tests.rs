//! A trigger fires a saved shortcut once workspace-wide: the device whose turn it is starts it
//! and writes where it got to in the workspace, and no device starts it again.

use std::sync::Mutex;

use automerge::transaction::{CommitOptions, Transactable as _};
use serde_json::json;
use tilo_peer_state::workspace_membership::{SettingsChange, WorkspaceMembershipCommand};

use super::*;
use crate::workspaces::fixtures::{governor, test_host};

/// The runs a test's broker was asked to start.
#[derive(Default)]
struct Started(Mutex<Vec<Firing>>);

impl StartRun for Started {
    fn start(&self, firing: &Firing) -> Result<u64, String> {
        let mut started = self.0.lock().unwrap();
        started.push(firing.clone());
        Ok(u64::try_from(started.len()).unwrap())
    }
}

tilo_test_env::scenario! {
    fn a_committed_change_is_a_record_event_only_with_its_entry;

    phase a_committed_verb_becomes_a_record_event_with_its_actor_and_facet;
    {
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "seed", 1).unwrap();
        doc.commit();
        let before = doc.get_heads();
        let context = ActionContext::for_app("so.tilo.chat", "Chat")
            .with_invoker(tilo_actions::ActionInvoker::person("bob"))
            .with_described(tilo_actions::Described::new(tilo_actions::Did::verb(
                tilo_actions::ActionKey::new("so.tilo.chat", "chat.workspace", "post"),
                [],
            )))
            .with_facet(tilo_actions::ActionFacet::new(
                tilo_actions::INCOMING_MESSAGE_FACET,
                "true",
            ));
        doc.put(automerge::ROOT, "text", "hi").unwrap();
        let hash = doc
            .commit_with(CommitOptions::default().with_message(context.change_message().unwrap()))
            .unwrap();
        let record = ("workspace-1", "rec-1", "chat.workspace");
        let events = record_events(record, &doc, Some(&before), 0);
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.change, hash.to_string());
        assert_eq!(event.actor, tilo_actions::Acting::person("bob"));
        assert!(event.incoming_message);
        assert!(
            record_events(record, &doc, Some(&doc.committed_heads()), 0).is_empty(),
            "nothing new after the last look"
        );
        assert_eq!(
            record_events(record, &doc, None, 0).len(),
            1,
            "a record's first look reports the changes made since it was watched"
        );
        assert!(
            record_events(record, &doc, None, i64::MAX).is_empty(),
            "and none made before"
        );
    }

    phase a_change_without_an_entry_is_no_event;
    {
        let mut doc = AutoCommit::new();
        let before = doc.get_heads();
        doc.put(automerge::ROOT, "x", 1).unwrap();
        doc.commit();
        assert!(record_events(("r", "rec", "k"), &doc, Some(&before), 0).is_empty());
    }
}

/// A shortcut saved with a moment that has come fires on the device that saved it, once: the
/// progress it wrote in the workspace stops every later look, its own included, from firing it
/// again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_moment_that_came_fires_its_shortcut_once_workspace_wide() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Home").await.unwrap().workspace;
    let mut shortcut: Shortcut = serde_json::from_value(json!({
        "id": "tidy",
        "name": "Tidy",
        "steps": [],
        "triggers": [{"trigger": "once", "at_ms": tilo_clock::unix_now_ms_i64() - 1_000}],
    }))
    .unwrap();
    tilo_peer_state::workspace::sign_saved_shortcut(&workspace, &mut shortcut, &cert, &key)
        .unwrap();
    workspaces
        .commit_own_membership(
            &workspace,
            WorkspaceMembershipCommand::Settings(SettingsChange::Shortcuts(
                tilo_automation::ShortcutChange::Save(Box::new(shortcut)),
            )),
            None,
        )
        .await
        .unwrap();

    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    runner.pass(Vec::new()).await;
    runner.pass(Vec::new()).await;
    let firings = started.0.lock().unwrap().clone();
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(firings[0].shortcut, "tidy");
    assert_eq!(firings[0].saver, hex::encode(cert.user_public_key));
    let mut again = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    again.arm().await;
    again.pass(Vec::new()).await;
    assert_eq!(
        started.0.lock().unwrap().len(),
        1,
        "a device that starts over reads the progress in the workspace and fires nothing again"
    );
    assert!(again.waiting().is_empty());
    host.shutdown().await;
}

/// Save `shortcut` in `workspace`, signed by the device `cert` names.
async fn save_signed(
    workspaces: &Workspaces,
    workspace: &str,
    mut shortcut: Shortcut,
    (key, cert): (&ed25519_dalek::SigningKey, &tilo_user_identity::DeviceCert),
) {
    tilo_peer_state::workspace::sign_saved_shortcut(workspace, &mut shortcut, cert, key).unwrap();
    workspaces
        .commit_own_membership(
            workspace,
            WorkspaceMembershipCommand::Settings(SettingsChange::Shortcuts(
                tilo_automation::ShortcutChange::Save(Box::new(shortcut)),
            )),
            None,
        )
        .await
        .unwrap();
}

/// A shortcut whose trigger is a change of an issue in the tasks app.
fn on_issue_change() -> Shortcut {
    serde_json::from_value(json!({
        "id": "tidy",
        "name": "Tidy",
        "steps": [],
        "triggers": [{
            "trigger": "resource_changed",
            "kind": "tasks.issue",
            "actions": [],
            "fields": [],
        }],
    }))
    .unwrap()
}

/// An app's write of a record a saved shortcut watches fires it once, on the device that wrote
/// it; looking again at the same record fires nothing more. A write whose resource the
/// trigger's filter does not match, by the fields the write left it with, fires nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_record_write_fires_the_shortcut_watching_it_once() {
    use tilo_resource_store::wire::Command;
    use tilo_resource_store::{Cell, Resource, Stamp, Value};

    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(crate::workspaces::fixtures::TestGroupKeys::default());
    let (key, cert) = governor();
    let host = crate::workspaces::fixtures::test_host_holding(dir.path(), &key, &cert, keys).await;
    let workspaces = host.workspaces();
    workspaces.set_member_device(Arc::new(crate::workspaces::MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    let workspace = workspaces.create("Tasks").await.unwrap().workspace;
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: "so.tilo.tasks".into(),
        cluster_id: workspace.clone(),
        document: String::new(),
    };
    let _ = workspaces.own_route_keys(&route, true).await.unwrap();
    let mut shortcut = on_issue_change();
    let Some(TriggerSpec::ResourceChanged { filter, .. }) = shortcut.triggers.first_mut() else {
        panic!("the shortcut watches a resource");
    };
    *filter = serde_json::from_value(json!([{ "field": "item.title", "is": "First" }])).unwrap();
    save_signed(workspaces, &workspace, shortcut, (&key, &cert)).await;

    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    let mut writes = workspaces.document_writes();
    for (id, title) in [("i-1", "First"), ("i-2", "Other")] {
        workspaces
            .own_resource_command(
                &workspace,
                "so.tilo.tasks",
                Command::Put {
                    kind: "tasks.issue".into(),
                    id: id.into(),
                    resource: Resource {
                        readers: "*".into(),
                        parent: None,
                        order: None,
                        fields: [("title".to_owned(), Cell::One(Value::Text(title.into())))].into(),
                    },
                    stamp: Stamp {
                        verb: "add".into(),
                        invoker: hex::encode(cert.user_public_key),
                        invocation: id.into(),
                        ..Stamp::default()
                    },
                },
            )
            .await
            .unwrap();
        let mut written = Vec::new();
        while let Ok(write) = writes.try_recv() {
            written.push(write);
        }
        assert!(!written.is_empty(), "the write is told to the runner");
        runner.pass(written.clone()).await;
        runner.pass(written).await;
    }
    let firings = started.0.lock().unwrap().clone();
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(firings[0].shortcut, "tidy");
    host.shutdown().await;
}

/// Only the shortcuts the device's own person saved arm here: one another person's device saved
/// in the same workspace fires nothing on this device.
#[test]
fn a_shortcut_saved_by_someone_elses_device_arms_nothing_here() {
    let (key, cert) = governor();
    let id = tilo_desktop_model::WorkspaceId::new();
    let workspace = id.to_string();
    let workspace = workspace.as_str();
    let mut shortcut = on_issue_change();
    tilo_peer_state::workspace::sign_saved_shortcut(workspace, &mut shortcut, &cert, &key).unwrap();
    let mut described = tilo_desktop_model::Workspace::new(id, "Home");
    described
        .shortcuts
        .push(tilo_automation::SavedShortcut::encode(&shortcut));
    let saver = hex::encode(cert.user_public_key);
    let now = tilo_clock::unix_now_ns();
    let none = tilo_user_identity::RevocationList::default;
    assert_eq!(
        armed(
            std::slice::from_ref(&described),
            |_| Some(none()),
            |_| saver.clone(),
            now
        )
        .len(),
        1,
        "the saver's own device arms it"
    );
    assert!(
        armed(
            &[described],
            |_| Some(none()),
            |_| "someone else".to_owned(),
            now
        )
        .is_empty(),
        "another person's device arms nothing"
    );
}

/// Whoever's turns an agent takes here: the agent `scout`, as a device that holds its key.
#[cfg(feature = "agents")]
struct ActsAsScout;

#[cfg(feature = "agents")]
impl crate::work::AgentEngine for ActsAsScout {
    fn acts_as<'a>(
        &'a self,
        _workspace: &'a str,
        agent: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { agent == "scout" })
    }
}

/// An agent's triggers fire on a message that mentions it, one in its direct conversation and
/// one an outside author's provider carried in, and each starts one turn of the agent, placed
/// once workspace-wide; what the agent writes itself, its answer, fires none of them.
#[cfg(feature = "agents")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, end to end, reads as one test"
)]
async fn an_agents_triggers_start_one_turn_per_message_and_never_on_its_own_answer() {
    use crate::agents::triggered::{self, Where};
    use tilo_action_policy::{Actor as Who, MemberId};
    use tilo_resource_store::wire::{Command, Edit};
    use tilo_resource_store::{Cell, Resource, Stamp, Value};

    const CHAT: &str = "so.tilo.lang.chat";
    const MESSAGE: &str = "tl.so.tilo.lang.chat.Message";
    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(crate::workspaces::fixtures::TestGroupKeys::default());
    let (key, cert) = governor();
    let host = crate::workspaces::fixtures::test_host_holding(dir.path(), &key, &cert, keys).await;
    let workspaces = host.workspaces();
    workspaces.set_member_device(Arc::new(crate::workspaces::MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    workspaces.run_agents(Arc::new(ActsAsScout));
    let workspace = workspaces.create("Chat").await.unwrap().workspace;
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: workspace.clone(),
        document: String::new(),
    };
    let _ = workspaces.own_route_keys(&route, true).await.unwrap();
    save_signed(
        workspaces,
        &workspace,
        triggered::triggers("scout", "Scout"),
        (&key, &cert),
    )
    .await;
    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    let mut writes = workspaces.document_writes();

    let person = tilo_actions::Acting::person(hex::encode(cert.user_public_key)).by;
    let scout = Who::member(MemberId::agent("scout"));
    let bridge = Who::member(MemberId::bot("bridge"));
    // `by` puts the resource `id` of `kind` in `parent`, with `fields` and the rich `body`.
    let put = async |kind: &str,
                     id: &str,
                     parent: Option<&str>,
                     by: &Who,
                     fields: Vec<(&str, &str)>,
                     body: Option<serde_json::Value>| {
        // The write serves an invocation of `by`'s, as the chat app's write for an action that
        // was delivered to it.
        let invocation = tilo_actions::InvocationId(uuid::Uuid::new_v4());
        let stamp = Stamp {
            verb: "send".into(),
            invoker: Stamp::by(by),
            invocation: invocation.0.to_string(),
            at: jiff::Timestamp::now().to_string(),
            ..Stamp::default()
        };
        let mut commands = vec![Command::Put {
            kind: kind.into(),
            id: id.into(),
            resource: Resource {
                readers: "*".into(),
                parent: parent.map(str::to_owned),
                order: None,
                fields: fields
                    .into_iter()
                    .map(|(name, value)| (name.to_owned(), Cell::One(Value::Text(value.into()))))
                    .collect(),
            },
            stamp: stamp.clone(),
        }];
        if let Some(body) = body {
            commands.push(Command::Edit {
                kind: kind.into(),
                id: id.into(),
                edits: vec![Edit::Set {
                    field: "body".into(),
                    value: body,
                }],
                stamp,
            });
        }
        let caller = tilo_actions::remote::Caller::authenticated(
            cert.user_public_key,
            cert.device_public_key,
        )
        .in_workspace(&workspace);
        let context =
            crate::apps::services::ResourceRequestContext::authenticated(CHAT, &workspace, caller)
                .serving(vec![(invocation, by.clone())]);
        workspaces
            .resource_command(&context, Command::Batch(commands))
            .await
            .unwrap();
    };
    // Rich text whose words are `words`, mentioning the agent where `mention` says.
    let rich = |words: &str, mention: bool| {
        let marks = if mention {
            json!([{"start": 0, "end": 5, "name": "mention", "value": {"Str": "{\"agent\":\"scout\"}"}}])
        } else {
            json!([])
        };
        json!({"blocks": [{"text": words, "marks": marks}]})
    };
    // What the runner fires on the records written since it last looked.
    let mut fired = async |runner: &mut Runner| {
        let mut since = Vec::new();
        while let Ok(write) = writes.try_recv() {
            since.push(write);
        }
        let before = started.0.lock().unwrap().len();
        runner.pass(since).await;
        started.0.lock().unwrap()[before..].to_vec()
    };
    put(
        "tl.so.tilo.lang.chat.TextChannel",
        "general",
        None,
        &person,
        vec![("name", "general")],
        None,
    )
    .await;
    put(
        "tl.so.tilo.lang.chat.Direct",
        "with-scout",
        None,
        &person,
        vec![
            ("name", "Scout"),
            ("person", &serde_json::to_string(&scout).unwrap()),
        ],
        None,
    )
    .await;
    let now = fired(&mut runner).await;
    assert!(
        now.is_empty(),
        "making the conversations asks nothing: {now:?}"
    );

    // A member mentions the agent: one firing, carrying the message.
    put(
        MESSAGE,
        "m-1",
        Some("general"),
        &person,
        vec![],
        Some(rich("Scout, look at this", true)),
    )
    .await;
    let firings = fired(&mut runner).await;
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(firings[0].shortcut, triggered::shortcut_id("scout"));
    let event: DocumentEvent =
        serde_json::from_value(firings[0].inputs[tilo_automation::EVENT_INPUT].clone()).unwrap();
    let place = Where::of(&event).unwrap();
    assert_eq!(
        (
            place.app.as_str(),
            place.record.as_str(),
            place.item.as_str()
        ),
        (CHAT, "general", "m-1"),
        "the answer goes to the message, in its conversation"
    );
    assert_eq!(
        triggered::asker(&event).as_deref(),
        Some(hex::encode(cert.user_public_key).as_str())
    );
    assert!(
        triggered::said(&event).contains("look at this"),
        "{event:?}"
    );
    // One device takes the turn, once.
    let turn = triggered::started_here(workspaces, "scout", &place);
    let claim = crate::work::takes_turn(workspaces, &workspace, &turn).await;
    assert!(claim.is_ok(), "this device takes the agent's turn");
    crate::work::keep_done(workspaces, &workspace, &turn, json!({"answered": true}))
        .await
        .unwrap();
    assert!(
        matches!(
            crate::work::takes_turn(
                workspaces,
                &workspace,
                &triggered::started_here(workspaces, "scout", &place)
            )
            .await,
            Err(crate::work::NotTaken::Done(_))
        ),
        "a turn answered is taken by no device again"
    );

    // The agent answers in the conversation, naming itself: nothing fires on its own words.
    put(
        MESSAGE,
        "m-2",
        Some("general"),
        &scout,
        vec![],
        Some(rich("Scout here: done", true)),
    )
    .await;
    let now = fired(&mut runner).await;
    assert!(now.is_empty(), "the agent's answer fires nothing: {now:?}");

    // In its direct conversation, a member's message asks it without naming it; its reply there
    // asks nothing.
    put(
        MESSAGE,
        "d-1",
        Some("with-scout"),
        &person,
        vec![],
        Some(rich("Can you help?", false)),
    )
    .await;
    let firings = fired(&mut runner).await;
    assert_eq!(firings.len(), 1, "{firings:?}");
    put(
        MESSAGE,
        "d-2",
        Some("with-scout"),
        &scout,
        vec![],
        Some(rich("Sure.", false)),
    )
    .await;
    let now = fired(&mut runner).await;
    assert!(
        now.is_empty(),
        "its reply in the direct conversation fires nothing: {now:?}"
    );

    // A message an outside author wrote, carried in by a provider, mentioning the agent: it
    // fires, and the turn is for the person whose trigger it is.
    put(
        MESSAGE,
        "m-3",
        Some("general"),
        &bridge,
        vec![],
        Some(rich("Scout, a question from outside", true)),
    )
    .await;
    let firings = fired(&mut runner).await;
    assert_eq!(firings.len(), 1, "{firings:?}");
    let event: DocumentEvent =
        serde_json::from_value(firings[0].inputs[tilo_automation::EVENT_INPUT].clone()).unwrap();
    assert_eq!(triggered::asker(&event), None, "no member asked");
    assert_eq!(firings[0].saver, hex::encode(cert.user_public_key));

    // A message nobody mentions it in fires nothing.
    put(
        MESSAGE,
        "m-4",
        Some("general"),
        &person,
        vec![],
        Some(rich("Lunch?", false)),
    )
    .await;
    let now = fired(&mut runner).await;
    assert!(
        now.is_empty(),
        "a message that names nobody fires nothing: {now:?}"
    );
    host.shutdown().await;
}
