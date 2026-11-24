//! The account sign-ins the daemon serves below the login's runtime directory, from the
//! workspaces it holds: the cloud service's ask for the file accounts that changed is held until
//! a workspace keeps one and let go once the asker goes away, and an origin connector the daemon
//! hands its sign-ins is told its workspace's accounts only and asks only their sign-ins, decided
//! for its app. The model manager is told the Hugging Face accounts alone, on a socket of its
//! own, and it alone keeps or forgets a sign-in.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use accounts::sign_ins::{self, Answer, Ask, Client};
use tilo_actions::remote::Caller;
use tilo_daemon::account_sign_ins::{self as daemon_sign_ins, AccountSignIns};
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
    let at = runtime.join(format!("held-{}", rand::random::<u32>()));
    daemon_sign_ins::serve(
        &at.join("socket"),
        &at.join("models"),
        workspaces.clone(),
        by,
    )
    .unwrap()
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
        .account_sign_ins::<AccountSignIns>()
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
                (&workspace, person(&workspaces).acting(), None),
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
                (&workspace, person(&workspaces).acting(), None),
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
                (&workspace, person(&workspaces).acting(), None),
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
        // Each is an agent of its own: the workspace's policy keeps every answer recorded, so one
        // agent's denial never narrows the next. An agent reads only what its person allowed it,
        // so each may use Passwords and read the apps in `allowed`, and is denied those in
        // `denied`: the app asking is what decides.
        let agent = async |name: &str, allowed: &[&str], denied: &[&str]| {
            let answered = |app: &str, answer| {
                tilo_action_policy::agent_answers::grant(
                    &workspace,
                    name,
                    (
                        &tilo_action_policy::agent_answers::AnswerScope::app(app),
                        tilo_action_policy::ActionKind::Read,
                    ),
                    answer,
                    tilo_action_policy::Grantee::person(hex::encode(cert.user_public_key)),
                )
            };
            let grants: Vec<_> = std::iter::once(tilo_app_id::apps::PASSWORDS)
                .chain(allowed.iter().copied())
                .map(|app| answered(app, tilo_action_policy::Answer::Allow))
                .chain(
                    denied
                        .iter()
                        .map(|app| answered(app, tilo_action_policy::Answer::Deny)),
                )
                .collect();
            if !grants.is_empty() {
                tilo_workspace_service::workspaces::fixtures::record_grants(
                    &workspaces,
                    &workspace,
                    &grants,
                )
                .await;
            }
            // Its person's delegation, as the workspace's roster records it: what its answers
            // then narrow, and nothing else, so an agent they leave alone reaches what its
            // person does.
            Caller::delegated_agent(name, cert.user_public_key, cert.device_public_key)
                .in_workspace(workspace.clone())
                .on_roster(0, cert.user_public_key)
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
        assert_eq!(
            refused(agent("off-mail", &[], &[MAIL]).await, true).await,
            MAY_NOT_READ_APP
        );
        let files = tilo_app_id::apps::FILE_EXPLORER;
        assert_eq!(
            refused(agent("off-files", &[], &[files]).await, false).await,
            MAY_NOT_READ_APP
        );
        let reading = refused(agent("reader", &[MAIL], &[]).await, true).await;
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
                (&workspace, person(&workspaces).acting(), None),
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

/// The workspace's Hugging Face account is told to the model manager on its own socket and to
/// nobody else: the cloud service's socket lists file accounts only. A sign-in the manager asks
/// for is Settings', so whoever may not read Settings gets none. Only the model manager keeps a
/// sign-in or forgets an account: the cloud service is refused before anything is read or
/// written.
#[test]
fn the_model_manager_is_told_the_hugging_face_account_alone() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let workspaces = host.daemon().workspaces().clone();
        let mut hub = account("hugging-face", sign_ins::MODEL_HUB_TYPE, "Hugging Face");
        hub.config = serde_json::json!({"server_url": sign_ins::MODEL_HUB_ORIGIN});
        workspaces
            .keep_workspace_account((&workspace, person(&workspaces).acting(), None), &hub)
            .await
            .unwrap();
        let models = sign_ins::models_socket().expect("the test's runtime directory");
        let (_, answer) = ask(connect(&models), Ask::Accounts { after: None }).await;
        assert_eq!(
            told(&answer),
            [(workspace.clone(), "hugging-face".to_owned())]
        );
        let (_, cloud) = ask(connect(&daemons_socket()), Ask::Accounts { after: None }).await;
        assert!(
            told(&cloud).is_empty(),
            "the cloud service is not told the Hugging Face account"
        );

        let cert = workspaces.device_cert().clone();
        // The workspace's policy is what narrows the agent: it may use Passwords, but not read
        // the app that asks.
        let answered = |app: &str, answer| {
            tilo_action_policy::agent_answers::grant(
                &workspace,
                "scout",
                (
                    &tilo_action_policy::agent_answers::AnswerScope::app(app),
                    tilo_action_policy::ActionKind::Read,
                ),
                answer,
                tilo_action_policy::Grantee::person(hex::encode(cert.user_public_key)),
            )
        };
        let grants = [
            answered(
                tilo_app_id::apps::PASSWORDS,
                tilo_action_policy::Answer::Allow,
            ),
            answered(
                tilo_app_id::apps::SETTINGS,
                tilo_action_policy::Answer::Deny,
            ),
        ];
        tilo_workspace_service::workspaces::fixtures::record_grants(
            &workspaces,
            &workspace,
            &grants,
        )
        .await;
        let agent = Caller::delegated_agent("scout", cert.user_public_key, cert.device_public_key)
            .in_workspace(workspace.clone());
        let served = served_as(&workspaces, agent);
        let models = served.path().with_file_name("models");
        let ask_for = Ask::SignIn {
            workspace: workspace.clone(),
            item: ENTRY.into(),
        };
        match ask(connect(&models), ask_for).await.1 {
            Answer::Refused { error } => assert_eq!(error, MAY_NOT_READ_APP),
            other => panic!("a refusal, not {other:?}"),
        }

        let keep = Ask::Keep {
            workspace: Some(workspace.clone()),
            secret: sign_ins::Withheld(zeroize::Zeroizing::new("hf_grant".into())),
            username: "ada".into(),
            name: None,
        };
        let forget = Ask::Forget {
            workspace: workspace.clone(),
            account: "hugging-face".into(),
        };
        for asked in [keep, forget] {
            match ask(connect(&daemons_socket()), asked).await.1 {
                Answer::Refused { error } => assert_eq!(error, daemon_sign_ins::KEEPS_NONE),
                other => panic!("a refusal, not {other:?}"),
            }
        }
        assert_eq!(
            workspaces
                .workspace_accounts(&workspace)
                .await
                .unwrap()
                .all()
                .len(),
            1,
            "the workspace still keeps its Hugging Face account"
        );
    });
}
