//! Actual Linux process-tree qualification. Missing kernel/sandbox facilities
//! are failures. This fixture proves custody barriers, not model/agent readiness.
use launcher::{
    managed::{
        ManagedBinding, ManagedLaunchAuthority, ManagedLaunchEvent, ManagedLaunchScope,
        ManagedLauncher, ProcessIdentity, VerifiedManagedProcess, VerifiedManagedSupervisor,
        channel::{ManagedProcessAuthority, PendingManagedConnection},
        test_support::{fixture_package, fixture_runtime},
    },
    services::{EnrolledServiceContext, ManagedProviderSelection},
};
use std::{
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{fs::PermissionsExt, net::UnixStream},
    },
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

// This test authority consumes exactly one claim and validates the immutable
// binding. The process proof still comes only from actual launcher/kernel
// admission; no persisted labels are converted into live witnesses.
struct Authority {
    binding: ManagedBinding,
    claimed: AtomicBool,
    bound: AtomicBool,
}
impl ManagedLaunchAuthority for Authority {
    fn claim_launch(&self, binding: &ManagedBinding) -> Result<(), String> {
        self.revalidate_launch(binding)?;
        if self.claimed.swap(true, Ordering::SeqCst) {
            return Err("test launch claim already consumed".into());
        }
        Ok(())
    }
    fn revalidate_launch(&self, binding: &ManagedBinding) -> Result<(), String> {
        if binding != &self.binding {
            return Err("test launch binding mismatch".into());
        }
        Ok(())
    }
}
impl ManagedProcessAuthority for Authority {
    fn bind_process(&self, process: &VerifiedManagedProcess) -> Result<(), String> {
        self.revalidate_launch(process.binding())?;
        process.revalidate(&self.binding, process.process())?;
        if self.bound.swap(true, Ordering::SeqCst) {
            return Err("test process admission already consumed".into());
        }
        Ok(())
    }
}

struct PinnedProcess {
    identity: ProcessIdentity,
    fd: OwnedFd,
}
impl PinnedProcess {
    fn capture(pid: u32) -> Self {
        let identity = ProcessIdentity::read(pid).expect("live /proc start identity");
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        assert!(raw >= 0, "pidfd_open: {}", std::io::Error::last_os_error());
        let result = Self {
            identity,
            fd: unsafe { OwnedFd::from_raw_fd(i32::try_from(raw).unwrap()) },
        };
        assert!(
            identity.is_current(),
            "process changed during pidfd capture"
        );
        assert!(!result.exited(), "process exited before capture");
        result
    }
    fn exited(&self) -> bool {
        let mut poll = libc::pollfd {
            fd: self.fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let count = unsafe { libc::poll(&raw mut poll, 1, 0) };
        assert!(count >= 0, "pidfd poll failed");
        assert_eq!(poll.revents & (libc::POLLERR | libc::POLLNVAL), 0);
        poll.revents & libc::POLLIN != 0
    }
    fn assert_live(&self) {
        assert!(
            !self.exited(),
            "original process was terminated: {:?}",
            self.identity
        );
        assert!(
            self.identity.is_current(),
            "original process identity changed: {:?}",
            self.identity
        );
    }
}

// Failure cleanup uses the retained exact task, never a numeric PID kill.
struct Orphan(PinnedProcess);
impl Drop for Orphan {
    fn drop(&mut self) {
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.0.fd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            );
        }
    }
}
struct Reap(Child);
impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn started(launcher: &mut ManagedLauncher) -> VerifiedManagedSupervisor {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let events = launcher.drain_events().unwrap();
        if !events.is_empty() {
            assert_eq!(events.len(), 1, "unexpected event before fixture admission");
            return match events.into_iter().next().unwrap() {
                ManagedLaunchEvent::Started(proof) => proof,
                ManagedLaunchEvent::Failed { reason, .. } => panic!("launch failed: {reason}"),
                ManagedLaunchEvent::Finished { .. } => panic!("fixture exited before admission"),
            };
        }
        assert!(Instant::now() < deadline, "actual managed Started deadline");
        std::thread::sleep(Duration::from_millis(5));
    }
}

