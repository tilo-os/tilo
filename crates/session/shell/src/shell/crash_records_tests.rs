//! The records of what went wrong, read from fixture files as the crash
//! log, the kernel and the journal write them: what each says happened, its
//! text only when asked by name, the computer stopping kept once however
//! often it is read, and an agent listing and reading them through the
//! broker as Settings does.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::TimeZone as _;
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::crash_records::{
    self as wire, CrashKind, CrashRecord, CrashRecordText, StopReason,
};
use tilo_actions::remote::{Caller, InvokeArgument, InvokeRequest};

use super::*;
use crate::shell::crash_notice::{Ended, take_news};
use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crash-records")
}

const DESKTOP_RECORDS: [&str; 3] = [
    "2026-10-24-210000-5.log",
    "2026-10-24-225201-412-freeze.log",
    "2026-10-24-230000-9.log",
];

/// A private crash directory holding the desktop's fixture records, and a
/// file of another kind beside them.
fn crash_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    tilo_files::native::private::directory(dir.path()).unwrap();
    for name in DESKTOP_RECORDS {
        let body = std::fs::read_to_string(fixtures().join(name)).unwrap();
        crate::crash_log::write_record(dir.path(), name, &body).unwrap();
    }
    std::fs::write(dir.path().join("notes.log"), "not a record").unwrap();
    dir
}

fn local(stamp: &str) -> i64 {
    Local
        .from_local_datetime(
            &chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S").unwrap(),
        )
        .earliest()
        .unwrap()
        .timestamp()
}

#[test]
fn each_record_says_what_happened_newest_first_without_its_text() {
    let dir = crash_dir();
    let marker = dir.path().join(crate::shell::crash_notice::NOTICED);
    std::fs::write(marker, "2026-10-24-230000-9.log").unwrap();
    let listed = list(dir.path());
    assert_eq!(
        listed,
        vec![
            CrashRecord {
                id: "2026-10-24-230000-9.log".into(),
                at: local("2026-10-24 23:00:00"),
                kind: CrashKind::PartRestarted,
                reason: None,
                version: Some("0.1.0".into()),
                build: None,
            },
            CrashRecord {
                id: "2026-10-24-225201-412-freeze.log".into(),
                at: local("2026-10-24 22:52:01"),
                kind: CrashKind::Froze,
                reason: None,
                version: Some("0.1.0".into()),
                build: Some("9f2c41d07be3a65e".into()),
            },
            CrashRecord {
                id: "2026-10-24-210000-5.log".into(),
                at: local("2026-10-24 21:00:00"),
                kind: CrashKind::Crashed,
                reason: None,
                version: Some("0.1.0".into()),
                build: Some("9f2c41d07be3a65e".into()),
            },
        ],
        "the build a record could not name is no build, and other files are no records"
    );
    let answered = serde_json::to_string(&listed).unwrap();
    assert!(
        !answered.contains("lock_contended") && !answered.contains("backtrace"),
        "the list carries no stack: {answered}"
    );
}

#[test]
fn a_record_is_read_by_its_name_alone() {
    let dir = crash_dir();
    let freeze = read(dir.path(), "2026-10-24-225201-412-freeze.log").unwrap();
    assert_eq!(freeze.record.kind, CrashKind::Froze);
    assert!(!freeze.truncated);
    assert!(
        freeze.text.contains("Mutex::lock_contended") && freeze.text.contains("thread 415"),
        "every thread's stack: {}",
        freeze.text
    );
    for name in [
        "../2026-10-24-225201-412-freeze.log",
        "/etc/2026-10-24-225201-412-freeze.log",
        "notes.log",
        ".noticed",
        "2026-10-25-000000-1.log",
    ] {
        assert!(read(dir.path(), name).is_err(), "{name} is no record here");
    }
}

#[test]
fn a_long_record_is_read_up_to_its_bound() {
    let dir = crash_dir();
    let name = "2026-10-24-235959-3.log";
    let body = format!(
        "tilo shell panic\ncontained: no: the session ends\n\n{}",
        "x".repeat(usize::try_from(MAX_TEXT).unwrap() + 10)
    );
    crate::crash_log::write_record(dir.path(), name, &body).unwrap();
    let long = read(dir.path(), name).unwrap();
    assert!(long.truncated);
    assert_eq!(long.text.len(), usize::try_from(MAX_TEXT).unwrap());
    assert_eq!(long.record.kind, CrashKind::Crashed);
}

