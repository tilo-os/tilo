//! Live-installer internal mode (USB-free install, design spec §6), formerly
//! `os/tests/staging_handoff_test.py`.
//!
//! Drives the helper's command line against fixture efivars, a fixture
//! staging ESP, fixture lsblk output and an in-process efibootmgr. Nothing
//! here touches a real disk or firmware variable.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    reason = "a failed fixture call fails the test; fixtures are small known values"
)]

use std::cell::RefCell;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tilo_install_helpers::staging_handoff::{Fail, Host, Outcome, Ran, read_bounded, run};

const EFI_GLOBAL: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";
const LOADER_VENDOR: &str = "4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";
const ESP_TYPE: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
const LINUX_TYPE: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";
const STAGING_ROOT_TYPE: &str = "d4c4daf6-f72c-43ee-b8e3-5ba32550311d";
const MSFT_TYPE: &str = "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7";
const GIB: i64 = 1024 * 1024 * 1024;

const DISK: &str = "/dev/nvme0n1";
const DISK_GUID: &str = "11111111-2222-4333-8444-555555555555";
const ESP_UUID: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const ROOT_UUID: &str = "bbbbbbbb-0000-4000-8000-000000000002";
const TARGET_ESP_UUID: &str = "cccccccc-0000-4000-8000-000000000003";
const DISK_SECTORS: i64 = 256 * GIB / 512;
// [ Windows ESP | C: | 1 MiB gap | target 64 GiB | staging ESP 1 GiB | staging root 40 GiB | free ]
const WIN_ESP: (i64, i64) = (2048, 2048 + 204_800 - 1);
const WINDOWS: (i64, i64) = (206_848, 100 * GIB / 512 - 1);
const TARGET: (i64, i64) = (
    100 * GIB / 512 + 2048,
    100 * GIB / 512 + 2048 + 64 * GIB / 512 - 1,
);
const STAGING_ESP: (i64, i64) = (TARGET.1 + 1, TARGET.1 + GIB / 512);
const STAGING_ROOT: (i64, i64) = (STAGING_ESP.1 + 1, STAGING_ESP.1 + 40 * GIB / 512);

fn handoff_document() -> Value {
    json!({
        "schema": 1,
        "operation_id": "0f0e0d0c-0b0a-4908-8706-050403020100",
        "host_os": "windows",
        "disk_guid": DISK_GUID.to_uppercase(),
        "disk_size_bytes": DISK_SECTORS * 512,
        "staging_esp": {"partuuid": ESP_UUID, "first_lba": STAGING_ESP.0, "last_lba": STAGING_ESP.1},
        "staging_root": {"partuuid": ROOT_UUID, "first_lba": STAGING_ROOT.0, "last_lba": STAGING_ROOT.1},
        "target_extent": {"first_lba": TARGET.0, "last_lba": TARGET.1},
        "owned_boot_entry": "0104",
        "original_boot_order": ["0001", "0000"],
        "profile": {"schema": 1, "hostname": "studio", "username": "ada", "locale": "en_US.UTF-8",
                    "keyboard": "de", "timezone": "Europe/Berlin"},
    })
}

fn partition(name: &str, uuid: &str, kind: &str, extent: (i64, i64), mounts: &[&str]) -> Value {
    let mounts: Vec<Value> = if mounts.is_empty() {
        vec![Value::Null]
    } else {
        mounts.iter().map(|m| json!(m)).collect()
    };
    json!({"name": name, "type": "part", "start": extent.0, "size": (extent.1 - extent.0 + 1) * 512,
           "partuuid": uuid, "parttype": kind, "pttype": "gpt", "ptuuid": DISK_GUID,
           "log-sec": 512, "mountpoints": mounts})
}

fn lsblk_document() -> Value {
    json!({"blockdevices": [{
    "name": DISK, "type": "disk", "start": null, "size": DISK_SECTORS * 512, "partuuid": null,
    "parttype": null, "pttype": "gpt", "ptuuid": DISK_GUID, "log-sec": 512, "mountpoints": [null],
    "children": [
        partition(&format!("{DISK}p1"), "00000000-0000-4000-8000-00000000000a", ESP_TYPE, WIN_ESP, &[]),
        partition(&format!("{DISK}p2"), "00000000-0000-4000-8000-00000000000b", MSFT_TYPE, WINDOWS, &[]),
        partition(&format!("{DISK}p3"), ESP_UUID, ESP_TYPE, STAGING_ESP, &[]),
        partition(&format!("{DISK}p4"), ROOT_UUID, STAGING_ROOT_TYPE, STAGING_ROOT, &["/sysroot", "/var", "/etc"]),
    ]}]})
}