// Read exactly one private marker and its kernel credentials. This never trusts
// a PID serialized by the fixture. Every received rights FD is RAII-owned even
// when unexpected control data makes the test fail.
fn marker(socket: &UnixStream) -> (u8, u32) {
    let mut byte = 0u8;
    let mut iov = libc::iovec {
        iov_base: (&raw mut byte).cast(),
        iov_len: 1,
    };
    let mut control = [0usize; 16];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&control);
    let count =
        unsafe { libc::recvmsg(socket.as_raw_fd(), &raw mut message, libc::MSG_CMSG_CLOEXEC) };
    let mut rights = Vec::new();
    let mut credentials = Vec::new();
    let mut unexpected = false;
    let mut header = unsafe { libc::CMSG_FIRSTHDR(&raw const message) };
    while !header.is_null() {
        let h = unsafe { &*header };
        if h.cmsg_level == libc::SOL_SOCKET && h.cmsg_type == libc::SCM_RIGHTS {
            let bytes = h
                .cmsg_len
                .saturating_sub(unsafe { libc::CMSG_LEN(0) } as usize);
            for i in 0..bytes.checked_div(std::mem::size_of::<i32>()).unwrap() {
                let raw_bytes = unsafe {
                    libc::CMSG_DATA(header)
                        .add(i.checked_mul(std::mem::size_of::<i32>()).unwrap())
                        .cast::<[u8; std::mem::size_of::<i32>()]>()
                        .read_unaligned()
                };
                let raw = i32::from_ne_bytes(raw_bytes);
                rights.push(unsafe { OwnedFd::from_raw_fd(raw) });
            }
            unexpected = true;
        } else if h.cmsg_level == libc::SOL_SOCKET
            && h.cmsg_type == libc::SCM_CREDENTIALS
            && h.cmsg_len
                == unsafe {
                    libc::CMSG_LEN(u32::try_from(std::mem::size_of::<libc::ucred>()).unwrap())
                } as usize
        {
            let mut credential: libc::ucred = unsafe { std::mem::zeroed() };
            unsafe {
                std::ptr::copy_nonoverlapping(
                    libc::CMSG_DATA(header),
                    (&raw mut credential).cast::<u8>(),
                    std::mem::size_of::<libc::ucred>(),
                );
            }
            credentials.push(credential);
        } else {
            unexpected = true;
        }
        header = unsafe { libc::CMSG_NXTHDR(&raw const message, header) };
    }
    assert_eq!(
        count,
        1,
        "fixture marker: {}",
        std::io::Error::last_os_error()
    );
    assert_eq!(message.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC), 0);
    assert!(
        !unexpected && rights.is_empty(),
        "unexpected fixture ancillary data"
    );
    assert_eq!(credentials.len(), 1);
    let pid = u32::try_from(credentials[0].pid).unwrap();
    assert!(pid > 0);
    (byte, pid)
}

fn proc_number(pid: u32, label: &str) -> u32 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    status
        .lines()
        .find_map(|line| line.strip_prefix(label))
        .unwrap()
        .split_whitespace()
        .last()
        .unwrap()
        .parse()
        .unwrap()
}

