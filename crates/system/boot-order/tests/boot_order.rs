//! File-backed firmware: the helper appends Tilo after Windows only when
//! every EFI fact matches, verifies the readback, and refuses everything
//! else. Never touches a real EFI variable.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::arithmetic_side_effects,
    clippy::string_slice,
    reason = "a test fixture: any failed fixture call fails the test, and it builds fixed-size EFI records"
)]

use std::cell::Cell;
use std::ffi::OsString;
use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output};

use tilo_boot_order::{EFI_GLOBAL, Host, LOADER_VENDOR, Refusal, enlist, ids, main};

const ESP: &str = "11111111-2222-3333-4444-555555555555";
const WIN_ESP: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const TILO: &str = r"\EFI\systemd\systemd-bootaa64.efi";
const WINDOWS: &str = r"\EFI\Microsoft\Boot\bootmgfw.efi";

/// `uuid.UUID(text).bytes_le`.
fn guid_le(text: &str) -> Vec<u8> {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    let bytes: Vec<u8> = (0..16)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
        .collect();
    let mut le = bytes;
    le[..4].reverse();
    le[4..6].reverse();
    le[6..8].reverse();
    le
}

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn boot_option(description: &str, esp: &str, path: &str) -> Vec<u8> {
    let mut hd = vec![4u8, 1];
    hd.extend(42u16.to_le_bytes());
    hd.extend(4u32.to_le_bytes());
    hd.extend(2048u64.to_le_bytes());
    hd.extend(4_194_304u64.to_le_bytes());
    hd.extend(guid_le(esp));
    hd.extend([2, 2]);
    let file_bytes = utf16(&format!("{path}\0"));
    let mut file = vec![4u8, 4];
    file.extend(u16::try_from(file_bytes.len() + 4).unwrap().to_le_bytes());
    file.extend(file_bytes);
    let nodes: Vec<u8> = [hd, file, vec![0x7f, 0xff, 4, 0]].concat();
    let mut option = 1u32.to_le_bytes().to_vec();
    option.extend(u16::try_from(nodes.len()).unwrap().to_le_bytes());
    option.extend(utf16(&format!("{description}\0")));
    option.extend(nodes);
    option
}

/// Firmware in a directory; `efibootmgr -q -o` rewrites `BootOrder`, and can
/// also set `BootNext` to model firmware that changes under the helper.
struct Firmware {
    _work: tempfile::TempDir,
    vars: PathBuf,
    mutate_next: Cell<bool>,
    fail: Cell<bool>,
    root: bool,
    calls: Cell<usize>,
}

impl Firmware {
    fn new() -> Self {
        let work = tempfile::tempdir().unwrap();
        let vars = work.path().join("vars");
        fs::create_dir(&vars).unwrap();
        let firmware = Self {
            _work: work,
            vars,
            mutate_next: Cell::new(false),
            fail: Cell::new(false),
            root: true,
            calls: Cell::new(0),
        };
        firmware.put("BootOrder", &1u16.to_le_bytes());
        firmware.put("BootCurrent", &0x100u16.to_le_bytes());
        firmware.put(
            "Boot0001",
            &boot_option("Windows Boot Manager", WIN_ESP, WINDOWS),
        );
        firmware.put("Boot0100", &boot_option("Tilo", ESP, TILO));
        firmware.put_vendor(
            "LoaderDevicePartUUID",
            &utf16(&format!("{ESP}\0")),
            LOADER_VENDOR,
        );
        firmware
    }

    fn path(&self, name: &str, vendor: &str) -> PathBuf {
        self.vars.join(format!("{name}-{vendor}"))
    }

    fn put_vendor(&self, name: &str, data: &[u8], vendor: &str) {
        fs::write(self.path(name, vendor), [&[7, 0, 0, 0][..], data].concat()).unwrap();
    }

    fn put(&self, name: &str, data: &[u8]) {
        self.put_vendor(name, data, EFI_GLOBAL);
    }

    fn order(&self) -> Vec<String> {
        ids(self, "BootOrder").unwrap().unwrap()
    }
}

impl Host for Firmware {
    fn efivars(&self) -> PathBuf {
        self.vars.clone()
    }

