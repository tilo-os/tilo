#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "a failed fixture call fails the test; fixtures are small known values"
)]

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use tilo_test_env::SpawnUnderEnvLock as _;

use serde_json::json;

fn helper(args: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(tilo_test_env::package_binary!("tilo-staging-handoff"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn_locked()
        .unwrap();
    // A command refusing its arguments exits before it reads stdin.
    if let Err(error) = child.stdin.take().unwrap().write_all(stdin) {
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    }
    child.wait_with_output().unwrap()
}

fn stdout(output: &Output) -> &str {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::str::from_utf8(&output.stdout).unwrap()
}

fn refused(output: &Output) -> String {
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn source(relative: &str) -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join(relative),
    )
    .unwrap()
}

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

fn geometry(values: &[u64], boot: bool, existing: &[&str]) -> Output {
    let mut args: Vec<String> = vec!["guided-geometry".into()];
    for (index, value) in values.iter().enumerate() {
        args.push(if !boot && (index == 6 || index == 7) {
            String::new()
        } else {
            value.to_string()
        });
    }
    args.extend(existing.iter().map(|item| (*item).to_owned()));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    helper(&args, b"")
}

const TILO_ROOT: &str = "7A2E8D75-3F01-4E5A-B2C9-51F6ED90B8AB";

fn sfdisk() -> Vec<u8> {
    json!({"partitiontable": {"label": "gpt", "id": "11111111-2222-4333-8444-555555555555",
        "device": "/dev/loop7", "unit": "sectors", "firstlba": 2048, "lastlba": 83_886_046,
        "sectorsize": 512, "partitions": [
            {"node": "/dev/loop7p1", "start": 2048, "size": 8192,
             "type": "0FC63DAF-8483-4772-8E79-3D69D8477DE4",
             "uuid": "00000000-0000-4000-8000-000000000001", "name": "existing"},
            {"node": "/dev/loop7p12", "start": 20_971_520, "size": 62_914_560,
             "type": TILO_ROOT, "uuid": "20000000-0000-4000-8000-00000000000A", "name": "Tilo root"}]}})
    .to_string()
    .into_bytes()
}

fn record(path: &str, bytes: &[u8]) -> Vec<u8> {
    [
        path.as_bytes(),
        b"\n",
        bytes.len().to_string().as_bytes(),
        b"\n",
        bytes,
    ]
    .concat()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one journey through one command reads as one flow"
)]
fn the_backends_helper_commands() {
    {
        for good in ["", "Ada's laptop", &"x".repeat(64)] {
            stdout(&helper(&["check-device-label", good], b""));
        }
        for bad in ["line\nbreak", &"x".repeat(65)] {
            assert_eq!(
                refused(&helper(&["check-device-label", bad], b"")),
                "Invalid device label\n"
            );
        }
        assert_eq!(helper(&["check-device-label"], b"").status.code(), Some(2));
    }

    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine-info");
        let path_text = path.to_str().unwrap();
        stdout(&helper(&["write-machine-info", path_text, "Tower"], b""));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "PRETTY_HOSTNAME=\"Tower\"\n"
        );
        fs::write(
            &path,
            "CHASSIS=laptop\n PRETTY_HOSTNAME=\"Old\"\r\nDEPLOYMENT=production",
        )
        .unwrap();
        stdout(&helper(
            &["write-machine-info", path_text, "Ada's \"$x\" `y` \\"],
            b"",
        ));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "CHASSIS=laptop\nDEPLOYMENT=production\n\
         PRETTY_HOSTNAME=\"Ada's \\\"\\$x\\\" \\`y\\` \\\\\"\n"
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no temporary left"
        );

        let target = dir.path().join("elsewhere");
        fs::write(&target, "PRETTY_HOSTNAME=\"Kept\"\n").unwrap();
        fs::remove_file(&path).unwrap();
        symlink(&target, &path).unwrap();
        assert_eq!(
            refused(&helper(&["write-machine-info", path_text, "Tower"], b"")),
            "machine-info is a symbolic link\n"
        );
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "PRETTY_HOSTNAME=\"Kept\"\n"
        );
    }

    {
        let disk = 100 * GIB;
        let start = 10 * GIB;
        let free = 60 * GIB;
        let with_boot = [
            disk,
            512,
            start,
            free,
            start,
            2 * GIB,
            start + 2 * GIB,
            GIB,
            start + 3 * GIB,
            free - 3 * GIB,
        ];
        let existing = format!("{}:{}:{}", "a".repeat(8), MIB, 4 * MIB);
        stdout(&geometry(&with_boot, true, &[&existing]));
        let mut without_boot = with_boot;
        without_boot[8] = start + 2 * GIB;
        without_boot[9] = free - 2 * GIB;
        stdout(&geometry(&without_boot, false, &[]));

        let cases: [(usize, u64, &str); 3] = [
            (2, start + 1, "MiB aligned"),
            (5, 3 * GIB, "dedicated 2 GiB EFI"),
            (9, 20 * GIB, "root must fill"),
        ];
        for (index, value, message) in cases {
            let mut values = with_boot;
            values[index] = value;
            let error = refused(&geometry(&values, true, &[]));
            assert!(
                error.starts_with("guided allocation: ") && error.contains(message),
                "{index}: {error}"
            );
        }
        let overlapping = format!("x:{}:{}", start - MIB, 2 * MIB);
        assert!(
            refused(&geometry(&with_boot, true, &[&overlapping])).contains("overlaps an existing")
        );
        assert!(refused(&geometry(&with_boot, true, &["x:1"])).contains("map is invalid"));
        let mut args = vec!["guided-geometry".to_owned(), "1e3".to_owned()];
        args.extend(with_boot[1..].iter().map(u64::to_string));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        assert!(refused(&helper(&args, b"")).starts_with("guided allocation: "));
    }

    {
        assert_eq!(
            stdout(&helper(&["gpt", "map"], &sfdisk())),
            "00000000-0000-4000-8000-000000000001:1048576:4194304\n\
         20000000-0000-4000-8000-00000000000a:10737418240:32212254720\n"
        );
        assert_eq!(stdout(&helper(&["gpt", "tilo-roots"], &sfdisk())), "1\n");
        let root = "20000000-0000-4000-8000-00000000000a";
        assert_eq!(
            stdout(&helper(&["gpt", "node", root], &sfdisk())),
            "/dev/loop7p12\n"
        );
        for query in ["node", "type"] {
            let missing = helper(
                &["gpt", query, "ffffffff-0000-4000-8000-000000000000"],
                &sfdisk(),
            );
            assert_eq!(missing.status.code(), Some(1), "{query}");
            assert!(
                missing.stdout.is_empty() && missing.stderr.is_empty(),
                "{query}"
            );
        }
        assert!(refused(&helper(&["gpt", "map"], b"not json")).contains("malformed"));
        assert_eq!(helper(&["gpt", "nodes"], &sfdisk()).status.code(), Some(1));
    }

    {
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("payload");
        let unpack = |bytes: &[u8], name: &str| {
            fs::write(&payload, bytes).unwrap();
            let destination = dir.path().join(name);
            fs::create_dir(&destination).unwrap();
            let output = helper(
                &[
                    "unpack-setup-handoff",
                    payload.to_str().unwrap(),
                    destination.to_str().unwrap(),
                ],
                b"",
            );
            (output, destination)
        };
        let (output, destination) = unpack(
            &[
                record("etc/marker", b"wifi"),
                record(
                    "./var/lib/tilo/device-private/keypair.bin",
                    &[0, 1, 10, 255],
                ),
                record("etc/empty", b""),
            ]
            .concat(),
            "good",
        );
        stdout(&output);
        assert_eq!(fs::read(destination.join("etc/marker")).unwrap(), b"wifi");
        assert_eq!(
            fs::read(destination.join("var/lib/tilo/device-private/keypair.bin")).unwrap(),
            [0, 1, 10, 255]
        );
        assert_eq!(fs::read(destination.join("etc/empty")).unwrap(), b"");

        for (name, bytes) in [
            ("parent", record("../escape", b"x")),
            ("absolute", record("/etc/passwd", b"x")),
            ("truncated", b"etc/marker\n10\nwifi".to_vec()),
        ] {
            let (output, _) = unpack(&bytes, name);
            assert!(refused(&output).contains("malformed"), "{name}");
        }
        assert!(!dir.path().join("escape").exists());
    }

    {
        let guided = source("os/usr/libexec/tilo-guided-partition");
        let extract = source("os/usr/libexec/tilo-installer-profile-extract");
        let backend = source("os/usr/libexec/tilo-install");
        assert!(!guided.contains("python"), "tilo-guided-partition");
        assert!(
            !extract.contains("python"),
            "tilo-installer-profile-extract"
        );
        for query in [
            "\"$STAGING_HANDOFF\" guided-geometry",
            "| \"$STAGING_HANDOFF\" gpt map",
            "| \"$STAGING_HANDOFF\" gpt tilo-roots)",
            "| \"$STAGING_HANDOFF\" gpt number-node \"${new_uuid,,}\")",
            "| \"$STAGING_HANDOFF\" gpt node \"${1,,}\"",
            "| \"$STAGING_HANDOFF\" gpt type \"${1,,}\"",
        ] {
            assert!(guided.contains(query), "{query}");
        }
        assert!(extract.contains(
            "\"$staging_handoff\" unpack-setup-handoff \"$payload\" \"$unpack\" || continue"
        ));
        assert!(backend.contains("\"$STAGING_HANDOFF\" check-device-label \"$DEVICE_LABEL\""));
        assert!(backend.contains(
            "\"$STAGING_HANDOFF\" write-machine-info \"$DEPLOY_ETC/machine-info\" \"$DEVICE_LABEL\""
        ));
        assert!(!backend.contains("python"), "tilo-install");
        assert!(
            backend
                .contains("  exec /usr/libexec/tilo-vm-install-proof code-refresh-probe \"$0\" ")
        );
        assert!(backend.contains(
            "/usr/libexec/tilo-vm-install-proof code-refresh-check /run/tilo-installer/code-refresh"
        ));

        // The offline target, the handoff copy, the owner's SSH key and the install
        // proof are all the compiled helpers, never their retired Python scripts.
        assert!(
            backend.contains("LOCAL_IMG=\"$(/usr/libexec/tilo-installer-offline-target import)\"")
        );
        assert!(backend.contains("/usr/libexec/tilo-installer-offline-target verify >/dev/null"));
        assert!(backend.contains("\"$(/usr/libexec/tilo-installer-offline-target receipt)\""));
        assert!(!backend.contains("tilo-installer-offline-target.py"));
        assert!(backend.contains(
            "/usr/libexec/tilo-install-handoff-copy \"$HANDOFF_SRC/etc\" \"$DEPLOY_ETC\""
        ));
        assert!(backend.contains(
            "/usr/libexec/tilo-install-handoff-copy \"$HANDOFF_SRC/var\" \"$HANDOFF_VAR\""
        ));
        assert!(!backend.contains("tilo-install-handoff-copy.py"));
        let ssh = backend.find("SSH_REQUEST=").unwrap();
        let ssh_check = ssh
            + backend[ssh..]
                .find("[ \"$OFFLINE_TARGET\" -eq 0 ]")
                .unwrap();
        let ssh_apply = ssh_check
            + backend[ssh_check..]
                .find("/usr/libexec/tilo-install-dev-ssh --etc")
                .unwrap();
        assert!(backend[ssh_check..ssh_apply].contains("/usr/lib/tilo/image-flavour"));
        assert!(backend[ssh_check..ssh_apply].contains("/usr/sbin/sshd"));
        assert!(
            backend
                .find("rm -f \"$DEPLOY_ETC/systemd/system/sshd.service\"")
                .unwrap()
                < ssh_apply
        );
        assert!(!backend.contains("tilo-install-dev-ssh.py"));
        let prepared = backend
            .find("VM_INSTALL_PROOF=/run/tilo-installer/vm-install-proof.json")
            .unwrap();
        let finished = backend
            .find("\"$DISK_ENCRYPT\" finish \"$ENC_LUKS\"")
            .unwrap();
        let emitted = backend
            .find("proof_line=\"TILO_VM_INSTALL_PROOF_${proof_version}=")
            .unwrap();
        assert!(prepared < finished && finished < emitted);
        assert!(
            backend.contains("VM_INSTALL_PROOF_PORT=/dev/virtio-ports/org.tilo.vm-install-proof")
        );
        assert!(backend.contains("systemd-detect-virt --vm --quiet"));
        assert!(backend.contains("printf '%s\\n' \"$proof_line\" > \"$VM_INSTALL_PROOF_PORT\""));
        assert!(!backend.contains("tilo-vm-install-proof.py"));
    }
}

