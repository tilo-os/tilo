//! The git credential helper the guest image installs
//! (`os/hosted-guest/git-credential-tilo`, set system-wide in `/etc/gitconfig`).
//!
//! A program in a workspace's VM clones and pushes over https with a token the
//! workspace's vault put in its environment, and nothing else: no token in the
//! VM, and no answer for a host the token is not for. These run the script the
//! image ships, as git runs it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use std::path::PathBuf;
use std::process::{Command, Stdio};

use tilo_test_env::SpawnUnderEnvLock as _;

/// What git writes to a helper's standard input for `host`.
fn request(host: &str) -> String {
    format!("protocol=https\nhost={host}\n\n")
}

/// Run the guest's helper as git does, with only `env` set, answering what it
/// printed.
fn helper(action: &str, env: &[(&str, &str)], stdin: &str) -> String {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../os/hosted-guest/git-credential-tilo");
    let mut command = Command::new("sh");
    command
        .arg(&script)
        .arg(action)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child = command.spawn_locked().expect("the guest's helper starts");
    std::io::Write::write_all(
        child.stdin.as_mut().expect("its standard input"),
        stdin.as_bytes(),
    )
    .expect("the request reaches the helper");
    let output = child.wait_with_output().expect("the helper ends");
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).expect("the helper answers text")
}

#[test]
fn the_helper_answers_the_host_its_token_is_for_and_nobody_else() {
    let token = [
        ("TILO_GIT_TOKEN", "s3cret"),
        ("TILO_GIT_HOST", "code.tilo.so"),
    ];
    assert_eq!(
        helper("get", &token, &request("code.tilo.so")),
        "username=git\npassword=s3cret\n"
    );
    // A repository at another host never gets the person's token, and a
    // program with none is left to git's own prompt.
    assert_eq!(helper("get", &token, &request("elsewhere.test")), "");
    assert_eq!(
        helper(
            "get",
            &[("TILO_GIT_HOST", "code.tilo.so")],
            &request("code.tilo.so")
        ),
        ""
    );
    // Only `get` is answered: git's store and erase write nothing anywhere.
    assert_eq!(helper("store", &token, "host=code.tilo.so\n\n"), "");
    assert_eq!(helper("erase", &token, "host=code.tilo.so\n\n"), "");
}

#[test]
fn the_helper_takes_the_username_and_a_token_the_person_grants_as_a_file() {
    let dir = tempfile::tempdir().expect("a directory");
    let file = dir.path().join("git-token");
    std::fs::write(&file, "from-a-file\n").expect("the token file");
    let granted = [
        ("TILO_GIT_TOKEN_FILE", file.to_str().expect("a path")),
        ("TILO_GIT_HOST", "Code.Tilo.So"),
        ("TILO_GIT_USER", "ada"),
    ];
    assert_eq!(
        helper("get", &granted, &request("code.tilo.so:443")),
        "username=ada\npassword=from-a-file\n"
    );
}