fn uuid_bytes_le(uuid: &str) -> Vec<u8> {
    let hex: String = uuid.chars().filter(|c| *c != '-').collect();
    let bytes: Vec<u8> = (0..16)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
        .collect();
    let order = [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15];
    order.iter().map(|at| bytes[*at]).collect()
}

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn load_option(
    uuid: &str,
    start: i64,
    size: i64,
    number: u32,
    path: &str,
    description: &str,
) -> Vec<u8> {
    let mut hd = vec![4u8, 1];
    hd.extend(42u16.to_le_bytes());
    hd.extend(number.to_le_bytes());
    hd.extend(start.cast_unsigned().to_le_bytes());
    hd.extend(size.cast_unsigned().to_le_bytes());
    hd.extend(uuid_bytes_le(uuid));
    hd.extend([2, 2]);
    let encoded = utf16(&format!("{path}\0"));
    let mut file_node = vec![4u8, 4];
    file_node.extend(u16::try_from(4 + encoded.len()).unwrap().to_le_bytes());
    file_node.extend(encoded);
    let mut device_path = hd;
    device_path.extend(file_node);
    device_path.extend([0x7F, 0xFF, 4, 0]);
    let mut body = 1u32.to_le_bytes().to_vec();
    body.extend(u16::try_from(device_path.len()).unwrap().to_le_bytes());
    body.extend(utf16(&format!("{description}\0")));
    body.extend(device_path);
    let mut data = vec![7, 0, 0, 0];
    data.extend(body);
    data
}

fn staging_option(uuid: &str, start: i64, size: i64) -> Vec<u8> {
    load_option(
        uuid,
        start,
        size,
        3,
        "\\EFI\\BOOT\\BOOTX64.EFI",
        "Tilo installer",
    )
}

fn ids(values: &[&str]) -> Vec<u8> {
    let mut data = vec![7, 0, 0, 0];
    for value in values {
        data.extend(u16::from_str_radix(value, 16).unwrap().to_le_bytes());
    }
    data
}

/// A live medium in a temporary directory, with an efibootmgr and an sfdisk
/// answered in-process.
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    efivars: PathBuf,
    esp: PathBuf,
    copy: PathBuf,
    lsblk: RefCell<Value>,
    log: RefCell<Vec<String>>,
    /// The bytes efibootmgr -c writes as Boot0105.
    create: RefCell<Option<Vec<u8>>>,
    /// sfdisk's table when lsblk has no udev database.
    sfdisk: RefCell<Option<Value>>,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let efivars = root.join("efivars");
        let esp = root.join("esp");
        fs::create_dir(&efivars).unwrap();
        fs::create_dir_all(esp.join("EFI/tilo-staging")).unwrap();
        let fixture = Self {
            _tmp: tmp,
            copy: root.join("handoff.json"),
            root,
            efivars,
            esp,
            lsblk: RefCell::new(lsblk_document()),
            log: RefCell::new(Vec::new()),
            create: RefCell::new(None),
            sfdisk: RefCell::new(None),
        };
        fixture.set_handoff(&handoff_document(), false);
        fixture.set_loader(&ESP_UUID.to_uppercase());
        fixture.write_var("BootOrder", &ids(&["0001", "0000"]), EFI_GLOBAL);
        fixture.write_var(
            "Boot0104",
            &staging_option(ESP_UUID, STAGING_ESP.0, STAGING_ESP.1 - STAGING_ESP.0 + 1),
            EFI_GLOBAL,
        );
        fixture.write_var(
            "Boot0001",
            &load_option(
                "00000000-0000-4000-8000-00000000000a",
                WIN_ESP.0,
                204_800,
                1,
                "\\EFI\\Microsoft\\Boot\\bootmgfw.efi",
                "Windows Boot Manager",
            ),
            EFI_GLOBAL,
        );
        fixture
    }

    fn write_var(&self, name: &str, data: &[u8], vendor: &str) {
        fs::write(self.efivars.join(format!("{name}-{vendor}")), data).unwrap();
    }

    fn var(&self, name: &str) -> Option<Vec<u8>> {
        fs::read(self.efivars.join(format!("{name}-{EFI_GLOBAL}"))).ok()
    }

    fn set_loader(&self, value: &str) {
        let mut data = vec![6, 0, 0, 0];
        data.extend(utf16(&format!("{value}\0")));
        self.write_var("LoaderDevicePartUUID", &data, LOADER_VENDOR);
    }

    fn set_handoff(&self, document: &Value, esp_only: bool) {
        self.set_handoff_text(&document.to_string(), esp_only);
    }

    fn set_handoff_text(&self, text: &str, esp_only: bool) {
        fs::write(self.esp.join("EFI/tilo-staging/handoff.json"), text).unwrap();
        if !esp_only {
            fs::write(&self.copy, text).unwrap();
        }
    }

    fn run(&self, words: &[&str]) -> Outcome {
        let args: Vec<OsString> = words.iter().map(OsString::from).collect();
        run(self, &args)
    }

    fn check(&self, mode: &str, overrides: &[(&str, i64)]) -> Outcome {
        let mut values = vec![
            ("free-start-bytes", TARGET.0 * 512),
            ("free-size-bytes", (TARGET.1 - TARGET.0 + 1) * 512),
            ("efi-start-bytes", TARGET.0 * 512),
            ("efi-size-bytes", 2 * GIB),
            ("boot-start-bytes", TARGET.0 * 512 + 2 * GIB),
            ("boot-size-bytes", GIB),
            ("root-start-bytes", TARGET.0 * 512 + 3 * GIB),
            ("root-size-bytes", (TARGET.1 - TARGET.0 + 1) * 512 - 3 * GIB),
            ("sector-size", 512),
        ];
        for (key, value) in overrides {
            values.iter_mut().find(|(k, _)| k == key).unwrap().1 = *value;
        }
        let copy = self.copy.to_string_lossy().into_owned();
        let mut argv: Vec<String> = [
            "check-install",
            "--handoff",
            &copy,
            "--disk",
            DISK,
            "--mode",
            mode,
        ]
        .iter()
        .map(|word| (*word).to_owned())
        .collect();
        for (key, value) in values {
            argv.push(format!("--{key}"));
            argv.push(value.to_string());
        }
        let words: Vec<&str> = argv.iter().map(String::as_str).collect();
        self.run(&words)
    }

    fn efibootmgr(&self, args: &[&str]) -> Ran {
        self.log.borrow_mut().push(args.join(" "));
        let args: Vec<&str> = args.iter().copied().filter(|arg| *arg != "-q").collect();
        let ok = Ran {
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        };
        match args.as_slice() {
            ["-b", id, "-B"] => {
                fs::remove_file(self.efivars.join(format!("Boot{id}-{EFI_GLOBAL}"))).unwrap();
            }
            ["-n", id] => self.write_var("BootNext", &ids(&[id]), EFI_GLOBAL),
            ["-c", ..] => {
                let created = self.create.borrow().clone().unwrap();
                self.write_var("Boot0105", &created, EFI_GLOBAL);
                let order = self.var("BootOrder").unwrap();
                let mut data = vec![7, 0, 0, 0];
                data.extend(0x105u16.to_le_bytes());
                data.extend(&order[4..]);
                self.write_var("BootOrder", &data, EFI_GLOBAL);
            }
            ["-o", order] => {
                let order: Vec<&str> = order.split(',').collect();
                self.write_var("BootOrder", &ids(&order), EFI_GLOBAL);
            }
            _ => {
                return Ran {
                    code: Some(2),
                    ..ok
                };
            }
        }
        ok
    }
}

