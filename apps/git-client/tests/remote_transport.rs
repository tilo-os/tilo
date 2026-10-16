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
use ui::time::Duration;

use git2::{Repository, Signature};
use ui::framework::testing::TestApp;

#[path = "support/ssh_remote.rs"]
mod ssh_remote;

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
        // Both connection names are valid so rejection specifically exercises
        // the untrusted issuer, rather than an unrelated hostname mismatch.
        let certificate =
            rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
                .expect("generate the untrusted Git TLS fixture certificate");
        fs::write(
            root.join("key.pem"),
            certificate.signing_key.serialize_pem(),
        )
        .unwrap();
        fs::write(root.join("certificate.pem"), certificate.cert.pem()).unwrap();
    }
    let mut command = Command::new(if cfg!(windows) { "python" } else { "python3" });
    command
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/http_remote.py"))
        .arg(root)
        .arg("--hang-bound")
        .arg(tilo_test_env::hang_bound().as_secs().to_string())
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
    let deadline = tilo_test_env::hang_deadline();
    let ready = root.join("ready.json");
    while !ready.exists() {
        assert!(
            server.0.try_wait().unwrap().is_none() && ui::time::now() < deadline,
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

// Run each transport contract with a private user configuration. The app may
// reopen repositories on worker threads, so attaching a config to one Repository
// handle is insufficient; changing the parent process's environment is unsafe.
fn isolated_credentials(test: &str) -> bool {
    const CHILD: &str = "TILO_GIT_HTTP_FIXTURE_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(test) {
        return false;
    }
    let home = tempfile::tempdir().unwrap();
    fs::write(home.path().join(".gitconfig"), "").unwrap();
    let result = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.path().join(".gitconfig"))
        .output()
        .expect("run isolated Git transport contract");
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8_lossy(&result.stdout).contains("1 passed"),
        "isolated transport contract did not run: {test}"
    );
    true
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
            url.replacen("://", &format!("://test:{password}@"), 1)
        ),
    )
    .unwrap();
    let store_path = credentials.to_str().unwrap().replace('\\', "/");
    let quoted_store_path = format!("'{}'", store_path.replace('\'', "'\\''"));
    let mut config = client.config().unwrap();
    config.set_bool("core.autocrlf", false).unwrap();
    drop(config);
    // libgit2's multivar writer does not correctly escape this shell value
    // for Git's config grammar. Let Git encode it in the isolated user's
    // global config, scoped to this remote; checkout-local helpers are untrusted.
    let mut command = Command::new("git");
    command
        .args(["config", "--global", "--add"])
        .arg(format!("credential.{url}.helper"))
        .arg(format!("store --file={quoted_store_path}"));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let output = command.output().expect("Git writes fixture configuration");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
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

/// Remote operations against a real server: fetch with configured credentials,
/// rejected credentials fail without retrying, push transfers objects and reports
/// rejection, and pull preserves conflicting local edits. Each phase is a former test with its own fresh fixture.
#[test]
fn remote_fetch_push_pull_move_real_objects_and_respect_credentials() {
    if isolated_credentials(
        "remote_transport::remote_fetch_push_pull_move_real_objects_and_respect_credentials",
    ) {
        return;
    }
    fetch_uses_configured_credentials_and_downloads_real_objects();
    rejected_credentials_fail_without_retrying_forever();
    push_transfers_real_objects_and_reports_remote_rejection();
    pull_preserves_conflicting_local_edits_and_head();
}

