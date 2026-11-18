//! An image's enrollment methods are what its machine may enroll with.
//!
//! Each image declares them once (`/usr/lib/tilo/enrollment-methods`); the one
//! enrollment every image uses reads that declaration before it does anything
//! and refuses a method the image does not offer. These start the real program,
//! so the refusal is the one a machine gets.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn enroll_binary() -> PathBuf {
    tilo_test_env::package_binary!("tilo-enroll")
}

struct Fixture {
    root: tempfile::TempDir,
    identity_root: PathBuf,
    methods: PathBuf,
}

impl Fixture {
    fn new(declaration: &str) -> Self {
        let root = tempfile::tempdir().expect("temp root");
        let methods = root.path().join("enrollment-methods");
        std::fs::write(&methods, declaration).expect("declaration");
        Self {
            identity_root: root.path().join("tilo"),
            methods,
            root,
        }
    }

    fn enroll(&self, arguments: &[&str]) -> Output {
        let mut command = Command::new(enroll_binary());
        command
            .arg("enroll")
            .arg("--methods-file")
            .arg(&self.methods)
            .arg("--identity-root")
            .arg(&self.identity_root);
        // Never whatever this machine's own /etc/tilo holds.
        if !arguments.contains(&"--token-file") {
            command
                .arg("--token-file")
                .arg(self.root.path().join("no-token.txt"));
        }
        command.args(arguments).output().expect("start tilo-enroll")
    }

    /// The token file a deployment leaves on the machine.
    fn token(&self, text: &str, mode: u32) -> PathBuf {
        let path = self.root.path().join("enrollment-token.txt");
        std::fs::write(&path, text).expect("token");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("mode");
        path
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn refused(output: &Output, said: &str) {
    assert!(
        !output.status.success(),
        "the enrollment was not refused: {output:?}"
    );
    let error = stderr(output);
    assert!(error.contains(said), "expected {said:?} in: {error}");
}

#[test]
fn the_server_image_pairs_no_local_network() {
    let fixture = Fixture::new("# Tilo Server\ntoken\ncode\n");
    let output = fixture.enroll(&["--method", "local-network"]);
    refused(&output, "does not offer local-network");
    refused(&output, "token, code");
}

#[test]
fn a_machine_offered_no_code_is_refused_before_it_waits() {
    let fixture = Fixture::new("token\n");
    // No token was handed to it, so the code would be how it enrolls; this
    // image offers none.
    let output = fixture.enroll(&[]);
    refused(&output, "does not offer code");
}

#[test]
fn a_token_offered_by_no_image_is_refused_even_when_this_machine_has_one() {
    let fixture = Fixture::new("code\n");
    let token = fixture.token("ABCDEF", 0o600);
    let output = fixture.enroll(&["--token-file", token.to_str().unwrap()]);
    refused(&output, "does not offer token");
    assert!(token.exists(), "a refused token is not spent");
}

#[test]
fn a_machine_with_no_token_does_not_enroll_with_one() {
    let fixture = Fixture::new("token\ncode\n");
    let output = fixture.enroll(&["--method", "token"]);
    refused(&output, "handed no enrollment token");
}

#[test]
fn a_token_others_could_read_is_refused() {
    let fixture = Fixture::new("token\ncode\n");
    let token = fixture.token("ABCDEF", 0o644);
    let output = fixture.enroll(&["--token-file", token.to_str().unwrap()]);
    refused(&output, "must be mode 0600");
}

#[test]
fn a_machine_with_a_declaration_offering_the_network_still_enrolls_with_a_code() {
    let fixture = Fixture::new("token\ncode\nlocal-network\n");
    let output = fixture.enroll(&["--method", "local-network"]);
    refused(&output, "a machine enrolls with a token or a code");
}

#[test]
fn the_placeholder_an_image_ships_is_no_token() {
    let fixture = Fixture::new("token\ncode\n");
    let token = fixture.token("# Replace with an enrollment token.\n", 0o644);
    // Read as no token, so a token-only image refuses it as no token rather
    // than as an unreadable file.
    let only_token = Fixture::new("token\n");
    let output = only_token.enroll(&["--token-file", token.to_str().unwrap()]);
    refused(&output, "does not offer code");
}

#[test]
fn an_unknown_method_in_an_image_declaration_stops_enrollment() {
    let fixture = Fixture::new("token\ncodes\n");
    let output = fixture.enroll(&["--method", "token"]);
    refused(&output, "is not an enrollment method");
}

#[test]
fn the_declaration_is_read_from_the_path_the_image_names() {
    // The shipped path is the one the image writes; a test machine without it
    // offers every method, which is why the refusal above passes the file.
    assert_eq!(
        tilo_identity::enrollment::DECLARATION,
        "/usr/lib/tilo/enrollment-methods"
    );
    assert!(Path::new(tilo_identity::enrollment::DECLARATION).is_absolute());
}