impl Host for Fixture {
    fn efivars(&self) -> PathBuf {
        self.efivars.clone()
    }

    fn efibootmgr(&self) -> String {
        "fake-efibootmgr".into()
    }

    fn run(&self, argv: &[&str]) -> Result<Ran, Fail> {
        match argv {
            ["fake-efibootmgr", rest @ ..] => Ok(self.efibootmgr(rest)),
            ["sfdisk", "--json", _] => {
                let table = self
                    .sfdisk
                    .borrow()
                    .clone()
                    .expect("sfdisk is only asked without udev");
                Ok(Ran {
                    code: Some(0),
                    stdout: json!({"partitiontable": table}).to_string(),
                    stderr: String::new(),
                })
            }
            other => panic!("unexpected tool {other:?}"),
        }
    }

    fn esp_handoff(&self, _esp_partuuid: &str) -> Result<Vec<u8>, Fail> {
        read_bounded(&self.esp.join("EFI/tilo-staging/handoff.json"))
    }

    fn lsblk(&self, _disk: &str) -> Result<Value, Fail> {
        Ok(self.lsblk.borrow().clone())
    }
}

fn assert_refused(result: &Outcome, fragment: &str) {
    assert_eq!(result.code, 1, "{result:?}");
    assert!(result.stderr.contains(fragment), "{fragment}: {result:?}");
    assert_eq!(result.stdout, "", "a refusal must not report facts");
}

fn facts(outcome: &Outcome) -> std::collections::BTreeMap<String, String> {
    outcome
        .stdout
        .lines()
        .map(|line| {
            let (key, value) = line.split_once('=').unwrap();
            (key.to_owned(), value.to_owned())
        })
        .collect()
}

