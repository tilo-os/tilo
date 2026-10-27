//! The selector accepts a board release only through the pinned key's exact
//! digest signature, binding provenance and current freshness; its commands
//! are bounded; its credentials stay private; and `tilo-verified-update`
//! activates only what the selector and gate verified.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "a test fixture: any failed fixture call fails the test"
)]

use std::cell::RefCell;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Value, json};
use tilo_board_successor::command;
use tilo_board_successor::registry::Registry;
use tilo_board_successor::release::{
    FRESHNESS, MAX_OUTPUT, REPOSITORY, SLSA, Successor, freshness, provenance, select, signature,
};
use tilo_board_successor::{Error, Result};

const UPDATER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../os/usr/libexec/tilo-verified-update"
);
const NOW: i128 = 1000;

type Mutation = Box<dyn Fn(&mut Value)>;

fn digest() -> String {
    format!("sha256:{}", "a".repeat(64))
}

fn source() -> String {
    "b".repeat(40)
}

fn envelope(predicate: &Value, kind: &str) -> String {
    let statement = json!({"predicateType": kind, "subject": [{"name": REPOSITORY,
        "digest": {"sha256": "a".repeat(64)}}], "predicate": predicate});
    let payload = base64::engine::general_purpose::STANDARD.encode(statement.to_string());
    json!([{"payload": payload}]).to_string()
}

fn documents() -> (Value, Value, String) {
    let provenance = json!({"buildDefinition": {
        "buildType": "https://tilo.sh/buildtypes/bootc-os/v1",
        "internalParameters": {"expectedReleaseDigest": digest()},
        "externalParameters": {"hardwareProfile": "lenovo-83ed", "architecture": "aarch64",
                               "generation": 2, "hardwareDefinitionSha256": "c".repeat(64),
                               "firmwareManifestSha256": "d".repeat(64)},
        "resolvedDependencies": [{"uri": format!("git+ssh://code.tilo.so/tilo/tilo@{}", source())},
                                 {"uri": format!("code.tilo.so/tilo/tilo-lenovo-83ed-firmware@sha256:{}", "e".repeat(64))}]},
        "runDetails": {"builder": {"id": "https://code.tilo.so/tilo/t/os-release-v1"},
                       "byproducts": [{"name": "independent-rebuild",
                                       "uri": "urn:tilo:verification:manifest-digest-equal"}]}});
    let freshness = json!({"schema": FRESHNESS, "repository": REPOSITORY,
                 "digest": digest(), "sourceRevision": source(), "generation": 2,
                 "issuedAt": NOW - 100, "expiresAt": NOW + 1000});
    let signature = json!([{"critical": {"image": {"docker-manifest-digest": digest()},
                              "identity": {"docker-reference": REPOSITORY}}}])
    .to_string();
    (provenance, freshness, signature)
}

/// Answers each verification command from the documents and records it.
struct Releases {
    provenance: Value,
    freshness: Value,
    signature: String,
    calls: RefCell<Vec<Vec<String>>>,
}

impl Releases {
    fn new() -> Self {
        let (provenance, freshness, signature) = documents();
        Self {
            provenance,
            freshness,
            signature,
            calls: RefCell::new(Vec::new()),
        }
    }

    fn answer(&self, args: &[&str]) -> String {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|a| (*a).to_owned()).collect());
        if args[0] == "/usr/bin/skopeo" {
            return digest();
        }
        if args[1] == "verify" {
            return self.signature.clone();
        }
        let kind = args.iter().skip_while(|a| **a != "--type").nth(1).unwrap();
        if *kind == "slsaprovenance1" {
            envelope(&self.provenance, SLSA)
        } else {
            envelope(&self.freshness, FRESHNESS)
        }
    }

    fn select(&self, current: u64) -> Result<Successor> {
        select(current, NOW, &|args: &[&str]| Ok(self.answer(args)))
    }
}

fn is_value_error<T: std::fmt::Debug>(result: &Result<T>) -> bool {
    matches!(result, Err(Error::Value(_)))
}

