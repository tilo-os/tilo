//! A freshly founded workspace takes the first write of each default app, on a device that runs
//! those apps from their signed packages and decides its own writes as the daemon's program
//! does: against the surfaces the installed packages ship, beside the records the system keeps
//! for them. A write the workspace does not take says why in the words the daemon logs.

use serde_json::json;

use std::time::Duration;

use crate::language::files_support::person_files;
use crate::language::host;

/// The default apps a computer's image installs, by their directories under `apps/tl`.
const INSTALLED: &[&str] = &[
    "passwords",
    "files",
    "chat",
    "tasks",
    "clocks",
    "contacts",
    "settings",
];

/// The built-in apps whose roots the workspace makes the first time a person reads them, as the
/// session's search and launcher read every app's.
const ROOTED: &[&str] = &[
    "so.tilo.chat",
    "so.tilo.tasks",
    "so.tilo.clocks",
    "so.tilo.contacts",
];

#[test]
fn a_founded_workspace_takes_the_first_write_of_each_installed_default_app() {
    host::run_deciding(INSTALLED, |host| async move {
        let workspace = host.workspace().to_owned();
        let daemon = host.daemon();
        let workspaces = daemon.workspaces();
        let mut refused = Vec::new();

        let opened = workspaces.open_vault(&workspace).await.unwrap();
        let entry = accounts::SecretItem::login("Example", "ada", "correct horse battery staple");
        if let Err(error) = workspaces
            .write_vault(
                &opened,
                vec![accounts::vault_items::VaultChange::Keep(entry)],
                None,
                None,
            )
            .await
        {
            refused.push(format!("the vault: {error}"));
        }
        if let Err(error) = person_files(
            &host,
            &workspace,
            "new-folder",
            json!({"parent": "", "name": "First"}),
        )
        .await
        {
            refused.push(format!("the Files tree: {error}"));
        }
        for app in ROOTED {
            if let Err(error) = workspaces.root_app(&workspace, app, None).await {
                refused.push(format!("{app}'s root: {error}"));
            }
        }
        assert!(
            refused.is_empty(),
            "a founded workspace refused its default apps' first writes:\n{}",
            refused.join("\n")
        );
    });
}

/// The person signs in on a computer whose daemon already runs with nobody on it, as a dev
/// image's profile has it: the daemon founds them then, and the shell that starts after asks it
/// to make the workspace it started with.
#[cfg(target_os = "linux")]
#[test]
fn the_workspace_a_shell_starts_with_after_the_first_sign_in_answers_at_once() {
    host::in_own_unclaimed_process(|profile| {
        tilo_runtime::cli::decide_against_installed_surfaces();
        let running = tilo_runtime::daemon::Running::start(profile).unwrap();
        let native = running.native_socket().to_owned();
        tilo_api::daemon::DaemonPlace::at(native.parent().unwrap(), profile)
            .found(&tilo_api::daemon::Founding {
                name: Some("plan cycle owner".to_owned()),
                tickets: Vec::new(),
            })
            .expect("the computer founds its person")
            .expect("a fresh computer has no person yet");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let link = std::os::unix::net::UnixStream::from(
            running
                .link_shell(Duration::from_secs(30))
                .expect("the daemon makes the shell's link"),
        );
        link.set_nonblocking(true).unwrap();
        let shell = {
            let _inside = runtime.enter();
            runtime_collab::ShellClient::open(tokio::net::UnixStream::from_std(link).unwrap()).0
        };
        founds_and_answers(&runtime, &shell);
        drop(shell);
        drop(runtime);
        running.stop().unwrap();
    });
}

/// The shell `shell` asks its daemon to make the workspace it started with, and then asks
/// something in it: each answered within 30 s.
#[cfg(target_os = "linux")]
fn founds_and_answers(runtime: &tokio::runtime::Runtime, shell: &runtime_collab::ShellClient) {
    let seed = uuid::Uuid::new_v4().to_string();
    let mut first = serde_json::Map::new();
    first.insert("name".into(), json!("Workspace"));
    first.insert("id".into(), json!(seed));
    let dispatch = shell.dispatch();
    let founding = dispatch.settled(
        None,
        tilo_session_client::workspace_view::request(
            tilo_actions::remote::RemoteActionTarget {
                provider: tilo_action_policy::agent_answers::ACCOUNT_APP.into(),
                address: tilo_actions::ActionAddress::new(
                    tilo_peer_state::workspace_view::LIST,
                    tilo_peer_state::workspace_view::CREATE,
                ),
            },
            first,
        ),
    );
    let founded = runtime
        .block_on(async { tokio::time::timeout(Duration::from_secs(30), founding).await })
        .expect("the daemon makes the shell's first workspace within 30 s");
    assert!(
        founded.is_ok(),
        "the shell's first workspace was not made: {founded:?}"
    );

    let trust = dispatch.settled(
        Some(seed.clone()),
        tilo_session_client::workspace_view::request(
            tilo_actions::remote::RemoteActionTarget::workspace_governance(&seed, "trust.list"),
            serde_json::Map::new(),
        ),
    );
    let trust = runtime
        .block_on(async { tokio::time::timeout(Duration::from_secs(30), trust).await })
        .expect("the workspace answers within 30 s");
    assert!(
        trust.is_ok(),
        "the shell's first workspace does not answer what is asked in it: {trust:?}"
    );
}

/// An app that runs as a process of its own makes its own root when it starts, written over its
/// connection and decided by the device as every other write: Terminal's `Here`, the record that
/// says where its sessions run, is in the workspace once Terminal has started.
#[test]
fn an_app_started_as_its_own_process_makes_its_root() {
    host::run_deciding(&["terminal"], |host| async move {
        host::start_app(&host, "so.tilo.terminal")
            .await
            .expect("the workspace starts Terminal");
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut read = Ok(serde_json::Value::Null);
        while std::time::Instant::now() < deadline {
            read = host::record(&host, "so.tilo.terminal", "tl.so.tilo.terminal.Here").await;
            if read.as_ref().is_ok_and(|record| !record.is_null()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        panic!("Terminal's root never stood in the workspace: {read:?}");
    });
}