#[test]
fn exact_staged_plan_is_accepted_and_reports_its_facts() {
    let fixture = Fixture::new();
    let result = fixture.check("guided", &[]);
    assert_eq!(result.code, 0, "{result:?}");
    let facts = facts(&result);
    assert_eq!(facts["INTERNAL_OWNED_BOOT_ENTRY"], "0104");
    assert_eq!(facts["INTERNAL_DISK_GUID"], DISK_GUID);
    assert_eq!(facts["INTERNAL_STAGING_ROOT_PARTUUID"], ROOT_UUID);
    assert_eq!(facts["INTERNAL_STAGING_ESP_PARTUUID"], ESP_UUID);
    assert_eq!(facts["INTERNAL_HOST_OS"], "windows");
    assert_eq!(
        result
            .stdout
            .lines()
            .map(|l| l.split('=').next().unwrap())
            .collect::<Vec<_>>(),
        [
            "INTERNAL_DISK_GUID",
            "INTERNAL_HOST_OS",
            "INTERNAL_OWNED_BOOT_ENTRY",
            "INTERNAL_STAGING_ESP_PARTUUID",
            "INTERNAL_STAGING_ROOT_PARTUUID"
        ]
    );
}

/// lsblk without a udev database: every GPT identity field is empty, and
/// sfdisk reports the GPT itself.
fn udevless(fixture: &Fixture, edit: impl Fn(&mut Value)) {
    let mut document = lsblk_document();
    let disk = &mut document["blockdevices"][0];
    let mut table = json!({"label": "gpt", "id": disk["ptuuid"], "partitions": []});
    disk["pttype"] = Value::Null;
    disk["ptuuid"] = Value::Null;
    for child in disk["children"].as_array_mut().unwrap() {
        table["partitions"].as_array_mut().unwrap().push(json!({
            "node": child["name"],
            "uuid": child["partuuid"].as_str().unwrap().to_uppercase(),
            "type": child["parttype"].as_str().unwrap().to_uppercase(),
        }));
        for key in ["partuuid", "parttype", "pttype", "ptuuid"] {
            child[key] = Value::Null;
        }
    }
    edit(&mut table);
    *fixture.lsblk.borrow_mut() = document;
    *fixture.sfdisk.borrow_mut() = Some(table);
}

#[test]
fn identities_missing_from_lsblk_are_read_from_the_gpt() {
    // Catches the guard depending on udev: in a container or early boot lsblk
    // reports no GPT identities, and the facts must then come from the GPT.
    let fixture = Fixture::new();
    udevless(&fixture, |_| {});
    let result = fixture.check("guided", &[]);
    assert_eq!(result.code, 0, "{result:?}");
    assert!(
        result
            .stdout
            .contains(&format!("INTERNAL_DISK_GUID={DISK_GUID}"))
    );
}

#[test]
fn a_gpt_that_disagrees_with_lsblk_is_refused() {
    let fixture = Fixture::new();
    udevless(&fixture, |table| {
        table["id"] = json!("99999999-2222-4333-8444-555555555555");
    });
    assert_refused(&fixture.check("guided", &[]), "disk GUID differs");
}

#[test]
fn strict_parser_rejects_unknown_duplicate_and_malformed_fields() {
    let fixture = Fixture::new();
    let mut cases = Vec::new();
    let mut extra = handoff_document();
    extra["target_disk"] = json!(DISK);
    cases.push(extra.to_string());
    let plain = handoff_document().to_string();
    cases.push(format!("{}, \"schema\": 1}}", &plain[..plain.len() - 1]));
    let mut bad_entry = handoff_document();
    bad_entry["owned_boot_entry"] = json!("0104; reboot");
    cases.push(bad_entry.to_string());
    let mut profile = handoff_document();
    profile["profile"]["password"] = json!("secret");
    cases.push(profile.to_string());
    let mut in_order = handoff_document();
    in_order["original_boot_order"]
        .as_array_mut()
        .unwrap()
        .push(json!("0104"));
    cases.push(in_order.to_string());
    let mut boolean = handoff_document();
    boolean["disk_size_bytes"] = json!(true);
    cases.push(boolean.to_string());
    cases.push(format!("{plain}{}", " ".repeat(64 * 1024)));
    for text in &cases {
        fixture.set_handoff_text(text, false);
        assert_eq!(
            fixture.check("guided", &[]).code,
            1,
            "{}",
            &text[..200.min(text.len())]
        );
    }
}

#[test]
fn refusals_name_the_field_as_the_script_did() {
    let fixture = Fixture::new();
    let plain = handoff_document().to_string();
    fixture.set_handoff_text(
        &format!("{}, \"schema\": 1}}", &plain[..plain.len() - 1]),
        false,
    );
    assert_refused(
        &fixture.check("guided", &[]),
        "handoff repeats the field 'schema'",
    );
    fixture.set_handoff_text(&format!("{plain}{}", " ".repeat(64 * 1024)), false);
    assert_refused(&fixture.check("guided", &[]), "is larger than 65536 bytes");
    let mut document = handoff_document();
    document["staging_root"]["partuuid"] = json!(ESP_UUID);
    fixture.set_handoff(&document, false);
    assert_refused(
        &fixture.check("guided", &[]),
        "handoff GUIDs are not distinct",
    );
    let mut document = handoff_document();
    document["staging_esp"]["last_lba"] = json!(1);
    fixture.set_handoff(&document, false);
    assert_refused(
        &fixture.check("guided", &[]),
        "handoff field staging_esp ends before it starts",
    );
    let mut document = handoff_document();
    document["profile"]["hostname"] = json!("h".repeat(257));
    fixture.set_handoff(&document, false);
    assert_refused(
        &fixture.check("guided", &[]),
        "handoff profile field hostname is not a bounded string",
    );
}

