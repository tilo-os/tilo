//! The persistent workspace as its deployment runs it: a real `tilo-server workspace
//! --persistent` process spoken to over its operator protocol, and the
//! plain-HTTP routes beside it. Every listener is bound on port 0 and the
//! workspace's first answers name the ports it got, so no test picks one another
//! process may already hold.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use tilo_peer_state::workspace_documents::{DeviceDocuments as _, DocumentRead};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::operator::{OperatorCommand, OperatorEvent, VERSION};
use tilo_session_client::{Cmd, Outbox};
use tilo_test_env::SpawnUnderEnvLock as _;

struct Workspace {
    child: Child,
    /// The operator's input; `None` once the operator closed it.
    stdin: Option<ChildStdin>,
    events: mpsc::Receiver<OperatorEvent>,
}

/// The revision a published image reports at `/mode`: the commit it was built
/// from, forty lowercase hex digits.
const SOURCE_REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

fn start(
    state: &std::path::Path,
    root_file: &std::path::Path,
    secrets: &std::path::Path,
) -> std::io::Result<Workspace> {
    start_with(state, root_file, secrets, &[])
}

fn start_with(
    state: &std::path::Path,
    root_file: &std::path::Path,
    secrets: &std::path::Path,
    extra: &[&str],
) -> std::io::Result<Workspace> {
    spawn_workspace(
        state,
        root_file,
        secrets,
        extra,
        "warn",
        Stdio::inherit(),
        &[],
    )
}

/// The workspace on `state`, as [`start_with`] starts it, logging `log`
/// (`TILO_SERVER_LOG`) to `stderr`, with `envs` set for it alone: an entry that
/// mails a release check's test takes its sender, key and mail service from
/// them without reaching any other test's process.
fn spawn_workspace(
    state: &std::path::Path,
    root_file: &std::path::Path,
    secrets: &std::path::Path,
    extra: &[&str],
    log: &str,
    stderr: Stdio,
    envs: &[(&str, &str)],
) -> std::io::Result<Workspace> {
    let mut command = Command::new(tilo_test_env::package_binary!("tilo-server"));
    command
        .env("TILO_SECRETS_DIR", secrets)
        .env("TILO_SERVER_LOG", log)
        .env("NO_COLOR", "1")
        // What a published image sets from the commit it was built from.
        .env("TILO_SOURCE_REVISION", SOURCE_REVISION)
        .args(["workspace", "--state"])
        .arg(state.join("server"))
        .args([
            // Port 0: the workspace picks a free port rather than a test choosing
            // one another test's socket may already hold.
            "--web",
            "127.0.0.1:0",
            "--tls",
            "dev",
            "--persistent",
            "plaza",
            "--root-file",
        ])
        .arg(root_file)
        .arg("--workspace-id-file")
        .arg(state.join("workspace-id"))
        .args(["--chat-channel", "general"])
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(stderr);
    for (name, value) in envs {
        command.env(name, value);
    }
    let mut child = command.spawn_locked()?;
    let stdin = Some(child.stdin.take().unwrap());
    let stdout = child.stdout.take().unwrap();
    let (sender, events) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(event) = OperatorEvent::from_line(&line)
                && sender.send(event).is_err()
            {
                break;
            }
        }
    });
    Ok(Workspace {
        child,
        stdin,
        events,
    })
}

impl Workspace {
    fn next(&self, what: impl Fn(&OperatorEvent) -> bool) -> OperatorEvent {
        loop {
            let event = self
                .events
                .recv_timeout(Duration::from_secs(120))
                .expect("the workspace said nothing in time");
            if what(&event) {
                return event;
            }
        }
    }

    /// The operator goes away, as it does in a deployment that runs the workspace
    /// without one: its input is at its end.
    fn close_operator(&mut self) {
        self.stdin = None;
    }

    fn stop(mut self) {
        match self.stdin.take() {
            Some(mut stdin) => {
                let _ = stdin.write_all(OperatorCommand::Shutdown {}.to_line().as_bytes());
                let _ = stdin.flush();
            }
            // Nobody is left to tell: the workspace stops with its process.
            None => {
                let _ = self.child.kill();
            }
        }
        let _ = self.child.wait();
    }

    /// Stop the workspace as `podman rm -f` does: killed, nothing flushed.
    fn kill(mut self) {
        self.child.kill().unwrap();
        let _ = self.child.wait();
    }
}

/// A fresh workspace has no member: the host that keeps workspaces of people's own
/// is nobody in them, and its owner becomes one by opening its owner link. It keeps
/// its id and root across restarts, and refuses a start under another root or none.
#[test]
fn a_persistent_workspace_keeps_its_id_and_owner_across_restarts() {
    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();

    let workspace = start(&state, &root_file, &secrets).unwrap();
    let OperatorEvent::Started {
        workspace: Some(id),
        version,
        user_public_key,
        ..
    } = workspace.next(|e| matches!(e, OperatorEvent::Started { .. }))
    else {
        panic!("no governed workspace announced")
    };
    assert_eq!(version, VERSION);
    // The host's root is nobody in its workspaces: nobody is a member before
    // anyone opened the workspace's owner link.
    let OperatorEvent::Online {
        people,
        members,
        online,
        ..
    } = workspace.next(|e| matches!(e, OperatorEvent::Online { .. }))
    else {
        panic!("no presence announced")
    };
    assert_eq!(
        members, 0,
        "a host that keeps workspaces of people's own is nobody"
    );
    assert_eq!(people, 0);
    assert!(online.is_empty());
    assert_eq!(
        user_public_key,
        hex::encode(
            ed25519_dalek::SigningKey::from_bytes(&[0x11; 32])
                .verifying_key()
                .to_bytes()
        ),
        "the root the host runs as, which its owner link admits as a person"
    );
    workspace.stop();

    // Restarted: the same workspace, and still nobody else.
    let workspace = start(&state, &root_file, &secrets).unwrap();
    let OperatorEvent::Started {
        workspace: Some(again),
        ..
    } = workspace.next(|e| matches!(e, OperatorEvent::Started { .. }))
    else {
        panic!("no governed workspace announced")
    };
    assert_eq!(again, id);
    let OperatorEvent::Online { members, .. } =
        workspace.next(|e| matches!(e, OperatorEvent::Online { .. }))
    else {
        panic!("no presence announced")
    };
    assert_eq!(members, 0, "the restart mints no member");
    workspace.stop();

    // Another root mounted: the start is refused, so no other owner is minted.
    std::fs::write(&root_file, "55".repeat(32)).unwrap();
    let mut refused = start(&state, &root_file, &secrets).unwrap();
    let status = refused.child.wait().unwrap();
    assert!(!status.success(), "a different root must refuse the start");

    // No root mounted at all: refused too.
    std::fs::remove_file(&root_file).unwrap();
    let mut missing = start(&state, &root_file, &secrets).unwrap();
    assert!(!missing.child.wait().unwrap().success());
}

