//! The installer backend's checks and writes, through the shipped
//! `tilo-staging-handoff` binary as `tilo-install`, `tilo-guided-partition`
//! and `tilo-installer-profile-extract` run it.

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

use serde_json::{Value, json};

fn helper(args: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tilo-staging-handoff"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
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

#[test]
fn device_labels_are_bounded_and_printable() {
    for good in ["", "Ada's laptop", "Café ☕", &"x".repeat(64)] {
        stdout(&helper(&["check-device-label", good], b""));
    }
    for bad in ["line\nbreak", "bell\u{7}", "c1\u{9b}", &"x".repeat(65)] {
        assert_eq!(
            refused(&helper(&["check-device-label", bad], b"")),
            "Invalid device label\n"
        );
    }
    assert_eq!(helper(&["check-device-label"], b"").status.code(), Some(2));
}

#[test]
fn machine_info_keeps_other_lines_and_quotes_the_label() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("machine-info");
    let path_text = path.to_str().unwrap();
    stdout(&helper(&["write-machine-info", path_text, "Desk"], b""));
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "PRETTY_HOSTNAME=\"Desk\"\n"
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
        refused(&helper(&["write-machine-info", path_text, "Desk"], b"")),
        "machine-info is a symbolic link\n"
    );
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "PRETTY_HOSTNAME=\"Kept\"\n"
    );
}

const CHOICES: [&str; 20] = [
    "--username",
    "ada",
    "--full-name",
    "Ada Lovelace",
    "--device-label",
    "Desk",
    "--purpose",
    "computer",
    "--hostname",
    "desk",
    "--locale",
    "en_GB.UTF-8",
    "--timezone",
    "Europe/London",
    "--filesystem",
    "btrfs",
    "--keymap",
    "us",
    "--keyvariant",
    "dvorak",
];

fn profile() -> Value {
    json!({"username": "ada", "full_name": "Ada Lovelace", "device_label": "Desk",
           "purpose": "computer", "hostname": "desk", "locale": "en_GB.UTF-8",
           "timezone": "Europe/London", "filesystem": "btrfs", "keyboard": "dvorak"})
}

fn check_choices(profile: &Value, choices: &[&str]) -> Output {
    let mut args = vec!["check-prepared-choices"];
    args.extend_from_slice(choices);
    helper(
        &args,
        json!({"schema": 1, "install_profile": profile})
            .to_string()
            .as_bytes(),
    )
}