#[test]
fn exact_signature_source_generation_and_freshness_select_immutable_digest() {
    let registry = Releases::new();
    let successor = registry.select(1).unwrap();
    assert_eq!(
        successor,
        Successor {
            exact: format!("{REPOSITORY}@{}", digest()),
            generation: 2,
            source: source(),
            expires: NOW + 1000,
        }
    );
    let calls = registry.calls.borrow();
    assert_eq!(
        calls[0].last().unwrap(),
        "docker://code.tilo.so/tilo/tilo:lenovo-83ed-latest"
    );
    assert_eq!(
        calls[calls.len() - 2].last().unwrap(),
        "code.tilo.so/tilo/tilo:lenovo-83ed-gen2"
    );
    assert_eq!(
        calls[calls.len() - 1].last().unwrap(),
        &format!("code.tilo.so/tilo/tilo:lenovo-83ed-{}", source())
    );
    for call in &calls[1..4] {
        assert_eq!(call.last().unwrap(), &format!("{REPOSITORY}@{}", digest()));
    }
    // Every cosign call names the pinned verifier and key.
    for call in &calls[1..] {
        assert_eq!(
            &call[..4],
            [
                "/usr/libexec/tilo-cosign",
                call[1].as_str(),
                "--key",
                "/usr/share/tilo/cosign.pub"
            ]
        );
    }
}

#[test]
fn wrong_profile_architecture_digest_source_and_ambiguous_provenance_refuse() {
    let (original, _, _) = documents();
    let mutations: Vec<Mutation> = vec![
        Box::new(|p| {
            p["buildDefinition"]["externalParameters"]["hardwareProfile"] = json!("foreign");
        }),
        Box::new(|p| p["buildDefinition"]["externalParameters"]["architecture"] = json!("x86_64")),
        Box::new(|p| {
            p["buildDefinition"]["internalParameters"]["expectedReleaseDigest"] =
                json!(format!("sha256:{}", "f".repeat(64)));
        }),
        Box::new(|p| p["buildDefinition"]["externalParameters"]["generation"] = json!(true)),
        Box::new(|p| p["buildDefinition"]["externalParameters"]["generation"] = json!(2.0)),
        Box::new(|p| p["buildDefinition"]["externalParameters"]["generation"] = json!(0)),
        Box::new(|p| {
            p["buildDefinition"]["resolvedDependencies"]
                .as_array_mut()
                .unwrap()
                .push(
                    json!({"uri": format!("git+ssh://code.tilo.so/tilo/tilo@{}", "f".repeat(40))}),
                );
        }),
        Box::new(|p| {
            p["buildDefinition"]["resolvedDependencies"]
                .as_array_mut()
                .unwrap()
                .remove(1);
        }),
        Box::new(|p| p["runDetails"]["byproducts"] = json!([])),
        Box::new(|p| {
            p["buildDefinition"]["externalParameters"]["hardwareDefinitionSha256"] = json!("");
        }),
        Box::new(|p| p["buildDefinition"]["externalParameters"]["children"] = json!(null)),
        Box::new(|p| p["runDetails"]["builder"] = json!({"id": "https://example.invalid/builder"})),
    ];
    for (index, mutation) in mutations.iter().enumerate() {
        let mut predicate = original.clone();
        mutation(&mut predicate);
        let result = provenance(&envelope(&predicate, SLSA), &digest());
        assert!(is_value_error(&result), "mutation {index}: {result:?}");
    }
    let rows: Vec<Value> = serde_json::from_str(&envelope(&original, SLSA)).unwrap();
    let doubled = json!([rows[0], rows[0]]).to_string();
    assert!(is_value_error(&provenance(&doubled, &digest())));
    assert_eq!(
        provenance(&envelope(&original, SLSA), &digest()).unwrap(),
        (source(), 2)
    );
    // One JSON document per line is read as well.
    let lines = format!("{}\n\n{}\n", rows[0], json!({"payload": "!!"}));
    assert_eq!(provenance(&lines, &digest()).unwrap(), (source(), 2));
}

