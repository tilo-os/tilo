//! `tilo-firstboot-user` with this helper, against synthetic credentials and
//! inert account tools: every interrupted delivery stays locked and retries,
//! and the hash travels only over stdin.
//!
//! The production script runs unchanged except for its paths. The account
//! tools are stubs keeping a fake shadow entry in the fixture; the home and
//! ownership is repaired by the built helper with real file operations. The
//! separate attesting seed fixture uses the test-only Python compatibility oracle.
#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "a test fixture: any failed fixture call fails the test"
)]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const HELPER: &str = env!("CARGO_BIN_EXE_tilo-firstboot-files");
const SOURCE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../os/usr/libexec/tilo-firstboot-user"
);
const HELPER_LINE: &str =
    r#"FILES_HELPER="$(dirname -- "${BASH_SOURCE[0]}")/tilo-firstboot-files""#;

fn hash() -> String {
    "$6$fixture$".to_owned() + &"A".repeat(86)
}

fn write_tool(directory: &Path, name: &str, body: &str) {
    let path = directory.join(name);
    fs::write(&path, format!("#!/bin/bash\nset -u\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

/// Only account authority is synthetic; publication, content capture and
/// receipt verification use the compatibility oracle, not a runtime script.
fn attesting_helper(root: &Path) {
    let oracle = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../os/tests/oracles/tilo-firstboot-home-seeds.py");
    let source = format!(
        r"#!/usr/bin/env python3
import importlib.util, os, stat
from pathlib import Path
from unittest.mock import patch
r = Path(os.environ['FIXTURE_ROOT'])
if (r / 'shadow').exists():
    assert (r / 'expiry').read_text() == '1970-01-02'
    assert (r / 'shadow').read_text().startswith('!')
if os.environ.get('FAIL_STAGE') == 'seeds':
    raise SystemExit(1)
spec = importlib.util.spec_from_file_location('seeds', {oracle:?})
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)
def trusted(path):
    info = path.lstat()
    if info.st_uid != os.getuid() or info.st_mode & 0o022 or stat.S_ISLNK(info.st_mode):
        raise ValueError('untrusted fixture authority')
writer = helper.atomic.write_file
def write(directory, name, source, mode, uid, gid):
    writer(directory, name, source, mode, os.getuid() if uid == 0 else uid,
           os.getgid() if gid == 0 else gid)
with patch.object(helper.os, 'geteuid', return_value=0), \
        patch.object(helper, 'trusted', side_effect=trusted), \
        patch.object(helper.atomic, 'write_file', side_effect=write):
    helper.seed(r / 'home/fixtureuser', r / 'skel', os.getuid(), os.getgid(), r / 'receipts')
if os.environ.get('FAIL_STAGE') == 'seeds-after':
    raise SystemExit(1)
",
        oracle = oracle.display().to_string(),
    );
    let path = root.join("attesting-seeds");
    fs::write(&path, source).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// The script with its paths moved into `root`, checking that each original
/// occurs as often as expected so a renamed path cannot go unnoticed.
fn script(replacements: &[(&str, String, usize)]) -> String {
    let mut source = read(Path::new(SOURCE));
    for (original, replacement, count) in replacements {
        assert_eq!(source.matches(original).count(), *count, "{original}");
        source = source.replace(original, replacement);
    }
    source
}

// ---- Interruption and retry ------------------------------------------------

/// What the transaction stubs share: the fixture root, the failure the case
/// injects, and the checks that delivery only ever runs on a locked, expired
/// account. A failing stub can also signal the script, as systemd would.
const TRANSACTION_COMMON: &str = r#"r="$FIXTURE_ROOT"
stage="${FAIL_STAGE:-}"
fail() {
  if [ "$stage" = "$1" ]; then
    if [ "${TERMINATE:-}" = 1 ]; then kill -TERM "$PPID"; fi
    if [ "$stage" = crash ]; then kill -KILL "$PPID"; fi
    exit 1
  fi
}
expired() {
  [ "$(cat "$r/expiry" 2>/dev/null)" = 1970-01-02 ] || { echo 'delivery ran unexpired' >&2; exit 1; }
}
locked() {
  expired
  case "$(cat "$r/shadow")" in '!'*) ;; *) echo 'delivery ran unlocked' >&2; exit 1 ;; esac
}
after() {
  local flag="$1"; shift
  while [ "$#" -gt 0 ]; do
    if [ "$1" = "$flag" ]; then printf '%s' "$2"; return 0; fi
    shift
  done
  exit 1
}
"#;

const TRANSACTION_TOOLS: &[(&str, &str)] = &[
    ("sync", r#"fail intent-sync; exec /usr/bin/sync "$@""#),
    ("id", r#"[ -e "$r/shadow" ]"#),
    (
        "stat",
        r#"if [ "$2" = '%u:%a:%h' ]; then echo 0:600:1; elif [ "$2" = '%u:%a' ]; then echo 0:700; else wc -c < "${@: -1}"; fi"#,
    ),
    (
        "getent",
        r#"if [ "$1" = shadow ]; then value="$(cat "$r/shadow")"; else value=x; fi
printf 'fixtureuser:%s:%s:%s::%s:/bin/bash\n' "$value" "$UID" "$(/usr/bin/id -g)" "$r/home/fixtureuser""#,
    ),
    (
        "useradd",
        r#"fail useradd-before
[ ! -e "$r/shadow" ] || exit 1
after -e "$@" > "$r/expiry"
expired
after -p "$@" > "$r/shadow"
printf '%s\n' "$@" > "$r/useradd"
fail useradd
fail crash"#,
    ),
    (
        "chpasswd",
        r#"expired
value="$(cat)"
value="${value#*:}"
fail password
printf '%s' "$value" > "$r/shadow""#,
    ),
    (
        "chage",
        r#"[ "$1" = -E ] && [ "$3" = fixtureuser ] || exit 1
value="$2"
if [ "$value" = -1 ]; then fail expiry-release; else fail expiry-retry; fi
if [ "$value" = -1 ] && [ "$(cat "$r/shadow")" != "$FIXTURE_HASH" ]; then exit 1; fi
printf '%s' "$value" > "$r/expiry""#,
    ),
    ("chgrp", "locked; fail key-repair"),
    ("chmod", "locked; fail key-mode"),
    ("shred", r#"rm -- "${@: -1}""#),
];

struct Transaction {
    _work: tempfile::TempDir,
    root: PathBuf,
}

impl Transaction {
    fn new() -> Self {
        let work = tempfile::tempdir().unwrap();
        let root = work.path().to_path_buf();
        for directory in [
            "etc",
            "state/device-private",
            "bin",
            "home/fixtureuser",
            "skel",
        ] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        let fixture = Self { _work: work, root };
        fixture.seed();
        let r = &fixture.root;
        fs::write(r.join("state/device-private/keypair.bin"), "key").unwrap();
        fs::write(r.join("skel/default"), "default").unwrap();
        fs::write(r.join("skel/seeded"), "skel").unwrap();
        fs::write(r.join("home/fixtureuser/seeded"), "installer").unwrap();
        for (name, body) in TRANSACTION_TOOLS {
            write_tool(
                &r.join("bin"),
                name,
                &(TRANSACTION_COMMON.to_owned() + body),
            );
        }
        attesting_helper(r);
        let display = |path: &str| r.join(path).display().to_string();
        let source = script(&[
            ("/etc/tilo", display("etc"), 10),
            ("/var/lib/tilo", display("state"), 17),
            ("/var/home", display("home"), 2),
            (
                r#""/home/$user""#,
                format!(r#""{}/home/$user""#, r.display()),
                2,
            ),
            (
                "seed_helper=/usr/libexec/tilo-firstboot-home-seeds",
                format!("seed_helper={}", display("attesting-seeds")),
                1,
            ),
            (HELPER_LINE, format!(r#"FILES_HELPER="{HELPER}""#), 1),
        ]);
        fs::write(r.join("firstboot"), source).unwrap();
        fixture
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn credential(&self) -> PathBuf {
        self.path("etc/install-user.conf")
    }

    /// The installer's seeds, as a fresh install leaves them.
    fn seed(&self) {
        for (name, content) in [
            ("etc/install-user.conf", format!("fixtureuser:{}\n", hash())),
            ("etc/install-user-full-name", "Fixture User\n".to_owned()),
        ] {
            fs::write(self.path(name), content).unwrap();
            fs::set_permissions(self.path(name), fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    fn boot_with(&self, fail: &str, terminate: bool) -> Output {
        let path = format!(
            "{}:{}",
            self.path("bin").display(),
            std::env::var("PATH").unwrap()
        );
        let output = Command::new("bash")
            .arg(self.path("firstboot"))
            .env("FIXTURE_ROOT", &self.root)
            .env("FIXTURE_HASH", hash())
            .env("FAIL_STAGE", fail)
            .env("TERMINATE", if terminate { "1" } else { "0" })
            .env("PATH", path)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!text.contains(&hash()), "the hash leaked: {text}");
        output
    }

    fn boot(&self, fail: &str) -> Output {
        self.boot_with(fail, false)
    }

    fn read(&self, relative: &str) -> String {
        read(&self.path(relative))
    }

    fn write(&self, relative: &str, content: &str) {
        fs::write(self.path(relative), content).unwrap();
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn every_failed_delivery_is_locked_and_retryable() {
    let t = Transaction::new();
    for stage in [
        "useradd",
        "crash",
        "seeds",
        "seeds-after",
        "key-repair",
        "key-mode",
        "password",
        "expiry-release",
    ] {
        let result = t.boot(stage);
        assert!(!result.status.success(), "{stage}");
        assert!(t.credential().exists(), "{stage}");
        assert!(t.path("etc/install-user-full-name").exists(), "{stage}");
        assert_eq!(t.read("expiry"), "1970-01-02", "{stage}");
        if stage == "expiry-release" {
            assert_eq!(t.read("shadow"), hash(), "{stage}");
        } else {
            assert!(t.read("shadow").starts_with('!'), "{stage}");
        }
        let created = t.read("useradd");
        let result = t.boot("");
        assert!(result.status.success(), "{stage}: {}", stderr(&result));
        assert_eq!(t.read("shadow"), hash(), "{stage}");
        assert_eq!(t.read("expiry"), "-1", "{stage}");
        assert_eq!(
            t.read("useradd"),
            created,
            "{stage}: the account is created once"
        );
        assert!(!t.credential().exists(), "{stage}");
        assert!(!t.path("etc/install-user.transaction").exists(), "{stage}");
        // Reset just this completed synthetic transaction for the next case.
        fs::remove_file(t.path("shadow")).unwrap();
        fs::remove_file(t.path("expiry")).unwrap();
        t.seed();
    }
}

#[test]
fn termination_before_release_is_retryable() {
    let t = Transaction::new();
    for stage in ["key-repair", "password", "expiry-release"] {
        let result = t.boot_with(stage, true);
        assert_eq!(result.status.signal(), Some(15), "{stage}: {result:?}");
        assert_eq!(t.read("expiry"), "1970-01-02", "{stage}");
        assert!(t.credential().exists(), "{stage}");
    }
    let result = t.boot("");
    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(t.read("expiry"), "-1");
}

#[test]
fn intent_before_account_creation_can_retry() {
    let t = Transaction::new();
    let intent = t.path("etc/install-user.transaction");
    fs::write(
        &intent,
        format!("fixtureuser:!tilo-firstboot-{}\n", "a".repeat(32)),
    )
    .unwrap();
    fs::set_permissions(&intent, fs::Permissions::from_mode(0o600)).unwrap();
    let result = t.boot("");
    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(t.read("shadow"), hash());
}

#[test]
fn retry_after_final_unlock_relocks_before_delivery() {
    let t = Transaction::new();
    assert!(!t.boot("key-repair").status.success());
    // Model interruption after expiry release but before seed cleanup.
    t.write("shadow", &hash());
    t.write("expiry", "-1");
    assert!(!t.boot("key-repair").status.success());
    assert!(t.read("shadow").starts_with('!'));
    assert!(t.credential().exists());
    assert_eq!(t.read("expiry"), "1970-01-02");
    let result = t.boot("");
    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(t.read("expiry"), "-1");
}

#[test]
fn retry_expiry_failure_does_not_provision() {
    let t = Transaction::new();
    assert!(!t.boot("key-repair").status.success());
    t.write("shadow", &hash());
    t.write("expiry", "-1");
    assert!(!t.boot("expiry-retry").status.success());
    assert_eq!(t.read("shadow"), hash());
    assert!(t.credential().exists());
    assert!(t.boot("").status.success());
}

#[test]
fn unrelated_existing_account_preserves_seed_and_shadow() {
    let t = Transaction::new();
    t.write("shadow", "!unrelated");
    t.write("expiry", "-1");
    assert!(!t.boot("").status.success());
    assert!(t.credential().exists());
    assert_eq!(t.read("shadow"), "!unrelated");
    assert!(!t.path("etc/install-user.transaction").exists());
    assert_eq!(t.read("expiry"), "-1");
}

#[test]
fn intent_does_not_adopt_wrong_shadow() {
    let t = Transaction::new();
    assert!(!t.boot("key-repair").status.success());
    t.write("shadow", "!unrelated");
    t.write("expiry", "-1");
    assert!(!t.boot("").status.success());
    assert_eq!(t.read("shadow"), "!unrelated");
    assert_eq!(t.read("expiry"), "-1");
    assert!(t.credential().exists());
}

#[test]
fn intent_is_durable_before_account_creation() {
    let t = Transaction::new();
    assert!(!t.boot("intent-sync").status.success());
    assert!(!t.path("shadow").exists());
    assert!(t.credential().exists());
    assert!(t.boot("").status.success());
}

#[test]
fn useradd_failure_before_creation_retries() {
    let t = Transaction::new();
    assert!(!t.boot("useradd-before").status.success());
    assert!(!t.path("shadow").exists());
    assert!(t.credential().exists());
    assert!(t.path("etc/install-user.transaction").exists());
    assert!(t.boot("").status.success());
}

#[test]
fn seeded_home_keeps_installer_content_and_gets_skel() {
    let t = Transaction::new();
    assert!(!t.boot("key-repair").status.success());
    let default = t.path("home/fixtureuser/default");
    let captured = fs::metadata(&default).unwrap();
    let receipt = t.path(&format!(
        "receipts/{}.json",
        rustix::process::getuid().as_raw()
    ));
    let proof = fs::read(&receipt).unwrap();
    let result = t.boot("");
    assert!(result.status.success(), "{}", stderr(&result));
    let repaired = fs::metadata(&default).unwrap();
    assert_eq!(
        (repaired.ctime(), repaired.ctime_nsec()),
        (captured.ctime(), captured.ctime_nsec())
    );
    assert_eq!(fs::read(&receipt).unwrap(), proof);
    assert_eq!(t.read("home/fixtureuser/seeded"), "installer");
    assert_eq!(t.read("home/fixtureuser/default"), "default");
    let args: Vec<String> = t.read("useradd").lines().map(str::to_owned).collect();
    assert!(args.iter().any(|a| a == "-M"), "{args:?}");
    assert!(args.iter().any(|a| a == "Fixture User"), "{args:?}");

    let tampered = Transaction::new();
    assert!(!tampered.boot("key-repair").status.success());
    tampered.write("home/fixtureuser/default", "changed after capture");
    let result = tampered.boot("");
    assert!(!result.status.success());
    assert!(stderr(&result).contains("home defaults could not be attested"));
    assert!(tampered.read("shadow").starts_with('!'));
    assert_eq!(tampered.read("expiry"), "1970-01-02");
    assert!(tampered.credential().exists());
}

#[test]
fn unseeded_home_is_repaired_after_account_committed() {
    let t = Transaction::new();
    fs::remove_dir_all(t.path("home/fixtureuser")).unwrap();
    assert!(!t.boot("useradd").status.success());
    assert!(!t.path("home/fixtureuser").exists());
    assert!(t.read("shadow").starts_with('!'));
    let result = t.boot("");
    assert!(result.status.success(), "{}", stderr(&result));
    assert!(t.read("useradd").lines().any(|a| a == "-M"));
    assert_eq!(t.read("home/fixtureuser/default"), "default");
    let home = fs::metadata(t.path("home/fixtureuser")).unwrap();
    assert_eq!(home.uid(), rustix::process::getuid().as_raw());
    assert_eq!(home.gid(), rustix::process::getgid().as_raw());
}

#[test]
fn home_symlink_cannot_unlock_or_modify_outside() {
    let t = Transaction::new();
    fs::remove_dir_all(t.path("home/fixtureuser")).unwrap();
    fs::create_dir(t.path("outside")).unwrap();
    symlink(t.path("outside"), t.path("home/fixtureuser")).unwrap();
    assert!(!t.boot("").status.success());
    assert!(t.read("shadow").starts_with('!'));
    assert_eq!(fs::read_dir(t.path("outside")).unwrap().count(), 0);
    assert!(t.credential().exists());
    assert_eq!(t.read("expiry"), "1970-01-02");
}

// ---- Credential transport --------------------------------------------------

/// Only the credential-file ownership view is mocked; the production parser,
/// account creation branches and cleanup run unchanged.
const TRANSPORT_TOOLS: &[(&str, &str)] = &[
    ("id", "exit 1"),
    (
        "stat",
        r#"if [ "$2" = '%u:%a:%h' ]; then echo 0:600:1; else wc -c < "${@: -1}"; fi"#,
    ),
    (
        "useradd",
        r#"printf '%s\n' "$@" > "$FIXTURE_ROOT/useradd.args""#,
    ),
    (
        "getent",
        r#"printf 'fixtureuser:x:%s:%s::%s:/bin/bash\n' "$UID" "$(/usr/bin/id -g)" "$FIXTURE_ROOT/home/fixtureuser""#,
    ),
    (
        "chpasswd",
        r#"printf '%s\n' "$@" > "$FIXTURE_ROOT/chpasswd.args"
cat > "$FIXTURE_ROOT/password-stdin"
exit "$FIXTURE_PASSWORD_FAILURE""#,
    ),
    (
        "chage",
        r#"[ "$*" = '-E -1 fixtureuser' ] || exit 1
echo released > "$FIXTURE_ROOT/expiry-release""#,
    ),
    ("shred", r#"rm -- "${@: -1}""#),
    ("chown", ":"),
    ("cp", ":"),
];

fn run_transport(root: &Path, full_name: bool, seeded_home: bool, fail_password: bool) -> Output {
    let credential = root.join("install-user.conf");
    fs::write(&credential, format!("fixtureuser:{}\n", hash())).unwrap();
    fs::set_permissions(&credential, fs::Permissions::from_mode(0o600)).unwrap();
    let name = root.join("full-name");
    if full_name {
        fs::write(&name, "Fixture User\n").unwrap();
        fs::set_permissions(&name, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fs::create_dir_all(root.join("home")).unwrap();
    if seeded_home {
        fs::create_dir(root.join("home/fixtureuser")).unwrap();
    }
    fs::create_dir(root.join("skel")).unwrap();
    let r = root.display();
    let source = script(&[
        (
            "CONF=/etc/tilo/install-user.conf",
            format!("CONF={}", credential.display()),
            1,
        ),
        (
            "FULL_NAME_CONF=/etc/tilo/install-user-full-name",
            format!("FULL_NAME_CONF={}", name.display()),
            1,
        ),
        (
            "TRANSACTION=/etc/tilo/install-user.transaction",
            format!("TRANSACTION={r}/install-user.transaction"),
            1,
        ),
        (
            "key=/var/lib/tilo/device-private/keypair.bin",
            format!("key={r}/absent-device-key"),
            1,
        ),
        (r#""/home/$user""#, format!(r#""{r}/home/$user""#), 2),
        (
            r#""/var/home/$user""#,
            format!(r#""{r}/var-home/$user""#),
            2,
        ),
        (HELPER_LINE, format!(r#"FILES_HELPER="{HELPER}""#), 1),
        (
            "seed_helper=/etc/tilo/firstboot-home-seeds",
            format!("seed_helper={r}/attesting-seeds"),
            1,
        ),
    ]);
    fs::write(root.join("firstboot"), source).unwrap();
    attesting_helper(root);
    let tools = root.join("bin");
    fs::create_dir(&tools).unwrap();
    for (tool, body) in TRANSPORT_TOOLS {
        write_tool(&tools, tool, body);
    }
    let output = Command::new("bash")
        .arg(root.join("firstboot"))
        .env(
            "PATH",
            format!("{}:{}", tools.display(), std::env::var("PATH").unwrap()),
        )
        .env("FIXTURE_ROOT", root)
        .env(
            "FIXTURE_PASSWORD_FAILURE",
            if fail_password { "1" } else { "0" },
        )
        .output()
        .unwrap();
    assert_transport(root, full_name, fail_password, &output);
    output
}

/// What every credential-transport run must show, whatever its outcome.
fn assert_transport(root: &Path, full_name: bool, fail_password: bool, output: &Output) {
    let arguments: Vec<String> = read(&root.join("useradd.args"))
        .lines()
        .map(str::to_owned)
        .collect();
    assert!(
        !arguments.contains(&hash()),
        "password hashes must not be process arguments"
    );
    let value = |flag: &str| {
        arguments
            .iter()
            .skip_while(|a| *a != flag)
            .nth(1)
            .unwrap()
            .clone()
    };
    let lock = value("-p");
    let token = lock.strip_prefix("!tilo-firstboot-").unwrap();
    assert!(
        token.len() == 32
            && token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "{lock}"
    );
    assert_eq!(value("-e"), "1970-01-02");
    assert_eq!(root.join("expiry-release").exists(), !fail_password);
    assert!(arguments.iter().any(|a| a == "-M"));
    assert!(!arguments.iter().any(|a| a == "-m"));
    assert_eq!(arguments.iter().any(|a| a == "-c"), full_name);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!text.contains(&hash()));
    assert_eq!(read(&root.join("chpasswd.args")), "--encrypted\n");
    assert_eq!(
        read(&root.join("password-stdin")),
        format!("fixtureuser:{}\n", hash())
    );
}

#[test]
fn hash_uses_stdin_for_every_account_creation_branch() {
    for full_name in [false, true] {
        for seeded_home in [false, true] {
            let work = tempfile::tempdir().unwrap();
            let output = run_transport(work.path(), full_name, seeded_home, false);
            assert!(
                output.status.success(),
                "{full_name} {seeded_home}: {}",
                stderr(&output)
            );
            assert!(!work.path().join("install-user.conf").exists());
        }
    }
}

#[test]
fn password_install_failure_never_creates_an_unlocked_account() {
    let work = tempfile::tempdir().unwrap();
    let output = run_transport(work.path(), false, false, true);
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("created account"));
}