#[test]
fn prepared_choices_must_match_the_authenticated_profile() {
    stdout(&check_choices(&profile(), &CHOICES));
    stdout(&check_choices(&Value::Null, &CHOICES));

    let mut defaults = profile();
    let map = defaults.as_object_mut().unwrap();
    map.remove("device_label");
    map.remove("purpose");
    let mut choices = CHOICES;
    choices[5] = "";
    stdout(&check_choices(&defaults, &choices));

    let mut plain = profile();
    plain["keyboard"] = json!("de");
    let mut choices = CHOICES;
    (choices[17], choices[19]) = ("de", "");
    stdout(&check_choices(&plain, &choices));

    for (index, value, message) in [
        (1, "eve", "prepared account differs"),
        (3, "Ada", "prepared account differs"),
        (5, "Laptop", "prepared device label differs"),
        (7, "server", "prepared purpose differs"),
        (9, "laptop", "prepared preferences differ"),
        (13, "UTC", "prepared preferences differ"),
        (15, "ext4", "prepared preferences differ"),
        (17, "dvorak", "prepared keyboard differs"),
        (19, "", "prepared keyboard differs"),
    ] {
        let mut choices = CHOICES;
        choices[index] = value;
        let error = refused(&check_choices(&profile(), &choices));
        assert!(error.contains(message), "{index}: {error}");
    }
    let mut missing = profile();
    missing.as_object_mut().unwrap().remove("timezone");
    assert!(refused(&check_choices(&missing, &CHOICES)).contains("preferences differ"));
    assert!(refused(&check_choices(&profile(), &CHOICES[..18])).contains("missing --keyvariant"));
    assert!(
        refused(&helper(
            &[&["check-prepared-choices"], &CHOICES[..]].concat(),
            br#"{"schema":2,"install_profile":null}"#
        ))
        .contains("fields differ")
    );
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

#[test]
fn guided_geometry_allows_only_the_reviewed_layout() {
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

    let cases: [(usize, u64, &str); 6] = [
        (1, 1024, "unsupported logical sector size"),
        (2, start + 1, "MiB aligned"),
        (5, 3 * GIB, "dedicated 2 GiB EFI"),
        (7, GIB / 2, "contiguous 1 GiB boot"),
        (9, 20 * GIB, "root must fill"),
        (0, start + free, "exceeds the disk"),
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
    assert!(refused(&geometry(&with_boot, true, &[&overlapping])).contains("overlaps an existing"));
    assert!(refused(&geometry(&with_boot, true, &["x:1"])).contains("map is invalid"));
    for raw in ["0", "012", "-5", "9223372036854775808", "1e3"] {
        let mut args = vec!["guided-geometry".to_owned(), raw.to_owned()];
        args.extend(with_boot[1..].iter().map(u64::to_string));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        assert!(
            refused(&helper(&args, b"")).starts_with("guided allocation: "),
            "{raw}"
        );
    }
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

#[test]
fn gpt_facts_come_from_sfdisk_json() {
    assert_eq!(
        stdout(&helper(&["gpt", "map"], &sfdisk())),
        "00000000-0000-4000-8000-000000000001:1048576:4194304\n\
         20000000-0000-4000-8000-00000000000a:10737418240:32212254720\n"
    );
    assert_eq!(stdout(&helper(&["gpt", "tilo-roots"], &sfdisk())), "1\n");
    let root = "20000000-0000-4000-8000-00000000000a";
    assert_eq!(
        stdout(&helper(&["gpt", "number-node", root], &sfdisk())),
        "12 /dev/loop7p12\n"
    );
    assert_eq!(
        stdout(&helper(&["gpt", "node", root], &sfdisk())),
        "/dev/loop7p12\n"
    );
    assert_eq!(
        stdout(&helper(&["gpt", "type", root], &sfdisk())),
        format!("{}\n", TILO_ROOT.to_lowercase())
    );
    for query in ["number-node", "node", "type"] {
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
    let empty = br#"{"partitiontable": {"label": "gpt", "sectorsize": 4096}}"#;
    assert_eq!(stdout(&helper(&["gpt", "map"], empty)), "");
    assert_eq!(stdout(&helper(&["gpt", "tilo-roots"], empty)), "0\n");
    let incomplete = br#"{"partitiontable": {"sectorsize": 512, "partitions": [{"node": "/dev/x1", "start": 2048, "size": 0, "uuid": "u"}]}}"#;
    assert!(
        refused(&helper(&["gpt", "map"], incomplete)).contains("incomplete GPT partition identity")
    );
    assert!(refused(&helper(&["gpt", "map"], b"not json")).contains("malformed"));
    assert_eq!(helper(&["gpt", "nodes"], &sfdisk()).status.code(), Some(1));
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
fn setup_handoff_records_unpack_only_below_the_destination() {
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
        ("nested-escape", record("etc/../../escape", b"x")),
        ("absolute", record("/etc/passwd", b"x")),
        ("itself", record(".", b"x")),
        ("truncated", b"etc/marker\n10\nwifi".to_vec()),
        ("no-size", b"etc/marker\n".to_vec()),
        ("signed-size", b"etc/marker\n+4\nwifi".to_vec()),
    ] {
        let (output, _) = unpack(&bytes, name);
        assert!(refused(&output).contains("malformed"), "{name}");
    }
    assert!(!dir.path().join("escape").exists());
}

#[test]
fn the_installer_scripts_ask_the_compiled_helper() {
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
    assert!(backend.contains("| \"$STAGING_HANDOFF\" check-prepared-choices \\\n"));
    assert!(backend.contains(
        "\"$STAGING_HANDOFF\" write-machine-info \"$DEPLOY_ETC/machine-info\" \"$DEVICE_LABEL\""
    ));
    // Only the development VM's own Python helper is still started from the
    // backend; it ships for development media alone.
    for line in backend.lines().filter(|line| line.contains("python")) {
        assert!(
            line.contains("python3 /usr/libexec/tilo-installer-dev-target-uki.py "),
            "{line}"
        );
    }
}