#[test]
fn whole_disk_and_prepared_modes_are_refused_on_the_live_disk() {
    let fixture = Fixture::new();
    for mode in ["erase", "prepared"] {
        assert_refused(&fixture.check(mode, &[]), "accepts only guided alongside");
    }
    let invalid = fixture.check("other", &[]);
    assert_eq!(invalid.code, 2);
    assert!(
        invalid
            .stderr
            .contains("argument --mode: invalid choice: 'other'")
    );
}

#[test]
fn mismatched_disk_guid_or_size_is_refused() {
    let fixture = Fixture::new();
    let mut document = lsblk_document();
    document["blockdevices"][0]["ptuuid"] = json!("99999999-2222-4333-8444-555555555555");
    *fixture.lsblk.borrow_mut() = document;
    assert_refused(&fixture.check("guided", &[]), "disk GUID differs");
    let mut document = lsblk_document();
    document["blockdevices"][0]["size"] = json!(DISK_SECTORS * 512 + 512);
    *fixture.lsblk.borrow_mut() = document;
    assert_refused(&fixture.check("guided", &[]), "disk size differs");
}

#[test]
fn extent_that_is_no_longer_unallocated_is_refused() {
    let fixture = Fixture::new();
    let mut document = lsblk_document();
    document["blockdevices"][0]["children"]
        .as_array_mut()
        .unwrap()
        .push(partition(
            &format!("{DISK}p5"),
            "00000000-0000-4000-8000-00000000000c",
            LINUX_TYPE,
            (TARGET.0 + 2048, TARGET.0 + 4095),
            &[],
        ));
    *fixture.lsblk.borrow_mut() = document;
    assert_refused(&fixture.check("guided", &[]), "no longer unallocated");
}

#[test]
fn handoff_whose_extents_overlap_is_refused() {
    let fixture = Fixture::new();
    let mut document = handoff_document();
    document["target_extent"]["last_lba"] = json!(STAGING_ESP.0);
    fixture.set_handoff(&document, false);
    assert_refused(&fixture.check("guided", &[]), "overlap");
}

#[test]
fn guided_plan_must_equal_the_staged_extent_and_end_at_staging() {
    let fixture = Fixture::new();
    assert_refused(
        &fixture.check(
            "guided",
            &[("free-start-bytes", TARGET.0 * 512 + 1024 * 1024)],
        ),
        "differs from the staged target extent",
    );
    let shorter = (TARGET.1 - TARGET.0 + 1) * 512 - 2 * GIB - 1024 * 1024;
    assert_refused(
        &fixture.check("guided", &[("root-size-bytes", shorter)]),
        "end exactly where the staged installer begins",
    );
}

#[test]
fn handoff_naming_a_different_esp_than_the_booted_one_is_refused() {
    let fixture = Fixture::new();
    fixture.set_loader("dddddddd-0000-4000-8000-000000000004");
    assert_refused(&fixture.check("guided", &[]), "different ESP");
}

#[test]
fn ui_copy_must_match_the_booted_staging_esp() {
    let fixture = Fixture::new();
    let mut document = handoff_document();
    document["target_extent"]["first_lba"] = json!(TARGET.0 + 2048);
    fixture.set_handoff(&document, true);
    assert_refused(
        &fixture.check("guided", &[]),
        "differs from the staging ESP",
    );
    fs::remove_file(fixture.esp.join("EFI/tilo-staging/handoff.json")).unwrap();
    assert_refused(&fixture.check("guided", &[]), "no longer holds a handoff");
}

#[test]
fn live_root_elsewhere_or_foreign_mounts_are_refused() {
    let fixture = Fixture::new();
    let mut document = lsblk_document();
    document["blockdevices"][0]["children"][3]["mountpoints"] = json!([null]);
    *fixture.lsblk.borrow_mut() = document;
    assert_refused(&fixture.check("guided", &[]), "not the staging root");
    let mut document = lsblk_document();
    document["blockdevices"][0]["children"][1]["mountpoints"] = json!(["/mnt/windows"]);
    *fixture.lsblk.borrow_mut() = document;
    assert_refused(
        &fixture.check("guided", &[]),
        "only the staged installer may be in use",
    );
}

