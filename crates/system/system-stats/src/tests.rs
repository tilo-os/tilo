//! The service end to end, over a machine laid out in a directory and real
//! processes for the ends.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::float_cmp,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;

use crate::apps::EndPolicy;
use crate::end::PidfdSignaller;
use crate::hub::{Hub, Spec};
use crate::probe::KernelProbe;
use crate::protocol::{
    Grant, Grants, Handoff, PROTOCOL, ProcessState, Refusal, Reply, Request, read_message,
    write_message,
};
use crate::server::{NoPrompter, Prompter};

const UID: u32 = 1000;
const APP_ROOT: &str = "/user.slice/user-1000.slice/user@1000.service/app.slice";

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// A process in the fake `/proc`, with an environment and a command line
/// that hold secrets.
struct FakeProcess<'a> {
    pid: u32,
    ppid: u32,
    name: &'a str,
    uid: u32,
    started_at: u64,
    cgroup: &'a str,
    cpu_ticks: u64,
}

fn add_process(root: &Path, process: &FakeProcess<'_>) {
    let FakeProcess {
        pid,
        ppid,
        name,
        uid,
        started_at,
        cgroup,
        cpu_ticks,
    } = process;
    write(
        root,
        &format!("proc/{pid}/stat"),
        &format!(
            "{pid} ({name}) S {ppid} {pid} {pid} 0 -1 4194560 0 0 0 0 {cpu_ticks} 0 0 0 20 0 3 0 {started_at} 104857600 512 0"
        ),
    );
    write(
        root,
        &format!("proc/{pid}/status"),
        &format!(
            "Name:\t{name}\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\nVmRSS:\t   20480 kB\nThreads:\t3\n"
        ),
    );
    write(
        root,
        &format!("proc/{pid}/cgroup"),
        &format!("0::{cgroup}\n"),
    );
    write(
        root,
        &format!("proc/{pid}/cmdline"),
        &format!("/usr/bin/{name}\0--token=cmdline-secret-{pid}\0"),
    );
    write(
        root,
        &format!("proc/{pid}/environ"),
        &format!("HOME=/home/ada\0API_KEY=environ-secret-{pid}\0"),
    );
}

