//! The account sign-ins the daemon serves below the login's runtime directory, from the
//! workspaces it holds: the cloud service's ask for the file accounts that changed is held until
//! a workspace keeps one and let go once the asker goes away, and an origin connector the daemon
//! hands its sign-ins is told its workspace's accounts only and asks only their sign-ins, decided
//! for its app.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use accounts::sign_ins::{self, Answer, Ask, Client};
use tilo_actions::remote::Caller;
use tilo_runtime::device::account_sign_ins::{self as daemon_sign_ins, AccountSignIns};
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::account_logins::MAY_NOT_READ_APP;

use crate::host;

const MAIL: &str = "so.tilo.mail";
/// A workspace this device does not hold.
const ELSEWHERE: &str = "6d1c1f36-0000-4000-8000-0000000000c2";
/// An entry of the vault no account names.
const ENTRY: &str = "vault.item:0123456789abcdef0123456789abcdef";

/// The person the daemon acts for, on this device.
fn person(workspaces: &Workspaces) -> Caller {
    let cert = workspaces.device_cert();
    Caller::authenticated(cert.user_public_key, cert.device_public_key)
}

/// The account sign-ins served again from the daemon's workspaces at a socket of the test's own,
/// as `by`: what shows how many asks it holds.
fn served_as(workspaces: &Workspaces, by: Caller) -> AccountSignIns {
    let runtime = tilo_paths::xdg::runtime_dir().expect("the test's runtime directory");
    let at = runtime
        .join(format!("held-{}", rand::random::<u32>()))
        .join("socket");
    daemon_sign_ins::serve(&at, workspaces.clone(), by).unwrap()
}

/// The daemon's own account sign-ins socket.
fn daemons_socket() -> PathBuf {
    sign_ins::socket().expect("the test's runtime directory")
}