#[test]
fn the_kernels_dumps_are_the_computer_stopping_and_its_console_log_is_not() {
    let entries = read_pstore_dir(&fixtures().join("pstore"));
    assert_eq!(entries.len(), 4, "every file is read");
    let mut found = stops(&entries);
    found.sort_by_key(|stop| stop.reason == StopReason::Oops);
    assert_eq!(found.len(), 2, "{found:#?}");
    let panic = &found[0];
    assert_eq!(panic.reason, StopReason::Panic);
    assert!(
        panic.written.is_some(),
        "the file's time is when it was written"
    );
    let older = panic.log.find("firmware crashed").unwrap();
    let newer = panic.log.find("Kernel panic - not syncing").unwrap();
    assert!(older < newer, "parts read oldest first: {}", panic.log);
    assert!(!panic.log.contains("Part"), "{}", panic.log);
    assert_eq!(found[1].reason, StopReason::Oops);
    assert!(found[1].log.contains("ath12k_dp_rx_process"));

    assert_eq!(dump_heading("Shutdown#1 Part1"), None, "a clean shutdown");
    assert_eq!(
        dump_heading("Emergency#3 Part2"),
        Some((StopReason::Emergency, 3, 2))
    );
}

#[test]
fn the_journal_carries_the_records_it_was_given() {
    let lines = std::fs::read_to_string(fixtures().join("systemd-pstore.json")).unwrap();
    let entries = journal_entries(&lines);
    let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(
        names,
        ["dmesg-ramoops-0", "console-ramoops-0", "dmesg-ramoops-1"]
    );
    assert!(entries.iter().all(|entry| entry.written.is_none()));
    assert_eq!(
        entries[2].content.last(),
        Some(&b'\n'),
        "a record that is not text arrives as its bytes"
    );
    assert!(entries[2].content.contains(&0xff));
    let found = stops(&entries);
    let reasons: Vec<StopReason> = found.iter().map(|stop| stop.reason).collect();
    assert_eq!(reasons, [StopReason::Panic, StopReason::Oops]);
    assert!(found[0].log.contains("softlockup"));
}

#[test]
fn the_computer_stopping_is_kept_once_and_is_news_at_the_next_start() {
    let dir = crash_dir();
    assert_eq!(
        take_news(dir.path()),
        None,
        "the first look marks what is there"
    );
    // Each source's dumps are joined on their own: the same dump number in
    // the journal and in the filesystem is two dumps.
    let mut found = stops(&read_pstore_dir(&fixtures().join("pstore")));
    let journal = std::fs::read_to_string(fixtures().join("systemd-pstore.json")).unwrap();
    found.extend(stops(&journal_entries(&journal)));
    let found_at = local("2026-10-25 08:14:00");

    let kept = keep_stops(dir.path(), &found, found_at).unwrap();
    assert_eq!(kept.len(), 4, "{kept:?}");
    assert!(kept.iter().all(|name| name.ends_with(STOPPED_SUFFIX)));
    assert!(
        kept.iter()
            .any(|name| name.starts_with("2026-10-25-081400-")),
        "a record the journal carried is timed by the start that found it: {kept:?}"
    );
    assert_eq!(
        keep_stops(dir.path(), &found, found_at + 60).unwrap(),
        Vec::<String>::new(),
        "read again at the next start, each is kept once"
    );

    let listed = list(dir.path());
    let stopped: Vec<&CrashRecord> = listed
        .iter()
        .filter(|record| record.kind == CrashKind::ComputerStopped)
        .collect();
    assert_eq!(stopped.len(), 2, "a panic in each source: {listed:#?}");
    assert!(
        stopped
            .iter()
            .all(|record| record.reason == Some(StopReason::Panic))
    );
    let faults: Vec<&CrashRecord> = listed
        .iter()
        .filter(|record| record.kind == CrashKind::SystemFault)
        .collect();
    assert_eq!(faults.len(), 2, "an oops in each source: {listed:#?}");
    assert!(
        faults
            .iter()
            .all(|record| record.reason == Some(StopReason::Oops))
    );
    let panic = stopped
        .iter()
        .find(|record| record.reason == Some(StopReason::Panic) && record.at == found_at)
        .unwrap();
    let text = read(dir.path(), &panic.id).unwrap();
    assert!(
        text.text.contains("softlockup: hung tasks"),
        "{}",
        text.text
    );

    let news = take_news(dir.path()).expect("the computer stopping is news");
    assert_eq!(news.ended, Ended::Stopped);
    let notice = crate::shell::crash_notice::notice_for(&news, news.at.date());
    assert_eq!(
        notice.title,
        crate::shell::i18n::t("crash-notice-stopped-title")
    );
}