/// A host that keeps its workspace id but none of its state founds nothing
/// under the id: the id file is written only once its workspace exists, so the
/// workspace is held elsewhere, and founded here again it would be a second
/// founding its holders refuse. The host announces no workspace and keeps
/// nothing of it until one of its holders hands it over.
#[test]
fn a_host_that_kept_only_its_workspace_id_waits_to_be_handed_it() {
    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();

    let workspace = start(&state, &root_file, &secrets).unwrap();
    let OperatorEvent::Started {
        workspace: Some(id),
        ..
    } = workspace.next(|e| matches!(e, OperatorEvent::Started { .. }))
    else {
        panic!("no governed workspace announced")
    };
    workspace.stop();

    // The state kept for the workspace went away; its id file and the sealed
    // root it keeps stay, as a host started on a fresh volume beside them is.
    std::fs::remove_dir_all(state.join("server")).unwrap();

    let mut waiting = start(&state, &root_file, &secrets).unwrap();
    // Longer than a start that founded the workspace took.
    match waiting.events.recv_timeout(Duration::from_secs(8)) {
        Err(mpsc::RecvTimeoutError::Timeout) => {}
        Ok(event) => panic!("the host said {event:?} before anyone handed it its workspace"),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("the host stopped: {:?}", waiting.child.try_wait())
        }
    }
    assert!(
        waiting.child.try_wait().unwrap().is_none(),
        "the host keeps running while it waits"
    );
    let founded = walkdir(&state.join("server"))
        .into_iter()
        .filter(|path| path.to_string_lossy().contains(&id))
        .collect::<Vec<_>>();
    assert!(
        founded.is_empty(),
        "nothing of the workspace is kept here: {founded:?}"
    );
    waiting.kill();
}

/// Every path below `dir`.
fn walkdir(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path.clone());
            }
            paths.push(path);
        }
    }
    paths
}

/// The workspace as the image's entrypoint starts it on the state volume a workspace
/// service leaves: the server's own state in `server/`, the workspace id beside
/// it, and the avatars and sealed secrets the image mounts.
fn start_lounge(
    state: &std::path::Path,
    root_file: &std::path::Path,
) -> std::io::Result<Workspace> {
    start_with(
        state,
        root_file,
        &state.join("secrets"),
        &[
            "--public-avatars",
            state.join("avatars").to_str().unwrap(),
            "--public-avatars-url",
            "https://plaza.example/avatars",
            "--listing-description",
            "Coffee and tea",
            "--default-role",
            "editor",
            "--web-plain",
            "127.0.0.1:0",
        ],
    )
}

/// A deployment moved onto this image starts on the volume the workspace service
/// left: the same workspace, its owner, and the people it took in stay members
/// across that first start.
#[test]
fn a_volume_a_workspace_service_left_keeps_its_workspace_and_people() {
    let scratch = tempfile::tempdir().unwrap();
    let state = scratch.path().join("plaza");
    // What the volume holds before the new image starts.
    for dir in ["server", "avatars", "secrets"] {
        std::fs::create_dir_all(state.join(dir)).unwrap();
    }
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();

    // The workspace the workspace service ran, and one person it took in.
    let workspace = start_lounge(&state, &root_file).unwrap();
    let started = announced(&workspace);
    let OperatorEvent::Online { members, .. } =
        workspace.next(|e| matches!(e, OperatorEvent::Online { .. }))
    else {
        panic!("no presence announced")
    };
    assert_eq!(
        members, 0,
        "a host that keeps workspaces of people's own is nobody"
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    join_through_entry(
        &runtime,
        &started.host_info,
        &started.workspace,
        own_device(0x41, "Ana"),
        "Ana",
    );
    let listed = runtime.block_on(directory_page(started.plain));
    assert_eq!(listed.workspaces[0].members, 1, "the person who joined");
    workspace.stop();

    // The new image's first start of that volume.
    let workspace = start_lounge(&state, &root_file).unwrap();
    let OperatorEvent::Started {
        workspace: Some(again),
        ..
    } = workspace.next(|e| matches!(e, OperatorEvent::Started { .. }))
    else {
        panic!("no governed workspace announced")
    };
    assert_eq!(
        again, started.workspace,
        "the volume's workspace is the one it serves"
    );
    let OperatorEvent::Online { members, .. } =
        workspace.next(|e| matches!(e, OperatorEvent::Online { .. }))
    else {
        panic!("no presence announced")
    };
    assert_eq!(members, 1, "the person who joined stays a member");
    workspace.stop();
}

/// A workspace started with an app it no longer names keeps the workspace it
/// served: members write as their roles allow, whatever apps they use.
#[test]
fn a_persistent_workspace_keeps_serving_across_restarts() {
    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();
    let started = |extra: &[&str]| {
        let workspace = start_with(&state, &root_file, &secrets, extra).unwrap();
        let OperatorEvent::Started {
            workspace: Some(id),
            ..
        } = workspace.next(|e| matches!(e, OperatorEvent::Started { .. }))
        else {
            panic!("no governed workspace announced")
        };
        (workspace, id)
    };

    let (workspace, id) = started(&[]);
    workspace.stop();
    let (workspace, again) = started(&[]);
    workspace.stop();
    assert_eq!(again, id, "the volume's workspace is the one it serves");
}

/// A page reaches its host only through a relay, so `/host` names the host's
/// own packet relay from its first answer: a host that restarts on kept state
/// opens its listener within the first moments of starting, before its endpoint
/// has finished connecting to the relay it runs, and a page told of no relay
/// binds an endpoint that can carry nothing.
#[test]
fn a_page_is_told_the_packet_relay_its_host_runs_from_the_first_answer() {
    let scratch = tempfile::tempdir().unwrap();
    let state = scratch.path().join("plaza");
    std::fs::create_dir_all(state.join("secrets")).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();
    let flags = [
        "--web-plain",
        "127.0.0.1:0",
        "--packet-relay",
        "127.0.0.1:0",
    ];

    // The volume the first start left: the workspace it seeded, so the restart
    // below serves its listener without seeding anything.
    let workspace = start_with(&state, &root_file, &state.join("secrets"), &flags).unwrap();
    announced(&workspace);
    workspace.stop();

    let workspace = start_with(&state, &root_file, &state.join("secrets"), &flags).unwrap();
    let started = announced(&workspace);
    let info = started.host_info;
    let address = info.address.expect("the host names an endpoint");
    let relay = address
        .relay_url
        .expect("the host names the packet relay it runs");
    assert!(
        relay.starts_with("http://127.0.0.1:"),
        "the relay is the host's own, not a public one: {relay}"
    );
    let named = address
        .endpoint_addr
        .relay_urls()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    assert_eq!(
        named,
        [relay],
        "the endpoint is dialled through the relay the page is told"
    );
    workspace.stop();
}

/// Plain HTTP/1.1 to the workspace's loopback listener: the page's `fetch`.
struct Plain(std::net::SocketAddr);
impl tilo_server_client::HttpClient for Plain {
    fn post(&self, path: &str, body: Vec<u8>) -> tilo_server_client::HttpFuture<'_> {
        use http_body_util::BodyExt as _;
        let request = hyper::Request::post(path)
            .header(hyper::header::HOST, "localhost")
            .header(hyper::header::CONTENT_TYPE, "application/octet-stream")
            .body(http_body_util::Full::new(bytes::Bytes::from(body)));
        let addr = self.0;
        Box::pin(async move {
            let request = request.map_err(|error| error.to_string())?;
            let tcp = tokio::net::TcpStream::connect(addr)
                .await
                .map_err(|error| error.to_string())?;
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(tcp))
                    .await
                    .map_err(|error| error.to_string())?;
            tokio::spawn(connection);
            let response = sender
                .send_request(request)
                .await
                .map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let body = response
                .into_body()
                .collect()
                .await
                .map_err(|error| error.to_string())?
                .to_bytes()
                .to_vec();
            Ok((status, body))
        })
    }
}