#[test]
fn staging_partition_with_changed_identity_is_refused() {
    let fixture = Fixture::new();
    let mut document = lsblk_document();
    document["blockdevices"][0]["children"][2]["parttype"] = json!(LINUX_TYPE);
    *fixture.lsblk.borrow_mut() = document;
    assert_refused(
        &fixture.check("guided", &[]),
        "staging ESP has an unexpected partition type",
    );
    let mut document = lsblk_document();
    document["blockdevices"][0]["children"][3]["start"] = json!(STAGING_ROOT.0 + 2048);
    *fixture.lsblk.borrow_mut() = document;
    assert_refused(&fixture.check("guided", &[]), "staging root extent differs");
    let mut document = lsblk_document();
    document["blockdevices"][0]["children"][3]["parttype"] = json!(LINUX_TYPE);
    *fixture.lsblk.borrow_mut() = document;
    assert_refused(
        &fixture.check("guided", &[]),
        "staging root has an unexpected partition type",
    );
}

#[test]
fn check_mounts_accepts_only_the_staging_partitions_in_use() {
    let fixture = Fixture::new();
    let copy = fixture.copy.to_string_lossy().into_owned();
    let result = fixture.run(&["check-mounts", "--handoff", &copy, "--disk", DISK]);
    assert_eq!((result.code, result.stdout.as_str()), (0, ""), "{result:?}");
    let mut document = lsblk_document();
    document["blockdevices"][0]["children"][1]["mountpoints"] = json!(["/mnt/windows"]);
    *fixture.lsblk.borrow_mut() = document;
    assert_refused(
        &fixture.run(&["check-mounts", "--handoff", &copy, "--disk", DISK]),
        "only the staged installer may be in use",
    );
}

fn detect(fixture: &Fixture) -> (PathBuf, PathBuf, Outcome) {
    let out = fixture.root.join("run/handoff.json");
    let error = fixture.root.join("run/handoff.error");
    let result = fixture.run(&[
        "detect",
        "--out",
        &out.to_string_lossy(),
        "--error",
        &error.to_string_lossy(),
    ]);
    assert_eq!(result.code, 0, "{result:?}");
    (out, error, result)
}

#[test]
fn detect_publishes_a_normalized_copy_for_the_unprivileged_ui() {
    let fixture = Fixture::new();
    let (out, error, result) = detect(&fixture);
    let published: Value = serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(published["disk_guid"], DISK_GUID);
    assert_eq!(published["staging_esp"]["partuuid"], ESP_UUID);
    let mode = std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&out).unwrap().permissions());
    assert_eq!(mode & 0o777, 0o644);
    assert!(!error.exists());
    assert_eq!(
        result.stdout,
        format!("tilo-staging-handoff: staged installation for disk {DISK_GUID} recognised\n")
    );
    // Canonical: sorted keys, no spaces, one newline.
    let text = fs::read_to_string(&out).unwrap();
    assert!(text.starts_with(
        "{\"disk_guid\":\"11111111-2222-4333-8444-555555555555\",\"disk_size_bytes\":"
    ));
    assert!(text.ends_with("}\n"));
}

#[test]
fn usb_media_without_a_handoff_publish_nothing() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.esp.join("EFI/tilo-staging/handoff.json")).unwrap();
    let (out, error, _) = detect(&fixture);
    assert!(!out.exists());
    assert!(!error.exists());
}

#[test]
fn handoff_naming_another_esp_is_reported_not_published() {
    let fixture = Fixture::new();
    fixture.set_loader("dddddddd-0000-4000-8000-000000000004");
    let (out, error, result) = detect(&fixture);
    assert!(!out.exists());
    assert!(
        fs::read_to_string(&error)
            .unwrap()
            .contains("different ESP")
    );
    assert!(
        result
            .stderr
            .starts_with("tilo-staging-handoff: the staging handoff names a different ESP")
    );
}

fn finalize(fixture: &Fixture, extra: &[&str]) -> Outcome {
    let copy = fixture.copy.to_string_lossy().into_owned();
    let mut words = vec![
        "firmware-finalize",
        "--handoff",
        &copy,
        "--target-esp-partuuid",
        TARGET_ESP_UUID,
    ];
    words.extend_from_slice(extra);
    fixture.run(&words)
}

fn add_target_entry(fixture: &Fixture, entry_id: &str) {
    fixture.write_var(
        &format!("Boot{entry_id}"),
        &load_option(
            TARGET_ESP_UUID,
            TARGET.0,
            4_194_304,
            5,
            "\\EFI\\systemd\\systemd-bootx64.efi",
            "Linux Boot Manager",
        ),
        EFI_GLOBAL,
    );
}

#[test]
fn matching_owned_entry_is_deleted_and_bootnext_targets_tilo() {
    let fixture = Fixture::new();
    add_target_entry(&fixture, "0006");
    let before = fixture.var("BootOrder");
    let result = finalize(&fixture, &[]);
    assert_eq!(result.code, 0, "{result:?}");
    assert_eq!(fixture.var("Boot0104"), None);
    assert_eq!(fixture.var("BootNext"), Some(ids(&["0006"])));
    assert_eq!(
        fixture.var("BootOrder"),
        before,
        "BootOrder must stay unchanged"
    );
    assert!(fixture.var("Boot0001").is_some());
    assert_eq!(
        result.stdout,
        "removed the staging boot entry Boot0104\n\
         the next restart starts the installed Tilo (Boot0006) once; the default is unchanged\n"
    );
}

