//! `tilo-uki-cmdline`, the shipped link to `tilo-esp-write`, editing PE
//! fixtures in place: what it prints, what it refuses, and that only the
//! section it edits (and that section's virtual size) changes. The installer
//! suites that run the Containerfile's and `tilo-install`'s own command lines
//! through it run here too, against this build.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    reason = "a failed fixture call fails the test; fixtures are small known values"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

fn helper() -> std::path::PathBuf {
    tilo_test_env::package_binary!("tilo-esp-write")
}
const REPO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
const OSREL: &[u8] = b"NAME=Tilo\nID=tilo\nPRETTY_NAME=\"Tilo 1\"\nSORT_KEY=tilo\n";

/// A PE with one 512-byte-aligned raw section per payload, at 4 KiB virtual
/// steps, after 1 KiB of headers.
fn pe(sections: &[(&str, &[u8])]) -> Vec<u8> {
    let raw_sizes: Vec<usize> = sections
        .iter()
        .map(|(_, value)| value.len().div_ceil(512).max(1) * 512)
        .collect();
    let mut data = vec![0u8; 1024 + raw_sizes.iter().sum::<usize>()];
    data[..2].copy_from_slice(b"MZ");
    data[60..64].copy_from_slice(&64u32.to_le_bytes());
    data[64..68].copy_from_slice(b"PE\0\0");
    data[68..70].copy_from_slice(&0xaa64u16.to_le_bytes());
    data[70..72].copy_from_slice(&(sections.len() as u16).to_le_bytes());
    data[84..86].copy_from_slice(&240u16.to_le_bytes());
    let mut raw = 1024;
    for (index, ((name, value), raw_size)) in sections.iter().zip(&raw_sizes).enumerate() {
        let header = header(index);
        data[header..header + name.len()].copy_from_slice(name.as_bytes());
        for (at, field) in [value.len(), 4096 * (index + 1), *raw_size, raw]
            .iter()
            .enumerate()
        {
            let at = header + 8 + 4 * at;
            data[at..at + 4].copy_from_slice(&(*field as u32).to_le_bytes());
        }
        data[raw..raw + value.len()].copy_from_slice(value);
        raw += raw_size;
    }
    data
}

/// The file offset of section `index`'s header in a [`pe`] fixture.
fn header(index: usize) -> usize {
    328 + 40 * index
}

fn field(data: &[u8], at: usize) -> usize {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap()) as usize
}

fn cmdline() -> Vec<u8> {
    format!(
        "root=tilo-auto tilo.mode=normal tilo.generation=42 composefs={} quiet tilo.install\0",
        "a".repeat(128)
    )
    .into_bytes()
}

/// The editor under its installed name, and a UKI to edit.
struct Fixture {
    _tmp: tempfile::TempDir,
    tool: PathBuf,
    uki: PathBuf,
}

impl Fixture {
    fn new(sections: &[(&str, &[u8])]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let tool = tmp.path().join("tilo-uki-cmdline");
        std::os::unix::fs::symlink(helper(), &tool).unwrap();
        let uki = tmp.path().join("uki.efi");
        fs::write(&uki, pe(sections)).unwrap();
        Self {
            _tmp: tmp,
            tool,
            uki,
        }
    }

    fn standard() -> Self {
        let cmdline = cmdline();
        Self::new(&[
            (".osrel", OSREL),
            (".cmdline", &cmdline),
            (".linux", b"KERNEL"),
            (".initrd", b"INITRD"),
            (".dtb", b"DEVICE TREE"),
        ])
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(&self.tool)
            .args(args)
            .arg(&self.uki)
            .output()
            .unwrap()
    }

    fn bytes(&self) -> Vec<u8> {
        fs::read(&self.uki).unwrap()
    }

    fn path(&self) -> String {
        self.uki.display().to_string()
    }
}

#[track_caller]
fn succeeded(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[track_caller]
fn refused(output: &Output, message: &str) {
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!("{message}\n")
    );
}

/// Every byte outside section `index`'s raw bytes and virtual size field.
fn outside(data: &[u8], index: usize) -> Vec<u8> {
    let header = header(index);
    let (raw, size) = (field(data, header + 20), field(data, header + 16));
    let mut kept = data.to_vec();
    kept[header + 8..header + 12].fill(0);
    kept[raw..raw + size].fill(0);
    kept
}