/// Poll `probe` until it answers or the shared hang bound passes; a timeout
/// names what `seen` says the page got instead.
async fn wait_for<T: Send>(
    what: &str,
    mut probe: impl FnMut() -> Option<T> + Send,
    seen: impl Fn() -> String + Send,
) -> T {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}: {}",
            seen()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The workspace's iroh endpoint, where its pages run their sessions.
fn host_of(info: tilo_server_protocol::web::ServerInfo) -> tilo_server_client::ServerAddress {
    tilo_server_client::ServerAddress::at(
        info.address
            .expect("the workspace publishes its iroh endpoint")
            .endpoint_addr,
    )
}

/// A page of `cert`'s device, on the workspace's session.
fn page(
    host: tilo_server_client::ServerAddress,
    key: ed25519_dalek::SigningKey,
    cert: tilo_user_identity::DeviceCert,
    server_cert: &tilo_user_identity::DeviceCert,
) -> tilo_session_client::Link {
    tilo_session_client::start_session(
        tilo_session_client::PeerConfig {
            host,
            cert,
            server_cert: server_cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
            entering: None,
        },
        std::sync::Arc::new(key),
        std::sync::Arc::new(tilo_session_client::MemoryPeerStore::default()),
    )
}

/// A person comes in through the workspace's entry on their session, as their computer does: their
/// device and certificate, a member now.
fn join_through_entry(
    runtime: &tokio::runtime::Runtime,
    info: &tilo_server_protocol::web::ServerInfo,
    workspace: &str,
    (device, cert): (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert),
    name: &str,
) -> (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert) {
    match runtime.block_on(enter_through_entry(info, workspace, (&device, &cert), name)) {
        Ok(_) => (device, cert),
        Err(refused) => panic!("{name} was not admitted: {refused:?}"),
    }
}

/// A session of `cert`'s device whose first message enters `workspace` through its entry, as a
/// person's page or computer opens one: what the workspace answered it.
async fn enter_through_entry(
    info: &tilo_server_protocol::web::ServerInfo,
    workspace: &str,
    device: (&ed25519_dalek::SigningKey, &tilo_user_identity::DeviceCert),
    name: &str,
) -> Result<tilo_server_client::login::Admission, tilo_session_client::device::DeviceRefusal> {
    enter_through_entry_as(info, workspace, device, name, device.1.device_public_key).await
}

/// [`enter_through_entry`] as the entry `request_id` names it: `tilo mcp` asks
/// with a fresh one on every run.
async fn enter_through_entry_as(
    info: &tilo_server_protocol::web::ServerInfo,
    workspace: &str,
    (device, cert): (&ed25519_dalek::SigningKey, &tilo_user_identity::DeviceCert),
    name: &str,
    request_id: [u8; 32],
) -> Result<tilo_server_client::login::Admission, tilo_session_client::device::DeviceRefusal> {
    let info = info.clone();
    let server_cert = info.server_cert.clone();
    let session = tilo_session_client::start_session(
        tilo_session_client::PeerConfig {
            host: host_of(info),
            cert: cert.clone(),
            server_cert,
            revocations: tilo_user_identity::RevocationList::new(),
            entering: Some(tilo_server_protocol::entering::Entering {
                workspace: workspace.to_owned(),
                name: name.to_owned(),
                request_id,
                through: tilo_server_protocol::entering::Through::Entry,
            }),
        },
        std::sync::Arc::new(device.clone()),
        std::sync::Arc::new(tilo_session_client::MemoryPeerStore::default()),
    );
    let answered = tilo_session_client::device::entered(&session).await;
    session.close();
    answered
}

