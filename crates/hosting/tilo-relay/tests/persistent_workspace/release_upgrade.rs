//! The release of tilo.sh that starts its relay on fresh state, on the state an
//! earlier release kept: the relay starts on its own new directory, the earlier
//! release's files stay as they were, and the people it kept arrive by the
//! export and the import.
//!
//! `tools/relay-upgrade/run.sh` builds the release the live relay runs, has it
//! write its state with that release's own harness
//! (`tools/relay-upgrade/<release>/seed.rs`) and runs this on the same volume,
//! with `TILO_UPGRADE_DIR` naming it. Nothing here writes the released state
//! itself: what the export reads is what the release wrote.

use super::*;
use tilo_relay_protocol::release_people::People;
use tilo_relay_protocol::workspaces::{WorkspacesReply, WorkspacesRequest};
use tilo_session_client::Event;

/// What the release's seed wrote beside its state, `seed.json`.
#[derive(serde::Deserialize)]
struct Seed {
    workspace: String,
    declared_owner: Person,
    /// The person the owner invited at an address, and who redeemed that
    /// invitation: the release kept an address for them.
    invited_by_address: Person,
}

#[derive(serde::Deserialize)]
struct Person {
    seed: u8,
    name: String,
}

impl Person {
    fn device(&self) -> (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert) {
        own_device(self.seed, &self.name)
    }

    fn root(&self) -> String {
        hex::encode(self.device().1.user_public_key)
    }
}

/// Every file under `root`, with the SHA-256 of its bytes: what the released
/// state is, to say it is what it was after something else ran beside it.
fn contents(root: &std::path::Path) -> std::collections::BTreeMap<String, String> {
    use sha2::Digest as _;
    let mut files = std::collections::BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                let name = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                files.insert(name, hex::encode(sha2::Sha256::digest(&bytes)));
            }
        }
    }
    files
}

/// Run `tilo-relay` with `arguments`, and what it said; a failure fails the
/// check with its words.
fn relay(arguments: &[&str]) -> String {
    let output = std::process::Command::new(tilo_test_env::package_binary!("tilo-relay"))
        .env("NO_COLOR", "1")
        .args(arguments)
        .output()
        .expect("tilo-relay runs");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "tilo-relay {} failed: {said}",
        arguments.join(" ")
    );
    said
}

/// One operator command over the workspace's socket, and the event it
/// answered.
fn operator(socket: &std::path::Path, command: serde_json::Value) -> serde_json::Value {
    use std::io::{Read as _, Write as _};
    let mut connection = std::os::unix::net::UnixStream::connect(socket)
        .unwrap_or_else(|error| panic!("{} does not listen: {error}", socket.display()));
    connection
        .write_all(format!("{command}\n").as_bytes())
        .unwrap();
    let mut answer = String::new();
    connection.read_to_string(&mut answer).unwrap();
    let line = answer.trim();
    let event = line
        .strip_prefix("tilo-workspace-operator ")
        .unwrap_or_else(|| panic!("the workspace answered {line:?}"));
    serde_json::from_str(event).unwrap_or_else(|error| panic!("{error}: {event}"))
}

/// Mail a door sends, kept as it was posted: what the people who were mailed an
/// invitation got, and nothing reaches a real mail service.
#[derive(Clone, Default)]
struct MailSink(std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>);

impl MailSink {
    /// A sink listening on a free loopback port, and the URL a door posts to.
    fn start() -> (Self, String) {
        use std::io::{BufRead as _, BufReader, Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
        let address = listener.local_addr().expect("the sink's address");
        let sink = Self::default();
        let kept = sink.0.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut reading = BufReader::new(stream.try_clone().expect("the request reads"));
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if !matches!(reading.read_line(&mut line), Ok(read) if read > 0) {
                        break;
                    }
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0; length];
                let _ = reading.read_exact(&mut body);
                if let Ok(mail) = serde_json::from_slice::<serde_json::Value>(&body) {
                    kept.lock().unwrap().push(mail);
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}",
                );
            }
        });
        (sink, format!("http://{address}/emails"))
    }

    fn mails(&self) -> Vec<serde_json::Value> {
        self.0.lock().unwrap().clone()
    }
}