/// The kernel keeps an oops as it keeps a panic, but it ends only the
/// faulting task and runs on: in the fixture the oops at 904 s is followed
/// by the session starting at 1840 s and the panic at 1843 s. Its record is
/// a fault of the system's core, said in its own words, and no start notice
/// claims the computer restarted for it.
#[test]
fn an_oops_the_system_ran_on_from_is_a_fault_and_not_the_computer_stopping() {
    let dir = crash_dir();
    assert_eq!(
        take_news(dir.path()),
        None,
        "the first look marks the start"
    );
    // Timed by the start that found them, not by the fixture files' times.
    let (oops, panic): (Vec<Stop>, Vec<Stop>) = stops(&read_pstore_dir(&fixtures().join("pstore")))
        .into_iter()
        .map(|stop| Stop {
            written: None,
            ..stop
        })
        .partition(|stop| stop.reason == StopReason::Oops);
    let found_at = local("2026-10-25 08:14:00");

    let kept = keep_stops(dir.path(), &oops, found_at).unwrap();
    assert_eq!(kept.len(), 1, "{kept:?}");
    let fault = read(dir.path(), &kept[0]).unwrap();
    assert_eq!(fault.record.kind, CrashKind::SystemFault);
    assert_eq!(fault.record.reason, Some(StopReason::Oops));
    assert!(
        fault.text.starts_with(FAULT_TITLE)
            && fault.text.contains("went on running")
            && !fault.text.contains("computer stopped"),
        "{}",
        fault.text
    );
    assert_eq!(
        take_news(dir.path()),
        None,
        "a fault the system ran on from is no restart to tell of"
    );

    keep_stops(dir.path(), &panic, found_at + 60).unwrap();
    assert_eq!(
        take_news(dir.path()).map(|news| news.ended),
        Some(Ended::Stopped),
        "the panic that followed stopped the computer"
    );
}

fn request(step: &str, argument: InvokeArgument) -> InvokeRequest {
    InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target: wire::target(step),
        argument,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
    }
}

/// What `agent`'s invocation of `request` answered, once it finished.
fn answered(
    broker: &tilo_action_broker::collab_broker::AppActionBroker,
    agent: &Caller,
    request: &InvokeRequest,
) -> serde_json::Value {
    let mut changes = broker.invocation_changes();
    broker
        .state()
        .action_invoke(agent, None, None, request)
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("admission answers at once")
        .expect("a read asks nothing");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    loop {
        match broker
            .state()
            .action_observe(agent, &request.invocation)
            .map(|state| state.update)
        {
            Some(InvocationUpdate::Succeeded { output, .. }) => {
                return output.value.expect("an answer");
            }
            Some(InvocationUpdate::Failed { error, .. }) => panic!("{}", error.message),
            _ => {}
        }
        let woke = runtime.block_on(async {
            tokio::time::timeout(tilo_test_env::hang_bound(), changes.changed()).await
        });
        assert!(woke.is_ok(), "the read answers");
    }
}

/// An agent allowed to look at Settings in a room lists the records and
/// reads one, through the broker, as Settings does: the list without a
/// stack, the stack only for the record it names.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the test keeps its broker alive through its assertions"
)]
#[test]
fn an_agent_lists_the_records_and_reads_one_through_the_broker() {
    use tilo_action_broker::test_support::{Grants as _, KindSet, Subject};

    let dir = crash_dir();
    let entries = read_pstore_dir(&fixtures().join("pstore"));
    keep_stops(dir.path(), &stops(&entries), local("2026-10-25 08:14:00")).unwrap();

    let mut workspace = Workspace::new(WorkspaceId(8), "Diagnosis");
    workspace.set_owner("owner");
    workspace.admit_agent("scribe", KindSet::READ);
    workspace.allow_app(
        Subject::agent("scribe"),
        settings_sections::APP_ID,
        KindSet::READ,
    );
    let room = workspace.room().to_string();
    let mut tree = WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    let broker =
        tilo_action_broker::collab_broker_capnp::CapnpBrokerState::test_broker_with_authority(
            Arc::new(crate::shell::broker_services::NativeWorkspaceAuthority(
                tree.shared(),
            )),
            tilo_action_broker::services::HostServices {
                host_steps: vec![Arc::new(BrokerDoor {
                    dir: Some(dir.path().to_path_buf()),
                })],
                ..Default::default()
            },
        );
    let agent = Caller::local_agent("scribe").in_room(room);

    let offered = broker
        .state()
        .action_query(
            &agent,
            &tilo_actions::remote::CatalogueQuery {
                provider: Some(wire::PROVIDER.into()),
                max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
                ..Default::default()
            },
        )
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the catalogue answers")
        .expect("the agent may look");
    for step in wire::STEPS {
        assert!(
            offered.rows.iter().any(|row| row
                .action
                .as_ref()
                .is_some_and(|action| action.address == wire::target(step).address)),
            "the records offer {step}"
        );
    }

    let listed: Vec<CrashRecord> = serde_json::from_value(answered(
        &broker,
        &agent,
        &request(wire::LIST, InvokeArgument::None),
    ))
    .unwrap();
    assert_eq!(listed.len(), 5, "{listed:#?}");
    assert!(listed.iter().any(|record| record.kind == CrashKind::Froze));
    let stopped = listed
        .iter()
        .find(|record| record.kind == CrashKind::ComputerStopped)
        .expect("the computer stopping is listed");

    let mut id = serde_json::Map::new();
    id.insert("id".into(), stopped.id.clone().into());
    let text: CrashRecordText = serde_json::from_value(answered(
        &broker,
        &agent,
        &request(wire::READ, InvokeArgument::Json(id)),
    ))
    .unwrap();
    assert_eq!(&text.record, stopped);
    assert!(
        text.text.contains("Call trace"),
        "the kernel's log, on an explicit read: {}",
        text.text
    );
}
