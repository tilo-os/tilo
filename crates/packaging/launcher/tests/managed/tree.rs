//! Actual Linux process-tree qualification. Missing kernel/sandbox facilities
//! are failures. This fixture proves custody barriers, not model/agent readiness.
use launcher::{
    managed::{
        InstalledManagedPackage, ManagedBinding, ManagedLaunchAuthority, ManagedLaunchEvent,
        ManagedLauncher, ManagedRuntime, PackageTrust, ProcessIdentity, VerifiedManagedProcess,
        VerifiedManagedSupervisor,
        channel::{ManagedProcessAuthority, PendingManagedConnection},
    },
    services::{EnrolledServiceContext, ManagedProviderSelection},
};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{
            fs::{OpenOptionsExt, PermissionsExt},
            net::UnixStream,
        },
    },
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tilo_app_package::BlobStore;

const DEADLINE: Duration = Duration::from_secs(10);

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
            fd: unsafe { OwnedFd::from_raw_fd(raw as i32) },
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
        let count = unsafe { libc::poll(&mut poll, 1, 0) };
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

fn fixture_package(root: &Path) -> (InstalledManagedPackage, PackageTrust) {
    let mut seed = [0; 32];
    File::open("/dev/urandom")
        .unwrap()
        .read_exact(&mut seed)
        .unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&seed);
    let public = key.verifying_key().to_bytes();
    let public_hex: String = public.iter().map(|b| format!("{b:02x}")).collect();
    let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
        r#"
[package]
id = "so.tilo.agent"
version = "1.0.0"
kind = "service"
runtime = "tilo-runtime/2026.07"
[package.isa]
x86_64 = "baseline"
aarch64 = "armv8-a"
[publisher]
identity = "ed25519:{public_hex}"
signature = ""
[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount"]
seccomp = "tilo-default"
landlock = "strict"
[sandbox.net]
mode = "off"
[sandbox.services]
collab = "required@1"
model-gateway = "required@1"
[entrypoint]
bin = "bin/agent"
type = "cli"
"#
    ))
    .unwrap();
    let source = root.join("package-source");
    std::fs::create_dir_all(source.join("bin")).unwrap();
    std::fs::copy(
        env!("CARGO_BIN_EXE_managed-tree-fixture"),
        source.join("bin/agent"),
    )
    .unwrap();
    let mut blobs = tilo_app_package::MemoryBlobStore::new();
    let id = tilo_app_package::pack(
        &source,
        &manifest,
        &tilo_app_package::SigningIdentity::from_signing_key(key),
        &mut blobs,
    )
    .unwrap();
    let trust = PackageTrust {
        publishers: vec![public],
        attestors: vec![],
        revoked: Default::default(),
    };
    assert!(matches!(
        tilo_app_package::verify_with_trust(
            id,
            &blobs,
            &trust.publishers,
            &trust.attestors,
            &trust.revoked
        ),
        tilo_app_package::Verification::Trusted { .. }
    ));
    let payload = root.join("installed-payload");
    tilo_app_package::unpack(id, &blobs, &payload).unwrap();
    let signed = blobs
        .get(&tilo_app_package::BlobHash(*id.as_bytes()))
        .unwrap()
        .unwrap();
    let signed_root = tilo_app_package::SignedRoot::decode(&signed).unwrap();
    std::fs::write(
        payload.join("manifest.toml"),
        blobs.get(&signed_root.manifest_hash).unwrap().unwrap(),
    )
    .unwrap();
    install::integrity::record(
        root,
        "so.tilo.agent",
        &signed,
        &blobs.get(&signed_root.manifest_hash).unwrap().unwrap(),
        &blobs.get(&signed_root.payload_root_hash).unwrap().unwrap(),
    )
    .unwrap();
    let package = InstalledManagedPackage::verify(
        launcher::registry::InstalledPackage {
            id: "so.tilo.agent".into(),
            package_id_hex: id.to_hex(),
            manifest_path: payload.join("manifest.toml"),
            payload_root: payload,
        },
        root.to_path_buf(),
        id,
        &trust,
    )
    .unwrap();
    (package, trust)
}

fn fixture_runtime(root: &Path) -> ManagedRuntime {
    let identity = runtime_bundle::Identity::generate();
    let identity_path = root.join("runtime-development-identity.json");
    let mut key_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&identity_path)
        .unwrap();
    key_file.write_all(&identity.to_json_bytes()).unwrap();
    assert_eq!(
        key_file.metadata().unwrap().permissions().mode() & 0o777,
        0o600
    );
    let bundle_root = root.join("runtime");
    let bundle = runtime_bundle::build_bundle_with(
        &bundle_root,
        "2026.07",
        &runtime_bundle::BuildOptions {
            from: None,
            arch: Some(std::env::consts::ARCH),
            identity: Some(&identity_path),
            extra_libs: &[],
        },
    )
    .unwrap();
    // Trust is separately selected from the freshly generated task-private key.
    assert_eq!(
        bundle.publisher_hex.as_deref(),
        Some(identity.public_key_hex().as_str())
    );
    ManagedRuntime {
        bundle_root,
        runtime_publisher: identity.public_key_hex(),
        sandbox_shim: env!("CARGO_BIN_EXE_tilo-launcher").into(),
        bwrap: std::env::var_os("TILO_MANAGED_TEST_BWRAP")
            .unwrap_or_else(|| "/usr/bin/bwrap".into())
            .into(),
    }
}

