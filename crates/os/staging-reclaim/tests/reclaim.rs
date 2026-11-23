//! Installed-system staging reclaim against a simulated GPT: the fake tools
//! apply sfdisk's delete and resize requests to one partition table, so the
//! cases observe the resulting map rather than command lines alone. No real
//! disk is touched; `os/tests/staging_reclaim_loop.sh` repeats the core path
//! on a loop device as root.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::panic_in_result_fn,
    reason = "a test fixture: any failed fixture call fails the test, and it computes fixed disk geometry"
)]

use std::cell::RefCell;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use advisory_lock::FileLock;
use serde_json::{Value, json};
use tilo_staging_reclaim::{Error, Host, Outcome, main};

const ESP_TYPE: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
const LINUX_TYPE: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";
const STAGING_ROOT_TYPE: &str = "d4c4daf6-f72c-43ee-b8e3-5ba32550311d";
const TILO_ROOT: &str = "7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab";
const MSFT_TYPE: &str = "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7";
const GIB_SECTORS: i64 = 1024 * 1024 * 1024 / 512;
const DISK: &str = "/dev/vda";
const DISK_GUID: &str = "11111111-2222-4333-8444-555555555555";
const STAGING_ESP_GUID: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const STAGING_ROOT_GUID: &str = "bbbbbbbb-0000-4000-8000-000000000002";
const ROOT_GUID: &str = "eeeeeeee-0000-4000-8000-000000000005";
const RECOVERY_GUID: &str = "00000000-0000-4000-8000-00000000000d";
const WINDOWS_GUID: &str = "00000000-0000-4000-8000-00000000000b";

const TARGET_ESP: (i64, i64) = (
    100 * GIB_SECTORS + 2048,
    100 * GIB_SECTORS + 2048 + 2 * GIB_SECTORS - 1,
);
const TARGET_ROOT: (i64, i64) = (TARGET_ESP.1 + 1, TARGET_ESP.1 + 62 * GIB_SECTORS);
const STAGING_ESP: (i64, i64) = (TARGET_ROOT.1 + 1, TARGET_ROOT.1 + GIB_SECTORS);
const STAGING_ROOT: (i64, i64) = (STAGING_ESP.1 + 1, STAGING_ESP.1 + 40 * GIB_SECTORS);
const RECOVERY: (i64, i64) = (STAGING_ROOT.1 + 1, STAGING_ROOT.1 + GIB_SECTORS);
const DISK_SIZE: i64 = (RECOVERY.1 + 4096) * 512;

#[derive(Clone)]
struct Part {
    number: u32,
    uuid: String,
    kind: String,
    start: i64,
    size: i64,
    mounts: Vec<String>,
    children: Value,
}

fn part(number: u32, uuid: &str, kind: &str, extent: (i64, i64)) -> Part {
    Part {
        number,
        uuid: uuid.to_owned(),
        kind: kind.to_owned(),
        start: extent.0,
        size: extent.1 - extent.0 + 1,
        mounts: Vec::new(),
        children: Value::Null,
    }
}

struct Session {
    id: &'static str,
    class: &'static str,
    uid: &'static str,
    state: &'static str,
}

struct State {
    ptuuid: String,
    partitions: Vec<Part>,
    sessions: Vec<Session>,
    fail_cryptsetup: bool,
    fstype: &'static str,
    calls: Vec<Vec<String>>,
}

