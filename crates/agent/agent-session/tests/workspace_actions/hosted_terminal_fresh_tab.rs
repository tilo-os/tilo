//! A Terminal window on a fresh tab of a hosted workspace gets its shell.
//!
//! The window shows its tab's default session record, which nothing wrote yet, and asks the
//! workspace's host to open that session's shell on a terminal: the shell lives with the
//! workspace. The host founds the record at that id, with the genesis every device derives for
//! it, so what the device writes to it later merges, and opens the shell.
use super::resource_support::{admit_person_at, open_admitted_at};
use super::*;
use tilo_actions::remote::ReceiptStatus;
use tilo_agent_session::ConfinedReplica;
use tilo_peer_state::join::Role;
use ui::resource::DocumentId;

/// The root changes of a record's saved document: its genesis.
fn genesis_of(bytes: &[u8]) -> Vec<automerge::ChangeHash> {
    let mut doc = automerge::AutoCommit::load(bytes).unwrap();
    let mut roots: Vec<_> = doc
        .get_changes(&[])
        .into_iter()
        .filter(|change| change.deps().is_empty())
        .map(|change| change.hash())
        .collect();
    roots.sort();
    roots
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_members_terminal_on_a_fresh_tab_of_a_hosted_workspace_gets_its_shell() {
    let (operator, _) = tilo_user_identity::UserIdentity::generate("Server operator", 1);
    let holder = enrol(&operator, "tilo-sh", DeviceCapabilities::member());
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let laptop = enrol(&ada, "laptop", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let shells = tempfile::tempdir().unwrap();
    let (server, _actions) = tilo_server::agents::start_serving_documents(
        crate::harness::hosted::on_this_machine(state.path()),
        tilo_server::daemon::ServerIdentity {
            user_public_key: operator.verifying_key().to_bytes(),
            device_signing_key: holder.key.clone(),
            device_cert: holder.cert.clone(),
            revocations: RevocationList::new(),
        },
        Some(Arc::new(crate::common::catalogue)),
        None,
        Some(tilo_server::agents::host_shells(shells.path().join("shells")).unwrap()),
        None,
    )
    .await
    .unwrap();
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    // The server keeps the workspace for Ada, handed to her as its owner.
    admit_person_at(&server, &holder, &workspace, &laptop, Role::Editor).await;
    workspaces
        .declare_owner(&workspace, ada.verifying_key().to_bytes(), Some("Ada"))
        .await
        .unwrap();
    let mut laptop_page = open_admitted_at(&server, &laptop, &holder, &workspace).await;

    // A fresh tab's Terminal window opens its default session's shell, as the session asks the
    // host for it: nothing wrote the record yet.
    let record = DocumentId::window_default(0x927e_0000_0000_0003);
    let app = ui::apps::TERMINAL;
    assert!(
        workspaces
            .resource_document(&workspace, app, record.route())
            .await
            .unwrap()
            .is_none(),
        "the tab's session is not written yet"
    );
    let mut arguments = serde_json::Map::new();
    arguments.insert("cols".into(), 100.into());
    arguments.insert("rows".into(), 30.into());
    let opened = laptop_page
        .invoke_settled(request(
            &format!(
                "{}:{app}:{}",
                tilo_actions::remote::RESOURCE_PROVIDER,
                record.route()
            ),
            // What Terminal's session offers a device to open its shell on a terminal.
            ActionAddress::new("terminal.session", "open-terminal"),
            InvokeArgument::Json(arguments),
        ))
        .await;
    let ReceiptStatus::Completed { output } = &opened.status else {
        panic!("the host opens the fresh tab's shell: {:?}", opened.status);
    };
    let value = output.value.clone().unwrap_or_default();
    let is_id = |name: &str| {
        value[name]
            .as_str()
            .is_some_and(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
    };
    assert!(
        is_id("stream") && is_id("session"),
        "a stream and a session: {value}"
    );

    // The host founded the record as every device's first window of it does: what the device
    // writes to it later merges with what the host holds.
    let held = workspaces
        .resource_document(&workspace, app, record.route())
        .await
        .unwrap()
        .expect("the host founded the tab's session");
    let devices = ConfinedReplica::open(
        &crate::common::catalogue(),
        app,
        &workspace,
        &MemberId::person(hex::encode(ada.verifying_key().to_bytes())),
        &ReplicaActor::Owner,
        Seed::NewAt(record.clone()),
        false,
    )
    .unwrap();
    let Some(collab_types::DocumentMutation::Snapshot(own)) = devices.take_mutation().unwrap()
    else {
        panic!("a new record is made whole");
    };
    assert_eq!(
        genesis_of(&held),
        genesis_of(&own),
        "one genesis on the host and the device"
    );

    laptop_page.peer.close();
    drop(server);
}