#[test]
fn shows_the_sealed_command_line_and_os_release() {
    let fixture = Fixture::standard();
    let mut expected = cmdline();
    expected.pop();
    expected.push(b'\n');
    assert_eq!(succeeded(&fixture.run(&["show"])).as_bytes(), expected);
    assert_eq!(
        succeeded(&fixture.run(&["osrel"])).as_bytes(),
        [OSREL, b"\n"].concat()
    );
}

#[test]
fn blank_and_set_keep_every_offset() {
    let fixture = Fixture::standard();
    let before = fixture.bytes();
    let out = succeeded(&fixture.run(&["blank", "tilo.generation"]));
    let blanked = String::from_utf8(cmdline())
        .unwrap()
        .trim_end_matches('\0')
        .replace("tilo.generation=42", &" ".repeat(18));
    assert_eq!(
        out,
        format!(
            "blanked tilo.generation=42 (18 bytes -> 18 spaces) in {}\nsealed cmdline is now:\n  {blanked}\n",
            fixture.path()
        )
    );
    let after = fixture.bytes();
    assert_eq!(after.len(), before.len());
    // Not even the virtual size moves.
    assert_eq!(outside(&after, 1), outside(&before, 1));
    assert_eq!(
        after[header(1)..header(1) + 40],
        before[header(1)..header(1) + 40]
    );

    let digest = "b".repeat(128);
    let out = succeeded(&fixture.run(&["set", &format!("composefs={digest}")]));
    assert!(out.starts_with(&format!(
        "rewrote composefs= in {} (138 bytes, unchanged)\n",
        fixture.path()
    )));
    assert!(out.contains(&format!("composefs={digest} quiet")));
    refused(
        &fixture.run(&["set", "tilo.mode=rep"]),
        "replacement is 13 bytes and the existing token is 16; a length-changing edit would \
         move every following PE section\n  have: tilo.mode=normal\n  want: tilo.mode=rep",
    );
    refused(
        &fixture.run(&["set", "missing=1"]),
        &format!(
            "missing= is not on the sealed command line:\n  {}",
            blanked.replace(&"a".repeat(128), &digest)
        ),
    );
    refused(
        &fixture.run(&["set", "nokey"]),
        "set takes KEY=VALUE, e.g. `set composefs=<digest>`",
    );
}

#[test]
fn replace_requires_the_expected_token() {
    let fixture = Fixture::standard();
    refused(
        &fixture.run(&["replace", "tilo.mode=xxxxxx", "tilo.mode=repair"]),
        "refusing to replace an unexpected token\n  have: tilo.mode=normal\n  expected: tilo.mode=xxxxxx",
    );
    refused(
        &fixture.run(&["replace", "a=1", "a=12"]),
        "replacement is 4 bytes and the existing token is 3; a length-changing edit would move \
         following PE sections",
    );
    let out = succeeded(&fixture.run(&["replace", "tilo.mode=normal", "tilo.mode=repair"]));
    assert!(out.contains("tilo.mode=repair tilo.generation=42"), "{out}");
}

#[test]
fn append_and_remove_change_only_the_command_line() {
    let fixture = Fixture::standard();
    let before = fixture.bytes();
    let length = cmdline().len() - 1;
    let out = succeeded(&fixture.run(&["remove", "tilo.install"]));
    assert_eq!(
        out,
        format!(
            "removed tilo.install from {} ({} of 512 bytes)\n",
            fixture.path(),
            length - " tilo.install".len()
        )
    );
    let out = succeeded(&fixture.run(&["append", "rd.luks.options=tpm2-device=auto"]));
    let used = length - " tilo.install".len() + " rd.luks.options=tpm2-device=auto".len();
    assert_eq!(
        out,
        format!(
            "appended rd.luks.options=tpm2-device=auto to {} ({used} of 512 bytes)\n",
            fixture.path()
        )
    );
    let after = fixture.bytes();
    // The other sections, the .dtb among them, keep every byte.
    assert_eq!(outside(&after, 1), outside(&before, 1));
    assert_eq!(field(&after, header(1) + 8), used);
    let raw = field(&after, header(1) + 20);
    assert!(after[raw + used..raw + 512].iter().all(|&b| b == 0));

    refused(
        &fixture.run(&["append", "root=other"]),
        "root is already on the sealed command line",
    );
    refused(
        &fixture.run(&["append", "a b"]),
        "append takes one complete KEY or KEY=VALUE token",
    );
    refused(
        &fixture.run(&["append", &format!("x={}", "y".repeat(600))]),
        &format!(
            ".cmdline would need {} bytes and only 512 are available without moving another PE section",
            used + 603
        ),
    );
    refused(
        &fixture.run(&["remove", "tilo.install"]),
        "expected exactly one tilo.install token, found 0",
    );
    refused(
        &fixture.run(&["remove", "a=b"]),
        "remove takes one KEY without = or whitespace",
    );
    assert_eq!(fixture.bytes(), after, "a refusal writes nothing");
}