fn started(launcher: &mut ManagedLauncher) -> VerifiedManagedSupervisor {
    let deadline = Instant::now() + DEADLINE;
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
        iov_base: (&mut byte as *mut u8).cast(),
        iov_len: 1,
    };
    let mut control = [0usize; 16];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&control);
    let count = unsafe { libc::recvmsg(socket.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) };
    let mut rights = Vec::new();
    let mut credentials = Vec::new();
    let mut unexpected = false;
    let mut header = unsafe { libc::CMSG_FIRSTHDR(&message) };
    while !header.is_null() {
        let h = unsafe { &*header };
        if h.cmsg_level == libc::SOL_SOCKET && h.cmsg_type == libc::SCM_RIGHTS {
            let bytes = h
                .cmsg_len
                .saturating_sub(unsafe { libc::CMSG_LEN(0) } as usize);
            for i in 0..bytes / std::mem::size_of::<i32>() {
                let raw = unsafe {
                    libc::CMSG_DATA(header)
                        .cast::<i32>()
                        .add(i)
                        .read_unaligned()
                };
                rights.push(unsafe { OwnedFd::from_raw_fd(raw) });
            }
            unexpected = true;
        } else if h.cmsg_level == libc::SOL_SOCKET
            && h.cmsg_type == libc::SCM_CREDENTIALS
            && h.cmsg_len
                == unsafe { libc::CMSG_LEN(std::mem::size_of::<libc::ucred>() as u32) } as usize
        {
            credentials.push(unsafe {
                libc::CMSG_DATA(header)
                    .cast::<libc::ucred>()
                    .read_unaligned()
            });
        } else {
            unexpected = true;
        }
        header = unsafe { libc::CMSG_NXTHDR(&message, header) };
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
    let (package, trust) = fixture_package(root.path());
    let runtime = fixture_runtime(root.path());
    let binding = ManagedBinding {
        room: uuid::Uuid::new_v4().to_string(),
        app: "so.tilo.agent".into(),
        agent: "tree-fixture".into(),
        task: "tree-task".into(),
        run: "tree-run".into(),
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
    model_peer.set_read_timeout(Some(DEADLINE)).unwrap();
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
    let agent_pidfd = PinnedProcess::capture(agent.process().pid);
    collab.set_read_timeout(Some(DEADLINE)).unwrap();
    collab.set_write_timeout(Some(DEADLINE)).unwrap();
    let mut orphan = None;
    let mut parent_reaped = false;
    for _ in 0..2 {
        let (byte, sender) = marker(&collab);
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
    // This observation is mandatory in both cases: the orphan demonstrably
    // survives its immediate parent and remains blocked before the effect.
    // No test command, EOF or voluntary fixture path can release that orphan.
    let observation_end = Instant::now() + Duration::from_millis(250);
    loop {
        agent_pidfd.assert_live();
        orphan.0.assert_live();
        unrelated_pidfd.assert_live();
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
    if owned_stop {
        launcher.stop(&binding).unwrap();
    } else {
        model_peer.write_all(b"E").unwrap();
    }
    let deadline = Instant::now() + DEADLINE;
    let mut finished = 0;
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
                        orphan.0.exited(),
                        "Finished exposed while original orphan remains live"
                    );
                    assert!(
                        agent_pidfd.exited(),
                        "Finished exposed before actual agent exit"
                    );
                    unrelated_pidfd.assert_live();
                    assert_eq!(actual, binding);
                    assert_eq!(process, Some(supervisor.process()));
                    assert_eq!(exit.binding(), &binding);
                    assert_eq!(exit.supervisor(), supervisor.process());
                    assert_eq!(exit.runtime_facts(), &facts);
                    if !owned_stop {
                        assert!(exit.status().success());
                    }
                    finished += 1;
                }
                ManagedLaunchEvent::Failed { reason, .. } => {
                    panic!("uncertain managed cleanup: {reason}")
                }
                ManagedLaunchEvent::Started(_) => panic!("duplicate Started"),
            }
        }
        unrelated_pidfd.assert_live();
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
    assert!(orphan.0.exited());
    unrelated_pidfd.assert_live();
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