#[test]
fn expiry_repository_source_generation_and_oversized_lifetime_refuse() {
    let (_, original, _) = documents();
    for (key, value) in [
        ("repository", json!("registry.tilo.so/tilo/tilo")),
        ("sourceRevision", json!("f".repeat(40))),
        ("generation", json!(3)),
        ("expiresAt", json!(NOW)),
        ("issuedAt", json!(NOW + 1)),
        ("expiresAt", json!(NOW + 604_801)),
        ("generation", json!(true)),
        ("expiresAt", json!(NOW - 100 + 299)),
    ] {
        let mut predicate = original.clone();
        predicate[key] = value;
        let result = freshness(
            &envelope(&predicate, FRESHNESS),
            &digest(),
            &source(),
            2,
            NOW,
        );
        assert!(is_value_error(&result), "{key}: {result:?}");
    }
    let mut extra = original.clone();
    extra["note"] = json!("x");
    assert!(is_value_error(&freshness(
        &envelope(&extra, FRESHNESS),
        &digest(),
        &source(),
        2,
        NOW
    )));
    assert_eq!(
        freshness(
            &envelope(&original, FRESHNESS),
            &digest(),
            &source(),
            2,
            NOW
        )
        .unwrap(),
        NOW + 1000
    );
}

#[test]
fn monotonic_release_counter_bound_and_signature_refusal() {
    let registry = Releases::new();
    assert!(is_value_error(&registry.select(3)));
    let mut registry = Releases::new();
    registry.provenance["buildDefinition"]["externalParameters"]["generation"] = json!(66);
    assert!(is_value_error(&registry.select(1)));
    let (_, _, signed) = documents();
    let foreign_digest = signed.replace(&digest(), &format!("sha256:{}", "f".repeat(64)));
    assert!(is_value_error(&signature(&foreign_digest, &digest())));
    let foreign_repository = signed.replace(REPOSITORY, "foreign/repository");
    assert!(is_value_error(&signature(&foreign_repository, &digest())));
    assert!(signature(&signed, &digest()).is_ok());
    assert!(signature("", &digest()).is_err());
    assert!(signature("[1]", &digest()).is_err());
}

#[test]
fn successor_hint_must_be_an_exact_digest() {
    for hint in [
        "latest",
        "sha256:abc",
        &format!("sha256:{}", "A".repeat(64)),
    ] {
        let answer = |args: &[&str]| -> Result<String> {
            assert_eq!(
                args[0], "/usr/bin/skopeo",
                "nothing is verified for a bad hint"
            );
            Ok(format!("{hint}\n"))
        };
        assert!(is_value_error(&select(1, NOW, &answer)), "{hint}");
    }
}

fn sh(script: &str, timeout: Duration, max_output: usize) -> Result<String> {
    command::run(&["/bin/sh", "-c", script], &[], timeout, max_output)
}

#[test]
fn child_output_deadline_and_failure_are_real_bounded_refusals() {
    let long = Duration::from_secs(20);
    let too_much = sh("head -c 4096 /dev/zero | tr '\\0' x", long, 1024);
    assert!(
        matches!(&too_much, Err(Error::Value(m)) if m.contains("exceeds its bound")),
        "{too_much:?}"
    );
    let noisy = sh("head -c 4096 /dev/zero | tr '\\0' x >&2", long, 1024);
    assert!(
        matches!(&noisy, Err(Error::Value(m)) if m.contains("exceeds its bound")),
        "{noisy:?}"
    );
    let slow = sh("sleep 1", Duration::from_millis(20), MAX_OUTPUT);
    assert!(slow.is_err(), "{slow:?}");
    let refused = sh("exit 7", long, MAX_OUTPUT);
    assert!(
        matches!(&refused, Err(Error::Value(m)) if m == "sh refused release verification (exit 7)"),
        "{refused:?}"
    );
    assert_eq!(
        sh("echo ready; echo noise >&2", long, MAX_OUTPUT).unwrap(),
        "ready\n"
    );
    let missing = command::run(&["/nonexistent/tilo-cosign"], &[], long, MAX_OUTPUT);
    assert!(matches!(missing, Err(Error::Os(_))));
}