/// A real change of `doc` saying `text`, as a page writes one.
fn edit(doc: &mut automerge::AutoCommit, text: &str) -> collab_types::DocumentMutation {
    use automerge::transaction::Transactable as _;
    let before = doc.get_heads();
    doc.put(automerge::ROOT, "edited", text).unwrap();
    doc.commit();
    collab_types::DocumentMutation::Incremental(doc.save_after(&before))
}

fn chat_channels(doc: &automerge::AutoCommit) -> Option<Vec<String>> {
    use tilo_resource::Resource as _;
    Some(
        chat::Chat::read(doc)
            .ok()?
            .channels
            .values()
            .map(|channel| channel.name.clone())
            .collect(),
    )
}

fn unenrolled_stranger_device() -> (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert) {
    // A device of a root the workspace has nobody of in it: membership follows a
    // person, so this is the device that stays out.
    let sibling_key = ed25519_dalek::SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(
        getrandom::SysRng,
    ));
    let owner_root = ed25519_dalek::SigningKey::from_bytes(&[0x12; 32]);
    let mut sibling_cert = tilo_user_identity::DeviceCert {
        user_public_key: owner_root.verifying_key().to_bytes(),
        device_public_key: sibling_key.verifying_key().to_bytes(),
        device_label: "Another laptop".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut sibling_cert, &owner_root).unwrap();
    (sibling_key, sibling_cert)
}

/// A device the workspace owner's root certified that the workspace never
/// enrolled: the owner's own, because membership follows the person and any
/// key holder admits a member's further device when it first asks
/// (`docs/architecture/workspace.md`).
fn another_owner_device() -> (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert) {
    let owner_root = ed25519_dalek::SigningKey::from_bytes(&[0x11; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[0x91; 32]);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: owner_root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Owner's other laptop".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &owner_root).unwrap();
    (device, cert)
}

/// A device whose certificate names the workspace owner's root but carries a
/// signature that root never made: a forgery claiming to be the owner's.
fn forged_owner_device() -> (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert) {
    let owner_root = ed25519_dalek::SigningKey::from_bytes(&[0x11; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[0x92; 32]);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: owner_root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Owner's laptop".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &owner_root).unwrap();
    cert.signature[0] ^= 0xff;
    (device, cert)
}

/// A device the workspace owner's root did sign, whose certificate expired.
fn expired_owner_device() -> (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert) {
    let owner_root = ed25519_dalek::SigningKey::from_bytes(&[0x11; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[0x94; 32]);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: owner_root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Owner's old laptop".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: Some(2),
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &owner_root).unwrap();
    (device, cert)
}

/// Wait until `visiting` reads `route` with every one of `channels`, and return
/// the page's copy of it.
async fn assert_page_reads_the_chat(
    who: &str,
    visiting: &tilo_session_client::Link,
    heard: &tilo_session_client::Events,
    workspace_id: WorkspaceId,
    route: &tilo_peer_state::document_frame::DocumentRoute,
    channels: &[&str],
) -> automerge::AutoCommit {
    use tilo_session_client::Event;

    let mut ready = false;
    let mut doc = automerge::AutoCommit::new();
    let events = std::sync::Mutex::new(Vec::new());
    wait_for(
        &format!("{who}'s page to read the chat's channels {channels:?}"),
        || {
            for event in heard.drain() {
                match &event {
                    Event::HostReady { ws: at, .. } if *at == workspace_id => {
                        ready = true;
                    }
                    // The workspace's other records land too; only the chat is read.
                    Event::Landed { route: landed, .. } if landed == route => {
                        let read = tilo_desktop_model::WorkspaceId::parse(&route.cluster_id)
                            .and_then(|id| {
                                tilo_workspace_service::documents::session_page::page_holder(
                                    visiting.engine(),
                                    id,
                                )
                            })
                            .map(|holder| {
                                holder.read(&route.cluster_id, &route.app_id, &route.document, None)
                            });
                        if let Some(Ok(DocumentRead::Whole { bytes, .. })) = read {
                            doc = automerge::AutoCommit::load(&bytes).unwrap();
                        }
                    }
                    _ => {}
                }
                events.lock().unwrap().push(format!("{event:?}"));
            }
            let read = chat_channels(&doc)?;
            (ready
                && channels
                    .iter()
                    .all(|wanted| read.iter().any(|name| name == wanted)))
            .then_some(())
        },
        || {
            format!(
                "{:?}; status {:?}",
                events.lock().unwrap(),
                visiting.status()
            )
        },
    )
    .await;
    let status = visiting.status();
    assert!(status.failures.is_empty(), "{:?}", status.failures);
    doc
}

/// What the workspace answered first: the workspace it serves, and the plain
/// listener it bound for the pages that speak HTTP. Tests dial the address
/// the workspace announces, never one they picked and the workspace may not get.
struct Started {
    /// The governed workspace's id.
    workspace: String,
    /// The plain loopback listener the workspace bound.
    plain: std::net::SocketAddr,
    /// Who the host is and where its endpoint is dialled, as it announced them.
    host_info: tilo_server_protocol::web::ServerInfo,
}

/// The workspace's first answer, with the addresses it bound.
fn announced(workspace: &Workspace) -> Started {
    let OperatorEvent::Started {
        workspace: Some(workspace),
        plain_origin: Some(plain_origin),
        host_info: Some(host_info),
        ..
    } = workspace.next(|e| matches!(e, OperatorEvent::Started { .. }))
    else {
        panic!("no governed workspace announced")
    };
    Started {
        plain: bound_address(&plain_origin),
        workspace,
        host_info: *host_info,
    }
}

/// The loopback address a listener's announced origin names.
fn bound_address(origin: &str) -> std::net::SocketAddr {
    let port = origin
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
        .unwrap_or_else(|| panic!("{origin} names no port"));
    std::net::SocketAddr::from(([127, 0, 0, 1], port))
}

/// The workspace's chat as a page on `plain` reaches it: the workspace host's
/// certificate and the chat's route for the workspace `cluster`.
fn chat_route(
    info: &tilo_server_protocol::web::ServerInfo,
    cluster: &str,
) -> (
    tilo_server_client::ServerAddress,
    tilo_user_identity::DeviceCert,
    tilo_peer_state::document_frame::DocumentRoute,
) {
    const CHAT: &str = "so.tilo.chat";
    let info = info.clone();
    let server_cert = info.server_cert.clone();
    let host = host_of(info);
    let catalogue = tilo_agent_session::AppCatalogue::native();
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: cluster.into(),
        document: tilo_workspace_service::agents::workspace_resource(&catalogue, CHAT),
    };
    (host, server_cert, route)
}

/// A person of their own: a root this test generates, and a device the root
/// certifies, as a computer that never met the workspace's owner does.
fn own_device(
    seed: u8,
    label: &str,
) -> (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert) {
    let root = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[seed.wrapping_add(1); 32]);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
    (device, cert)
}