/// A small machine: two cores, memory, a disk, a network card, a battery
/// and a sensor.
fn machine(root: &Path, busy_ticks: u64) {
    write(
        root,
        "proc/stat",
        &format!(
            "cpu  {busy_ticks} 0 0 {idle} 0 0 0 0 0 0\ncpu0 {busy_ticks} 0 0 {core_idle} 0 0 0 0 0 0\ncpu1 0 0 0 500 0 0 0 0 0 0\nbtime 1700000000\n",
            idle = 1000 + busy_ticks,
            core_idle = 500 + busy_ticks,
        ),
    );
    write(root, "proc/sys/kernel/hostname", "ada-laptop\n");
    write(root, "proc/sys/kernel/osrelease", "7.0.9\n");
    write(
        root,
        "proc/cpuinfo",
        "processor\t: 0\nmodel name\t: Test CPU 9000\n",
    );
    write(root, "proc/uptime", "5000.12 9000.00\n");
    write(root, "proc/loadavg", "1.50 1.00 0.50 2/300 4242\n");
    write(
        root,
        "proc/meminfo",
        "MemTotal:       16000000 kB\nMemFree:  1000 kB\nMemAvailable:    4000000 kB\nSwapTotal: 2000000 kB\nSwapFree: 1500000 kB\n",
    );
    for name in ["cpu", "memory", "io"] {
        write(
            root,
            &format!("proc/pressure/{name}"),
            "some avg10=2.50 avg60=1.00 avg300=0.50 total=1\nfull avg10=1.25 avg60=0.00 avg300=0.00 total=0\n",
        );
    }
    write(
        root,
        "proc/diskstats",
        &format!(
            " 259 0 nvme0n1 10 0 {busy_ticks} 0 10 0 {busy_ticks} 0 0 0 0\n 259 1 nvme0n1p1 1 0 2 0 1 0 2 0 0 0 0\n"
        ),
    );
    std::fs::create_dir_all(root.join("sys/block/nvme0n1/device")).unwrap();
    write(
        root,
        "proc/self/mountinfo",
        "22 1 259:1 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p1 rw\n",
    );
    write(
        root,
        "proc/net/dev",
        &format!(
            "Inter-|   Receive\n face |bytes\n    lo: 999 0 0 0 0 0 0 0 999 0 0 0 0 0 0 0\n  wlan0: {} 0 0 0 0 0 0 0 {} 0 0 0 0 0 0 0\n",
            busy_ticks * 10,
            busy_ticks * 5
        ),
    );
    write(root, "sys/class/drm/card0/device/uevent", "DRIVER=amdgpu\n");
    write(root, "sys/class/drm/card0/device/gpu_busy_percent", "37\n");
    write(
        root,
        "sys/class/drm/card0/device/mem_info_vram_used",
        "1048576\n",
    );
    write(root, "sys/class/power_supply/BAT0/type", "Battery\n");
    write(root, "sys/class/power_supply/BAT0/capacity", "81\n");
    write(root, "sys/class/power_supply/BAT0/status", "Discharging\n");
    write(root, "sys/class/power_supply/BAT0/power_now", "7500000\n");
    write(
        root,
        "sys/class/thermal/thermal_zone0/type",
        "cpu-thermal\n",
    );
    write(root, "sys/class/thermal/thermal_zone0/temp", "47250\n");
    write(
        root,
        "etc/passwd",
        "root:x:0:0::/root:/bin/sh\nada:x:1000:1000::/home/ada:/bin/sh\n",
    );
}

fn fake_hub(root: &Path) -> Hub {
    Hub::new(
        Box::new(KernelProbe::at(root)),
        EndPolicy::for_user(UID, std::process::id()),
        Box::new(PidfdSignaller),
    )
}

fn standard_processes(root: &Path) {
    add_process(
        root,
        &FakeProcess {
            pid: 4242,
            ppid: 1,
            name: "notes",
            uid: UID,
            started_at: 5000,
            cgroup: &format!("{APP_ROOT}/app-tilo-so.tilo.notes-4242.scope"),
            cpu_ticks: 100,
        },
    );
    add_process(
        root,
        &FakeProcess {
            pid: 4243,
            ppid: 4242,
            name: "notes helper",
            uid: UID,
            started_at: 5001,
            cgroup: &format!("{APP_ROOT}/app-tilo-so.tilo.notes-4242.scope"),
            cpu_ticks: 0,
        },
    );
    add_process(
        root,
        &FakeProcess {
            pid: 700,
            ppid: 1,
            name: "sshd",
            uid: 0,
            started_at: 10,
            cgroup: "/system.slice/sshd.service",
            cpu_ticks: 5,
        },
    );
}

/// Take snapshots from `hub` as a subscriber holding `grants` would.
fn snapshots(
    hub: &Hub,
    grants: Grants,
) -> (
    crate::hub::Subscription,
    mpsc::Receiver<crate::protocol::Snapshot>,
) {
    let (tx, rx) = mpsc::channel();
    let subscription = hub.subscribe(
        Spec::every(250, true),
        grants,
        Box::new(move |snapshot| {
            let _ = tx.send(snapshot);
        }),
    );
    (subscription, rx)
}

fn next(rx: &mpsc::Receiver<crate::protocol::Snapshot>) -> crate::protocol::Snapshot {
    tilo_test_env::recv("the hub's snapshot of the interval", rx)
}