fn tilo_install_fixture(dir: &Path) -> (std::path::PathBuf, String) {
    let bin_dir = dir.join("bin");
    fs::create_dir(&bin_dir).unwrap();

    let write_stub = |path: &Path, body: &str| {
        fs::write(path, body).unwrap();
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    };

    write_stub(&bin_dir.join("id"), "#!/bin/sh\nprintf '0\\n'\n");
    let board_stub = dir.join("tilo-board-successor");
    write_stub(
        &board_stub,
        "#!/bin/sh\nprintf 'ostree-image-signed:docker://ghcr.io/tilo/tilo:latest\\n'\n",
    );

    let raw = source("os/usr/libexec/tilo-install");
    for expected in [
        "/usr/libexec/tilo-board-successor",
        "/usr/libexec/tilo-staging-handoff",
    ] {
        assert!(raw.contains(expected), "missing {expected} in tilo-install");
    }
    let rewritten = raw
        .replace(
            "/usr/libexec/tilo-board-successor",
            board_stub.to_str().unwrap(),
        )
        .replace(
            "/usr/libexec/tilo-staging-handoff",
            tilo_test_env::package_binary!("tilo-staging-handoff")
                .to_str()
                .unwrap(),
        );
    let script_path = dir.join("tilo-install");
    write_stub(&script_path, &rewritten);
    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    (script_path, path_env)
}