fn initial_state() -> State {
    let mut target_esp = part(
        5,
        "cccccccc-0000-4000-8000-000000000003",
        ESP_TYPE,
        TARGET_ESP,
    );
    target_esp.mounts = vec!["/boot/efi".to_owned()];
    let mut root = part(6, ROOT_GUID, TILO_ROOT, TARGET_ROOT);
    root.children =
        json!([{"name": "/dev/mapper/luks-1234", "type": "crypt", "mountpoints": ["/sysroot"]}]);
    State {
        ptuuid: DISK_GUID.to_owned(),
        sessions: vec![Session {
            id: "2",
            class: "user",
            uid: "1000",
            state: "active",
        }],
        partitions: vec![
            part(
                1,
                "00000000-0000-4000-8000-00000000000a",
                ESP_TYPE,
                (2048, 206_847),
            ),
            part(2, WINDOWS_GUID, MSFT_TYPE, (206_848, 100 * GIB_SECTORS - 1)),
            target_esp,
            root,
            part(3, STAGING_ESP_GUID, ESP_TYPE, STAGING_ESP),
            part(4, STAGING_ROOT_GUID, STAGING_ROOT_TYPE, STAGING_ROOT),
            part(7, RECOVERY_GUID, MSFT_TYPE, RECOVERY),
        ],
        fail_cryptsetup: false,
        fstype: "btrfs",
        calls: Vec::new(),
    }
}

fn record() -> Value {
    json!({
        "schema": 1, "operation_id": "0f0e0d0c-0b0a-4908-8706-050403020100",
        "disk_guid": DISK_GUID, "disk_size_bytes": DISK_SIZE, "sector_bytes": 512,
        "staging_esp": {"partuuid": STAGING_ESP_GUID, "first_lba": STAGING_ESP.0, "last_lba": STAGING_ESP.1},
        "staging_root": {"partuuid": STAGING_ROOT_GUID, "first_lba": STAGING_ROOT.0, "last_lba": STAGING_ROOT.1},
        "target_root": {"partuuid": ROOT_GUID, "first_lba": TARGET_ROOT.0, "last_lba": TARGET_ROOT.1},
        "target_esp_partuuid": "cccccccc-0000-4000-8000-000000000003",
    })
}

/// The machine: one disk whose GPT the fake tools read and change.
struct Machine {
    _work: tempfile::TempDir,
    record: PathBuf,
    state: RefCell<State>,
    log: RefCell<Vec<String>>,
}

impl Machine {
    fn new() -> Self {
        let work = tempfile::tempdir().unwrap();
        let record_path = work.path().join("var/lib/tilo/staging-reclaim.json");
        fs::create_dir_all(record_path.parent().unwrap()).unwrap();
        fs::write(&record_path, record().to_string()).unwrap();
        Self {
            _work: work,
            record: record_path,
            state: RefCell::new(initial_state()),
            log: RefCell::new(Vec::new()),
        }
    }

    fn with(state: State) -> Self {
        let machine = Self::new();
        *machine.state.borrow_mut() = state;
        machine
    }

    fn partitions(&self) -> Vec<(String, (i64, i64))> {
        let mut parts: Vec<_> = self
            .state
            .borrow()
            .partitions
            .iter()
            .map(|p| (p.uuid.clone(), (p.start, p.start + p.size - 1)))
            .collect();
        parts.sort();
        parts
    }

    fn extent(&self, uuid: &str) -> Option<(i64, i64)> {
        self.partitions()
            .into_iter()
            .find(|(u, _)| u == uuid)
            .map(|(_, e)| e)
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.state.borrow().calls.clone()
    }

    fn mutations(&self) -> Vec<Vec<String>> {
        self.calls()
            .into_iter()
            .filter(|c| {
                c[0] == "cryptsetup" || c[0] == "btrfs" || (c[0] == "sfdisk" && c[1] != "--json")
            })
            .collect()
    }

    fn log(&self) -> Vec<String> {
        self.log.borrow().clone()
    }

    fn run(&self) -> i32 {
        main(self)
    }
}

fn ok(stdout: String) -> Outcome {
    Outcome {
        code: 0,
        stdout,
        stderr: String::new(),
    }
}

impl Host for Machine {
    fn record(&self) -> PathBuf {
        self.record.clone()
    }

    fn lock(&self, _disk: &str) -> Result<Option<FileLock>, Error> {
        Ok(None)
    }

    fn log(&self, message: &str) {
        self.log
            .borrow_mut()
            .push(format!("tilo-staging-reclaim: {message}"));
    }