/// USER REQUIREMENT, the service's half: the resource monitor reads the
/// machine's processes and stats through the service.
// USER REQUIREMENT (2026-10-18): "the resource monitor isn't allowed to read the system process list/or any system stats"
#[test]
fn the_monitor_is_served_the_machines_processes_and_stats() {
    let root = tempfile::tempdir().unwrap();
    machine(root.path(), 100);
    standard_processes(root.path());
    let hub = fake_hub(root.path());
    let (subscription, rx) = snapshots(&hub, Grants::IMAGE_DEFAULT);

    let first = next(&rx);
    let system = first.system.as_ref().expect("system-info is granted");
    assert_eq!(system.host.hostname, "ada-laptop");
    assert_eq!(system.host.kernel, "7.0.9");
    assert_eq!(system.host.cpu_model.as_deref(), Some("Test CPU 9000"));
    assert_eq!(system.host.logical_cpus, 2);
    assert!(system.cpu.is_none(), "load needs two readings");
    assert_eq!(system.memory.total, 16_000_000 * 1024);
    assert_eq!(system.memory.used(), 12_000_000 * 1024);
    assert_eq!(system.memory.swap_used(), 500_000 * 1024);
    let pressure = system.pressure.expect("pressure is read");
    assert_eq!(pressure.memory.some_avg10, 2.5);
    assert_eq!(pressure.io.full_avg10, Some(1.25));
    assert_eq!(system.load_average, [1.5, 1.0, 0.5]);
    assert_eq!(system.disks.len(), 1, "partitions count under their disk");
    assert_eq!(system.disks[0].device, "nvme0n1");
    assert_eq!(system.disks[0].mount.as_deref(), Some("/"));
    assert!(
        system.disks[0].total_bytes > 0,
        "the mounted file system's size"
    );
    assert_eq!(system.gpus[0].driver, "amdgpu");
    assert_eq!(system.gpus[0].busy_percent, Some(37.0));
    assert_eq!(system.batteries[0].percent, Some(81.0));
    assert_eq!(system.batteries[0].power_watts, Some(7.5));
    assert_eq!(system.thermal[0].celsius, 47.25);
    assert!(
        system.network.interfaces.iter().all(|i| i.name != "lo"),
        "loopback is not network traffic"
    );

    assert!(first.processes_included);
    let notes = first.processes.iter().find(|p| p.pid == 4242).unwrap();
    assert_eq!(notes.name, "notes");
    assert_eq!(notes.app.as_deref(), Some("so.tilo.notes"));
    assert_eq!(notes.user, "ada");
    assert_eq!(notes.memory_bytes, 20480 * 1024);
    assert_eq!(notes.threads, 3);
    assert_eq!(notes.state, ProcessState::Sleeping);
    assert_eq!(notes.started_at, 5000);
    assert!(notes.endable, "the person's own app");
    let sshd = first.processes.iter().find(|p| p.pid == 700).unwrap();
    assert_eq!(sshd.user, "root");
    assert_eq!(sshd.app, None, "a system service is not an app");
    assert!(!sshd.endable);
    assert_eq!(first.apps.len(), 1, "one app, its two processes together");
    assert_eq!(first.apps[0].app, "so.tilo.notes");
    assert_eq!(first.apps[0].processes, 2);
    assert_eq!(first.apps[0].memory_bytes, 2 * 20480 * 1024);

    drop(subscription);

    // The machine works for a while: the next reading has rates. Read
    // through the collector at chosen times, so the interval is exact.
    let mut collector = crate::collect::Collector::new(
        Box::new(KernelProbe::at(root.path())),
        EndPolicy::for_user(UID, std::process::id()),
    );
    let want = crate::probe::Want {
        system: true,
        processes: true,
        command_lines: false,
    };
    collector.read(want, 1_000_000);
    machine(root.path(), 400);
    add_process(
        root.path(),
        &FakeProcess {
            pid: 4242,
            ppid: 1,
            name: "notes",
            uid: UID,
            started_at: 5000,
            cgroup: &format!("{APP_ROOT}/app-tilo-so.tilo.notes-4242.scope"),
            cpu_ticks: 150,
        },
    );
    let second = collector.read(want, 1_001_000);
    assert_eq!(second.window_ms, 1000);
    let system = second.system.as_ref().unwrap();
    let cpu = system.cpu.as_ref().expect("two readings make a load");
    // Busy 300 of 600 ticks, all on the first core.
    assert_eq!(cpu.total, 50.0);
    assert_eq!(cpu.per_core, vec![50.0, 0.0]);
    assert_eq!(system.disks[0].read_bytes_per_sec, 300.0 * 512.0);
    assert_eq!(system.network.rx_bytes_per_sec, 3000.0);
    assert_eq!(system.network.tx_bytes_per_sec, 1500.0);
    let processes = second.processes.as_ref().unwrap();
    let notes = processes.iter().find(|p| p.pid == 4242).unwrap();
    // 50 ticks of 100 a second over one second on two cores.
    assert_eq!(notes.cpu, 25.0);
    assert_eq!(second.apps[0].cpu, 25.0);
}