fn qualify_tree(owned_stop: bool) {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let (package, trust) = fixture_package(
        root.path(),
        std::path::Path::new(env!("CARGO_BIN_EXE_managed-tree-fixture")),
    )
    .unwrap();
    let runtime = fixture_runtime(
        root.path(),
        std::path::Path::new(env!("CARGO_BIN_EXE_tilo-launcher")),
    )
    .unwrap();
    let binding = ManagedBinding {
        room: uuid::Uuid::new_v4().to_string(),
        app: "so.tilo.agent".into(),
        agent: "tree-fixture".into(),
        scope: ManagedLaunchScope::Task {
            task: "tree-task".into(),
            run: "tree-run".into(),
        },
        launch: uuid::Uuid::new_v4().to_string(),
        generation: 1,
    };
    let authority = Arc::new(Authority {
        binding: binding.clone(),
        claimed: false.into(),
        bound: false.into(),
    });
    let mut launcher = ManagedLauncher::new(package, trust, runtime, authority.clone()).unwrap();
    let facts = launcher.runtime_lease().revalidate().unwrap();
    eprintln!("managed-tree fixture, owned_stop={owned_stop}, runtime={facts:?}");
    let mut permit = launcher.authorize(binding.clone()).unwrap();
    let (pending, collab_child) = PendingManagedConnection::pair(&mut permit).unwrap();
    let (identity_child, _identity_peer) = UnixStream::pair().unwrap();
    let (model_child, mut model_peer) = UnixStream::pair().unwrap();
    model_peer
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let services = EnrolledServiceContext::new(
        binding.clone(),
        root.path().to_path_buf(),
        facts.runtime_epoch.clone(),
        pending.channel_id().to_string(),
        ManagedProviderSelection {
            profile_id: "test-only".into(),
            model_id: "no-provider".into(),
            protocol: "anthropic".into(),
        },
        identity_child,
        collab_child,
        model_child,
    )
    .unwrap();
    // Consume the genuine launcher-to-gateway handoff on the host endpoint;
    // the fixture's model descriptor carries only our later private exit byte.
    let mut length = [0; 4];
    model_peer.read_exact(&mut length).unwrap();
    let length = u32::from_le_bytes(length) as usize;
    assert!(length <= 4096);
    let mut handoff = vec![0; length];
    model_peer.read_exact(&mut handoff).unwrap();
    let handoff: serde_json::Value = serde_json::from_slice(&handoff).unwrap();
    assert_eq!(handoff["app_id"], binding.app);
    launcher.launch(permit, services).unwrap();
    let supervisor = started(&mut launcher);
    let admitted = pending
        .admit_connection(&supervisor, authority.as_ref())
        .unwrap();
    let (agent, collab) = admitted.into_parts();
    let topology = qualify_topology(&agent, &collab, &supervisor);
    qualify_pre_effect(&mut launcher, &topology, owned_stop);
    if owned_stop {
        launcher.stop(&binding).unwrap();
    } else {
        model_peer.write_all(b"E").unwrap();
    }
    qualify_finished(
        &mut launcher,
        &topology,
        (&binding, &facts, &supervisor),
        owned_stop,
    );
}

struct Topology {
    agent_pidfd: PinnedProcess,
    orphan: Orphan,
    unrelated_pidfd: PinnedProcess,
    // The exact unrelated child remains owned until all proof steps complete.
    _unrelated: Reap,
}