/// A connection to `socket` once it is served.
fn connect(socket: &Path) -> Client {
    let started = Instant::now();
    loop {
        match Client::connect(socket) {
            Ok(client) => return client,
            Err(error) => assert!(
                started.elapsed() < tilo_test_env::hang_bound(),
                "the account sign-ins are never served at {}: {error}",
                socket.display()
            ),
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// `client` asked `ask`, off the runtime: the client blocks.
async fn ask(mut client: Client, ask: Ask) -> (Client, Answer) {
    tokio::task::spawn_blocking(move || {
        let answer = client.ask(&ask).unwrap();
        (client, answer)
    })
    .await
    .unwrap()
}

/// The workspaces and ids of the accounts `answer` tells.
fn told(answer: &Answer) -> Vec<(String, String)> {
    match answer {
        Answer::Accounts { accounts, .. } => accounts
            .iter()
            .map(|held| (held.workspace.clone(), held.account.id.as_str().to_owned()))
            .collect(),
        other => panic!("the accounts, not {other:?}"),
    }
}

/// The revision of the accounts `client` is told now, none of them yet.
async fn revision(client: Client) -> (Client, String) {
    let (client, answer) = ask(client, Ask::Accounts { after: None }).await;
    assert!(
        told(&answer).is_empty(),
        "the workspace keeps no account yet"
    );
    let Answer::Accounts { revision, .. } = answer else {
        unreachable!("told the accounts");
    };
    (client, revision)
}

/// Wait until `served` holds `count` asks open on a change of the accounts, well before an ask's
/// longest wait lets it go anyway.
async fn until_waiting(served: &AccountSignIns, count: usize) {
    let started = Instant::now();
    while served.waiting() != count {
        assert!(
            started.elapsed() < sign_ins::LONGEST_WAIT.checked_div(5).unwrap(),
            "the account sign-ins hold {} asks open, not {count}",
            served.waiting(),
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// `id`, an account of type `type_id` called `name`.
fn account(id: &str, type_id: &str, name: &str) -> accounts::Account {
    let mut account = accounts::Account::new(accounts::AccountId::from(id), type_id);
    account.display_name = name.into();
    account
}

/// Mail's connector in `workspace`, as the daemon starts it: handed its account sign-ins by
/// `sign_ins` over the socket it took with its launch.
fn mails_connector(sign_ins: &AccountSignIns, workspace: &str) -> Client {
    use runtime_portal::origin_connector::{self, Handed};
    let (ours, theirs) = origin_connector::pair().unwrap();
    sign_ins.hand(&ours, MAIL, workspace).unwrap();
    let Some(Handed::SignIns(handed)) = origin_connector::receive(&theirs).unwrap() else {
        panic!("the connector is handed its account sign-ins");
    };
    Client::over(std::os::unix::net::UnixStream::from(handed)).unwrap()
}

/// The account sign-ins `daemon` serves.
fn daemons(daemon: &tilo_runtime::device::services::Daemon) -> &AccountSignIns {
    daemon
        .account_sign_ins()
        .expect("the daemon serves the account sign-ins")
}

#[test]
fn a_held_ask_is_answered_once_the_workspace_keeps_a_file_account() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let workspaces = host.daemon().workspaces().clone();
        let served = served_as(&workspaces, person(&workspaces));
        let (_, before) = revision(connect(served.path())).await;
        let asked = Instant::now();
        let held = tokio::spawn(ask(
            connect(served.path()),
            Ask::Accounts {
                after: Some(before),
            },
        ));
        until_waiting(&served, 1).await;

        // An account the cloud service does not mount is no news to it: the ask stays held.
        workspaces
            .keep_workspace_account(
                &workspace,
                &account("team@example.test", accounts::email::TYPE_ID, "Team"),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !held.is_finished(),
            "the ask is held through a change of accounts it is not told"
        );
        until_waiting(&served, 1).await;

        workspaces
            .keep_workspace_account(
                &workspace,
                &account("team-files", accounts::cloud_types::WEBDAV, "Team files"),
            )
            .await
            .unwrap();
        let (_, answer) = tilo_test_env::within("the held ask is answered", held)
            .await
            .unwrap();
        assert!(
            asked.elapsed() < sign_ins::LONGEST_WAIT,
            "answered when the accounts record changed, not when the wait ran out"
        );
        assert_eq!(told(&answer), [(workspace, "team-files".to_owned())]);
        until_waiting(&served, 0).await;
    });
}

#[test]
fn an_asker_that_goes_away_is_no_longer_waited_for() {
    host::run(|host| async move {
        let workspaces = host.daemon().workspaces().clone();
        let served = served_as(&workspaces, person(&workspaces));
        let (_, before) = revision(connect(served.path())).await;
        let mut asker = tilo_files::native::socket::connect(served.path()).unwrap();
        let mut line = serde_json::to_vec(&Ask::Accounts {
            after: Some(before),
        })
        .unwrap();
        line.push(b'\n');
        asker.write_all(&line).unwrap();
        until_waiting(&served, 1).await;

        drop(asker);
        until_waiting(&served, 0).await;
    });
}

/// An origin connector of Mail the daemon starts is told its workspace's accounts, its email
/// account among them, which the cloud service is not; a sign-in it asks of another workspace is
/// refused before anything is read; one of its own workspace is decided by the daemon, in words,
/// with no fallback: the account names no vault entry.
#[test]
fn a_connector_is_told_its_workspaces_accounts_and_asks_their_sign_ins_only() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let workspaces = host.daemon().workspaces().clone();
        workspaces
            .keep_workspace_account(
                &workspace,
                &account("team@example.test", accounts::email::TYPE_ID, "Team"),
            )
            .await
            .unwrap();
        let daemon = host.daemon();
        let connector = mails_connector(daemons(&daemon), &workspace);
        let (connector, answer) = ask(connector, Ask::Accounts { after: None }).await;
        assert_eq!(
            told(&answer),
            [(workspace.clone(), "team@example.test".to_owned())]
        );
        let (_, cloud) = ask(connect(&daemons_socket()), Ask::Accounts { after: None }).await;
        assert!(
            told(&cloud).is_empty(),
            "the cloud service is told file accounts only"
        );

        let (connector, elsewhere) = ask(
            connector,
            Ask::SignIn {
                workspace: ELSEWHERE.into(),
                item: ENTRY.into(),
            },
        )
        .await;
        match elsewhere {
            Answer::Refused { error } => assert_eq!(error, daemon_sign_ins::OTHER_WORKSPACE),
            other => panic!("a refusal, not {other:?}"),
        }
        let (_, own) = ask(
            connector,
            Ask::SignIn {
                workspace: workspace.clone(),
                item: ENTRY.into(),
            },
        )
        .await;
        match own {
            Answer::Refused { error } => assert!(!error.is_empty(), "said in words"),
            other => panic!("a refusal, not {other:?}"),
        }
    });
}