    fn run(&self, argv: &[&str], input: Option<&str>) -> Result<Outcome, Error> {
        let mut state = self.state.borrow_mut();
        state
            .calls
            .push(argv.iter().map(|a| (*a).to_owned()).collect());
        let node = |p: &Part| format!("{DISK}{}", p.number);
        let tool = argv[0];
        let rest = &argv[1..];
        Ok(match tool {
            "lsblk" if rest.contains(&"-d") => ok(json!({"blockdevices": [{"name": DISK, "type": "disk",
                "pttype": "gpt", "ptuuid": state.ptuuid, "size": DISK_SIZE}]})
            .to_string()),
            "lsblk" => {
                let wanted = *rest.last().unwrap();
                let p = state.partitions.iter().find(|p| node(p) == wanted).unwrap();
                let mounts: Value = if p.mounts.is_empty() { json!([null]) } else { json!(p.mounts) };
                ok(json!({"blockdevices": [{"name": node(p), "type": "part", "mountpoints": mounts,
                    "children": p.children}]})
                .to_string())
            }
            "sfdisk" if rest[0] == "--json" => ok(json!({"partitiontable": {"label": "gpt",
                "id": state.ptuuid.to_uppercase(), "device": DISK, "unit": "sectors", "sectorsize": 512,
                "partitions": state.partitions.iter().map(|p| json!({"node": node(p), "start": p.start,
                    "size": p.size, "type": p.kind.to_uppercase(), "uuid": p.uuid.to_uppercase()}))
                    .collect::<Vec<_>>()}})
            .to_string()),
            "sfdisk" if rest.contains(&"--delete") => {
                let number: u32 = rest.last().unwrap().parse().unwrap();
                state.partitions.retain(|p| p.number != number);
                ok(String::new())
            }
            "sfdisk" if rest.contains(&"-N") => {
                let number: u32 = rest[rest.iter().position(|a| *a == "-N").unwrap() + 1].parse().unwrap();
                let request = input.unwrap();
                let (start, size) = request.trim().split_once(", ").unwrap();
                let start: i64 = start.strip_prefix("start=").unwrap().parse().unwrap();
                let size: i64 = size.strip_prefix("size=").unwrap().parse().unwrap();
                let p = state.partitions.iter_mut().find(|p| p.number == number).unwrap();
                assert_eq!(start, p.start);
                p.size = size;
                ok(String::new())
            }
            "cryptsetup" if state.fail_cryptsetup => Outcome {
                code: 1,
                stdout: String::new(),
                stderr: "no key available\n".to_owned(),
            },
            "findmnt" => ok(format!("{}\n", state.fstype)),
            "loginctl" if rest[0] == "list-sessions" => ok(state.sessions.iter().fold(String::new(), |mut out, s| {
                let _ = writeln!(out, "{} {} user seat0", s.id, s.uid);
                out
            })),
            "loginctl" => {
                let s = state.sessions.iter().find(|s| s.id == rest[1]).unwrap();
                ok(format!("Class={}\nUid={}\nState={}\n", s.class, s.uid, s.state))
            }
            "partx" | "btrfs" | "cryptsetup" | "/usr/lib/systemd/systemd-growfs" => ok(String::new()),
            other => panic!("unexpected tool {other}"),
        })
    }
}

type Mutation = Box<dyn Fn(&mut Value)>;