/// What the workspace lists in its directory now.
async fn directory_page(
    plain: std::net::SocketAddr,
) -> tilo_server_protocol::directory::DirectoryPage {
    use tilo_server_client::HttpClient as _;
    let (status, body) = Plain(plain)
        .post("/peer/v1/directory", Vec::new())
        .await
        .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    serde_json::from_slice(&body).unwrap()
}

/// The workspace's entry, with no operator command: two people of their own roots
/// join the listed workspace at its default role, each as their own person,
/// and the directory counts both.
#[test]
fn the_server_entry_admits_two_people_as_distinct_members_at_its_default_role() {
    use tilo_server_protocol::workspaces::Role;

    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();
    let workspace = start_with(
        &state,
        &root_file,
        &secrets,
        &[
            "--listing-description",
            "Coffee and tea",
            "--default-role",
            "editor",
            "--web-plain",
            "127.0.0.1:0",
        ],
    )
    .unwrap();
    let started = announced(&workspace);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let before = runtime.block_on(directory_page(started.plain));
    assert_eq!(before.workspaces.len(), 1);
    assert_eq!(before.workspaces[0].description, "Coffee and tea");
    assert_eq!(
        before.workspaces[0].default_role,
        Role::Editor,
        "the listing the operator asked for"
    );

    let mut people = Vec::new();
    for (index, (seed, name)) in [(0x41, "Ana"), (0x51, "Ben")].into_iter().enumerate() {
        let (device, cert) = own_device(seed, name);
        let admitted = runtime
            .block_on(enter_through_entry(
                &started.host_info,
                &started.workspace,
                (&device, &cert),
                name,
            ))
            .unwrap_or_else(|refused| panic!("{name} {index} was not admitted: {refused:?}"));
        assert_eq!(admitted.role, Role::Editor);
        people.push(cert.user_public_key);
    }
    assert_ne!(people[0], people[1], "two people, not one");

    let after = runtime.block_on(directory_page(started.plain));
    assert_eq!(
        after.workspaces[0].members,
        before.workspaces[0].members + 2,
        "both joined as members"
    );
    workspace.stop();
}

/// A person who joined through the real workspace process's entry subscribes, reads
/// the workspace's chat with its starting channel, and has every write refused as
/// the viewer the listing made them; the owner's root still admits none of its
/// other devices by itself. The same workspace, listing people as its editors,
/// takes the next person's write in: what someone may change is the role the
/// listing gives them, and nothing else about the page changes.
#[test]
fn a_persistent_workspaces_pages_read_the_workspace_and_write_what_their_role_lets_them() {
    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let workspace = start_with(
        &state,
        &root_file,
        &secrets,
        &[
            "--chat-channel",
            "general",
            "--listing-description",
            "Coffee and tea",
            "--web-plain",
            "127.0.0.1:0",
        ],
    )
    .unwrap();
    let started = announced(&workspace);
    let workspace_id = tilo_server::WorkspaceId::parse(&started.workspace).unwrap();
    let id = started.workspace;
    let visiting = join_through_entry(
        &runtime,
        &started.host_info,
        &id,
        own_device(0x71, "Ana"),
        "Ana",
    );
    let writer = join_through_entry(
        &runtime,
        &started.host_info,
        &id,
        own_device(0x72, "Ben"),
        "Ben",
    );
    let sibling = unenrolled_stranger_device();
    runtime.block_on(persons_read_the_workspace_and_are_refused(
        &started.host_info,
        &id,
        workspace_id,
        visiting,
        writer,
        sibling,
    ));
    workspace.stop();

    // The same workspace, listing people as its editors: a new person joins
    // through the entry as the others did, and their write is taken in.
    let workspace = start_with(
        &state,
        &root_file,
        &secrets,
        &[
            "--chat-channel",
            "general",
            "--listing-description",
            "Coffee and tea",
            "--default-role",
            "editor",
            "--web-plain",
            "127.0.0.1:0",
        ],
    )
    .unwrap();
    let started = announced(&workspace);
    assert_eq!(started.workspace, id, "the same workspace opens");
    let editor = join_through_entry(
        &runtime,
        &started.host_info,
        &started.workspace,
        own_device(0x73, "Cy"),
        "Cy",
    );
    let workspace_id = tilo_server::WorkspaceId::parse(&started.workspace).unwrap();
    runtime.block_on(an_editor_page_writes_and_is_committed(
        &started.host_info,
        &started.workspace,
        workspace_id,
        editor,
    ));
    workspace.stop();
}