#[test]
fn tilo_install_enforces_63_char_hostname_before_disk_checks() {
    let dir = tempfile::tempdir().unwrap();
    let (script_path, path_env) = tilo_install_fixture(dir.path());
    let run_installer = |hostname: &str| -> Output {
        Command::new("bash")
            .arg(&script_path)
            .args([
                "--disk",
                "/dev/tilo-test-missing",
                "--confirm-erase",
                "/dev/tilo-test-missing",
                "--expect-disk-size-bytes",
                "68719476736",
                "--expect-disk-devnum",
                "252:0",
                "--expect-disk-serial",
                "TESTDISK",
                "--user",
                "ada",
                "--pin",
                "--yes",
                "--hostname",
                hostname,
                "--locale",
                "",
            ])
            .env("TILO_INSTALL_TRANSIENT", "1")
            .env("PATH", &path_env)
            .stdin(Stdio::null())
            .output_locked()
            .unwrap()
    };

    // 64 characters is refused by the hostname length guard before reaching locale or disk checks.
    let too_long = "h".repeat(64);
    let refused_long = refused(&run_installer(&too_long));
    assert!(
        refused_long.contains("use at most 63 characters"),
        "{refused_long}"
    );
    assert!(!refused_long.contains("missing --locale"), "{refused_long}");

    // 63 characters passes the hostname guard and advances to the immediately following
    // argument validation (--locale), still without touching /proc/cmdline or any disk.
    let max_valid = "h".repeat(63);
    let accepted_max = refused(&run_installer(&max_valid));
    assert!(
        accepted_max.contains("missing --locale <locale>"),
        "{accepted_max}"
    );
    assert!(!accepted_max.contains("invalid hostname"), "{accepted_max}");

    // Other invalid hostnames are likewise refused before locale/disk checks.
    for (bad, expected) in [
        ("", "missing --hostname <name>"),
        ("-studio", "invalid hostname"),
        ("studio-", "invalid hostname"),
        ("bad host", "letters, digits, hyphens only"),
    ] {
        let stderr = refused(&run_installer(bad));
        assert!(stderr.contains(expected), "{bad:?}: {stderr}");
        assert!(!stderr.contains("missing --locale"), "{bad:?}: {stderr}");
    }
}

/// Every helper is a `/usr/libexec` command whose usage error is status 2, as the
/// installer's own argument contract documents; the exact argparse wording is the
/// shared parser's, and one binary checks it against the parser
/// (`staging_handoff::usage_errors_exit_2`).
#[test]
fn every_helper_reports_a_usage_error_as_status_2() {
    for binary in [
        tilo_test_env::package_binary!("tilo-install-handoff-copy"),
        tilo_test_env::package_binary!("tilo-install-dev-ssh"),
        tilo_test_env::package_binary!("tilo-installer-offline-target"),
        tilo_test_env::package_binary!("tilo-staging-handoff"),
        tilo_test_env::package_binary!("tilo-vm-install-proof"),
    ] {
        let output = Command::new(&binary).output_locked().unwrap();
        assert_eq!(output.status.code(), Some(2), "{}", binary.display());
        assert!(!output.stderr.is_empty(), "{}", binary.display());
        assert!(output.stdout.is_empty(), "{}", binary.display());
    }
}