fn call(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

#[test]
fn deletes_both_staging_partitions_and_grows_the_adjacent_root() {
    let machine = Machine::new();
    assert_eq!(machine.run(), 0, "{:?}", machine.log());
    assert_eq!(machine.extent(STAGING_ESP_GUID), None);
    assert_eq!(machine.extent(STAGING_ROOT_GUID), None);
    assert_eq!(
        machine.extent(ROOT_GUID),
        Some((TARGET_ROOT.0, STAGING_ROOT.1))
    );
    assert_eq!(
        machine.extent(RECOVERY_GUID),
        Some(RECOVERY),
        "WinRE must be untouched"
    );
    assert_eq!(
        machine.extent(WINDOWS_GUID),
        Some((206_848, 100 * GIB_SECTORS - 1))
    );
    let calls = machine.calls();
    assert!(calls.contains(&call(&[
        "cryptsetup",
        "resize",
        "--batch-mode",
        "luks-1234"
    ])));
    assert!(calls.contains(&call(&["btrfs", "filesystem", "resize", "max", "/sysroot"])));
    assert!(!machine.record.exists());
    assert_eq!(machine.log().last().unwrap(), "tilo-staging-reclaim: done");
    assert_eq!(
        machine.log(),
        [
            format!("tilo-staging-reclaim: deleted the staging ESP ({STAGING_ESP_GUID})"),
            format!("tilo-staging-reclaim: deleted the staging root ({STAGING_ROOT_GUID})"),
            format!(
                "tilo-staging-reclaim: grew the Tilo root partition to LBA {}",
                STAGING_ROOT.1
            ),
            "tilo-staging-reclaim: grew the btrfs filesystem at /sysroot".to_owned(),
            "tilo-staging-reclaim: returned the temporary installer space to Tilo".to_owned(),
            "tilo-staging-reclaim: done".to_owned(),
        ]
    );
}

#[test]
fn resumes_after_a_crash_between_the_two_deletions() {
    let mut state = initial_state();
    state.partitions.retain(|p| p.number != 3);
    let machine = Machine::with(state);
    assert_eq!(machine.run(), 0, "{:?}", machine.log());
    assert_eq!(
        machine.extent(ROOT_GUID),
        Some((TARGET_ROOT.0, STAGING_ROOT.1))
    );
    assert!(!machine.record.exists());
}

#[test]
fn resumes_after_growth_but_before_the_filesystem_grew() {
    let mut state = initial_state();
    state.partitions.retain(|p| p.number != 3 && p.number != 4);
    let root = state.partitions.iter_mut().find(|p| p.number == 6).unwrap();
    root.size = STAGING_ROOT.1 - root.start + 1;
    let machine = Machine::with(state);
    assert_eq!(machine.run(), 0, "{:?}", machine.log());
    let calls = machine.calls();
    assert!(
        !calls.iter().any(|c| c[0] == "sfdisk" && c[1] != "--json"),
        "no second GPT write"
    );
    assert!(calls.contains(&call(&["btrfs", "filesystem", "resize", "max", "/sysroot"])));
    assert!(
        calls.contains(&call(&["partx", "-u", "--nr", "6", DISK])),
        "the kernel sees the grown size"
    );
}

#[test]
fn waits_for_the_first_user_session() {
    for (class, uid, state_name) in [
        ("greeter", "972", "active"),
        ("user", "999", "active"),
        ("user", "1000", "closing"),
        ("user", "60000", "online"),
    ] {
        let mut state = initial_state();
        state.sessions = vec![Session {
            id: "1",
            class,
            uid,
            state: state_name,
        }];
        let machine = Machine::with(state);
        assert_eq!(machine.run(), 0);
        assert_eq!(
            machine.mutations(),
            Vec::<Vec<String>>::new(),
            "{class} {uid} {state_name}"
        );
        assert!(machine.record.exists());
        assert_eq!(
            machine.log(),
            [
                "tilo-staging-reclaim: waiting for the first user session before reclaiming installer space"
            ]
        );
    }
}

#[test]
fn locked_luks_key_defers_growth_to_the_next_boot() {
    let mut state = initial_state();
    state.fail_cryptsetup = true;
    let machine = Machine::with(state);
    assert_eq!(machine.run(), 0, "{:?}", machine.log());
    assert!(
        machine
            .log()
            .iter()
            .any(|l| l.contains("grows at the next boot") && l.contains("no key available"))
    );
    assert!(!machine.calls().contains(&call(&[
        "btrfs",
        "filesystem",
        "resize",
        "max",
        "/sysroot"
    ])));
    assert!(!machine.record.exists());
}

#[test]
fn other_filesystems_grow_with_systemd_growfs() {
    let mut state = initial_state();
    state.fstype = "ext4";
    let machine = Machine::with(state);
    assert_eq!(machine.run(), 0);
    assert!(
        machine
            .calls()
            .contains(&call(&["/usr/lib/systemd/systemd-growfs", "/sysroot"]))
    );
}

#[test]
fn nothing_to_do_without_a_record() {
    let machine = Machine::new();
    fs::remove_file(&machine.record).unwrap();
    assert_eq!(machine.run(), 0);
    assert!(machine.calls().is_empty());
    assert!(machine.log().is_empty());
}

fn assert_untouched(machine: &Machine, fragment: &str) {
    let before = machine.partitions();
    assert_eq!(machine.run(), 1, "{:?}", machine.log());
    let log = machine.log().join("\n");
    assert!(log.contains(fragment), "{fragment}: {log}");
    assert!(
        log.contains("tilo-staging-reclaim: refused: left the installer partitions untouched: "),
        "{log}"
    );
    assert!(!log.contains("tilo-staging-reclaim: done"));
    assert_eq!(machine.partitions(), before);
    assert_eq!(machine.mutations(), Vec::<Vec<String>>::new());
    assert!(
        machine.record.exists(),
        "a refused reclaim keeps its evidence"
    );
}

#[test]
fn foreign_partition_in_the_staging_space_leaves_everything_untouched() {
    let mut state = initial_state();
    state.partitions.retain(|p| p.number != 4);
    state.partitions.push(part(
        8,
        "00000000-0000-4000-8000-00000000000e",
        LINUX_TYPE,
        (STAGING_ROOT.0, STAGING_ROOT.0 + 2047),
    ));
    assert_untouched(
        &Machine::with(state),
        "now occupies the recorded staging space",
    );
}

#[test]
fn changed_staging_identity_leaves_everything_untouched() {
    for kind in [TILO_ROOT, LINUX_TYPE] {
        let mut state = initial_state();
        state
            .partitions
            .iter_mut()
            .find(|p| p.number == 4)
            .unwrap()
            .kind = kind.to_owned();
        assert_untouched(&Machine::with(state), "staging root type changed");
    }
    let mut state = initial_state();
    state
        .partitions
        .iter_mut()
        .find(|p| p.number == 3)
        .unwrap()
        .size -= 1;
    assert_untouched(&Machine::with(state), "staging ESP extent changed");
}

#[test]
fn mounted_or_held_staging_partition_leaves_everything_untouched() {
    let mut state = initial_state();
    state
        .partitions
        .iter_mut()
        .find(|p| p.number == 3)
        .unwrap()
        .mounts = vec!["/mnt".to_owned()];
    assert_untouched(&Machine::with(state), "staging ESP is in use");
    let mut state = initial_state();
    state
        .partitions
        .iter_mut()
        .find(|p| p.number == 4)
        .unwrap()
        .children = json!([{"name": "/dev/dm-1", "type": "crypt"}]);
    assert_untouched(&Machine::with(state), "staging root is in use");
}

#[test]
fn other_disk_guid_or_moved_root_leaves_everything_untouched() {
    let mut state = initial_state();
    state.ptuuid = "99999999-2222-4333-8444-555555555555".to_owned();
    assert_untouched(
        &Machine::with(state),
        "expected exactly one disk with GPT GUID",
    );
    let mut state = initial_state();
    state
        .partitions
        .iter_mut()
        .find(|p| p.number == 6)
        .unwrap()
        .size -= 2048;
    assert_untouched(&Machine::with(state), "ends neither at its recorded end");
    let mut state = initial_state();
    state
        .partitions
        .iter_mut()
        .find(|p| p.number == 6)
        .unwrap()
        .kind = LINUX_TYPE.to_owned();
    assert_untouched(
        &Machine::with(state),
        "the Tilo root partition type or start changed",
    );
}

#[test]
fn malformed_records_leave_everything_untouched() {
    let cases: Vec<(&str, Mutation)> = vec![
        (
            "not a lowercase GUID",
            Box::new(|r| {
                r["staging_esp"]["partuuid"] = json!("AAAAAAAA-0000-4000-8000-000000000001");
            }),
        ),
        (
            "fields differ from schema 1",
            Box::new(|r| r["schema"] = json!(2)),
        ),
        (
            "fields differ from schema 1",
            Box::new(|r| r["extra"] = json!(1)),
        ),
        (
            "sector size is unsupported",
            Box::new(|r| r["sector_bytes"] = json!(1024)),
        ),
        (
            "disk_size_bytes is not a bounded integer",
            Box::new(|r| r["disk_size_bytes"] = json!(true)),
        ),
        (
            "ends before it starts",
            Box::new(|r| r["staging_root"]["last_lba"] = json!(1)),
        ),
        (
            "has unexpected shape",
            Box::new(|r| r["target_root"] = json!([])),
        ),
        (
            "GUIDs are not distinct",
            Box::new(|r| r["staging_root"]["partuuid"] = json!(STAGING_ESP_GUID)),
        ),
        (
            "before or inside the Tilo root",
            Box::new(|r| r["target_root"]["last_lba"] = json!(STAGING_ESP.0)),
        ),
    ];
    for (fragment, mutate) in cases {
        let machine = Machine::new();
        let mut broken = record();
        mutate(&mut broken);
        fs::write(&machine.record, broken.to_string()).unwrap();
        assert_untouched(&machine, fragment);
    }
    let machine = Machine::new();
    fs::write(&machine.record, "{").unwrap();
    assert_untouched(&machine, "record is not valid JSON");
    let machine = Machine::new();
    fs::write(&machine.record, vec![b' '; 16 * 1024 + 1]).unwrap();
    assert_untouched(&machine, "record is unexpectedly large");
}

#[test]
fn python_equality_and_trailing_newlines_are_kept() {
    // The original compared with ==, so 1.0 is schema 1 and 512.0 a sector
    // size; a GUID may carry the one trailing newline `$` allowed.
    let machine = Machine::new();
    let mut lenient = record();
    lenient["schema"] = json!(1.0);
    lenient["sector_bytes"] = json!(512.0);
    lenient["operation_id"] = json!("0f0e0d0c-0b0a-4908-8706-050403020100\n");
    fs::write(&machine.record, lenient.to_string()).unwrap();
    assert_eq!(machine.run(), 0, "{:?}", machine.log());
}

#[test]
fn units_are_scoped_to_their_side_of_the_install() {
    let repo = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.."));
    let units = repo.join("os/usr/lib/systemd/system");
    let reclaim = fs::read_to_string(units.join("tilo-staging-reclaim.service")).unwrap();
    assert!(reclaim.contains("ExecStart=/usr/libexec/tilo-staging-reclaim\n"));
    assert!(reclaim.contains("ConditionKernelCommandLine=!tilo.install"));
    assert!(reclaim.contains("ConditionPathExists=/var/lib/tilo/staging-reclaim.json"));
    assert!(
        reclaim
            .contains("After=local-fs.target tilo-firstboot-user.service tilo-boot-health.service")
    );
    for live in [
        "tilo-staging-handoff.service",
        "tilo-installer-grow-root.service",
    ] {
        let text = fs::read_to_string(units.join(live)).unwrap();
        assert!(
            text.contains("ConditionKernelCommandLine=tilo.install"),
            "{live}"
        );
        assert!(text.contains("Before=greetd.service"), "{live}");
    }
    assert!(
        fs::read_to_string(units.join("tilo-installer-grow-root.service"))
            .unwrap()
            .contains("ExecStart=/usr/lib/systemd/systemd-growfs /sysroot")
    );
    let installer = fs::read_to_string(repo.join("os/Containerfile.installer")).unwrap();
    assert!(installer.contains(
        "systemctl enable tilo-staging-handoff.service tilo-installer-grow-root.service tilo-staging-reclaim.timer"
    ));
    // The compiled helper reaches the installer, and the targets it installs,
    // from the base image.
    let components = fs::read_to_string(repo.join("os/components.toml")).unwrap();
    assert!(components.contains(
        r#"{ from = "tilo-staging-reclaim", to = "/usr/libexec/tilo-staging-reclaim", mode = "0755" }"#
    ));
}
