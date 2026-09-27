//! A disposable SSH server and agent; neither reads nor modifies the user's agent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use russh::keys::agent::client::AgentClient;
use russh::keys::{PrivateKey, PublicKey};
use russh::server::{self, Session};
use russh::{Channel, ChannelId};

pub(super) const CHILD_ROOT: &str = "TILO_GIT_SSH_FIXTURE_ROOT";
/// The `#[test]` the child re-runs, as the `it` harness names it (module path included).
/// With `--exact`, a stale name selects nothing and the child "passes" without ever
/// fetching, so the parent also requires `CHILD_VERIFIED` on the child's stdout.
const CHILD_TEST: &str =
    "remote_transport::production_backend_supports_https_and_ssh_and_rejects_untrusted_tls";
const CHILD_VERIFIED: &str = "ssh-child-verified-fetch";

fn fixture_key(seed: u8) -> PrivateKey {
    // Deliberately public test-only material; never used outside this fixture.
    russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[seed; 32]).into()
}

trait AgentStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + Sync {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + Sync> AgentStream for T {}
type Stream = Box<dyn AgentStream>;

async fn start_agent(root: &Path) -> (String, AgentClient<Stream>) {
    let (sender, receiver) = tokio::sync::mpsc::channel::<std::io::Result<Stream>>(4);
    let streams = Box::pin(futures_util::stream::unfold(
        receiver,
        |mut receiver| async { receiver.recv().await.map(|stream| (stream, receiver)) },
    ));
    tokio::spawn(async move {
        russh::keys::agent::server::serve(streams, ())
            .await
            .unwrap();
    });
    #[cfg(unix)]
    {
        let path = root.join("agent.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                if sender.send(Ok(Box::new(stream))).await.is_err() {
                    break;
                }
            }
        });
        let stream: Stream = Box::new(tokio::net::UnixStream::connect(&path).await.unwrap());
        (path.to_str().unwrap().into(), AgentClient::connect(stream))
    }
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
        let path = format!(
            r"\\.\pipe\tilo-git-fixture-{}-{}",
            std::process::id(),
            root.file_name().unwrap().to_str().unwrap()
        );
        let mut pipe = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&path)
            .unwrap();
        let next_path = path.clone();
        tokio::spawn(async move {
            loop {
                pipe.connect().await.unwrap();
                let next = ServerOptions::new().create(&next_path).unwrap();
                if sender.send(Ok(Box::new(pipe))).await.is_err() {
                    break;
                }
                pipe = next;
            }
        });
        let stream: Stream = Box::new(ClientOptions::new().open(&path).unwrap());
        (path, AgentClient::connect(stream))
    }
}

struct GitServer {
    repository: PathBuf,
    identity: PublicKey,
    channels: HashMap<ChannelId, Channel<server::Msg>>,
}

impl server::Handler for GitServer {
    type Error = russh::Error;

    async fn auth_publickey(
        &mut self,
        user: &str,
        key: &PublicKey,
    ) -> Result<server::Auth, Self::Error> {
        Ok(if user == "fixture" && key == &self.identity {
            server::Auth::Accept
        } else {
            server::Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            }
        })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        id: ChannelId,
        command: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // Accept only the two exact fixture operations, without a command shell.
        let program = match command {
            b"git-upload-pack '/remote.git'" => "upload-pack",
            b"git-receive-pack '/remote.git'" => "receive-pack",
            _ => {
                session.channel_failure(id)?;
                return Ok(());
            }
        };
        session.channel_success(id)?;
        let stream = self.channels.remove(&id).unwrap().into_stream();
        let handle = session.handle();
        let repository = self.repository.clone();
        tokio::spawn(async move {
            let mut command = tokio::process::Command::new("git");
            command
                .arg(program)
                .arg(repository)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            #[cfg(windows)]
            command.creation_flags(0x0800_0000);
            let mut child = command
                .spawn()
                .expect("Git is required for the SSH fixture");
            let mut stdin = child.stdin.take().unwrap();
            let mut stdout = child.stdout.take().unwrap();
            let (mut input, mut output) = tokio::io::split(stream);
            let incoming =
                tokio::spawn(async move { tokio::io::copy(&mut input, &mut stdin).await });
            let copied = tokio::io::copy(&mut stdout, &mut output).await;
            let status = child.wait().await.unwrap();
            incoming.abort();
            copied.unwrap();
            handle
                .exit_status_request(id, u32::try_from(status.code().unwrap_or(1)).unwrap())
                .await
                .unwrap();
            handle.eof(id).await.unwrap();
            handle.close(id).await.unwrap();
        });
        Ok(())
    }
}

