//! A secret of the workspace's vault reaches the programs its Linux machine starts, and nothing
//! else: the person keeps a token in the vault, names it in the workspace's machine secrets under
//! a variable, and the next command their agent starts in the machine hands its program it.
//! The token is in no record, no answer, no log line and no file on the server's disk in the
//! clear. Removed, it stops arriving with the next command; an item the vault does not hold stops
//! the command in plain words; whoever may not read the vault is refused it; and a terminal the
//! person opens in the machine starts its programs with it too.
use super::agent_mcp::tool;
use super::*;
use serde_json::json;
use tilo_actions::remote::person_vault;
use tilo_environment::machine_secrets::{DOCUMENT, MachineSecrets};
use tilo_mcp::McpServer;
use tilo_session_actions::SessionActions;
use tilo_session_client::Outbox as _;
/// A stand-in for the service selling the workspace's Linux machines.
type Selling = ush::hosted_vm::test_host::TestHost;

use super::hosted_secret_use::{files_holding, logged};

/// What the person keeps in the vault: nothing but a program in the machine may hold it.
const TOKEN: &str = "sk-ant-oat01-only-a-program-in-the-machine-may-read-this";
const VARIABLE: &str = "CLAUDE_CODE_OAUTH_TOKEN";
/// The account the machine is paid from.
const ACCOUNT: &str = "acct-1";

/// The workspace's machine secrets document.
fn secrets_route() -> DocumentRoute {
    DocumentRoute {
        app_id: ui::apps::TERMINAL.into(),
        cluster_id: WORKSPACE.into(),
        document: DOCUMENT.into(),
    }
}