fn qualify_topology(
    agent: &VerifiedManagedProcess,
    collab: &UnixStream,
    supervisor: &VerifiedManagedSupervisor,
) -> Topology {
    let agent_pidfd = PinnedProcess::capture(agent.process().pid);
    collab
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    collab
        .set_write_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let mut orphan = None;
    let mut parent_reaped = false;
    for _ in 0..2 {
        let (byte, sender) = marker(collab);
        match byte {
            b'R' => {
                assert!(orphan.is_none());
                orphan = Some(Orphan(PinnedProcess::capture(sender)));
            }
            b'P' => {
                assert_eq!(sender, agent.process().pid);
                assert!(!parent_reaped);
                parent_reaped = true;
            }
            other => panic!("unexpected fixture marker {other}"),
        }
    }
    assert!(parent_reaped);
    let orphan = orphan.expect("reparented grandchild READY required");
    assert_ne!(orphan.0.identity, agent.process());
    let reaper = proc_number(agent.process().pid, "PPid:");
    assert_eq!(
        proc_number(agent.process().pid, "NSpid:"),
        2,
        "bwrap default application PID"
    );
    assert_eq!(
        proc_number(reaper, "NSpid:"),
        1,
        "actual bwrap namespace reaper"
    );
    assert_eq!(
        proc_number(orphan.0.identity.pid, "PPid:"),
        reaper,
        "grandchild actually reparented"
    );
    assert!(proc_number(orphan.0.identity.pid, "NSpid:") > 2);
    let namespace = std::fs::read_link(format!("/proc/{}/ns/pid", agent.process().pid)).unwrap();
    assert_eq!(
        std::fs::read_link(format!("/proc/{}/ns/pid", orphan.0.identity.pid)).unwrap(),
        namespace
    );
    assert_eq!(
        std::fs::read_link(format!("/proc/{reaper}/ns/pid")).unwrap(),
        namespace
    );
    let unrelated = Reap(
        Command::new("/bin/sleep")
            .arg("60")
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let unrelated_pidfd = PinnedProcess::capture(unrelated.0.id());
    eprintln!(
        "managed-tree topology: supervisor={:?} agent={:?} reaper={reaper} orphan={:?} unrelated={:?}",
        supervisor.process(),
        agent.process(),
        orphan.0.identity,
        unrelated_pidfd.identity
    );
    Topology {
        agent_pidfd,
        orphan,
        unrelated_pidfd,
        _unrelated: unrelated,
    }
}

fn qualify_pre_effect(launcher: &mut ManagedLauncher, topology: &Topology, owned_stop: bool) {
    // This observation is mandatory in both cases: the orphan demonstrably
    // survives its immediate parent and remains blocked before the effect.
    // No test command, EOF or voluntary fixture path can release that orphan.
    let observation_end = Instant::now()
        .checked_add(Duration::from_millis(250))
        .unwrap();
    loop {
        topology.agent_pidfd.assert_live();
        topology.orphan.0.assert_live();
        topology.unrelated_pidfd.assert_live();
        assert!(
            launcher.drain_events().unwrap().is_empty(),
            "event exposed while fixture tree must remain live"
        );
        if Instant::now() >= observation_end {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    eprintln!(
        "managed-tree pre-effect: owned_stop={owned_stop}, exact agent and reparented orphan live for >=250ms, no Finished"
    );
}
fn qualify_finished(
    launcher: &mut ManagedLauncher,
    topology: &Topology,
    expected: (
        &ManagedBinding,
        &launcher::managed::ManagedRuntimeFacts,
        &VerifiedManagedSupervisor,
    ),
    owned_stop: bool,
) {
    let (binding, facts, supervisor) = expected;
    let deadline = tilo_test_env::hang_deadline();
    let mut finished = 0_u32;
    while finished == 0 {
        for event in launcher.drain_events().unwrap() {
            match event {
                ManagedLaunchEvent::Finished {
                    binding: actual,
                    process,
                    exit,
                    ..
                } => {
                    // No wait/grace period after observing Finished: its barrier
                    // must already cover the exact retained original descendant.
                    assert!(
                        topology.orphan.0.exited(),
                        "Finished exposed while original orphan remains live"
                    );
                    assert!(
                        topology.agent_pidfd.exited(),
                        "Finished exposed before actual agent exit"
                    );
                    topology.unrelated_pidfd.assert_live();
                    assert_eq!(&actual, binding);
                    assert_eq!(process, Some(supervisor.process()));
                    assert_eq!(exit.binding(), binding);
                    assert_eq!(exit.supervisor(), supervisor.process());
                    assert_eq!(exit.runtime_facts(), facts);
                    if !owned_stop {
                        assert!(exit.status().success());
                    }
                    finished = finished.checked_add(1).unwrap();
                }
                ManagedLaunchEvent::Failed { reason, .. } => {
                    panic!("uncertain managed cleanup: {reason}")
                }
                ManagedLaunchEvent::Started(_) => panic!("duplicate Started"),
            }
        }
        topology.unrelated_pidfd.assert_live();
        assert!(
            Instant::now() < deadline,
            "whole-tree Finished deadline; owned_stop={owned_stop}"
        );
        if finished == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert_eq!(finished, 1);
    launcher
        .shutdown()
        .expect("exact managed worker shutdown must finish without uncertainty");
    assert!(
        launcher.drain_events().unwrap().is_empty(),
        "duplicate/late terminal event after join"
    );
    assert!(topology.orphan.0.exited());
    topology.unrelated_pidfd.assert_live();
    eprintln!(
        "managed-tree PASS: owned_stop={owned_stop}, Finished=1, original agent and orphan pidfds exited, unrelated task live, shutdown joined; no orphan release path"
    );
}

#[test]
fn managed_bwrap_wait_covers_reparented_descendant_natural_exit() {
    qualify_tree(false);
}

#[test]
fn managed_bwrap_wait_covers_reparented_descendant_owned_stop() {
    qualify_tree(true);
}