pub(super) fn verify_transport() {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        verify_child(Path::new(&root));
        return;
    }
    let root = tempfile::tempdir().unwrap();
    super::seed_remote(&root.path().join("remote.git"));
    let fixture_root = root.path().to_path_buf();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let worker = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            let identity = fixture_key(23);
            let host = fixture_key(42);
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let (agent_path, mut agent) = start_agent(&fixture_root).await;
            agent.add_identity(&identity, &[]).await.unwrap();
            std::fs::create_dir(fixture_root.join(".ssh")).unwrap();
            std::fs::write(fixture_root.join(".ssh/known_hosts"), format!("[127.0.0.1]:{port} {}\n", host.public_key().to_openssh().unwrap())).unwrap();
            std::fs::write(fixture_root.join("port"), port.to_string()).unwrap();
            let config = Arc::new(server::Config { keys: vec![host], auth_rejection_time: Duration::ZERO, inactivity_timeout: Some(Duration::from_secs(15)), ..Default::default() });
            ready_tx.send(agent_path).unwrap();
            tokio::pin!(stop_rx);
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.unwrap();
                        let config = config.clone();
                        let handler = GitServer { repository: fixture_root.join("remote.git"), identity: identity.public_key().clone(), channels: HashMap::new() };
                        tokio::spawn(async move {
                            // The final test deliberately rejects the host during handshake.
                            if let Ok(session) = server::run_stream(config, stream, handler).await {
                                let _ = session.await;
                            }
                        });
                    }
                }
            }
        });
    });
    let agent = ready_rx
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("SSH fixture startup");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", CHILD_TEST, "--nocapture"])
        .env(CHILD_ROOT, root.path())
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .env("SSH_AUTH_SOCK", agent)
        .env_remove("GIT_SSH")
        .env_remove("GIT_SSH_COMMAND")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.path().join("empty-config"));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let result = command.output();
    let _ = stop_tx.send(());
    worker.join().unwrap();
    let result = result.unwrap();
    assert!(
        result.status.success(),
        "SSH transfer child failed: {}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8_lossy(&result.stdout).contains(CHILD_VERIFIED),
        "SSH transfer child never ran the verification (is {CHILD_TEST} still the test name?): {}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn verify_child(root: &Path) {
    let port = std::fs::read_to_string(root.join("port")).unwrap();
    let remote = git2::Repository::open_bare(root.join("remote.git")).unwrap();
    let expected = remote.head().unwrap().target().unwrap();
    let client = git2::Repository::init(root.join("client")).unwrap();
    client
        .remote(
            "origin",
            &format!("ssh://fixture@127.0.0.1:{port}/remote.git"),
        )
        .unwrap();
    let path = client.workdir().unwrap();
    git_client::repo::fetch(path, None).expect("fetch through the real SSH agent and server");
    assert_eq!(
        client
            .find_reference("refs/remotes/origin/main")
            .unwrap()
            .target(),
        Some(expected)
    );
    let tree = client.find_commit(expected).unwrap().tree().unwrap();
    assert_eq!(
        client
            .find_blob(tree.get_name("proof.txt").unwrap().id())
            .unwrap()
            .content(),
        b"remote proof\n"
    );
    super::checkout_main(&client);
    let pushed = super::commit_content(&client, b"pushed through SSH\n");
    git_client::repo::push(path, None).expect("push through the real SSH agent and server");
    assert_eq!(remote.head().unwrap().target(), Some(pushed));
    let tree = remote.find_commit(pushed).unwrap().tree().unwrap();
    assert_eq!(
        remote
            .find_blob(tree.get_name("proof.txt").unwrap().id())
            .unwrap()
            .content(),
        b"pushed through SSH\n"
    );
    std::fs::write(root.join(".ssh/known_hosts"), "").unwrap();
    let error = git_client::repo::fetch(path, None).unwrap_err();
    assert!(
        format!("{error:#}").contains("unknown SSH host"),
        "host trust must fail specifically: {error:#}"
    );
    assert_eq!(
        client
            .find_reference("refs/remotes/origin/main")
            .unwrap()
            .target(),
        Some(pushed)
    );
    println!("{CHILD_VERIFIED}");
}