    fn efibootmgr(&self, args: &[&str]) -> Result<Output, Refusal> {
        self.calls.set(self.calls.get() + 1);
        assert_eq!(&args[..2], ["-q", "-o"]);
        if self.fail.get() {
            return Ok(Output {
                status: ExitStatus::from_raw(1 << 8),
                stdout: Vec::new(),
                stderr: b"Could not set BootOrder\n".to_vec(),
            });
        }
        let order: Vec<u8> = args[2]
            .split(',')
            .flat_map(|id| u16::from_str_radix(id, 16).unwrap().to_le_bytes())
            .collect();
        self.put("BootOrder", &order);
        if self.mutate_next.get() {
            self.put("BootNext", &2u16.to_le_bytes());
        }
        Ok(Output {
            status: ExitStatus::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }

    fn euid(&self) -> u32 {
        if self.root { 0 } else { 1000 }
    }
}

type Tamper = Box<dyn Fn(&Firmware)>;

fn refusal(result: Result<String, Refusal>) -> String {
    result.unwrap_err().0
}

#[test]
fn appends_after_windows_and_is_idempotent() {
    let firmware = Firmware::new();
    assert_eq!(
        enlist(&firmware, "0100", ESP, TILO).unwrap(),
        "Boot0100 appended to BootOrder; Windows remains first"
    );
    assert_eq!(firmware.order(), ["0001", "0100"]);
    assert_eq!(
        enlist(&firmware, "0100", ESP, TILO).unwrap(),
        "Boot0100 is already in BootOrder; Windows remains first"
    );
    assert_eq!(firmware.order(), ["0001", "0100"]);
    assert_eq!(firmware.calls.get(), 1);
}

#[test]
fn detects_failed_firmware_readback() {
    let firmware = Firmware::new();
    firmware.mutate_next.set(true);
    assert!(refusal(enlist(&firmware, "0100", ESP, TILO)).contains("firmware readback differs"));
}

#[test]
fn refuses_busy_bootnext_and_wrong_identity() {
    let firmware = Firmware::new();
    firmware.put("BootNext", &2u16.to_le_bytes());
    assert!(refusal(enlist(&firmware, "0100", ESP, TILO)).contains("BootNext is occupied"));
    assert_eq!(firmware.order(), ["0001"]);
    fs::remove_file(firmware.path("BootNext", EFI_GLOBAL)).unwrap();
    assert!(
        refusal(enlist(
            &firmware,
            "0100",
            "bbbbbbbb-cccc-dddd-eeee-ffffffffffff",
            TILO
        ))
        .contains("does not match")
    );
    assert_eq!(firmware.order(), ["0001"]);
}

#[test]
fn refuses_non_windows_default_or_non_current_tilo() {
    let firmware = Firmware::new();
    firmware.put("Boot0001", &boot_option("other", WIN_ESP, TILO));
    assert!(
        refusal(enlist(&firmware, "0100", ESP, TILO))
            .contains("Windows Boot Manager is not the first")
    );
    firmware.put(
        "Boot0001",
        &boot_option("Windows Boot Manager", WIN_ESP, WINDOWS),
    );
    firmware.put("BootCurrent", &1u16.to_le_bytes());
    assert!(refusal(enlist(&firmware, "0100", ESP, TILO)).contains("not the current"));
    assert_eq!(firmware.order(), ["0001"]);
}

#[test]
fn refuses_ambiguous_or_modified_boot_option() {
    let firmware = Firmware::new();
    firmware.put(
        "BootOrder",
        &[1u16.to_le_bytes(), 1u16.to_le_bytes()].concat(),
    );
    assert!(refusal(enlist(&firmware, "0100", ESP, TILO)).contains("duplicated"));
    firmware.put("BootOrder", &1u16.to_le_bytes());
    firmware.put(
        "Boot0100",
        &boot_option("Tilo", ESP, r"\EFI\BOOT\other.efi"),
    );
    assert!(refusal(enlist(&firmware, "0100", ESP, TILO)).contains("does not match"));
    assert_eq!(firmware.order(), ["0001"]);
}

#[test]
fn refuses_malformed_options_and_loader_reports() {
    let cases: Vec<(&str, Tamper)> = vec![
        (
            "inactive",
            Box::new(|f| {
                let mut option = boot_option("Tilo", ESP, TILO);
                option[0] = 0;
                f.put("Boot0100", &option);
            }),
        ),
        (
            "absent or truncated",
            Box::new(|f| f.put("Boot0100", &[1, 0, 0, 0, 0, 0])),
        ),
        (
            "device path is truncated",
            Box::new(|f| {
                let option = boot_option("Tilo", ESP, TILO);
                f.put("Boot0100", &option[..option.len() - 2]);
            }),
        ),
        (
            "did not report its loaded ESP",
            Box::new(|f| fs::remove_file(f.path("LoaderDevicePartUUID", LOADER_VENDOR)).unwrap()),
        ),
        (
            "loaded from another ESP",
            Box::new(|f| {
                f.put_vendor(
                    "LoaderDevicePartUUID",
                    &utf16(&format!("{WIN_ESP}\0")),
                    LOADER_VENDOR,
                );
            }),
        ),
        ("odd length", Box::new(|f| f.put("BootOrder", &[1, 0, 0]))),
        (
            "truncated or too large",
            Box::new(|f| fs::write(f.path("BootCurrent", EFI_GLOBAL), [7, 0]).unwrap()),
        ),
        (
            "not a regular EFI variable",
            Box::new(|f| {
                fs::remove_file(f.path("BootNext", EFI_GLOBAL)).ok();
                fs::create_dir(f.path("BootNext", EFI_GLOBAL)).unwrap();
            }),
        ),
        (
            "absent, empty or duplicated",
            Box::new(|f| f.put("BootOrder", &[])),
        ),
    ];
    for (expected, tamper) in cases {
        let firmware = Firmware::new();
        tamper(&firmware);
        let message = refusal(enlist(&firmware, "0100", ESP, TILO));
        assert!(message.contains(expected), "{expected}: {message}");
        assert_eq!(firmware.calls.get(), 0, "{expected}");
    }
}

#[test]
fn a_failed_efibootmgr_is_reported() {
    let firmware = Firmware::new();
    firmware.fail.set(true);
    assert_eq!(
        refusal(enlist(&firmware, "0100", ESP, TILO)),
        "efibootmgr failed: Could not set BootOrder"
    );
    assert_eq!(firmware.order(), ["0001"]);
}

fn cli(firmware: &Firmware, words: &[&str]) -> (i32, String, String) {
    let args: Vec<OsString> = words.iter().map(OsString::from).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = main(firmware, &args, &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn the_command_line_checks_identities_before_touching_firmware() {
    let firmware = Firmware::new();
    let args = |esp: &str, id: &str, loader: &str| {
        vec![
            "enlist",
            "--esp-partuuid",
            esp,
            "--boot-id",
            id,
            "--loader",
            loader,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>()
    };
    let run = |words: Vec<String>| {
        cli(
            &firmware,
            &words.iter().map(String::as_str).collect::<Vec<_>>(),
        )
    };
    for (words, message) in [
        (
            args("not-a-guid", "0100", TILO),
            "expected a canonical ESP GUID and four-digit Boot ID",
        ),
        (
            args(ESP, "100", TILO),
            "expected a canonical ESP GUID and four-digit Boot ID",
        ),
        (
            args(ESP, "0100", WINDOWS),
            "loader must be the ARM64 systemd-boot primary or fallback path",
        ),
    ] {
        assert_eq!(
            run(words),
            (1, String::new(), format!("tilo-boot-order: {message}\n"))
        );
    }
    assert_eq!(firmware.calls.get(), 0);
    // Identities are case-insensitive, as the original's casefold and upper/lower.
    let upper = ESP.to_uppercase();
    let (code, out, err) = run(args(&upper, "0100", r"\efi\SYSTEMD\systemd-BOOTAA64.EFI"));
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (
            0,
            "Boot0100 appended to BootOrder; Windows remains first\n",
            ""
        )
    );
    let lower_id = cli(
        &firmware,
        &[
            "enlist",
            "--esp-partuuid",
            ESP,
            "--boot-id",
            "0100",
            "--loader",
            TILO,
        ],
    );
    assert_eq!(lower_id.0, 0);
    assert!(lower_id.1.contains("already in BootOrder"));
}

#[test]
fn only_root_may_enlist() {
    let mut firmware = Firmware::new();
    firmware.root = false;
    let (code, _, err) = cli(
        &firmware,
        &[
            "enlist",
            "--esp-partuuid",
            ESP,
            "--boot-id",
            "0100",
            "--loader",
            TILO,
        ],
    );
    assert_eq!(
        (code, err.as_str()),
        (
            1,
            "tilo-boot-order: run as root after validating Tilo's first boot\n"
        )
    );
}

#[test]
fn the_binary_answers_usage_and_help_without_firmware() {
    let binary = Path::new(env!("CARGO_BIN_EXE_tilo-boot-order"));
    let usage = Command::new(binary).arg("enlist").output().unwrap();
    assert_eq!(usage.status.code(), Some(2));
    let stderr = String::from_utf8(usage.stderr).unwrap();
    assert!(
        stderr.starts_with("usage: tilo-boot-order [-h]"),
        "{stderr}"
    );
    assert!(stderr.ends_with(
        "error: the following arguments are required: --esp-partuuid, --boot-id, --loader\n"
    ));
    let help = Command::new(binary).arg("--help").output().unwrap();
    assert_eq!(help.status.code(), Some(0));
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("never creates or replaces an EFI boot option")
    );
}
