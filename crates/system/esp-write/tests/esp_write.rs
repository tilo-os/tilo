//! tilo-esp-write applies only its caller's allowed ESP changes, atomically,
//! and never follows a symlink; tilo-deployment's plans are what it accepts.
//!
//! Every case runs the built helper as its unit does, against a fixture ESP.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    reason = "fixture helpers build fixed-size PE images and fail the test on any I/O error"
)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const HELPER: &str = env!("CARGO_BIN_EXE_tilo-esp-write");

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn deployment() -> String {
    fs::read_to_string(repo().join("os/usr/libexec/tilo-deployment")).unwrap()
}

fn sha256(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn uid() -> String {
    let out = Command::new("id").arg("-u").output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// The text of `text` from `start` up to (not including) `end`.
fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let from = text.find(start).unwrap();
    let to = text.find(end).unwrap();
    &text[from..to]
}

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    esp: PathBuf,
    plans: PathBuf,
    plan_dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let esp = root.join("esp");
        fs::create_dir_all(esp.join("loader/entries")).unwrap();
        fs::create_dir_all(esp.join("EFI/Linux")).unwrap();
        fs::write(
            esp.join("loader/entries/bootc_fedora-44-1.conf"),
            "title a\n",
        )
        .unwrap();
        let plans = root.join("plans");
        let plan_dir = plans.join("boot-entries");
        fs::create_dir_all(&plan_dir).unwrap();
        fs::set_permissions(&plan_dir, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            _tmp: tmp,
            root,
            esp,
            plans,
            plan_dir,
        }
    }

    fn command(&self, program: &str, extra: &[(&str, &str)]) -> Command {
        let mut cmd = Command::new(program);
        cmd.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("TILO_ESP", &self.esp)
            .env("TILO_ESP_PLAN_ROOT", &self.plans)
            .env("TILO_ESP_PLAN_OWNER", uid())
            .env("TILO_ESP_WRITE_NO_REMOUNT", "1");
        for (key, value) in extra {
            cmd.env(key, value);
        }
        cmd
    }

    fn stage(&self, name: &str, data: &[u8]) -> String {
        fs::write(self.plan_dir.join(name), data).unwrap();
        sha256(data)
    }

    fn run_plan(&self, ops: &Value, caller: &str, extra: &[(&str, &str)]) -> Output {
        fs::write(
            self.plan_dir.join("plan.json"),
            json!({ "ops": ops }).to_string(),
        )
        .unwrap();
        self.command(HELPER, extra).arg(caller).output().unwrap()
    }

    fn run(&self, ops: &Value) -> Output {
        self.run_plan(ops, "boot-entries", &[])
    }

    fn plan_dir_entries(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.plan_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    /// Minimal PE fixture read by the shipped UKI parser, rather than a fake
    /// parser that could conceal a sealed-command-line mismatch.
    fn make_uki(&self, name: &str, digest: &str) {
        let mut payload =
            format!("root=tilo-auto composefs={digest} tilo.mode=normal").into_bytes();
        payload.push(0);
        let mut image = vec![0u8; 512 + payload.len()];
        image[0x3c..0x40].copy_from_slice(&128u32.to_le_bytes());
        image[128..132].copy_from_slice(b"PE\0\0");
        image[134..136].copy_from_slice(&1u16.to_le_bytes());
        image[152..160].copy_from_slice(b".cmdline");
        let len = u32::try_from(payload.len()).unwrap();
        for (i, value) in [len, 4096, len, 512].into_iter().enumerate() {
            let at = 160 + 4 * i;
            image[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        image[512..].copy_from_slice(&payload);
        let directory = self.esp.join("EFI/Linux/bootc");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(name), image).unwrap();
    }

    fn bootc_entry(&self, directory: &str, name: &str, uki: &str) {
        let path = self.esp.join("loader").join(directory);
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join(name),
            format!("title Tilo\nuki /EFI/Linux/bootc/{uki}\n"),
        )
        .unwrap();
    }

    /// tilo-deployment's `entries` pass, with this helper as its writer.
    fn run_entries(&self, source: Option<&str>, fail_writer: bool) -> Output {
        let text = deployment();
        let helpers = between(&text, "esp_path() {", "# --- listing");
        let counters = between(
            &text,
            "cmd_entries() {",
            "# Derive the repair-mode boot entry",
        );
        let script = format!(
            "{helpers}{counters}
id() {{ printf '0\\n'; }}
findmnt() {{ printf '%s\\n' \"$FIXTURE_ROOT_SOURCE\"; }}
bootctl() {{ printf 'Current Entry: bootc_fedora-44-1.conf\\n'; }}
die() {{ echo \"$*\" >&2; exit 1; }}
log() {{ echo \"$*\"; }}
cmd_repair_entry() {{ :; }}
cmd_entries
"
        );
        let writer = if fail_writer {
            "false".to_owned()
        } else {
            format!("{HELPER} boot-entries")
        };
        let root_source =
            source.map_or_else(|| format!("composefs:{}", "a".repeat(128)), str::to_owned);
        let uki_cmdline = repo().join("os/dev/uki-cmdline.py");
        let plan_dir = self.plan_dir.display().to_string();
        self.command(
            "bash",
            &[
                ("TILO_ESP_PLAN_DIR", &plan_dir),
                ("TILO_ESP_WRITE_CMD", &writer),
                ("UKI_CMDLINE", uki_cmdline.to_str().unwrap()),
                ("BOOT_TRIES", "3"),
                ("FIXTURE_ROOT_SOURCE", &root_source),
            ],
        )
        .arg("-c")
        .arg(script)
        .output()
        .unwrap()
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[track_caller]
fn assert_ok(out: &Output) {
    assert_eq!(out.status.code(), Some(0), "{}", stderr(out));
}

#[track_caller]
fn assert_refused(out: &Output, text: &str) {
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        stderr(out)
    );
    assert!(
        stderr(out).contains(text),
        "expected {text:?} in {:?}",
        stderr(out)
    );
}

#[test]
fn applies_counters_and_the_repair_entry() {
    let f = Fixture::new();
    let digest = f.stage("repair.efi", b"MZ repair image");
    let out = f.run(&json!([
        {"op": "mkdir", "path": "EFI/tilo"},
        {"op": "rename", "src": "loader/entries/bootc_fedora-44-1.conf",
         "dst": "loader/entries/bootc_fedora-44-1+3.conf"},
        {"op": "install", "file": "repair.efi", "sha256": digest,
         "dst": "EFI/Linux/tilo-repair.efi"},
        {"op": "remove", "path": "EFI/tilo/repair.efi"},
    ]));
    assert_ok(&out);
    assert!(
        f.esp
            .join("loader/entries/bootc_fedora-44-1+3.conf")
            .is_file()
    );
    assert!(!f.esp.join("loader/entries/bootc_fedora-44-1.conf").exists());
    assert_eq!(
        fs::read(f.esp.join("EFI/Linux/tilo-repair.efi")).unwrap(),
        b"MZ repair image"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("caller=boot-entries"), "{stdout}");
    // The journal line is the operation as it always read: sorted keys.
    assert!(
        stdout
            .contains(r#"tilo-esp-write: caller=boot-entries {"op": "mkdir", "path": "EFI/tilo"}"#),
        "{stdout}"
    );
    // The plan and its staged files are gone once applied.
    assert_eq!(f.plan_dir_entries(), Vec::<String>::new());
}

#[test]
fn staged_counters_stay_in_the_same_directory() {
    let f = Fixture::new();
    let staged = f.esp.join("loader/entries.staged");
    fs::create_dir(&staged).unwrap();
    let source = staged.join("bootc_fedora-44-1.conf");
    fs::write(&source, "incoming\n").unwrap();
    for (src, dst) in [
        (
            "loader/entries.staged/bootc_fedora-44-1.conf",
            "loader/entries/bootc_fedora-44-1+3.conf",
        ),
        (
            "loader/entries/bootc_fedora-44-1.conf",
            "loader/entries.staged/bootc_fedora-44-1+3.conf",
        ),
    ] {
        assert_refused(
            &f.run(&json!([{"op": "rename", "src": src, "dst": dst}])),
            "may not rename",
        );
    }
    let out = f.run(&json!([{"op": "rename",
        "src": "loader/entries.staged/bootc_fedora-44-1.conf",
        "dst": "loader/entries.staged/bootc_fedora-44-1+3.conf"}]));
    assert_ok(&out);
    assert_eq!(
        fs::read_to_string(staged.join("bootc_fedora-44-1+3.conf")).unwrap(),
        "incoming\n"
    );
    assert!(!source.exists());
}

#[test]
fn staging_counts_incoming_and_preserves_booted_identity() {
    let f = Fixture::new();
    f.make_uki("old.efi", &"a".repeat(128));
    f.make_uki("new.efi", &"b".repeat(128));
    f.bootc_entry("entries", "bootc_fedora-44-1.conf", "old.efi");
    f.bootc_entry("entries.staged", "bootc_fedora-44-0.conf", "old.efi");
    f.bootc_entry("entries.staged", "bootc_fedora-44-1.conf", "new.efi");
    let out = f.run_entries(None, false);
    assert_ok(&out);
    assert!(f.esp.join("loader/entries/bootc_fedora-44-1.conf").exists());
    assert!(
        f.esp
            .join("loader/entries.staged/bootc_fedora-44-0.conf")
            .exists()
    );
    let counted = f.esp.join("loader/entries.staged/bootc_fedora-44-1+3.conf");
    assert!(counted.exists());
    assert!(!counted.with_file_name("bootc_fedora-44-1.conf").exists());
    // A repeated maintenance pass must not reset tries already spent.
    let spent = counted.with_file_name("bootc_fedora-44-1+2-1.conf");
    fs::rename(&counted, &spent).unwrap();
    assert_ok(&f.run_entries(None, false));
    assert!(spent.exists());
}

#[test]
fn finalized_filename_collision_does_not_count_old_fallback() {
    let f = Fixture::new();
    f.make_uki("old.efi", &"a".repeat(128));
    f.make_uki("new.efi", &"b".repeat(128));
    f.bootc_entry("entries", "bootc_fedora-44-0.conf", "old.efi");
    f.bootc_entry("entries", "bootc_fedora-44-1.conf", "new.efi");
    assert_ok(&f.run_entries(None, false));
    assert!(f.esp.join("loader/entries/bootc_fedora-44-0.conf").exists());
    assert!(
        f.esp
            .join("loader/entries/bootc_fedora-44-1+3.conf")
            .exists()
    );
}

#[test]
fn unobserved_identity_cannot_change_the_esp() {
    let f = Fixture::new();
    f.make_uki("new.efi", &"b".repeat(128));
    f.bootc_entry("entries", "bootc_fedora-44-1.conf", "new.efi");
    let out = f.run_entries(None, false);
    assert_ne!(out.status.code(), Some(0));
    assert!(
        stderr(&out).contains("no retained entry matches"),
        "{}",
        stderr(&out)
    );
    assert!(f.esp.join("loader/entries/bootc_fedora-44-1.conf").exists());
    assert!(
        !f.esp
            .join("loader/entries/bootc_fedora-44-1+3.conf")
            .exists()
    );
    fs::write(f.esp.join("EFI/Linux/bootc/new.efi"), b"unreadable UKI").unwrap();
    let out = f.run_entries(None, false);
    assert_ne!(out.status.code(), Some(0));
    assert!(
        stderr(&out).contains("cannot read entry identity"),
        "{}",
        stderr(&out)
    );
    assert!(f.esp.join("loader/entries/bootc_fedora-44-1.conf").exists());
}

#[test]
fn counter_writer_failure_is_fatal() {
    let f = Fixture::new();
    f.make_uki("old.efi", &"a".repeat(128));
    f.make_uki("new.efi", &"b".repeat(128));
    f.bootc_entry("entries", "bootc_fedora-44-1.conf", "old.efi");
    f.bootc_entry("entries.staged", "bootc_fedora-44-1.conf", "new.efi");
    let out = f.run_entries(None, true);
    assert_ne!(out.status.code(), Some(0));
    assert!(
        stderr(&out).contains("could not apply the boot counters"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn non_composefs_keeps_bootloader_selected_entry() {
    let f = Fixture::new();
    f.bootc_entry("entries", "bootc_fedora-43-1.conf", "other.efi");
    assert_ok(&f.run_entries(Some("/dev/mapper/root"), false));
    assert!(f.esp.join("loader/entries/bootc_fedora-44-1.conf").exists());
    assert!(
        f.esp
            .join("loader/entries/bootc_fedora-43-1+3.conf")
            .exists()
    );
}

#[test]
fn only_the_callers_operations() {
    let f = Fixture::new();
    let digest = sha256(b"x");
    for op in [
        json!({"op": "install", "file": "x.efi", "sha256": digest, "dst": "EFI/Linux/bootc/x.efi"}),
        json!({"op": "rename", "src": "loader/entries/bootc_fedora-44-1.conf",
               "dst": "loader/entries/other.conf"}),
        json!({"op": "remove", "path": "loader/entries/bootc_fedora-44-1.conf"}),
        json!({"op": "mkdir", "path": "EFI/other"}),
        json!({"op": "chmod", "path": "EFI/Linux"}),
    ] {
        f.stage("x.efi", b"x");
        let out = f.run(&json!([op.clone()]));
        assert_refused(&out, "refused");
        // A refused plan is removed with its staged files.
        assert_eq!(f.plan_dir_entries(), Vec::<String>::new(), "{op}");
    }
    assert!(f.esp.join("loader/entries/bootc_fedora-44-1.conf").exists());
    assert!(!f.esp.join("EFI/other").exists());
    assert_refused(
        &f.run(&json!([{"op": "chmod", "path": "EFI/Linux"}])),
        "unknown operation 'chmod'",
    );
    let out = f.run_plan(&json!([]), "someone-else", &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("usage: tilo-esp-write {boot-entries|secureboot-sign}"));
    let out = f.command(HELPER, &[]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn the_signer_only_replaces_managed_artefacts() {
    let f = Fixture::new();
    let signer = f.plans.join("secureboot-sign");
    fs::create_dir(&signer).unwrap();
    fs::set_permissions(&signer, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(f.esp.join("EFI/Linux/bootc")).unwrap();
    fs::write(f.esp.join("EFI/Linux/bootc/uki.efi"), b"MZ unsigned").unwrap();

    let sign = |dst: &str| {
        let data = b"MZ signed";
        fs::write(signer.join("signed.efi"), data).unwrap();
        let plan = json!({"ops": [{"op": "install", "file": "signed.efi",
                                   "sha256": sha256(data), "dst": dst}]});
        fs::write(signer.join("plan.json"), plan.to_string()).unwrap();
        f.command(HELPER, &[])
            .arg("secureboot-sign")
            .output()
            .unwrap()
    };

    assert_ok(&sign("EFI/Linux/bootc/uki.efi"));
    assert_eq!(
        fs::read(f.esp.join("EFI/Linux/bootc/uki.efi")).unwrap(),
        b"MZ signed"
    );
    // A new .efi would be a new boot menu entry: the signer may not add one.
    assert_refused(&sign("EFI/Linux/bootc/new.efi"), "may only replace");
    assert!(!f.esp.join("EFI/Linux/bootc/new.efi").exists());
    // Nor touch loader entries, or anything but an install.
    assert_refused(
        &sign("loader/entries/bootc_fedora-44-1.conf"),
        "may not install",
    );
}

#[test]
fn the_signer_finds_its_esp_as_tilo_secureboot_does() {
    // Without TILO_ESP the helper looks where its caller looks; a fixture has
    // neither /boot/efi/EFI nor, on a build host, a writable one.
    let f = Fixture::new();
    let signer = f.plans.join("secureboot-sign");
    fs::create_dir(&signer).unwrap();
    fs::set_permissions(&signer, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(signer.join("plan.json"), r#"{"ops": []}"#).unwrap();
    let out = f
        .command(HELPER, &[("TILO_ESP", "")])
        .arg("secureboot-sign")
        .output()
        .unwrap();
    if Path::new("/boot/efi/EFI").is_dir() || Path::new("/boot/EFI").is_dir() {
        // A real ESP is read-only to the test, or (as root) writable: either
        // way it must not be refused for being absent.
        assert!(!stderr(&out).contains("no ESP at"), "{}", stderr(&out));
    } else {
        assert_refused(&out, "no ESP at /boot or /boot/efi");
    }
    assert_eq!(fs::read_dir(&signer).unwrap().count(), 0);
}

#[test]
fn traversal_is_refused() {
    let f = Fixture::new();
    for path in [
        "../outside",
        "EFI/../../outside",
        "/boot/EFI/tilo",
        "EFI//tilo",
        "EFI/./tilo",
    ] {
        assert_refused(&f.run(&json!([{"op": "mkdir", "path": path}])), "refused");
    }
    assert!(!f.root.join("outside").exists());
}

#[test]
fn symlinks_on_the_esp_are_refused() {
    let f = Fixture::new();
    let outside = f.root.join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, f.esp.join("EFI/tilo")).unwrap();
    let digest = f.stage("repair.source", b"bootc/x.efi\n");
    let out = f.run(
        &json!([{"op": "install", "file": "repair.source", "sha256": digest,
                            "dst": "EFI/tilo/repair.source"}]),
    );
    assert_refused(&out, "symlink");
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    // A symlinked destination file is refused too, not written through.
    symlink(
        outside.join("target"),
        f.esp.join("EFI/Linux/tilo-repair.efi"),
    )
    .unwrap();
    let digest = f.stage("repair.efi", b"MZ");
    let out = f.run(
        &json!([{"op": "install", "file": "repair.efi", "sha256": digest,
                            "dst": "EFI/Linux/tilo-repair.efi"}]),
    );
    assert_refused(&out, "symlink");
    assert!(!outside.join("target").exists());
    // Nor is a symlinked rename source or remove target followed.
    symlink(
        outside.join("entry"),
        f.esp.join("loader/entries/bootc_link.conf"),
    )
    .unwrap();
    let out = f.run(
        &json!([{"op": "rename", "src": "loader/entries/bootc_link.conf",
                            "dst": "loader/entries/bootc_link+3.conf"}]),
    );
    assert_refused(&out, "symlink");
    let out = f.run(&json!([{"op": "remove", "path": "EFI/Linux/tilo-repair.efi"}]));
    assert_refused(&out, "symlink");
    assert!(fs::symlink_metadata(f.esp.join("EFI/Linux/tilo-repair.efi")).is_ok());
}

#[test]
fn staged_files_are_verified() {
    let f = Fixture::new();
    f.stage("repair.efi", b"what was planned");
    let out = f.run(&json!([{"op": "install", "file": "repair.efi",
                            "sha256": sha256(b"something else"),
                            "dst": "EFI/Linux/tilo-repair.efi"}]));
    assert_refused(&out, "SHA-256");
    fs::write(f.root.join("elsewhere"), b"x").unwrap();
    symlink(f.root.join("elsewhere"), f.plan_dir.join("link.efi")).unwrap();
    let out = f.run(
        &json!([{"op": "install", "file": "link.efi", "sha256": sha256(b"x"),
                            "dst": "EFI/Linux/tilo-repair.efi"}]),
    );
    assert_ne!(out.status.code(), Some(0));
    assert!(!f.esp.join("EFI/Linux/tilo-repair.efi").exists());
    let out = f.run(
        &json!([{"op": "install", "file": "../elsewhere", "sha256": sha256(b"x"),
                            "dst": "EFI/Linux/tilo-repair.efi"}]),
    );
    assert_refused(&out, "not a staged file name");
    assert!(!f.esp.join("EFI/Linux/tilo-repair.efi").exists());
}

#[test]
fn the_plan_directory_must_be_the_callers_alone() {
    let f = Fixture::new();
    fs::set_permissions(&f.plan_dir, fs::Permissions::from_mode(0o755)).unwrap();
    let digest = f.stage("repair.efi", b"MZ");
    let out = f.run(
        &json!([{"op": "install", "file": "repair.efi", "sha256": digest,
                            "dst": "EFI/Linux/tilo-repair.efi"}]),
    );
    assert_refused(&out, "0700");
    assert!(!f.esp.join("EFI/Linux/tilo-repair.efi").exists());
    // Someone else's directory is not read, nor cleared.
    assert_eq!(f.plan_dir_entries(), ["plan.json", "repair.efi"]);
    fs::set_permissions(&f.plan_dir, fs::Permissions::from_mode(0o700)).unwrap();
    let out = f.run_plan(
        &json!([]),
        "boot-entries",
        &[("TILO_ESP_PLAN_OWNER", "4242")],
    );
    assert_refused(&out, "must be root's alone (0700)");
}

#[test]
fn a_plan_without_an_operation_list_is_refused() {
    let f = Fixture::new();
    fs::write(f.plan_dir.join("plan.json"), r#"{"ops": {}}"#).unwrap();
    let out = f.command(HELPER, &[]).arg("boot-entries").output().unwrap();
    assert_refused(&out, "plan.json has no operation list");
    assert_eq!(f.plan_dir_entries(), Vec::<String>::new());
}

#[test]
fn an_interruption_between_operations_leaves_whole_files() {
    let f = Fixture::new();
    fs::create_dir(f.esp.join("EFI/tilo")).unwrap();
    fs::write(f.esp.join("EFI/tilo/repair.source"), "old\n").unwrap();
    let new_efi = f.stage("repair.efi", b"MZ new");
    let new_src = f.stage("repair.source", b"new\n");
    let out = f.run_plan(
        &json!([
            {"op": "install", "file": "repair.efi", "sha256": new_efi, "dst": "EFI/Linux/tilo-repair.efi"},
            {"op": "install", "file": "repair.source", "sha256": new_src, "dst": "EFI/tilo/repair.source"},
        ]),
        "boot-entries",
        &[("TILO_ESP_WRITE_CRASH_AFTER", "1")],
    );
    assert_eq!(out.status.code(), Some(99), "{}", stderr(&out));
    assert_eq!(
        fs::read(f.esp.join("EFI/Linux/tilo-repair.efi")).unwrap(),
        b"MZ new"
    );
    assert_eq!(
        fs::read_to_string(f.esp.join("EFI/tilo/repair.source")).unwrap(),
        "old\n"
    );
    // The interrupted plan stays, as a crash would leave it.
    assert_eq!(
        f.plan_dir_entries(),
        ["plan.json", "repair.efi", "repair.source"]
    );
    let mut leftovers = Vec::new();
    for dir in ["EFI/Linux", "EFI/tilo", "loader/entries"] {
        for entry in fs::read_dir(f.esp.join(dir)).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            if name.starts_with(".tilo-esp-write.") {
                leftovers.push(name);
            }
        }
    }
    assert_eq!(leftovers, Vec::<String>::new());
    // No partial *.efi in EFI/Linux, where it would be a boot entry.
    let mut names: Vec<String> = fs::read_dir(f.esp.join("EFI/Linux"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["tilo-repair.efi"]);
}

#[test]
fn leftovers_of_an_earlier_interruption_are_removed() {
    let f = Fixture::new();
    fs::write(f.esp.join("EFI/Linux/.tilo-esp-write.1.tmp"), b"partial").unwrap();
    let digest = f.stage("repair.efi", b"MZ");
    assert_ok(&f.run(
        &json!([{"op": "install", "file": "repair.efi", "sha256": digest,
                             "dst": "EFI/Linux/tilo-repair.efi"}]),
    ));
    let mut names: Vec<String> = fs::read_dir(f.esp.join("EFI/Linux"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["tilo-repair.efi"]);
}

#[test]
fn removing_what_is_not_there_succeeds() {
    let f = Fixture::new();
    assert_ok(&f.run(&json!([
        {"op": "remove", "path": "EFI/tilo/repair.efi"},
        {"op": "remove", "path": "EFI/Linux/tilo-repair.efi"},
    ])));
    let out = f.run(
        &json!([{"op": "rename", "src": "loader/entries/bootc_gone.conf",
                            "dst": "loader/entries/bootc_gone+3.conf"}]),
    );
    assert_refused(&out, "no loader/entries/bootc_gone.conf to rename");
}

#[test]
fn tilo_deployment_plans_are_accepted() {
    let f = Fixture::new();
    let text = deployment();
    let helpers = between(&text, "ESP_PLAN_DIR=", "# --- listing");
    let script = format!(
        "{helpers}
die() {{ echo \"$*\" >&2; exit 1; }}
esp_plan_reset
esp_plan_op rename src loader/entries/bootc_fedora-44-1.conf dst loader/entries/bootc_fedora-44-1+3.conf
printf 'bootc/x.efi\\n' > \"$ESP_PLAN_DIR/repair.source\"
esp_plan_op mkdir path EFI/tilo
esp_plan_install repair.source EFI/tilo/repair.source
esp_plan_commit
"
    );
    let plan_dir = f.plan_dir.display().to_string();
    let writer = format!("{HELPER} boot-entries");
    let out = f
        .command(
            "bash",
            &[
                ("TILO_ESP_PLAN_DIR", &plan_dir),
                ("TILO_ESP_WRITE_CMD", &writer),
            ],
        )
        .arg("-c")
        .arg(script)
        .output()
        .unwrap();
    assert_ok(&out);
    assert!(
        f.esp
            .join("loader/entries/bootc_fedora-44-1+3.conf")
            .is_file()
    );
    assert_eq!(
        fs::read_to_string(f.esp.join("EFI/tilo/repair.source")).unwrap(),
        "bootc/x.efi\n"
    );
    assert!(!text.contains("mv -f \"$f\""));
    assert!(!text.contains("> \"$stamp\""));
}