/// The first start's pages: two people who joined through the entry read the
/// chat and the viewer's writes are refused; another device of the owner's
/// root joins, while a device of a root with nobody in the workspace, one
/// claiming the owner's root with a signature not the root's, and one whose
/// certificate expired are not admitted.
async fn persons_read_the_workspace_and_are_refused(
    info: &tilo_server_protocol::web::ServerInfo,
    cluster: &str,
    workspace_id: WorkspaceId,
    (visiting_key, visiting_cert): (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert),
    (writing_key, writing_cert): (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert),
    (sibling_key, sibling_cert): (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert),
) {
    use tilo_session_client::Event;

    let (host, server_cert, route) = chat_route(info, cluster);

    let visiting = page(host.clone(), visiting_key, visiting_cert, &server_cert);

    let visiting_heard = visiting.events();
    visiting.send(Cmd::Join(workspace_id)).unwrap();
    visiting.send(Cmd::OpenRoute(route.clone())).unwrap();
    assert_page_reads_the_chat(
        "Ana",
        &visiting,
        &visiting_heard,
        workspace_id,
        &route,
        &["general"],
    )
    .await;

    // Another person holds the listing's default role, a viewer's: they read
    // the workspace and write nothing.
    let writer = page(host.clone(), writing_key, writing_cert, &server_cert);
    let writer_heard = writer.events();
    writer.send(Cmd::Join(workspace_id)).unwrap();
    writer.send(Cmd::OpenRoute(route.clone())).unwrap();
    let mut chat = assert_page_reads_the_chat(
        "Ben",
        &writer,
        &writer_heard,
        workspace_id,
        &route,
        &["general"],
    )
    .await;
    writer
        .write(route.clone(), &edit(&mut chat, "a viewer's edit"))
        .unwrap();
    // Held across the waits below, which need what they borrow to be shared.
    let saved = std::sync::Mutex::new(writer.barrier().unwrap());
    let mut committed = false;
    let refused = wait_for(
        "the viewer's write to be refused",
        || {
            committed |= matches!(saved.lock().unwrap().try_recv(), Ok(Ok(())));
            writer
                .status()
                .failures
                .into_iter()
                .find(|failure| failure.message.contains("not granted"))
        },
        || format!("{:?}", writer.status()),
    )
    .await;
    assert!(!committed, "the viewer's write was committed: {refused:?}");

    let other = page(host.clone(), sibling_key, sibling_cert, &server_cert);

    let other_heard = other.events();
    other.send(Cmd::Join(workspace_id)).unwrap();
    let refused = wait_for(
        "the sibling device to be refused",
        || {
            other
                .status()
                .failures
                .into_iter()
                .find(|failure| failure.code == "not-admitted")
        },
        || format!("{:?}", other.status()),
    )
    .await;
    assert!(
        !other_heard
            .drain()
            .iter()
            .any(|event| matches!(event, Event::HostReady { .. })),
        "a sibling device was admitted: {refused:?}"
    );

    owner_devices_join_and_forgeries_are_refused(host, &server_cert, workspace_id).await;

    other.close();
    writer.close();
    visiting.close();
}

/// Who the workspace's owner is in it: a device the owner's root certified is
/// the owner's, enrolled or not, so it joins; a certificate that names the
/// owner's root without the root's signature, and one of the owner's root's
/// that expired, are not admitted.
async fn owner_devices_join_and_forgeries_are_refused(
    host: tilo_server_client::ServerAddress,
    server_cert: &tilo_user_identity::DeviceCert,
    workspace_id: WorkspaceId,
) {
    use tilo_session_client::Event;

    let (owner_key, owner_cert) = another_owner_device();
    let owner_device = page(host.clone(), owner_key, owner_cert, server_cert);
    owner_device.send(Cmd::Join(workspace_id)).unwrap();
    wait_for(
        "another device of the workspace's owner to be admitted",
        || {
            owner_device
                .status()
                .workspaces
                .get(&workspace_id)
                .copied()
                .filter(|ready| *ready)
        },
        || format!("{:?}", owner_device.status()),
    )
    .await;

    // A certificate that names the owner's root but carries a signature the
    // root never made is no device of the owner's, and the page hears that its
    // own certificate was refused.
    let (forged_key, forged_cert) = forged_owner_device();
    let forged = page(host.clone(), forged_key, forged_cert, server_cert);
    let forged_heard = forged.events();
    forged.send(Cmd::Join(workspace_id)).unwrap();
    let refused = wait_for(
        "a device claiming the owner's root with a signature not the root's to be refused",
        || {
            forged
                .status()
                .failures
                .into_iter()
                .find(|failure| failure.code == "certificate-rejected")
        },
        || format!("{:?}", forged.status()),
    )
    .await;
    assert!(
        !forged_heard
            .drain()
            .iter()
            .any(|event| matches!(event, Event::HostReady { .. })),
        "a forged owner's device was admitted: {refused:?}"
    );

    // A device of the owner's root whose certificate expired is no longer
    // theirs to offer.
    let (expired_key, expired_cert) = expired_owner_device();
    let expired = page(host, expired_key, expired_cert, server_cert);
    let expired_heard = expired.events();
    expired.send(Cmd::Join(workspace_id)).unwrap();
    let refused = wait_for(
        "the expired certificate to be refused",
        || {
            expired
                .status()
                .failures
                .into_iter()
                .find(|failure| failure.code == "certificate-expired")
        },
        || format!("{:?}", expired.status()),
    )
    .await;
    assert!(
        !expired_heard
            .drain()
            .iter()
            .any(|event| matches!(event, Event::HostReady { .. })),
        "an expired certificate was admitted: {refused:?}"
    );

    owner_device.close();
    forged.close();
    expired.close();
}

/// The restart's page: a person who joined through the entry writes, and the
/// workspace commits it, as the listing's role now lets them.
async fn an_editor_page_writes_and_is_committed(
    info: &tilo_server_protocol::web::ServerInfo,
    cluster: &str,
    workspace_id: WorkspaceId,
    (editor_key, editor_cert): (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert),
) {
    let (host, server_cert, route) = chat_route(info, cluster);
    let editor = page(host, editor_key, editor_cert, &server_cert);
    let editor_heard = editor.events();
    editor.send(Cmd::Join(workspace_id)).unwrap();
    editor.send(Cmd::OpenRoute(route.clone())).unwrap();
    let mut chat = assert_page_reads_the_chat(
        "editor",
        &editor,
        &editor_heard,
        workspace_id,
        &route,
        &["general"],
    )
    .await;
    editor
        .write(route.clone(), &edit(&mut chat, "an editor's edit"))
        .unwrap();
    // Held across the waits below, which need what they borrow to be shared.
    let saved = std::sync::Mutex::new(editor.barrier().unwrap());
    let refused = wait_for(
        "the editor's write to be committed",
        || {
            let committed = matches!(saved.lock().unwrap().try_recv(), Ok(Ok(())));
            committed.then(|| editor.status().failures)
        },
        || format!("{:?}", editor.status()),
    )
    .await;
    assert!(
        refused.is_empty(),
        "an editor's write was refused: {refused:?}"
    );
    editor.close();
}