/// The person's agent, bound off Mail or Files, may use the vault but not read the app asking: it
/// gets no password through a connector or the cloud service, as the app's own route gets it none.
/// Bound off nothing, the ask goes on to the account.
#[test]
fn whoever_may_not_read_the_app_asking_gets_no_password() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let workspaces = host.daemon().workspaces().clone();
        let cert = workspaces.device_cert().clone();
        let agent = |denied: &[&str]| {
            let read = tilo_action_policy::KindSet::of(tilo_action_policy::ActionKind::Read);
            let rules = denied.iter().map(|app| {
                let node = tilo_action_policy::TreePath::workspace(&workspace).app(*app);
                tilo_action_policy::Rule::new(node, read, tilo_action_policy::Answer::Deny)
            });
            Caller::delegated_agent("scout", cert.user_public_key, cert.device_public_key)
                .in_workspace(workspace.clone())
                .held_to(rules.collect())
        };
        // What a sign-in asked by Mail's connector, or else by the cloud service, is refused with
        // when the account sign-ins answer as `by`.
        let refused = async |by: Caller, connector: bool| {
            let served = served_as(&workspaces, by);
            let client = if connector {
                mails_connector(&served, &workspace)
            } else {
                connect(served.path())
            };
            let ask_for = Ask::SignIn {
                workspace: workspace.clone(),
                item: ENTRY.into(),
            };
            match ask(client, ask_for).await.1 {
                Answer::Refused { error } => error,
                other => panic!("a refusal, not {other:?}"),
            }
        };
        assert_eq!(refused(agent(&[MAIL]), true).await, MAY_NOT_READ_APP);
        let files = tilo_app_id::apps::FILE_EXPLORER;
        assert_eq!(refused(agent(&[files]), false).await, MAY_NOT_READ_APP);
        let reading = refused(agent(&[]), true).await;
        assert_ne!(
            reading, MAY_NOT_READ_APP,
            "reading Mail, the account decides: {reading}"
        );
    });
}

/// Mail's connector, holding an ask for its workspace's accounts that changed, is told the email
/// account Settings adds once the workspace keeps it, well before the ask's longest wait: the
/// account reaches a mailbox that is already open, with no reload.
#[test]
fn a_connector_is_told_an_account_added_while_it_waits() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let workspaces = host.daemon().workspaces().clone();
        let daemon = host.daemon();
        let connector = mails_connector(daemons(&daemon), &workspace);
        let (connector, before) = revision(connector).await;
        let asked = Instant::now();
        let held = tokio::spawn(ask(
            connector,
            Ask::Accounts {
                after: Some(before),
            },
        ));
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!held.is_finished(), "the ask is held while nothing changed");

        workspaces
            .keep_workspace_account(
                &workspace,
                &account("lab@story.example", accounts::email::TYPE_ID, "Ada"),
            )
            .await
            .unwrap();
        let (_, answer) = tilo_test_env::within("the held ask is answered", held)
            .await
            .unwrap();
        assert!(
            asked.elapsed() < sign_ins::LONGEST_WAIT,
            "told when the account was kept, not when the wait ran out"
        );
        assert_eq!(told(&answer), [(workspace, "lab@story.example".to_owned())]);
    });
}