#[test]
fn owned_entry_that_no_longer_points_at_staging_is_left_alone() {
    let fixture = Fixture::new();
    add_target_entry(&fixture, "0006");
    let foreign = load_option(
        "00000000-0000-4000-8000-00000000000a",
        WIN_ESP.0,
        204_800,
        1,
        "\\EFI\\BOOT\\BOOTX64.EFI",
        "Tilo installer",
    );
    fixture.write_var("Boot0104", &foreign, EFI_GLOBAL);
    let result = finalize(&fixture, &[]);
    assert_eq!(result.code, 3, "{result:?}");
    assert_eq!(fixture.var("Boot0104"), Some(foreign));
    assert!(!fixture.log.borrow().iter().any(|line| line.contains("-B")));
    // Tilo is still what the next restart starts.
    assert_eq!(fixture.var("BootNext"), Some(ids(&["0006"])));
}

#[test]
fn owned_entry_with_a_different_extent_is_left_alone() {
    let fixture = Fixture::new();
    add_target_entry(&fixture, "0006");
    let moved = staging_option(
        ESP_UUID,
        STAGING_ESP.0 + 2048,
        STAGING_ESP.1 - STAGING_ESP.0 + 1,
    );
    fixture.write_var("Boot0104", &moved, EFI_GLOBAL);
    assert_eq!(finalize(&fixture, &[]).code, 3);
    assert_eq!(fixture.var("Boot0104"), Some(moved));
}

#[test]
fn missing_target_entry_is_created_without_changing_bootorder() {
    let fixture = Fixture::new();
    *fixture.create.borrow_mut() = Some(load_option(
        TARGET_ESP_UUID,
        TARGET.0,
        4_194_304,
        5,
        "\\EFI\\systemd\\systemd-bootx64.efi",
        "Tilo",
    ));
    let before = fixture.var("BootOrder");
    let result = finalize(
        &fixture,
        &[
            "--target-disk",
            DISK,
            "--target-esp-partition-number",
            "5",
            "--loader",
            "\\EFI\\systemd\\systemd-bootx64.efi",
        ],
    );
    assert_eq!(result.code, 0, "{result:?}");
    assert_eq!(fixture.var("BootNext"), Some(ids(&["0105"])));
    assert_eq!(fixture.var("BootOrder"), before);
    assert!(
        fixture
            .log
            .borrow()
            .iter()
            .any(|line| line == "-q -o 0001,0000")
    );
}

#[test]
fn ambiguous_or_absent_target_entry_does_not_set_bootnext() {
    let fixture = Fixture::new();
    assert_eq!(finalize(&fixture, &[]).code, 1);
    assert_eq!(fixture.var("BootNext"), None);
    // The staging entry is gone already; the next run reports that.
    add_target_entry(&fixture, "0006");
    add_target_entry(&fixture, "0007");
    let result = finalize(&fixture, &[]);
    assert_eq!(result.code, 1);
    assert!(
        result.stderr.contains("exactly one firmware entry"),
        "{result:?}"
    );
    assert!(
        result
            .stdout
            .contains("staging boot entry Boot0104 is already gone")
    );
    assert_eq!(fixture.var("BootNext"), None);
}

#[test]
fn record_binds_both_staging_identities_and_the_adjacent_target_root() {
    let fixture = Fixture::new();
    let var = fixture.root.join("state/os/default/var");
    fs::create_dir_all(&var).unwrap();
    let root_start = TARGET.0 * 512 + 3 * GIB;
    let root_size = (TARGET.1 + 1) * 512 - root_start;
    let copy = fixture.copy.to_string_lossy().into_owned();
    let var_text = var.to_string_lossy().into_owned();
    let start = root_start.to_string();
    let record = |size: i64| {
        let size = size.to_string();
        fixture.run(&[
            "write-reclaim-record",
            "--handoff",
            &copy,
            "--target-var",
            &var_text,
            "--target-root-partuuid",
            "EEEEEEEE-0000-4000-8000-000000000005",
            "--target-esp-partuuid",
            TARGET_ESP_UUID,
            "--target-root-start-bytes",
            &start,
            "--sector-size",
            "512",
            "--target-root-size-bytes",
            &size,
        ])
    };
    let result = record(root_size);
    assert_eq!(result.code, 0, "{result:?}");
    assert_eq!(
        result.stdout,
        "recorded staging reclaim for the installed system\n"
    );
    let record_path = var.join("lib/tilo/staging-reclaim.json");
    let written: Value = serde_json::from_str(&fs::read_to_string(&record_path).unwrap()).unwrap();
    let mode =
        std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&record_path).unwrap().permissions());
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(written["disk_guid"], DISK_GUID);
    assert_eq!(written["staging_esp"]["partuuid"], ESP_UUID);
    assert_eq!(written["staging_root"]["last_lba"], STAGING_ROOT.1);
    assert_eq!(
        written["target_root"]["partuuid"],
        "eeeeeeee-0000-4000-8000-000000000005"
    );
    assert_eq!(
        written["target_root"]["last_lba"].as_i64().unwrap() + 1,
        STAGING_ESP.0
    );
    assert_eq!(written["sector_bytes"], 512);
    let short = record(root_size - 1024 * 1024);
    assert_eq!(
        short.code, 1,
        "a root that does not end at staging is not reclaimable"
    );
}