/// A workspace service stops its workspace hard (`podman rm -f`): killed after it
/// decided things and wrote them, and killed again while it starts. Each
/// next start opens the same workspace, settles its chat and lets a person who
/// joins through its entry read it.
#[test]
fn a_persistent_workspace_reopens_after_hard_stops() {
    const CHAT: &str = "so.tilo.chat";
    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();
    let extra = [
        "--chat-channel",
        "general",
        "--chat-channel",
        "show-and-tell",
        "--listing-description",
        "Coffee and tea",
        "--web-plain",
        "127.0.0.1:0",
    ];

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let workspace = start_with(&state, &root_file, &secrets, &extra).unwrap();
    let first = announced(&workspace);
    let id = first.workspace.clone();
    join_through_entry(
        &runtime,
        &first.host_info,
        &id,
        own_device(0x74, "Ana"),
        "Ana",
    );
    workspace.kill();

    // Killed again before it says it started.
    let starting = start_with(&state, &root_file, &secrets, &extra).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    starting.kill();

    let workspace = start_with(&state, &root_file, &secrets, &extra).unwrap();
    let again = announced(&workspace);
    assert_eq!(again.workspace, id, "the same workspace opens");
    let workspace_id = tilo_server::WorkspaceId::parse(&again.workspace).unwrap();
    let (key, cert) = join_through_entry(
        &runtime,
        &again.host_info,
        &id,
        own_device(0x75, "Ben"),
        "Ben",
    );
    runtime.block_on(async {
        let info = again.host_info.clone();
        let server_cert = info.server_cert.clone();
        let host = host_of(info);
        let catalogue = tilo_agent_session::AppCatalogue::native();
        let route = tilo_peer_state::document_frame::DocumentRoute {
            app_id: CHAT.into(),
            cluster_id: id.clone(),
            document: tilo_workspace_service::agents::workspace_resource(&catalogue, CHAT),
        };
        let visiting = page(host, key, cert, &server_cert);
        let visiting_heard = visiting.events();
        visiting.send(Cmd::Join(workspace_id)).unwrap();
        visiting.send(Cmd::OpenRoute(route.clone())).unwrap();
        assert_page_reads_the_chat(
            "visiting",
            &visiting,
            &visiting_heard,
            workspace_id,
            &route,
            &["general"],
        )
        .await;
        visiting.close();
    });
    workspace.stop();
}

/// One answer from the deployment's plain-HTTP routes.
struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// A GET to `addr`, as a health check, a browser following a link or a
/// Discord sign-in coming back does.
async fn try_get(addr: std::net::SocketAddr, path: &str) -> Result<Answer, String> {
    use http_body_util::BodyExt as _;
    let request = hyper::Request::get(path)
        .header(hyper::header::HOST, "localhost")
        .body(http_body_util::Full::new(bytes::Bytes::new()))
        .map_err(|error| error.to_string())?;
    let tcp = tokio::net::TcpStream::connect(addr)
        .await
        .map_err(|error| error.to_string())?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(tcp))
            .await
            .map_err(|error| error.to_string())?;
    tokio::spawn(connection);
    let response = sender
        .send_request(request)
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|error| error.to_string())?
        .to_bytes()
        .to_vec();
    Ok(Answer {
        status,
        headers,
        body,
    })
}

/// `try_get` once the listener answers, until the hang bound passes.
fn get(runtime: &tokio::runtime::Runtime, addr: std::net::SocketAddr, path: &str) -> Answer {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        match runtime.block_on(try_get(addr, path)) {
            Ok(answer) => return answer,
            Err(error) if std::time::Instant::now() < deadline => {
                let _ = error;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!("nothing answered {path} at {addr}: {error}"),
        }
    }
}

/// What a bot publishes as an avatar: PNG bytes, addressed by their hash.
const AVATAR_PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

/// An avatar the workspace published is served by its content address as
/// bytes that never change, and nothing else is served there.
fn answers_its_avatars(
    runtime: &tokio::runtime::Runtime,
    management: std::net::SocketAddr,
    published: &str,
) {
    let avatar = get(runtime, management, &format!("/avatars/{published}"));
    assert_eq!(avatar.status, 200);
    assert_eq!(avatar.header("content-type"), Some("image/png"));
    assert_eq!(avatar.body, AVATAR_PNG, "the published bytes, as they are");
    assert_eq!(
        avatar.header("cache-control"),
        Some("public, max-age=31536000, immutable"),
        "a content address never changes"
    );
    assert_eq!(
        get(runtime, management, &format!("/avatars/{}", "0".repeat(64))).status,
        404,
        "an avatar the workspace did not publish"
    );
    assert_eq!(get(runtime, management, "/avatars/not-a-hash").status, 404);
}

