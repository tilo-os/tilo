//! The cloud service's ask for the file accounts that changed, over the socket it reaches: held
//! through a change of another record, answered once Settings keeps an account in the workspace's
//! accounts record, and let go once the asker goes away.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, and each test keeps its session to its end"
)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use accounts::person::{ACCOUNTS_OWNER, PersonAccounts};
use accounts::sign_ins::{self, Answer, Ask, Client};
use tilo_actions::remote::Caller;
use tilo_resource::{Replica, Resource as _};
use tilo_workspace_service::workspaces::account_logins::accounts_document;

use crate::records::testing::Device;

/// The workspace this device holds.
const WORKSPACE: &str = "6d1c1f36-0000-4000-8000-0000000000c1";
/// Another app's record of it, which changes while the cloud service waits.
const NOTES: &str = "so.tilo.notes";
const NOTE: &str = "tab:00000000000000c1";

/// This device holding the workspace, and its socket served as the session serves it.
struct Served {
    device: Device,
    socket: PathBuf,
    _runtime: tokio::runtime::Runtime,
    _dir: tempfile::TempDir,
}

fn served() -> Served {
    let device = Device::holding(&[WORKSPACE]);
    let dir = tempfile::tempdir().unwrap();
    let socket = sign_ins::socket_in(dir.path());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (workspaces, changes, at) = (device.workspaces(), device.changes(), socket.clone());
    runtime.spawn(async move {
        let by = Caller::authenticated([0x30; 32], [0x31; 32]);
        super::serving(&at, workspaces, by, changes).await
    });
    Served {
        device,
        socket,
        _runtime: runtime,
        _dir: dir,
    }
}

/// A connection to `socket` once the session serves it.
fn connect(socket: &Path) -> Client {
    let started = Instant::now();
    loop {
        match Client::connect(socket) {
            Ok(client) => return client,
            Err(error) => assert!(
                started.elapsed() < tilo_test_env::hang_bound(),
                "the session never served its socket: {error}"
            ),
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The revision of the file accounts the workspace holds now, as the cloud service first asks.
fn revision(socket: &Path) -> String {
    match connect(socket).ask(&Ask::Accounts { after: None }) {
        Ok(Answer::Accounts { revision, accounts }) => {
            assert!(accounts.is_empty(), "the workspace keeps no file account yet");
            revision
        }
        other => panic!("the accounts, not {other:?}"),
    }
}

/// Wait until `served`'s session holds `count` asks open on the stream of record changes, well
/// before an ask's longest wait lets it go anyway.
fn until_waiting(served: &Served, count: usize) {
    let started = Instant::now();
    while served.device.changes().waiting() != count {
        assert!(
            started.elapsed() < sign_ins::LONGEST_WAIT.checked_div(5).unwrap(),
            "the session holds {} asks open, not {count}",
            served.device.changes().waiting(),
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The workspace's accounts record keeping `account`, as Settings writes it.
fn accounts_keeping(account: &accounts::Account) -> automerge::AutoCommit {
    let document = accounts_document();
    let mut doc = automerge::AutoCommit::new();
    collaboration::resource::genesis(
        &mut doc,
        &collab_types::document::DocumentCheckout::new(
            collab_types::document::DocumentId::parse(&document).unwrap(),
            collab_types::document::ResourceType::new(PersonAccounts::KIND),
        ),
        WORKSPACE,
        |doc| PersonAccounts::initialize(doc).map_err(|error| error.to_string()),
    )
    .unwrap();
    let mut replica = Replica::<PersonAccounts>::from_doc(doc).in_app(ACCOUNTS_OWNER, "Settings");
    replica
        .apply_confirmed(PersonAccounts::saving(account))
        .unwrap();
    automerge::AutoCommit::load(&replica.save()).unwrap()
}

#[test]
fn a_held_ask_is_answered_once_the_workspace_keeps_a_file_account() {
    let served = served();
    let before = revision(&served.socket);
    let (answered, answer) = mpsc::channel();
    let socket = served.socket.clone();
    let asked = Instant::now();
    std::thread::spawn(move || {
        let held = connect(&socket).ask(&Ask::Accounts {
            after: Some(before),
        });
        answered.send(held.unwrap()).ok();
    });
    until_waiting(&served, 1);

    // Another app's record changing is no news to the cloud service.
    served
        .device
        .name(NOTES, WORKSPACE, &[(NOTE, "document", Some("Groceries"))]);
    assert!(
        matches!(
            answer.recv_timeout(Duration::from_millis(200)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "the ask is held through a change of another record"
    );

    let mut account = accounts::Account::new(
        accounts::AccountId::from("team-files"),
        accounts::cloud_types::WEBDAV,
    );
    account.display_name = "Team files".into();
    served.device.keep(
        WORKSPACE,
        (ACCOUNTS_OWNER, &accounts_document()),
        accounts_keeping(&account),
    );
    let held = std::cell::RefCell::new(None);
    served.device.until(|| {
        if let Ok(answered) = answer.try_recv() {
            *held.borrow_mut() = Some(answered);
        }
        held.borrow().is_some()
    });
    assert!(
        asked.elapsed() < sign_ins::LONGEST_WAIT,
        "answered when the accounts record changed, not when the wait ran out"
    );
    match held.into_inner() {
        Some(Answer::Accounts { accounts, .. }) => assert_eq!(
            accounts
                .iter()
                .map(|held| (held.workspace.as_str(), held.account.id.as_str()))
                .collect::<Vec<_>>(),
            [(WORKSPACE, "team-files")],
        ),
        other => panic!("the accounts, not {other:?}"),
    }
    until_waiting(&served, 0);
}

#[test]
fn an_asker_that_goes_away_is_no_longer_waited_for() {
    let served = served();
    let before = revision(&served.socket);
    let mut asker = tilo_files::native::socket::connect(&served.socket).unwrap();
    let mut line = serde_json::to_vec(&Ask::Accounts {
        after: Some(before),
    })
    .unwrap();
    line.push(b'\n');
    asker.write_all(&line).unwrap();
    until_waiting(&served, 1);

    drop(asker);
    until_waiting(&served, 0);
}