/// The Lounge as this checkout's image starts it, on `state`, its operator
/// socket at `socket`, its door mailing through `mail` at `entry`. Its owner
/// root is the one the release kept: the deployment mounts the same secret
/// before and after the switch.
fn start_release(
    state: &std::path::Path,
    root: &std::path::Path,
    socket: &std::path::Path,
    key: &std::path::Path,
    entry: &str,
    mail: &str,
) -> Workspace {
    std::fs::create_dir_all(state).unwrap();
    let log = std::env::var("TILO_UPGRADE_RELAY_LOG").unwrap_or_else(|_| "warn".to_owned());
    spawn_workspace(
        state,
        root,
        &state.join("secrets"),
        &[
            "--workspace-name",
            "Lounge",
            "--listing-description",
            "The Tilo community: chat, notes and a shared desktop",
            "--default-role",
            "editor",
            "--invitation-only",
            "--wait-list-max",
            "500",
            "--web-entry",
            entry,
            // The plain listener the deployment serves behind its ingress; the
            // check dials the address it announces.
            "--web-plain",
            "127.0.0.1:0",
            "--operator-socket",
            socket.to_str().unwrap(),
        ],
        &log,
        std::process::Stdio::inherit(),
        &[
            ("TILO_WORKSPACES_LOCAL", "1"),
            (
                "TILO_WORKSPACES_MAIL_FROM",
                "Tilo Lounge <lounge@example.org>",
            ),
            ("TILO_WORKSPACES_RESEND_KEY_FILE", key.to_str().unwrap()),
            ("TILO_WORKSPACES_RESEND_URL", mail),
        ],
    )
    .unwrap()
}

/// The workspace's first answer, or why there was none: a start the state
/// refused ends the process, and says so on its standard error.
fn started_or_why(workspace: &mut Workspace) -> Started {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        match workspace.events.recv_timeout(Duration::from_millis(200)) {
            Ok(OperatorEvent::Started {
                workspace: Some(workspace),
                plain_origin: Some(plain_origin),
                ..
            }) => {
                return Started {
                    plain: bound_address(&plain_origin),
                    workspace,
                };
            }
            Ok(started @ OperatorEvent::Started { .. }) => {
                panic!("the relay started without a plain listener: {started:?}");
            }
            Ok(_) | Err(_) => {}
        }
        if let Some(status) = workspace.child.try_wait().unwrap() {
            panic!("the relay refused the release's own start and stopped: {status}");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the relay did not start on the release's own state"
        );
    }
}

/// A page of `person`'s device in the Lounge `workspace` (the id the release
/// started with, not the one the released state names), once the host let it
/// in.
async fn their_page(
    plain: std::net::SocketAddr,
    workspace: WorkspaceId,
    person: &Person,
) -> tilo_session_client::Link {
    let (host, host_cert, _) = chat_route(plain, &workspace.to_string()).await;
    let (key, cert) = person.device();
    let link = page(host, key, cert, &host_cert);
    let events = link.events();
    link.send(Cmd::Join(workspace)).unwrap();
    wait_for(
        &format!("{} to be let in", person.name),
        || {
            events
                .drain()
                .iter()
                .any(|event| matches!(event, Event::HostReady { ws, .. } if *ws == workspace))
                .then_some(())
        },
        || format!("{:?}", link.status()),
    )
    .await;
    link
}

/// What the host answers `request` from `link`.
async fn administer(
    link: &tilo_session_client::Link,
    request: WorkspacesRequest,
) -> WorkspacesReply {
    let receipt = link.ask_workspaces(request.clone()).unwrap();
    tokio::time::timeout(tilo_test_env::hang_bound(), receipt)
        .await
        .unwrap_or_else(|_| panic!("no answer to {request:?}"))
        .unwrap_or_else(|_| panic!("the session dropped {request:?}"))
        .unwrap_or_else(|refused| panic!("{request:?} refused: {refused}"))
}

/// The addresses on the Lounge's wait list, as its owner sees them.
async fn wait_list(
    plain: std::net::SocketAddr,
    workspace: WorkspaceId,
    owner: &Person,
) -> Vec<String> {
    let link = their_page(plain, workspace, owner).await;
    let reply = administer(
        &link,
        // The workspace as the relay governs it: the id it minted at this
        // start.
        WorkspacesRequest::WaitList {
            workspace: workspace.to_string(),
        },
    )
    .await;
    link.close();
    let WorkspacesReply::WaitList(entries) = reply else {
        panic!("no wait list: {reply:?}");
    };
    entries.into_iter().map(|entry| entry.contact).collect()
}