/// What Discord's consent page comes back with: on to the entry when the
/// person said no, and a page saying why when the round trip was not one this
/// host started or Discord did not confirm it.
fn answers_its_sign_in(runtime: &tokio::runtime::Runtime, management: std::net::SocketAddr) {
    let declined = get(runtime, management, "/discord/callback?error=access_denied");
    assert_eq!(declined.status, 302);
    assert_eq!(declined.header("location"), Some("https://tilo.example/"));
    let malformed = get(
        runtime,
        management,
        "/discord/callback?code=the-code&state=short",
    );
    assert_eq!(malformed.status, 400);
    assert_eq!(
        malformed.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert!(
        String::from_utf8_lossy(&malformed.body).contains("href=\"https://tilo.example/\""),
        "{}",
        String::from_utf8_lossy(&malformed.body)
    );
    // A round trip this host did start, whose account Discord would not
    // confirm (the workspace's Discord is unreachable from a test): the
    // browser is not sent on as if it had linked anything.
    let unconfirmed = get(
        runtime,
        management,
        "/discord/callback?code=the-code&state=0123456789abcdef0123456789abcdef",
    );
    assert_eq!(unconfirmed.status, 400);
    assert!(
        String::from_utf8_lossy(&unconfirmed.body).contains("Discord didn't confirm"),
        "{}",
        String::from_utf8_lossy(&unconfirmed.body)
    );
}

/// A page's own error report still lands on the entry and is answered as
/// reports always are.
fn answers_the_entry(
    runtime: &tokio::runtime::Runtime,
    plain: std::net::SocketAddr,
    workspace: &str,
) {
    let reported = runtime.block_on(async {
        use tilo_server_client::HttpClient as _;
        Plain(plain)
            .post(
                &format!("/peer/v1/directory/{workspace}/errors"),
                br#"{"kind":"error","message":"boom","page":"/","build":"abc"}"#.to_vec(),
            )
            .await
            .unwrap()
    });
    assert_eq!(reported.0, 204, "{}", String::from_utf8_lossy(&reported.1));
}

/// The workspace as its deployment runs it: no operator at all, and the plain-HTTP
/// routes the ingress publishes — the health check, the avatars its bots
/// publish and Discord's sign-in callback — answering beside the browser
/// listener, whose entry still logs a page's error and answers it.
#[test]
fn the_workspace_serves_without_an_operator_and_answers_its_deployment_routes() {
    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    let published = blake3::hash(AVATAR_PNG).to_hex().to_string();
    std::fs::create_dir_all(state.join("avatars")).unwrap();
    std::fs::write(state.join("avatars").join(&published), AVATAR_PNG).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();
    let secret = scratch.path().join("discord-client-secret");
    std::fs::write(&secret, "s3cret").unwrap();

    let avatars = state.join("avatars");
    let avatars = avatars.to_str().unwrap();
    let secret = secret.to_str().unwrap();
    let mut workspace = start_with(
        &state,
        &root_file,
        &secrets,
        &[
            "--listing-description",
            "Coffee and tea",
            "--default-role",
            "editor",
            "--web-plain",
            "127.0.0.1:0",
            "--web-entry",
            "https://tilo.example/",
            "--http",
            "127.0.0.1:0",
            "--public-avatars",
            avatars,
            "--public-avatars-url",
            "https://eu1.example/avatars",
            "--discord-client-id",
            "123",
            "--discord-client-secret-file",
            secret,
            "--discord-redirect-url",
            "https://eu1.example/discord/callback",
            "--discord-authorize-url",
            "https://discord.com/oauth2/authorize",
            // Discord itself, in production; no test reaches it.
            "--discord-api-base",
            "http://127.0.0.1:9",
        ],
    )
    .unwrap();
    // The deployment runs the workspace with nobody at the operator's end.
    workspace.close_operator();
    let started = announced(&workspace);
    let OperatorEvent::DeploymentRoutes { origin } =
        workspace.next(|e| matches!(e, OperatorEvent::DeploymentRoutes { .. }))
    else {
        panic!("no deployment routes announced")
    };
    let management = bound_address(&origin);
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let mode = get(&runtime, management, "/mode");
    assert_eq!(mode.status, 200, "{}", String::from_utf8_lossy(&mode.body));
    let mode: serde_json::Value = serde_json::from_slice(&mode.body).unwrap();
    assert_eq!(mode["server"], "up", "the health check a deploy waits on");
    assert_eq!(mode["revision"], SOURCE_REVISION, "the image it built");

    answers_its_avatars(&runtime, management, &published);
    answers_its_sign_in(&runtime, management);
    assert_eq!(get(&runtime, management, "/join").status, 404);
    assert_eq!(get(&runtime, management, "/wait-list").status, 404);
    answers_the_entry(&runtime, started.plain, &started.workspace);

    workspace.stop();
}

/// What the entry keeps of the people it let in, in the workspace's state: the file
/// beside the workspace's record, wherever the run laid its state out.
fn entry_admissions(state: &std::path::Path) -> std::path::PathBuf {
    let mut workspaces = vec![state.to_path_buf()];
    let mut kept: Vec<std::path::PathBuf> = Vec::new();
    while let Some(dir) = workspaces.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                workspaces.push(path);
            } else if path.extension().is_some_and(|kind| kind == "admissions") {
                kept.push(path);
            }
        }
    }
    assert_eq!(kept.len(), 1, "one workspace, one entry: {kept:?}");
    kept.into_iter()
        .next()
        .expect("the workspace keeps the entry's admissions")
}

/// The entry's record as it stands once nothing is writing it: an entry's
/// admission is written as it settles, which a session's answer can run ahead
/// of.
fn settled_admissions(path: &std::path::Path) -> Vec<u8> {
    let mut last = std::fs::read(path).unwrap_or_default();
    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let now = std::fs::read(path).unwrap_or_default();
        if now == last {
            return now;
        }
        last = now;
    }
    last
}

/// A computer that came in through the workspace's entry before enters again without
/// it: the entry decides nothing for someone already in, so a returning run
/// (a `tilo mcp` restarted, a page opened again) has no invitation put into the
/// workspace's governance, none published to everyone there and none spent.
#[test]
fn a_device_that_came_in_before_enters_again_without_the_entry() {
    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let workspace = start_with(
        &state,
        &root_file,
        &secrets,
        &[
            "--listing-description",
            "Coffee and tea",
            "--web-plain",
            "127.0.0.1:0",
        ],
    )
    .unwrap();
    let started = announced(&workspace);
    let id = started.workspace;
    // A computer `tilo login` made a device of the person's, in through the
    // listed workspace's entry.
    let (key, cert) = join_through_entry(
        &runtime,
        &started.host_info,
        &id,
        own_device(0xc1, "Claude Code on build-box"),
        "Claude Code",
    );
    let entry = entry_admissions(&state);
    let before = settled_admissions(&entry);
    // The same computer's next `tilo mcp`: its session enters the workspace
    // again, under an entry of its own as every run makes one.
    let answered = runtime.block_on(enter_through_entry_as(
        &started.host_info,
        &id,
        (&key, &cert),
        "Claude Code",
        rand::random(),
    ));
    assert!(answered.is_ok(), "{answered:?}");
    // The entry was asked for nothing: no invitation was issued for a member,
    // nothing of its entry published, nothing spent.
    assert_eq!(
        settled_admissions(&entry),
        before,
        "a member's entry asked the entry for an invitation"
    );
    workspace.stop();
}