#[test]
fn no_environment_ever_and_command_lines_only_with_their_own_grant() {
    let root = tempfile::tempdir().unwrap();
    machine(root.path(), 100);
    standard_processes(root.path());
    let hub = fake_hub(root.path());

    let (without, rx) = snapshots(&hub, Grants::IMAGE_DEFAULT);
    let wire = serde_json::to_string(&next(&rx)).unwrap();
    drop(without);
    assert!(wire.contains("notes"), "the list is there");
    assert!(!wire.contains("environ-secret"), "no environment: {wire}");
    assert!(!wire.contains("API_KEY"));
    assert!(
        !wire.contains("cmdline-secret"),
        "no command line without the grant"
    );

    let (with, rx) = snapshots(&hub, Grants::IMAGE_DEFAULT.with(Grant::CommandLines));
    let snapshot = next(&rx);
    drop(with);
    let notes = snapshot.processes.iter().find(|p| p.pid == 4242).unwrap();
    assert_eq!(
        notes.command.as_deref(),
        Some("/usr/bin/notes --token=cmdline-secret-4242")
    );
    let wire = serde_json::to_string(&snapshot).unwrap();
    assert!(
        !wire.contains("environ-secret"),
        "never the environment: {wire}"
    );

    // System information alone carries no process at all.
    let system_only = Grants {
        system_info: true,
        ..Grants::default()
    };
    let (only, rx) = snapshots(&hub, system_only);
    let snapshot = next(&rx);
    drop(only);
    assert!(snapshot.system.is_some());
    assert!(snapshot.processes.is_empty() && snapshot.apps.is_empty());
    assert!(!snapshot.processes_included);
    let wire = serde_json::to_string(&snapshot).unwrap();
    assert!(!wire.contains("notes"), "{wire}");
}

#[test]
fn nobody_subscribed_means_no_thread_and_no_reading() {
    let root = tempfile::tempdir().unwrap();
    machine(root.path(), 100);
    let hub = fake_hub(root.path());
    assert!(!hub.is_reading(), "nothing runs before anyone subscribes");
    let (subscription, rx) = snapshots(&hub, Grants::IMAGE_DEFAULT);
    next(&rx);
    assert!(hub.is_reading());
    drop(subscription);
    hub.wait_until_idle();
    assert!(
        !hub.is_reading(),
        "the last subscriber leaving stops the thread"
    );
    // The subscriber's callback went with it: whatever was read before is
    // all there will be.
    let _read_before: Vec<_> = rx.try_iter().collect();
    assert_eq!(
        rx.try_recv().unwrap_err(),
        mpsc::TryRecvError::Disconnected,
        "and nothing more is read"
    );
}

/// A connection as the launcher hands it: the service end runs `serve`.
fn connect(hub: &Hub, grants: Grants, prompter: std::sync::Arc<dyn Prompter>) -> UnixStream {
    connect_within(hub, grants, Grants::ALL, prompter)
}