fn fetch_uses_configured_credentials_and_downloads_real_objects() {
    let root = tempfile::Builder::new()
        .prefix("tilo git user's credentials ")
        .tempdir()
        .unwrap();
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

/// The production transport backend: HTTPS and SSH remotes are supported and an
/// untrusted server certificate is rejected. Each phase is a former test with its own fresh fixture.
#[test]
fn production_backend_supports_https_and_ssh_and_rejects_untrusted_tls() {
    if isolated_credentials(
        "remote_transport::production_backend_supports_https_and_ssh_and_rejects_untrusted_tls",
    ) {
        return;
    }
    // The SSH phase re-runs this test in a child process with the fixture's HOME and
    // agent; that child performs only the SSH verification the parent is waiting on.
    if std::env::var_os(ssh_remote::CHILD_ROOT).is_some() {
        production_backend_supports_ssh_remotes();
        return;
    }
    production_backend_supports_https_remotes();
    production_backend_supports_ssh_remotes();
    https_rejects_an_untrusted_server_certificate();
}

fn production_backend_supports_https_remotes() {
    let root = tempfile::tempdir().unwrap();
    let expected = seed_remote(&root.path().join("remote.git"));
    let (_server, url) = start_server_with_tls(root.path(), true);
    let client = authenticated_client(root.path(), &url, "test");
    client
        .config()
        .unwrap()
        .set_str(
            "http.sslCAInfo",
            root.path().join("certificate.pem").to_str().unwrap(),
        )
        .unwrap();

    git_client::repo::fetch(client.workdir().unwrap(), Some("origin"))
        .expect("authenticated HTTPS fetch with an explicitly trusted certificate");

    assert_eq!(
        client
            .find_reference("refs/remotes/origin/main")
            .unwrap()
            .target(),
        Some(expected)
    );
    let tree = client.find_commit(expected).unwrap().tree().unwrap();
    let entry = tree.get_path(Path::new("proof.txt")).unwrap();
    assert_eq!(
        client.find_blob(entry.id()).unwrap().content(),
        b"remote proof\n"
    );
}

fn production_backend_supports_ssh_remotes() {
    ssh_remote::verify_transport();
}

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

/// Remote effects from the app: a fetch keeps the app responsive and reports
/// completion to its caller, and a failure reaches the caller and visible
/// status. Each phase is a former test with its own fresh fixture.
#[test]
fn app_remote_operations_stay_responsive_and_report_outcomes() {
    if isolated_credentials(
        "remote_transport::app_remote_operations_stay_responsive_and_report_outcomes",
    ) {
        return;
    }
    remote_fetch_keeps_app_responsive_and_reports_completion();
    remote_failure_is_reported_to_the_caller_and_visible_status();
}

/// The published row of the remote `name`.
fn remote_row(name: &str) -> String {
    format!("git_client.remote:{}", hex::encode(name))
}

/// Whether the app publishes a row for a transfer in flight.
fn transfer_published(app: &mut TestApp<git_client::GitClientShell>) -> bool {
    fn find(resource: &tilo_actions::Resource) -> bool {
        resource.id.as_str() == "git_client.remote-job" || resource.children.iter().any(find)
    }
    app.harness().resources().iter().any(find)
}

fn remote_fetch_keeps_app_responsive_and_reports_completion() {
    use git_client::actions::GitClientEffect;
    use tilo_actions::InvocationUpdate;
    let root = tempfile::tempdir().unwrap();
    let expected = seed_remote(&root.path().join("remote.git"));
    let (_server, url) = start_server(root.path());
    let client = authenticated_client(root.path(), &url, "test");
    let path = client.workdir().unwrap();
    fs::write(path.join("unstaged.txt"), "keep unstaged").unwrap();
    let mut app =
        TestApp::<git_client::GitClientShell>::launched(&[path.to_str().unwrap()]).unwrap();
    app.settle();
    fs::write(root.path().join("block"), "").unwrap();

    // Started as the palette or an agent does on the remote's row, without
    // waiting for the transfer.
    let instance_id = app.harness().component_instance_id().unwrap();
    let address = ui::bound_dispatch::BoundActionAddress {
        instance_id,
        address: tilo_actions::ActionAddress::new(remote_row("origin"), "fetch"),
    };
    let (invocation, started) = app
        .harness()
        .start_bound_job(&address, &serde_json::Map::new(), false)
        .unwrap();
    assert!(started.completed().is_none(), "the fetch runs as a job");
    let request_deadline = tilo_test_env::hang_deadline();
    while !root.path().join("request-started").exists() {
        assert!(
            ui::time::now() < request_deadline,
            "worker never contacted the server"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    app.settle();
    assert!(transfer_published(&mut app), "the transfer is a live row");
    assert!(
        app.harness()
            .job_updates(invocation)
            .iter()
            .any(|update| matches!(update, InvocationUpdate::Progress { .. })),
        "the caller hears the transfer is running"
    );

    app.effect(&GitClientEffect::ShowHistory)
        .expect("navigation stays available during a transfer");
    assert!(
        app.effect(&GitClientEffect::StageAll).is_err(),
        "repository changes wait for the transfer"
    );
    app.with_surface::<git_client::GitClientApp, _>(git_client::GitClientApp::test_stage_all)
        .unwrap();
    app.effect(&GitClientEffect::ShowChanges)
        .expect("a rejected mutation must not disable navigation");
    assert_eq!(
        client.index().unwrap().len(),
        0,
        "UI actions must not race the worker"
    );

    fs::write(root.path().join("release"), "").unwrap();
    let finished = |harness: &ui::headless::FrameHarness| {
        harness
            .job_updates(invocation)
            .iter()
            .find(|update| update.is_terminal())
            .cloned()
    };
    app.harness().wait_until(
        Duration::from_secs(tilo_test_env::hang_bound().as_secs()),
        |harness| finished(harness).is_some(),
    );
    let outcome = finished(app.harness()).expect("remote operation did not complete");
    assert!(
        matches!(outcome, InvocationUpdate::Succeeded { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        client
            .find_reference("refs/remotes/origin/main")
            .unwrap()
            .target(),
        Some(expected)
    );
    assert!(
        !transfer_published(&mut app),
        "the finished transfer is gone"
    );
    app.effect(&GitClientEffect::StageAll).unwrap();
    assert_eq!(Repository::open(path).unwrap().index().unwrap().len(), 1);
}

fn remote_failure_is_reported_to_the_caller_and_visible_status() {
    use git_client::actions::GitClientEffect;
    let root = tempfile::tempdir().unwrap();
    seed_remote(&root.path().join("remote.git"));
    let (_server, url) = start_server(root.path());
    let client = authenticated_client(root.path(), &url, "wrong");
    let path = client.workdir().unwrap();
    let mut app =
        TestApp::<git_client::GitClientShell>::launched(&[path.to_str().unwrap()]).unwrap();
    app.settle();
    let error = app
        .effect_on(
            &remote_row("origin"),
            &GitClientEffect::Fetch {
                remote: "origin".into(),
            },
        )
        .expect_err("authentication fails");
    let tilo_actions::DoorError::Failed(message) = error else {
        panic!("the transfer ran and failed, it was not refused: {error:?}");
    };
    assert!(message.contains("authentication failed"), "{message}");
    assert_eq!(
        app.with_surface::<git_client::GitClientApp, _>(|app| {
            app.test_last_error().map(str::to_owned)
        })
        .unwrap()
        .as_deref(),
        Some(message.as_str()),
        "the failure shows where the person looks"
    );
    assert!(!transfer_published(&mut app));
}