/// The Lounge a release of tilo.sh starts on fresh state, on the state the live
/// relay's release kept: it starts on its own new directory, comes up on a clean
/// Lounge with the release's files untouched, and takes the people it kept in by
/// the export and the import — their addresses on the wait list, and an
/// invitation mailed to each of them.
#[test]
#[ignore = "run by tools/relay-upgrade/run.sh on the state a release kept"]
fn the_people_a_release_kept_arrive_in_the_release_that_starts_fresh() {
    let volume = std::path::PathBuf::from(
        std::env::var_os("TILO_UPGRADE_DIR").expect("TILO_UPGRADE_DIR names the release's state"),
    );
    let seed: Seed =
        serde_json::from_slice(&std::fs::read(volume.join("seed.json")).unwrap()).unwrap();
    // The volume as the release left it: its state at the root, as the live
    // host kept it.
    let released = volume.join("lounge");
    let before = contents(&released);
    assert!(!before.is_empty(), "the release kept no state");

    // What the release kept, read out of a copy of its state, write nothing.
    let people_path = volume.join("people.json");
    relay(&[
        "release-people",
        "export",
        "--state",
        released.to_str().unwrap(),
        "--out",
        people_path.to_str().unwrap(),
    ]);
    let people: People = serde_json::from_slice(&std::fs::read(&people_path).unwrap()).unwrap();
    assert!(
        !people.members.is_empty(),
        "the export found nobody: {people:?}"
    );
    assert!(
        people.members.iter().any(|member| member.email.is_some()),
        "the release kept no address: {people:?}"
    );
    assert_eq!(
        contents(&released),
        before,
        "the export wrote to the release's state"
    );

    // The release's own start: a new, empty directory on the same volume, the
    // way the deployment mounts it.
    let state = volume.join("state");
    let socket = state.join("operator.sock");
    let key = state.join("resend-key");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(&key, "re_local_sink").unwrap();
    let (mails, mail_url) = MailSink::start();
    let entry = "http://127.0.0.1:1/";
    let mut workspace = start_release(
        &state.join("lounge"),
        &volume.join("root"),
        &socket,
        &key,
        entry,
        &mail_url,
    );
    let started = started_or_why(&mut workspace);
    assert_ne!(
        started.workspace.to_lowercase(),
        seed.workspace.to_lowercase(),
        "the release's Lounge is a new workspace, not the released one read back"
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let listed = directory_page(started.plain).await;
        let lounge = listed
            .workspaces
            .iter()
            .find(|listed| listed.workspace == started.workspace)
            .unwrap_or_else(|| panic!("the Lounge is listed: {listed:?}"));
        assert!(
            lounge.members <= 1,
            "the Lounge starts with nobody in it but its owner: {lounge:?}"
        );
        assert_eq!(listed.workspaces.len(), 1, "{listed:?}");
    });
    assert_eq!(
        contents(&released),
        before,
        "the release's start wrote to the release's state"
    );

    // The operator declares the person it keeps the Lounge for, as the
    // deployment does, so they can see the wait list the people arrive on.
    let declared = operator(
        &socket,
        serde_json::json!({
            "op": "declare_owner",
            "user_public_key": seed.declared_owner.root(),
            "name": seed.declared_owner.name,
        }),
    );
    assert_eq!(
        declared.get("event").and_then(|event| event.as_str()),
        Some("owner_declared"),
        "{declared}"
    );
    let lounge = WorkspaceId::parse(&started.workspace).expect("the started workspace");
    runtime.block_on(async {
        assert!(
            wait_list(started.plain, lounge, &seed.declared_owner)
                .await
                .is_empty(),
            "the new Lounge starts with an empty wait list"
        );
    });

    // The import, over the socket the deployment's operator uses, as the
    // release runbook runs it.
    let said = relay(&[
        "release-people",
        "import",
        "--socket",
        socket.to_str().unwrap(),
        "--file",
        people_path.to_str().unwrap(),
    ]);

    let addressed: Vec<&str> = people
        .members
        .iter()
        .filter_map(|member| member.email.as_deref())
        .collect();
    let others: Vec<&tilo_relay_protocol::release_people::Member> = people
        .members
        .iter()
        .filter(|member| !member.role.eq_ignore_ascii_case("owner") && member.email.is_none())
        .collect();
    runtime.block_on(async {
        let waiting = wait_list(started.plain, lounge, &seed.declared_owner).await;
        for address in &addressed {
            assert!(
                waiting.iter().any(|contact| contact == address),
                "{address} is not on the wait list: {waiting:?}"
            );
        }
        // The release's own wait list, as the door kept it, is on it too.
        for kept in &people.wait_list {
            assert!(
                waiting.iter().any(|contact| *contact == kept.contact),
                "the release's {} did not come over",
                kept.contact
            );
        }
    });
    for person in &others {
        assert!(
            said.contains(person.name.as_str()),
            "{} was not invited: {said}",
            person.name
        );
    }
    // The person the release invited at an address: the address came over, and
    // the invitation mail went to it.
    let invited = &seed.invited_by_address;
    assert!(
        people
            .members
            .iter()
            .any(|member| member.name == invited.name && member.email.is_some()),
        "{}'s address is not in the export: {people:?}",
        invited.name
    );
    let mailed = mails.mails();
    for address in &addressed {
        assert!(
            mailed.iter().any(|mail| {
                mail.get("to")
                    .and_then(|to| to.as_array())
                    .is_some_and(|to| to.iter().any(|sent| sent.as_str() == Some(*address)))
                    && mail
                        .get("text")
                        .and_then(|text| text.as_str())
                        .is_some_and(|text| text.contains(entry))
            }),
            "{address} was not mailed an invitation on the door: {mailed:?}"
        );
    }
    assert!(
        !people.members.is_empty() && mailed.len() >= addressed.len(),
        "the door mailed {} invitations for {} addresses: {mailed:?}",
        mailed.len(),
        addressed.len()
    );
    assert_eq!(
        contents(&released),
        before,
        "the import wrote to the release's state"
    );
    workspace.stop();
}