fn connect_within(
    hub: &Hub,
    grants: Grants,
    ceiling: Grants,
    prompter: std::sync::Arc<dyn Prompter>,
) -> UnixStream {
    let (app, service) = UnixStream::pair().unwrap();
    let hub = hub.clone();
    let handoff = Handoff {
        protocol: PROTOCOL,
        app_id: "com.example.tool".into(),
        workspace_id: Some("room".into()),
        grants,
        ceiling,
    };
    std::thread::spawn(move || {
        let reader = service.try_clone().unwrap();
        crate::server::serve(reader, service, &handoff, &hub, prompter.as_ref());
    });
    app.set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    app
}

fn ask(app: &mut UnixStream, request: &Request) -> Reply {
    write_message(app, request).unwrap();
    loop {
        let reply: Reply = read_message(app).unwrap().expect("an answer");
        if !matches!(reply, Reply::Snapshot { .. }) {
            return reply;
        }
    }
}

struct Yes;

impl Prompter for Yes {
    fn granted(&self, _handoff: &Handoff, _grant: Grant) -> bool {
        false
    }
    fn ask(&self, handoff: &Handoff, _grant: Grant) -> bool {
        handoff.app_id == "com.example.tool"
    }
}

#[test]
fn an_app_sees_and_does_only_what_it_was_granted() {
    let root = tempfile::tempdir().unwrap();
    machine(root.path(), 100);
    standard_processes(root.path());
    let hub = fake_hub(root.path());

    // A user-installed app nobody granted anything: refused outright.
    let mut app = connect(&hub, Grants::default(), std::sync::Arc::new(NoPrompter));
    assert_eq!(
        ask(
            &mut app,
            &Request::Subscribe {
                subscription: 1,
                interval_ms: 250,
                processes: true,
            }
        ),
        Reply::SubscriptionRefused {
            subscription: 1,
            refusal: Refusal::NotGranted {
                grant: Grant::SystemInfo
            },
        }
    );
    assert_eq!(
        ask(
            &mut app,
            &Request::EndApp {
                request: 2,
                app: "so.tilo.notes".into(),
                force: false,
            }
        ),
        Reply::Refused {
            request: 2,
            refusal: Refusal::NotGranted {
                grant: Grant::Processes
            },
        }
    );
    assert_eq!(
        ask(
            &mut app,
            &Request::AskGrant {
                request: 3,
                grant: Grant::Processes,
            }
        ),
        Reply::Refused {
            request: 3,
            refusal: Refusal::Declined,
        },
        "nobody to ask is a no"
    );

    // The same app, asking the person, who says yes: the list arrives on the
    // subscription it already holds.
    let mut app = connect(
        &hub,
        Grants {
            system_info: true,
            ..Grants::default()
        },
        std::sync::Arc::new(Yes),
    );
    write_message(
        &mut app,
        &Request::Subscribe {
            subscription: 9,
            interval_ms: 250,
            processes: true,
        },
    )
    .unwrap();
    let Reply::Snapshot { snapshot, .. } = read_message(&mut app).unwrap().unwrap() else {
        panic!("a snapshot first");
    };
    assert!(snapshot.system.is_some() && snapshot.processes.is_empty());
    let Reply::Grants { grants, .. } = ask(
        &mut app,
        &Request::AskGrant {
            request: 4,
            grant: Grant::Processes,
        },
    ) else {
        panic!("granted");
    };
    assert!(grants.processes);
    let listed = loop {
        let Reply::Snapshot { snapshot, .. } = read_message(&mut app).unwrap().unwrap() else {
            continue;
        };
        if snapshot.processes_included {
            break snapshot;
        }
    };
    assert!(listed.processes.iter().any(|p| p.name == "notes"));
    assert!(listed.processes.iter().all(|p| p.command.is_none()));

    // An app whose manifest declares only system information is never
    // asked about, nor given, the process list.
    let system_only = Grants {
        system_info: true,
        ..Grants::default()
    };
    let mut app = connect_within(
        &hub,
        Grants::IMAGE_DEFAULT,
        system_only,
        std::sync::Arc::new(Yes),
    );
    assert_eq!(
        ask(
            &mut app,
            &Request::AskGrant {
                request: 5,
                grant: Grant::Processes,
            }
        ),
        Reply::Refused {
            request: 5,
            refusal: Refusal::Declined,
        }
    );
    assert_eq!(
        ask(
            &mut app,
            &Request::EndProcess {
                request: 6,
                pid: 4242,
                started_at: 5000,
                force: false,
            }
        ),
        Reply::Refused {
            request: 6,
            refusal: Refusal::NotGranted {
                grant: Grant::Processes
            },
        },
        "a grant past the manifest is not held"
    );
}