/// The person changes the workspace's machine secrets from their Terminal: the change is made
/// on their copy of the record and written as theirs, and the server takes it as the
/// workspace's policy lets them.
async fn change_secrets(
    page: &Browser,
    workspaces: &tilo_server::workspaces::Workspaces,
    mine: &mut ui::tilo_resource::Replica<MachineSecrets>,
    change: ui::tilo_resource::Invoke<MachineSecrets>,
) {
    mine.set_now_ms(tilo_clock::unix_now_ms_i64());
    mine.apply(change).expect("the change applies");
    let changed = mine.doc_mut().save_incremental();
    page.peer
        .write(
            secrets_route(),
            &collab_types::DocumentMutation::Incremental(changed),
        )
        .unwrap();
    let wanted = mine.state().unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let held = workspaces
            .resource_document(WORKSPACE, ui::apps::TERMINAL, DOCUMENT)
            .await
            .ok()
            .flatten()
            .and_then(|bytes| automerge::AutoCommit::load(&bytes).ok())
            .and_then(|doc| <MachineSecrets as ui::tilo_resource::Resource>::read(&doc).ok());
        if held.as_ref() == Some(&wanted) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the server never takes the change"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The session the terminal opens on.
const TERMINAL_SESSION: &str = "run:terminal";

/// A new terminal on [`TERMINAL_SESSION`] for `device`, opened by `by` through the host's
/// shells, as Terminal's `open-terminal` opens one.
async fn open_terminal(
    shells: &Arc<tilo_server::agents::HostShells>,
    device: &Device,
    by: &tilo_actions::remote::Caller,
) -> Result<tilo_agent_session::workspace_actions::Opened, String> {
    use tilo_agent_session::workspace_actions::WorkspaceShell as _;
    let opening = tilo_agent_session::workspace_actions::Opening {
        workspace: WORKSPACE.into(),
        session: TERMINAL_SESSION.into(),
        device: device.cert.device_public_key,
        cols: 100,
        rows: 30,
        person: None,
        attach: None,
    };
    let (shells, by) = (Arc::clone(shells), by.clone());
    tokio::task::spawn_blocking(move || shells.open(&opening, &by, Arc::new(|_| {})))
        .await
        .unwrap()
}

/// The variables the newest program the machine started was given.
fn last_started(vm: &Selling) -> (Vec<String>, Vec<(String, String)>) {
    vm.launched().pop().expect("the machine started a program")
}

fn has_secret(env: &[(String, String)]) -> bool {
    env.iter()
        .any(|(name, value)| name == VARIABLE && value == TOKEN)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vault_item_named_for_the_machine_reaches_its_programs_and_nothing_else() {
    let lines = logged();
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let laptop_device = enrol(&root, "laptop", DeviceCapabilities::primary());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let state = crate::support::scratch_dir();
    let machines = tempfile::tempdir().unwrap();
    let vm = Selling::start(machines.path());
    vm.add_card(ACCOUNT);
    let shells = Arc::new(tilo_server::agents::HostShells::at(
        tilo_server::agents::ShellsAt::Here(
            ush::environment_terminals::EnvironmentTerminals::new(Arc::new(
                ush::workspace_shell::WorkspaceShells::new(
                    machines.path().join("shells"),
                    Some(vm.target(ACCOUNT)),
                ),
            )),
            Arc::new(tilo_server::machine_agents::MachineAgentsServer::new()),
        ),
    ));
    let hosted = crate::harness::hosted::start(
        &root,
        &server_device,
        state.path(),
        Options {
            shell: Some(Arc::clone(&shells)),
            ..Options::offering(crate::common::catalogue)
        },
    )
    .await;
    let _asked = crate::harness::hosted::Laptop::new(
        ["Claude Code".to_owned()],
        crate::harness::hosted::Unlisted::Deny,
    )
    .spawn(&hosted.server, root.clone(), &laptop_device);
    let mut page = Browser::open(&hosted, &browser_device, &server_device).await;
    let agent = Browser::open(&hosted, &computer, &server_device).await;
    let _ = tilo_session_client::device::session_as_agent(&agent.peer, "Claude Code").await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let server = McpServer::new(
        SessionActions::new(
            &agent.peer,
            agent.workspace,
            server_device.cert.device_public_key,
        ),
        WORKSPACE.into(),
        person.clone(),
    )
    .with_agent_session("session-1");
    let command = |line: &'static str| {
        let server = &server;
        async move {
            tool(
                server,
                "tilo_do",
                json!({"path": "terminal#run", "args": {"command": line}}),
            )
            .await
        }
    };

    // The person keeps the token in the vault from their browser, which answers its reference.
    let kept = page
        .invoke_on(
            person_vault::PROVIDER,
            ActionAddress::new(person_vault::RESOURCE, person_vault::ADD),
            InvokeArgument::Json(
                json!({"kind": "key", "belongsTo": ["https://api.anthropic.com"], "secret": TOKEN})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .expect("the vault keeps the token")
        .value
        .unwrap_or_default();
    let reference = kept["reference"].as_str().expect("a reference").to_owned();

    // Before anything is named, the machine's programs get nothing.
    let (ran, failed) = command("claude --version").await;
    assert!(!failed && ran.contains("ran claude --version"), "{ran}");
    assert!(!has_secret(&last_started(&vm).1));

    // The person names the item for the machine, and the next program gets it.
    let workspaces = hosted.server.workspaces().unwrap().clone();
    // A member's Terminal opens the record, which creates it empty.
    let mut mine = ui::tilo_resource::Replica::<MachineSecrets>::new()
        .as_author(&person)
        .in_app(ui::apps::TERMINAL, "Terminal");
    let bytes = mine.doc_mut().save();
    page.peer
        .write(
            secrets_route(),
            &collab_types::DocumentMutation::Snapshot(bytes),
        )
        .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    while workspaces
        .resource_document(WORKSPACE, ui::apps::TERMINAL, DOCUMENT)
        .await
        .ok()
        .flatten()
        .is_none()
    {
        assert!(
            Instant::now() < deadline,
            "the server never holds the record"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    change_secrets(
        &page,
        &workspaces,
        &mut mine,
        MachineSecrets::ADD_SECRET.with((VARIABLE.to_owned(), reference.clone())),
    )
    .await;
    let (ran, failed) = command("claude --version").await;
    assert!(!failed && ran.contains("ran claude --version"), "{ran}");
    let (argv, env) = last_started(&vm);
    assert!(
        has_secret(&env),
        "the program starts with the token: {argv:?}"
    );
    assert!(!ran.contains(TOKEN), "the answer never holds it");
    assert!(
        !argv.iter().any(|argument| argument.contains(TOKEN)),
        "nor do the program's arguments"
    );

    let opener = tilo_actions::remote::Caller::local_person(person.clone());

    // A terminal the person opens on the session starts its programs with it too, and the
    // shell itself never holds it.
    let opened = open_terminal(&shells, &browser_device, &opener)
        .await
        .expect("the terminal opens");
    let tilo_server::agents::ShellsAt::Here(terminals, _) = shells.shells() else {
        panic!("the shells run here");
    };
    let typing = terminals
        .attach(WORKSPACE, TERMINAL_SESSION, &opened.session)
        .expect("the session is kept");
    let _ = typing.hear(Duration::from_secs(30));
    assert!(typing.tell(&ush::environment_terminals::ToTerminal::In(
        b"claude --from-the-terminal\r".to_vec()
    )));
    let typed = tilo_test_env::wait_for("the program typed at the terminal to start", || {
        vm.launched().into_iter().find(|(argv, _)| {
            argv.iter()
                .any(|argument| argument == "--from-the-terminal")
        })
    });
    assert!(
        has_secret(&typed.1),
        "the terminal's program starts with the token"
    );
    drop(typing);

    // Whoever may not read the workspace's vault is refused its item, and opens no terminal
    // with it.
    let stranger = hex::encode([7_u8; 32]);
    let refused = open_terminal(
        &shells,
        &browser_device,
        &tilo_actions::remote::Caller::local_person(stranger.clone()),
    )
    .await;
    assert!(
        refused
            .as_ref()
            .is_err_and(|reason| reason.contains("may not read the workspace's vault")),
        "{:?}",
        refused.map(|_| ())
    );
    let refused = workspaces
        .workspace_secret(
            WORKSPACE,
            &person_vault::ItemRef::parse(&reference).unwrap(),
            &tilo_actions::remote::Caller::local_person(stranger),
            VARIABLE,
        )
        .await;
    assert!(
        refused
            .as_ref()
            .is_err_and(|reason| reason.contains("may not read the workspace's vault")),
        "{:?}",
        refused.map(|_| ())
    );

    // Removed, it no longer arrives with the next command.
    change_secrets(
        &page,
        &workspaces,
        &mut mine,
        MachineSecrets::REMOVE_SECRET.with(VARIABLE.to_owned()),
    )
    .await;
    let (ran, failed) = command("claude --version").await;
    assert!(!failed && ran.contains("ran claude --version"), "{ran}");
    assert!(!has_secret(&last_started(&vm).1));

    // An item the vault does not hold stops the command in plain words, and starts nothing.
    change_secrets(
        &page,
        &workspaces,
        &mut mine,
        MachineSecrets::ADD_SECRET.with((
            VARIABLE.to_owned(),
            format!("vault.item:{}", "0".repeat(32)),
        )),
    )
    .await;
    let launched = vm.launched().len();
    let (ran, failed) = command("claude --version").await;
    assert!(
        failed && ran.contains("That item is not kept here"),
        "{ran}"
    );
    let refused = open_terminal(&shells, &browser_device, &opener).await;
    assert!(
        refused
            .as_ref()
            .is_err_and(|reason| reason.contains("That item is not kept here")),
        "a terminal does not open without it: {:?}",
        refused.map(|_| ())
    );
    assert_eq!(vm.launched().len(), launched, "nothing was started");

    // The token is nowhere a reader looks: no log line, no file of the server's or the
    // machine's in the clear, and not in the document that names it.
    let logged = String::from_utf8_lossy(&lines.0.lock().unwrap()).into_owned();
    assert!(!logged.contains(TOKEN), "no log line holds the token");
    for dir in [state.path(), machines.path()] {
        assert_eq!(
            files_holding(dir, TOKEN.as_bytes()),
            Vec::<std::path::PathBuf>::new(),
            "no file holds the token in the clear"
        );
    }
    let saved = workspaces
        .resource_document(WORKSPACE, ui::apps::TERMINAL, DOCUMENT)
        .await
        .unwrap()
        .expect("the machine secrets document");
    assert!(
        !saved
            .windows(TOKEN.len())
            .any(|window| window == TOKEN.as_bytes()),
        "the document names the item, never its secret"
    );
}

/// A member names a vault item for the machine before any window made the record: the app's
/// record is founded so its document has somewhere to stand, the record's declared verbs are
/// offered there, and the document the server's machine secrets read names the item.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_names_a_vault_item_before_any_window_made_the_record() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = crate::harness::hosted::start(
        &root,
        &server_device,
        state.path(),
        Options::offering(crate::common::catalogue),
    )
    .await;
    let mut page = Browser::open(&hosted, &browser_device, &server_device).await;
    // The person keeps the token in the vault from their browser, which answers its reference.
    let kept = page
        .invoke_on(
            person_vault::PROVIDER,
            ActionAddress::new(person_vault::RESOURCE, person_vault::ADD),
            InvokeArgument::Json(
                json!({"kind": "key", "belongsTo": ["https://api.anthropic.com"], "secret": TOKEN})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .expect("the vault keeps the token")
        .value
        .unwrap_or_default();
    let reference = kept["reference"].as_str().expect("a reference").to_owned();
    let workspaces = hosted.server.workspaces().unwrap().clone();
    let route = secrets_route();
    assert!(
        workspaces
            .resource_document(WORKSPACE, ui::apps::TERMINAL, DOCUMENT)
            .await
            .unwrap()
            .is_none(),
        "nobody made the record yet"
    );
    // Its own listing offers what the record declares, its `add-secret` among them; the record's
    // resources stand under the app's own, which the page lists first.
    let provider = resource_provider(&route);
    let top = page
        .catalogue(CatalogueQuery {
            provider: Some(provider.clone()),
            max_results: 200,
            ..CatalogueQuery::default()
        })
        .await
        .unwrap();
    assert!(
        top.rows
            .iter()
            .any(|row| row.resource.id == "terminal.session"),
        "the page stands on the app's own record, which publishes the document: {:?}",
        top.rows
    );
    let listed = page
        .catalogue(CatalogueQuery {
            provider: Some(provider.clone()),
            parent_resource: Some("terminal.session".to_owned()),
            max_results: 200,
            ..CatalogueQuery::default()
        })
        .await
        .unwrap();
    let offered: Vec<String> = listed
        .rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .map(|action| format!("{}#{}", action.address.resource, action.address.action))
        .collect();
    assert!(
        offered
            .iter()
            .any(|shown| shown == "terminal.machine-secrets#add-secret"),
        "the record offers add-secret: {offered:?} of {:?}",
        listed.rows
    );
    // The member invokes it as `tilo do` does.
    let added = page
        .invoke_settled(request(
            &provider,
            ActionAddress::new("terminal.machine-secrets", "add-secret"),
            InvokeArgument::Json(
                json!({"variable": VARIABLE, "item": reference})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        ))
        .await;
    assert!(
        matches!(added.status, ReceiptStatus::Completed { .. }),
        "a member names the item: {added:?}"
    );
    // What the server reads when a program starts in the machine names the item.
    let caller = tilo_actions::remote::Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser_device.cert.device_public_key,
    )
    .in_workspace(WORKSPACE);
    let named = workspaces
        .machine_secrets(WORKSPACE, &caller)
        .await
        .expect("the machine's secrets read");
    assert_eq!(
        named,
        vec![tilo_actions::remote::environment_commands::VmSecret {
            variable: VARIABLE.into(),
            value: TOKEN.into(),
        }],
        "the document the server reads names the item"
    );
}