#[test]
fn a_timed_out_child_is_killed() {
    let work = tempfile::tempdir().unwrap();
    let pid_file = work.path().join("pid");
    // The verifier would run for an hour unless the deadline ends it.
    let script = format!("echo $$ > {}; exec sleep 3600", pid_file.display());
    assert!(sh(&script, Duration::from_millis(50), MAX_OUTPUT).is_err());
    // A verifier the deadline caught before it wrote its pid never ran.
    if let Some(pid) = fs::read_to_string(&pid_file)
        .ok()
        .and_then(|pid| pid.trim().parse::<u32>().ok())
    {
        tilo_test_env::wait_until("the timed-out verifier to die", || {
            fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |stat| {
                stat.rsplit_once(") ")
                    .is_some_and(|(_, rest)| rest.starts_with('Z'))
            })
        });
    }
}

#[test]
fn existing_bootc_auth_precedence_private_snapshot_and_cleanup() {
    use std::os::unix::fs::MetadataExt;
    let work = tempfile::tempdir().unwrap();
    let root = work.path();
    let runtime = root.join("run/ostree");
    let persistent = root.join("etc/ostree");
    fs::create_dir_all(&runtime).unwrap();
    fs::create_dir_all(&persistent).unwrap();
    for (path, fixture) in [
        (
            runtime.join("auth.json"),
            r#"{"auths": {"test": {"auth": "fixture-runtime"}}}"#,
        ),
        (
            persistent.join("auth.json"),
            r#"{"auths": {"test": {"auth": "fixture-persistent"}}}"#,
        ),
    ] {
        fs::write(&path, fixture).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let owner = fs::metadata(root).unwrap().uid();
    // Fixture root; production checks each path with the profile reader's
    // trusted_path.
    let trusted = |relative: &str| -> Result<PathBuf> { Ok(root.join(relative)) };
    let snapshot;
    {
        let registry = Registry::new(root, &trusted, root, owner).unwrap();
        let environment: Vec<_> = registry.environment().to_vec();
        assert_eq!(environment[0].0, "DOCKER_CONFIG");
        assert_eq!(environment[1].0, "REGISTRY_AUTH_FILE");
        snapshot = PathBuf::from(&environment[1].1);
        assert_eq!(Path::new(&environment[0].1), snapshot.parent().unwrap());
        assert_eq!(
            fs::read(&snapshot).unwrap(),
            fs::read(runtime.join("auth.json")).unwrap()
        );
        assert_eq!(fs::metadata(&snapshot).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            fs::metadata(snapshot.parent().unwrap()).unwrap().mode() & 0o777,
            0o700
        );
        assert!(
            snapshot
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("tilo-board-verifier-")
        );
        let seen = registry
            .run(&[
                "/bin/sh",
                "-c",
                "cat \"$REGISTRY_AUTH_FILE\"; echo \"$DOCKER_CONFIG\"",
            ])
            .unwrap();
        assert!(seen.contains("fixture-runtime"), "{seen}");
        assert!(seen.contains(&environment[0].1.to_string_lossy().into_owned()));
    }
    assert!(!snapshot.exists());
    assert!(!snapshot.parent().unwrap().exists());
    fs::set_permissions(runtime.join("auth.json"), fs::Permissions::from_mode(0o644)).unwrap();
    let exposed = Registry::new(root, &trusted, root, owner);
    assert!(exposed.is_err(), "exposed bootc credentials were accepted");
    // Without the runtime file the persistent one is used; without any, `{}`.
    fs::remove_file(runtime.join("auth.json")).unwrap();
    let registry = Registry::new(root, &trusted, root, owner).unwrap();
    let path = PathBuf::from(&registry.environment()[1].1);
    assert!(
        fs::read_to_string(path)
            .unwrap()
            .contains("fixture-persistent")
    );
    fs::remove_file(persistent.join("auth.json")).unwrap();
    let registry = Registry::new(root, &trusted, root, owner).unwrap();
    assert_eq!(
        fs::read(PathBuf::from(&registry.environment()[1].1)).unwrap(),
        b"{}"
    );
    fs::write(persistent.join("auth.json"), "[]").unwrap();
    fs::set_permissions(
        persistent.join("auth.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(
        Registry::new(root, &trusted, root, owner).is_err(),
        "credentials must be an object"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn updater_uses_exact_switch_and_verifies_before_activation() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path();
    let calls = root.join("calls");
    let script = |name: &str, body: &str| -> String {
        let path = root.join(name);
        fs::write(
            &path,
            format!("#!/usr/bin/env bash\nset -euo pipefail\n{body}"),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    };
    let successor = script(
        "successor",
        &format!(
            "if [ \"$1\" = applicable ]; then exit 0; fi\nprintf '%s\\n' '{REPOSITORY}@{}' 2 '{}' 2000\n",
            digest(),
            source()
        ),
    );
    let bootc = script(
        "bootc",
        &format!("printf 'bootc %s\\n' \"$*\" >>'{}'\n", calls.display()),
    );
    let gate = script(
        "gate",
        &format!(
            r#"if [ "$1" = --trusted-time ]; then printf '1000000000000\n'; exit 0; fi
[ "$1" = --successor ] && [ "$2" = '{}' ] && [ "$3" = 2 ]
[ "$4" = 2000 ] && [ -e "/proc/self/fd/$5" ]
if flock -n "$6" true; then echo 'parent lock lost' >&2; exit 1; fi
printf 'gate\n' >>'{}'
[ "${{REFUSE_GATE:-0}}" != 1 ]
"#,
            digest(),
            calls.display()
        ),
    );
    let run = |refuse: bool| {
        Command::new("bash")
            .arg(UPDATER)
            .arg("--quiet")
            .env("TILO_IMAGE_TRUST_BOOTC", &bootc)
            .env("TILO_IMAGE_TRUST_GATE", &gate)
            .env("TILO_IMAGE_TRUST_SUCCESSOR", &successor)
            .env("TILO_IMAGE_TRUST_UPDATE_LOCK", root.join("update.lock"))
            .env("TILO_IMAGE_TRUST_DIR", root.join("trust"))
            .env("REFUSE_GATE", if refuse { "1" } else { "0" })
            .output()
            .unwrap()
    };
    let result = run(false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let recorded = fs::read_to_string(&calls).unwrap();
    assert_eq!(
        recorded.lines().collect::<Vec<_>>(),
        [
            format!(
                "bootc switch --download-only --quiet {REPOSITORY}@{}",
                digest()
            ),
            "gate".to_owned(),
            "bootc switch --from-downloaded --quiet".to_owned(),
        ]
    );
    fs::remove_file(&calls).unwrap();
    let result = run(true);
    assert!(!result.status.success());
    assert_eq!(fs::read_to_string(&calls).unwrap().lines().count(), 2);
}

#[test]
fn the_updater_runs_the_compiled_selector() {
    let updater = fs::read_to_string(UPDATER).unwrap();
    assert!(updater.contains(
        r#"SUCCESSOR="${TILO_IMAGE_TRUST_SUCCESSOR:-/usr/libexec/tilo-board-successor}""#
    ));
}

#[test]
fn the_command_line_is_one_argument() {
    let binary = tilo_test_env::package_binary!("tilo-board-successor");
    for args in [&[][..], &["applicable", "extra"][..]] {
        let output = Command::new(&binary).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "tilo-board-successor: usage: tilo-board-successor \
             applicable|NOW_NS|install-update-origin OVERRIDE\n"
        );
    }
}