#[test]
fn set_osrel_rewrites_in_place_within_the_raw_section() {
    let fixture = Fixture::standard();
    let before = fixture.bytes();
    let out = succeeded(&fixture.run(&[
        "set-osrel",
        "PRETTY_NAME=Tilo repair",
        "SORT_KEY=zz",
        "NEW=1",
    ]));
    let text = "NAME=Tilo\nID=tilo\nPRETTY_NAME=Tilo repair\nSORT_KEY=zz\nNEW=1\n";
    assert_eq!(
        out,
        format!(
            "rewrote .osrel in {} ({} of 512 bytes available in place)\nit now reads:\n  \
             NAME=Tilo\n  ID=tilo\n  PRETTY_NAME=Tilo repair\n  SORT_KEY=zz\n  NEW=1\n",
            fixture.path(),
            text.len()
        )
    );
    let after = fixture.bytes();
    assert_eq!(outside(&after, 0), outside(&before, 0));
    assert_eq!(field(&after, header(0) + 8), text.len());
    refused(
        &fixture.run(&["set-osrel", &format!("X={}", "y".repeat(600))]),
        &format!(
            ".osrel would need {} bytes and only 512 are available in place (raw size 512). \
             Growing the section would move every following one and produce a PE that does not load.",
            text.len() + 603
        ),
    );
    refused(
        &fixture.run(&["set-osrel", "bad"]),
        "set-osrel takes KEY=VALUE, got 'bad'",
    );
}

#[test]
fn boot_digest_hashes_the_virtual_kernel_and_initramfs() {
    let fixture = Fixture::standard();
    let expected = hex::encode(Sha256::digest(b"KERNELINITRD"));
    assert_eq!(
        succeeded(&fixture.run(&["boot-digest"])),
        format!("{expected}\n")
    );
    // Slack after the virtual size, and the command line, are not identity.
    let mut data = fixture.bytes();
    let raw = field(&data, header(2) + 20);
    data[raw + 6] = b'!';
    let cmdline = field(&data, header(1) + 20);
    data[cmdline] = b'R';
    fs::write(&fixture.uki, &data).unwrap();
    assert_eq!(
        succeeded(&fixture.run(&["boot-digest"])),
        format!("{expected}\n")
    );
    let twice = Fixture::new(&[(".linux", b"K"), (".linux", b"K"), (".initrd", b"I")]);
    refused(&twice.run(&["boot-digest"]), "UKI needs exactly one .linux");
}

#[test]
fn usage_and_missing_sections_are_refusals() {
    let fixture = Fixture::new(&[(".linux", b"K"), (".initrd", b"I")]);
    refused(
        &fixture.run(&["show"]),
        "this PE has no .cmdline section, so it is not a UKI",
    );
    refused(
        &fixture.run(&["osrel"]),
        "this PE has no .osrel section, so systemd-boot would have no title to show for it",
    );
    let output = Command::new(&fixture.tool).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("tilo-uki-cmdline set-osrel KEY=VALUE")
    );
    let output = Command::new(&fixture.tool)
        .args(["boot-digest"])
        .output()
        .unwrap();
    refused(&output, "boot-digest needs one UKI path");
    // Under its own name the binary is still the ESP writer.
    let output = Command::new(helper()).arg("show").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
}

/// The installer suites that run the Containerfile's and `tilo-install`'s
/// own editor invocations, with this build as `/usr/libexec/tilo-uki-cmdline`.
fn suite(script: &str) {
    let fixture = Fixture::standard();
    let output = Command::new("python3")
        .arg(Path::new(REPO).join(script))
        .env("TILO_UKI_CMDLINE", &fixture.tool)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{script}: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn installer_target_cmdline_projection() {
    suite("os/tests/test_installer_target_cmdline.py");
}

#[test]
fn installer_target_boot_digest_guard() {
    suite("os/tests/test_installer_target_boot_digest.py");
}
