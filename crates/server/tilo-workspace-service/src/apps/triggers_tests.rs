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
    for (id, title) in [("i-0", "Other"), ("i-1", "First")] {
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
    }
    let mut written = Vec::new();
    while let Ok(write) = writes.try_recv() {
        written.push(write);
    }
    assert!(!written.is_empty(), "the write is told to the runner");
    runner.pass(written.clone()).await;
    runner.pass(written).await;
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
