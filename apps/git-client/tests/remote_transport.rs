//! Checks the transports compiled into the production Git backend.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixture failures must fail the test"
)]

use std::fs;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use git2::{Repository, Signature};
use ui::widget::Widget;

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_server(root: &Path) -> (Server, String) {
    start_server_with_tls(root, false)
}

fn start_server_with_tls(root: &Path, tls: bool) -> (Server, String) {
    if tls {
        let mut certificate = Command::new("openssl");
        certificate
            .args([
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-days",
                "1",
                "-subj",
                "/CN=localhost",
                "-keyout",
            ])
            .arg(root.join("key.pem"))
            .arg("-out")
            .arg(root.join("certificate.pem"));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            certificate.creation_flags(0x0800_0000);
        }
        let output = certificate
            .output()
            .expect("OpenSSL is required for Git TLS tests");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut command = Command::new(if cfg!(windows) { "python" } else { "python3" });
    command
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/http_remote.py"))
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(
            fs::File::create(root.join("server.log")).unwrap(),
        ));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    if tls {
        command.arg("--tls");
    }
    let mut server = Server(
        command
            .spawn()
            .expect("Python 3 is required for Git HTTP tests"),
    );
    let deadline = Instant::now().checked_add(Duration::from_secs(10)).unwrap();
    let ready = root.join("ready.json");
    while !ready.exists() {
        assert!(
            server.0.try_wait().unwrap().is_none() && Instant::now() < deadline,
            "Git HTTP fixture failed: {}",
            fs::read_to_string(root.join("server.log")).unwrap()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let value: serde_json::Value = serde_json::from_slice(&fs::read(ready).unwrap()).unwrap();
    let port = value.get("port").unwrap().as_u64().unwrap();
    let scheme = if tls { "https" } else { "http" };
    (server, format!("{scheme}://127.0.0.1:{port}/remote.git"))
}

fn seed_remote(path: &Path) -> git2::Oid {
    let repository = Repository::init_bare(path).unwrap();
    repository
        .config()
        .unwrap()
        .set_bool("http.receivepack", true)
        .unwrap();
    let commit = commit_content(&repository, b"remote proof\n");
    repository.set_head("refs/heads/main").unwrap();
    commit
}

fn commit_content(repository: &Repository, content: &[u8]) -> git2::Oid {
    let blob = repository.blob(content).unwrap();
    let mut builder = repository.treebuilder(None).unwrap();
    builder.insert("proof.txt", blob, 0o100_644).unwrap();
    let tree = repository.find_tree(builder.write().unwrap()).unwrap();
    let signature = Signature::now("Fixture", "fixture@example.invalid").unwrap();
    let parent = repository
        .find_reference("refs/heads/main")
        .ok()
        .map(|reference| reference.peel_to_commit().unwrap());
    repository
        .commit(
            Some("refs/heads/main"),
            &signature,
            &signature,
            "proof",
            &tree,
            &parent.iter().collect::<Vec<_>>(),
        )
        .unwrap()
}

fn authenticated_client(root: &Path, url: &str, password: &str) -> Repository {
    let path = root.join("client");
    let client = Repository::init(&path).unwrap();
    client.remote("origin", url).unwrap();
    let credentials = root.join("credentials");
    fs::write(
        &credentials,
        format!(
            "{}\n",
            url.replacen("http://", &format!("http://test:{password}@"), 1)
        ),
    )
    .unwrap();
    let store_path = credentials.to_str().unwrap().replace('\\', "/");
    let mut config = client.config().unwrap();
    config.set_str("credential.helper", "").unwrap();
    config
        .set_multivar(
            "credential.helper",
            "^does-not-match$",
            &format!("store --file={store_path}"),
        )
        .unwrap();
    config.set_bool("core.autocrlf", false).unwrap();
    client
}

fn checkout_main(client: &Repository) {
    let commit = client
        .find_reference("refs/remotes/origin/main")
        .unwrap()
        .peel_to_commit()
        .unwrap();
    let mut branch = client.branch("main", &commit, true).unwrap();
    branch.set_upstream(Some("origin/main")).unwrap();
    client.set_head("refs/heads/main").unwrap();
    client.checkout_head(None).unwrap();
}

#[test]
fn fetch_uses_configured_credentials_and_downloads_real_objects() {
    let root = tempfile::tempdir().unwrap();
    let expected = seed_remote(&root.path().join("remote.git"));
    let (_server, url) = start_server(root.path());
    let client = authenticated_client(root.path(), &url, "test");
    let path = root.path().join("client");

    git_client::repo::fetch(&path, Some("origin")).expect("authenticated smart HTTP fetch");

    let reference = client.find_reference("refs/remotes/origin/main").unwrap();
    assert_eq!(reference.target(), Some(expected));
    let tree = client.find_commit(expected).unwrap().tree().unwrap();
    let entry = tree.get_path(Path::new("proof.txt")).unwrap();
    assert_eq!(
        client.find_blob(entry.id()).unwrap().content(),
        b"remote proof\n"
    );
}

#[test]
fn rejected_credentials_fail_without_retrying_forever() {
    let root = tempfile::tempdir().unwrap();
    seed_remote(&root.path().join("remote.git"));
    let (_server, url) = start_server(root.path());
    let client = authenticated_client(root.path(), &url, "wrong");
    let error = git_client::repo::fetch(client.workdir().unwrap(), None).unwrap_err();
    assert!(
        format!("{error:#}").contains("authentication failed"),
        "{error:#}"
    );
    assert!(client.find_reference("refs/remotes/origin/main").is_err());
}

#[test]
fn push_transfers_real_objects_and_reports_remote_rejection() {
    let root = tempfile::tempdir().unwrap();
    let remote_path = root.path().join("remote.git");
    seed_remote(&remote_path);
    let (_server, url) = start_server(root.path());
    let client = authenticated_client(root.path(), &url, "test");
    let path = client.workdir().unwrap();
    git_client::repo::fetch(path, None).unwrap();
    checkout_main(&client);
    let pushed = commit_content(&client, b"pushed through HTTP\n");
    git_client::repo::push(path, None).unwrap();
    let remote = Repository::open_bare(&remote_path).unwrap();
    assert_eq!(remote.head().unwrap().target(), Some(pushed));
    let tree = remote.find_commit(pushed).unwrap().tree().unwrap();
    assert_eq!(
        remote
            .find_blob(tree.get_name("proof.txt").unwrap().id())
            .unwrap()
            .content(),
        b"pushed through HTTP\n"
    );

    let hook = remote_path.join("hooks/pre-receive");
    fs::write(&hook, "#!/bin/sh\necho fixture-rejection >&2\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    commit_content(&client, b"must be rejected\n");
    let error = git_client::repo::push(path, None).unwrap_err();
    assert!(
        format!("{error:#}").contains("remote rejected"),
        "{error:#}"
    );
    assert_eq!(remote.head().unwrap().target(), Some(pushed));
}

#[test]
fn pull_preserves_conflicting_local_edits_and_head() {
    let root = tempfile::tempdir().unwrap();
    let remote_path = root.path().join("remote.git");
    let original = seed_remote(&remote_path);
    let (_server, url) = start_server(root.path());
    let client = authenticated_client(root.path(), &url, "test");
    let path = client.workdir().unwrap();
    git_client::repo::fetch(path, None).unwrap();
    checkout_main(&client);
    fs::write(path.join("proof.txt"), b"unsaved local edits\n").unwrap();
    let remote = Repository::open_bare(&remote_path).unwrap();
    let updated = commit_content(&remote, b"upstream edits\n");
    assert!(
        git_client::repo::pull(path, None).is_err(),
        "pull must refuse to overwrite local edits"
    );
    assert_eq!(
        fs::read(path.join("proof.txt")).unwrap(),
        b"unsaved local edits\n"
    );
    assert_eq!(client.head().unwrap().target(), Some(original));
    client
        .checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
    git_client::repo::pull(path, None).unwrap();
    assert_eq!(client.head().unwrap().target(), Some(updated));
    assert_eq!(
        fs::read(path.join("proof.txt")).unwrap(),
        b"upstream edits\n"
    );
}

#[test]
fn production_backend_supports_https_remotes() {
    assert!(
        git2::Version::get().https(),
        "Git Client exposes remote operations and must include HTTPS transport"
    );
}

#[test]
fn production_backend_supports_ssh_remotes() {
    assert!(
        git2::Version::get().ssh(),
        "Git Client exposes remote operations and must include SSH transport"
    );
}

#[test]
fn https_rejects_an_untrusted_server_certificate() {
    let root = tempfile::tempdir().unwrap();
    seed_remote(&root.path().join("remote.git"));
    let (_server, url) = start_server_with_tls(root.path(), true);
    let client = Repository::init(root.path().join("client")).unwrap();
    client.remote("origin", &url).unwrap();
    let error = git_client::repo::fetch(client.workdir().unwrap(), None).unwrap_err();
    assert_eq!(
        error.downcast_ref::<git2::Error>().unwrap().code(),
        git2::ErrorCode::Certificate,
        "must fail certificate verification specifically: {error:#}"
    );
    assert!(client.find_reference("refs/remotes/origin/main").is_err());
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one production lifecycle scenario checks admission, responsiveness, isolation and completion in order"
)]
fn remote_fetch_keeps_app_responsive_and_reports_completion_after_sync() {
    use git_client::actions::{GIT_CLIENT_OPERATION_TYPE, GitClientOperation};
    use tilo_actions::{InvocationId, InvocationUpdate, OperationKind};
    let root = tempfile::tempdir().unwrap();
    let expected = seed_remote(&root.path().join("remote.git"));
    let (_server, url) = start_server(root.path());
    let client = authenticated_client(root.path(), &url, "test");
    let path = client.workdir().unwrap();
    fs::write(path.join("unstaged.txt"), "keep unstaged").unwrap();
    let mut app = git_client::GitClientApp::open_at(path).unwrap();
    let mut doc = automerge::AutoCommit::new();
    let invocation = InvocationId::new();
    let payload = serde_json::to_vec(&GitClientOperation::Fetch {
        remote: "origin".into(),
    })
    .unwrap();
    fs::write(root.path().join("block"), "").unwrap();
    let started = app
        .start_resource_operation(
            invocation,
            OperationKind::Effect,
            GIT_CLIENT_OPERATION_TYPE,
            &payload,
            &doc,
            &automerge::ROOT,
        )
        .unwrap();
    assert!(matches!(
        started,
        Some(ui::widget::ResourceOperationStart::Accepted)
    ));
    assert!(app.needs_animation_frame());
    let request_deadline = Instant::now().checked_add(Duration::from_secs(5)).unwrap();
    while !root.path().join("request-started").exists() {
        assert!(
            Instant::now() < request_deadline,
            "worker never contacted the server"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let view = serde_json::to_vec(&GitClientOperation::ShowHistory).unwrap();
    assert!(
        app.invoke_resource_operation(
            OperationKind::View,
            GIT_CLIENT_OPERATION_TYPE,
            &view,
            &doc,
            &automerge::ROOT
        )
        .unwrap()
    );
    let mutation = serde_json::to_vec(&GitClientOperation::StageAll).unwrap();
    assert!(
        app.invoke_resource_operation(
            OperationKind::Effect,
            GIT_CLIENT_OPERATION_TYPE,
            &mutation,
            &doc,
            &automerge::ROOT
        )
        .is_err()
    );
    app.test_stage_all();
    assert!(
        app.invoke_resource_operation(
            OperationKind::View,
            GIT_CLIENT_OPERATION_TYPE,
            &view,
            &doc,
            &automerge::ROOT
        )
        .unwrap(),
        "a rejected mutation must not disable navigation"
    );
    assert_eq!(
        client.index().unwrap().len(),
        0,
        "UI actions must not race the worker"
    );
    assert!(
        app.poll_resource_operation(InvocationId::new(), false, &doc, &automerge::ROOT)
            .is_none()
    );
    assert!(matches!(
        app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT),
        Some(InvocationUpdate::Progress { .. })
    ));
    app.sync(&mut doc, &automerge::ROOT);
    assert!(
        app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
            .is_none()
    );
    fs::write(root.path().join("release"), "").unwrap();
    let deadline = Instant::now().checked_add(Duration::from_secs(10)).unwrap();
    loop {
        app.sync(&mut doc, &automerge::ROOT);
        if let Some(update) = app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
        {
            assert!(
                matches!(update, InvocationUpdate::Succeeded { .. }),
                "{update:?}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "remote operation did not complete"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        client
            .find_reference("refs/remotes/origin/main")
            .unwrap()
            .target(),
        Some(expected)
    );
    assert!(
        app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
            .is_none()
    );
    assert!(!app.needs_animation_frame());
    assert!(
        app.invoke_resource_operation(
            OperationKind::Effect,
            GIT_CLIENT_OPERATION_TYPE,
            &mutation,
            &doc,
            &automerge::ROOT
        )
        .unwrap()
    );
    assert_eq!(Repository::open(path).unwrap().index().unwrap().len(), 1);
}

#[test]
fn remote_failure_is_reported_to_the_invocation_and_visible_status() {
    use git_client::actions::{GIT_CLIENT_OPERATION_TYPE, GitClientOperation};
    use tilo_actions::{InvocationId, InvocationUpdate, OperationKind};
    let root = tempfile::tempdir().unwrap();
    seed_remote(&root.path().join("remote.git"));
    let (_server, url) = start_server(root.path());
    let client = authenticated_client(root.path(), &url, "wrong");
    let mut app = git_client::GitClientApp::open_at(client.workdir().unwrap()).unwrap();
    let mut doc = automerge::AutoCommit::new();
    // Settle the initial collapsed sidebar through a real view action so its
    // startup animation does not keep requesting frames after the job ends.
    let view = serde_json::to_vec(&GitClientOperation::ShowHistory).unwrap();
    app.invoke_resource_operation(
        OperationKind::View,
        GIT_CLIENT_OPERATION_TYPE,
        &view,
        &doc,
        &automerge::ROOT,
    )
    .unwrap();
    assert!(!app.needs_animation_frame());
    let invocation = InvocationId::new();
    let payload = serde_json::to_vec(&GitClientOperation::Fetch {
        remote: "origin".into(),
    })
    .unwrap();
    assert!(matches!(
        app.start_resource_operation(
            invocation,
            OperationKind::Effect,
            GIT_CLIENT_OPERATION_TYPE,
            &payload,
            &doc,
            &automerge::ROOT
        )
        .unwrap(),
        Some(ui::widget::ResourceOperationStart::Accepted)
    ));
    let deadline = Instant::now().checked_add(Duration::from_secs(10)).unwrap();
    loop {
        app.sync(&mut doc, &automerge::ROOT);
        // A cancellation request is not evidence of cancellation: authentication
        // still failed, and the worker's actual outcome must be returned.
        if let Some(InvocationUpdate::Failed {
            invocation: actual,
            error,
        }) = app.poll_resource_operation(invocation, true, &doc, &automerge::ROOT)
        {
            assert_eq!(actual, invocation);
            assert!(!error.retryable);
            assert!(
                error.message.contains("authentication failed"),
                "{}",
                error.message
            );
            assert_eq!(app.test_last_error(), Some(error.message.as_str()));
            break;
        }
        assert!(Instant::now() < deadline, "failure was not delivered");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!app.needs_animation_frame());
}