/// A real process, described in the fake `/proc` as living in `cgroup` and
/// owned by `uid`.
fn real_child(root: &Path, cgroup: &str, uid: u32) -> (std::process::Child, u64) {
    let child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let real = std::fs::read_to_string(format!("/proc/{}/stat", child.id())).unwrap();
    let started_at = crate::probe::parse_stat(child.id(), &real)
        .unwrap()
        .started_at;
    add_process(
        root,
        &FakeProcess {
            pid: child.id(),
            ppid: std::process::id(),
            name: "sleep",
            uid,
            started_at,
            cgroup,
            cpu_ticks: 0,
        },
    );
    (child, started_at)
}

fn still_running(child: &mut std::process::Child) -> bool {
    child.try_wait().unwrap().is_none()
}

#[test]
fn only_the_persons_own_apps_and_processes_can_be_ended() {
    let root = tempfile::tempdir().unwrap();
    machine(root.path(), 100);
    let hub = fake_hub(root.path());
    let app_scope = format!("{APP_ROOT}/app-tilo-so.tilo.notes-1.scope");

    let (mut system, system_start) = real_child(root.path(), "/system.slice/demo.service", UID);
    assert_eq!(
        hub.end_process(system.id(), system_start, false),
        Err(Refusal::NotAnApp),
        "a system service"
    );
    let (mut session, session_start) = real_child(
        root.path(),
        "/user.slice/user-1000.slice/session-2.scope",
        UID,
    );
    assert_eq!(
        hub.end_process(session.id(), session_start, false),
        Err(Refusal::NotAnApp),
        "the session that draws the screen"
    );
    let (mut foreign, foreign_start) = real_child(root.path(), &app_scope, 0);
    assert_eq!(
        hub.end_process(foreign.id(), foreign_start, false),
        Err(Refusal::NotYours),
        "another user's"
    );
    let (mut own, own_start) = real_child(root.path(), &app_scope, UID);
    assert_eq!(
        hub.end_process(own.id(), own_start.wrapping_add(1), false),
        Err(Refusal::Gone),
        "a stale lifetime reaches nothing"
    );
    for child in [&mut system, &mut session, &mut foreign, &mut own] {
        assert!(still_running(child), "nothing was signalled yet");
    }

    assert_eq!(hub.end_process(own.id(), own_start, false), Ok(()));
    let status = own.wait().unwrap();
    assert!(!status.success(), "ended: {status:?}");

    // Ending the app ends every process of it, and nothing else.
    let (mut first, _) = real_child(root.path(), &app_scope, UID);
    let (mut second, _) = real_child(root.path(), &app_scope, UID);
    // The ended child's entry is still in the fake /proc; it is gone and
    // counts for nothing.
    assert_eq!(hub.end_app("so.tilo.notes", true), Ok(2));
    assert!(!first.wait().unwrap().success());
    assert!(!second.wait().unwrap().success());
    assert_eq!(hub.end_app("so.tilo.nothing", false), Err(Refusal::Gone));

    for child in [&mut system, &mut session, &mut foreign] {
        assert!(still_running(child), "untouched");
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