/// Provisioning only: the Cloud shutdown regression supplies its real local
/// gateway socket/provider, then loads this base through strict ManagedConfig.
/// This does not launch a process or construct/export any live witness.
#[test]
#[ignore = "explicit task-private P2 fixture provisioning; requires TILO_MANAGED_TEST_EXPORT_DIR"]
fn export_p2_fixture() {
    use std::os::unix::fs::MetadataExt;

    fn write_new(path: &Path, bytes: &[u8]) {
        assert!(bytes.len() <= 64 * 1024, "fixture metadata exceeds bound");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all(bytes).unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }

    let root = std::path::PathBuf::from(
        std::env::var_os("TILO_MANAGED_TEST_EXPORT_DIR")
            .expect("explicit empty private fixture destination required"),
    );
    assert!(root.is_absolute(), "fixture destination must be absolute");
    let metadata = std::fs::symlink_metadata(&root).unwrap();
    assert!(
        metadata.is_dir(),
        "fixture destination must be a real directory"
    );
    assert_eq!(
        metadata.uid(),
        unsafe { libc::geteuid() },
        "fixture destination must be owned by this user"
    );
    assert_eq!(
        metadata.permissions().mode() & 0o777,
        0o700,
        "fixture destination must already be private"
    );
    assert_eq!(
        root.canonicalize().unwrap(),
        root,
        "fixture destination must be canonical without symlink components"
    );
    assert!(
        std::fs::read_dir(&root).unwrap().next().is_none(),
        "fixture destination must be empty; never overwrite an existing fixture or user state"
    );
    // create_new claims the empty destination against a second exporter. A
    // failed attempt deliberately stays nonempty and cannot silently resume.
    write_new(&root.join(".export-in-progress"), b"provisioning only\n");
    let (package, trust) = fixture_package(&root);
    let runtime = fixture_runtime(&root);
    package.revalidate(&trust).unwrap();
    runtime_bundle::verify_bundle_with(
        &runtime.bundle_root,
        &runtime_bundle::VerifyOptions {
            require_publisher: Some(runtime.runtime_publisher.clone()),
            require_signature: true,
            require_arch: Some(std::env::consts::ARCH.into()),
        },
    )
    .unwrap();
    let publishers: Vec<String> = trust
        .publishers
        .iter()
        .map(|key| key.iter().map(|byte| format!("{byte:02x}")).collect())
        .collect();
    assert_eq!(publishers.len(), 1);
    let attestors = root.join("trusted-attestors.toml");
    let revoked = root.join("revoked-keys.toml");
    let attestor_text = tilo_app_package::trust::TrustedAttestors::empty()
        .to_toml_string()
        .unwrap();
    let revoked_text = tilo_app_package::trust::RevocationList::empty()
        .to_toml_string()
        .unwrap();
    assert!(
        tilo_app_package::trust::TrustedAttestors::from_toml_str(&attestor_text)
            .unwrap()
            .attestor
            .is_empty()
    );
    assert_eq!(
        tilo_app_package::trust::RevocationList::from_toml_str(&revoked_text).unwrap(),
        tilo_app_package::trust::RevocationList::empty()
    );
    write_new(&attestors, attestor_text.as_bytes());
    write_new(&revoked, revoked_text.as_bytes());
    let agent_data_home = root.join("agent-data");
    std::fs::create_dir(&agent_data_home).unwrap();
    std::fs::set_permissions(&agent_data_home, std::fs::Permissions::from_mode(0o700)).unwrap();

    let package_hash = package.package_id().to_hex();
    let installed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let registry = serde_json::json!({"entry":[{
        "id":"so.tilo.agent", "version":"1.0.0",
        "package_id_hex":package_hash, "payload_root":package.payload_root(),
        "manifest_path":package.manifest_path(), "publisher_key_hex":publishers[0],
        "pinned":true, "installed_at":installed_at, "attested_by":"", "cli_commands":[],
        "runtime":"tilo-runtime/2026.07", "source":"managed-tree test fixture"
    }]});
    write_new(
        &root.join("registry.toml"),
        toml::to_string(&registry).unwrap().as_bytes(),
    );
    let config = serde_json::json!({
        "version":1, "package_hash":package_hash,
        "package_payload":package.payload_root(), "package_manifest":package.manifest_path(),
        "package_data_root":root, "trusted_publishers":publishers,
        "trusted_attestors":attestors, "revoked_keys":revoked,
        "runtime_bundle":runtime.bundle_root, "runtime_publisher":runtime.runtime_publisher,
        "sandbox_shim":runtime.sandbox_shim.canonicalize().unwrap(),
        "bwrap":runtime.bwrap.canonicalize().unwrap(), "agent_data_home":agent_data_home
    });
    // Deliberately omit gateway_socket/provider. They must come from the actual
    // Cloud test's local configuration; this fragment cannot advertise readiness.
    let config_path = root.join("managed-config-base.json");
    write_new(&config_path, &serde_json::to_vec_pretty(&config).unwrap());
    std::fs::rename(root.join(".export-in-progress"), root.join(".export-ready")).unwrap();
    eprintln!(
        "provisioned signed test fixture metadata at {}; no process/provider execution or runtime qualification performed",
        config_path.display()
    );
}