#[test]
fn usage_errors_exit_2() {
    let fixture = Fixture::new();
    assert_eq!(fixture.run(&[]).code, 2);
    let unknown = fixture.run(&["reboot"]);
    assert_eq!(unknown.code, 2);
    assert!(unknown.stderr.contains("invalid choice: 'reboot'"));
    let missing = fixture.run(&["check-mounts", "--handoff", "x"]);
    assert_eq!(missing.code, 2);
    assert!(
        missing
            .stderr
            .ends_with("error: the following arguments are required: --disk\n")
    );
    let not_int = fixture.run(&[
        "write-reclaim-record",
        "--handoff",
        "x",
        "--target-var",
        "v",
        "--target-root-partuuid",
        "a",
        "--target-esp-partuuid",
        "b",
        "--target-root-start-bytes",
        "x",
        "--target-root-size-bytes",
        "1",
        "--sector-size",
        "512",
    ]);
    assert_eq!(not_int.code, 2);
    assert!(
        not_int
            .stderr
            .contains("argument --target-root-start-bytes: invalid int value: 'x'")
    );
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// The backend must route every live-disk install through the helper.
#[test]
fn backend_wiring_order() {
    let source = fs::read_to_string(repo().join("os/usr/libexec/tilo-install")).unwrap();
    assert!(source.contains("is the live medium's own disk — refusing to install onto it"));
    let guard = source
        .split_once("INTERNAL_MODE=0\nif [ \"$TARGET_MAJ_MIN\" = \"$LIVE_MAJ_MIN\" ]; then")
        .unwrap()
        .1;
    let guard = guard.split_once("\nfi\n").unwrap().0;
    assert!(
        guard.find("[ -n \"$INTERNAL_HANDOFF\" ]").unwrap() < guard.find("check-install").unwrap()
    );
    assert!(guard.contains("--mode guided"));
    assert!(guard.contains("INTERNAL_MODE=1"));
    assert!(source.contains("requires the disk that holds the staged installer"));
    // An internal handoff rejects erase and prepared before any disk work.
    let check = source
        .find("--internal-handoff accepts only --guided-alongside")
        .unwrap();
    assert!(check < source.find("validate_disk_identity\n").unwrap());
    // Firmware is finalized only after the encrypted root is closed.
    let finish = source.find("\"$DISK_ENCRYPT\" finish").unwrap();
    let record = source.find("write-reclaim-record").unwrap();
    let finalize = source.find("firmware-finalize --handoff").unwrap();
    assert!(record < finish && finish < finalize);
    assert!(source.contains("[ \"$INTERNAL_MODE\" -eq 0 ] || GUIDED_ARGS+=(--in-use-disk)"));
    assert!(source.contains("check-mounts --handoff"));
    assert!(source.contains("STAGING_HANDOFF=/usr/libexec/tilo-staging-handoff\n"));
    // The guided helper skips only the whole-table reread on an in-use disk.
    let guided = fs::read_to_string(repo().join("os/usr/libexec/tilo-guided-partition")).unwrap();
    assert!(
        guided.contains("if [ \"$IN_USE_DISK\" -eq 0 ]; then\n  blockdev --rereadpt \"$DISK\"\nfi")
    );
    assert!(guided.contains(
        "if [ \"$IN_USE_DISK\" -eq 1 ]; then\n    partx -a -n \"$new_number:$new_number\" \"$DISK\" || true\n  fi"
    ));
    assert!(guided.contains(
        "kernel_partition_matches \"$new_node\" \"$new_number\" \"$new_start\" \"$new_sectors\" \"$new_uuid\""
    ));
    assert!(guided.contains("blkid -p -s PART_ENTRY_UUID -o value \"$node\""));
    assert!(
        guided.find("kernel_partition_matches() {").unwrap()
            < guided.find("mkfs.vfat -n TILO-EFI").unwrap()
    );
    assert!(guided.contains("[ $((ROOT_START + ROOT_SIZE)) -eq $((FREE_START + FREE_SIZE)) ]"));
}
