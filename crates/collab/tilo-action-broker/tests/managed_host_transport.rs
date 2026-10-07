#![cfg(feature = "enrolled-task-test")]
//! Actual signed sandbox/process and Capnp qualification; unavailable subjects fail.
tilo_automerge_isolate::test_worker!();

use launcher::managed::{InstalledManagedPackage, ManagedRuntime, PackageTrust};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

fn fixture_package(root: &Path) -> Qualification<(InstalledManagedPackage, PackageTrust)> {
    let identity = tilo_app_package::SigningIdentity::generate();
    let public = identity.public_key_bytes();
    let public_hex = hex::encode(public);
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
    ))?;
    let source = root.join("package-source");
    std::fs::create_dir_all(source.join("bin"))?;
    std::fs::copy(
        env!("CARGO_BIN_EXE_enrolled-task-fixture"),
        source.join("bin/agent"),
    )?;
    let packed = tilo_app_package::pack(&source, &manifest, &identity)?;
    let id = packed.id();
    let trust = PackageTrust {
        publishers: vec![public],
        attestors: vec![],
        revoked: tilo_app_package::RevocationList::default(),
    };
    require(
        matches!(
            tilo_app_package::verify_with_trust(
                &packed.meta,
                &trust.publishers,
                &trust.attestors,
                &trust.revoked
            ),
            tilo_app_package::Verification::Trusted { .. }
        ),
        "qualification condition failed",
    )?;
    let payload = root.join("installed-payload");
    tilo_app_package::unpack(&packed.meta, &packed, &payload)?;
    std::fs::write(payload.join("manifest.toml"), &packed.meta.manifest_bytes)?;
    let evidence = package_store::integrity::legacy_record_dir(root, "so.tilo.agent");
    std::fs::create_dir_all(&evidence)?;
    let signed = packed.meta.signed_root_bytes();
    for (name, bytes) in [
        ("signed-root", signed.as_slice()),
        ("manifest.toml", packed.meta.manifest_bytes.as_slice()),
        ("collection.qcl", packed.meta.collection_bytes.as_slice()),
    ] {
        std::fs::write(evidence.join(name), bytes)?;
    }
    let package = InstalledManagedPackage::verify(
        launcher::managed::agent_package_row(
            id.to_hex(),
            payload.clone(),
            payload.join("manifest.toml"),
        ),
        root.to_path_buf(),
        id,
        &trust,
    )?;
    Ok((package, trust))
}

fn fixture_runtime(root: &Path) -> Qualification<ManagedRuntime> {
    let identity = runtime_bundle::Identity::generate();
    let identity_path = root.join("runtime-development-identity.json");
    let mut key_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&identity_path)?;
    key_file.write_all(&identity.to_json_bytes())?;
    require_eq(&(key_file.metadata()?.permissions().mode() & 0o777), 0o600)?;
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
    )?;
    // Trust is separately selected from the freshly generated task-private key.
    require_eq(
        &(bundle.publisher_hex.as_deref()),
        Some(identity.public_key_hex().as_str()),
    )?;
    Ok(ManagedRuntime {
        bundle_root,
        runtime_publisher: identity.public_key_hex(),
        sandbox_shim: env!("CARGO_BIN_EXE_enrolled-task-fixture").into(),
        bwrap: std::env::var_os("TILO_MANAGED_TEST_BWRAP")
            .unwrap_or_else(|| "/usr/bin/bwrap".into())
            .into(),
    })
}

#[path = "enrolled/native.rs"]
mod native;
#[path = "managed_host/native_host.rs"]
mod native_host;
use ed25519_dalek::Signer as _;
use launcher::{
    managed::{ManagedBinding, ManagedLaunchEvent},
    services::ManagedProviderSelection,
};
use std::{
    os::unix::net::UnixStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};
use tilo_action_broker::{
    collab_broker::FanoutState, collab_core::BrokerChannels, collab_outbound::PeerPublisher,
    services::HostServices,
};
use tilo_peer_state::{
    owner_commit::{CommitOperation, OwnerCommitRequest},
    workspace_task::{WorkspaceTaskAuthority, WorkspaceTaskCommand},
};

type Qualification<T> = Result<T, Box<dyn std::error::Error>>;
struct Broker {
    quit: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    fanout: Arc<FanoutState>,
}
impl Drop for Broker {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _result = thread.join();
        }
    }
}
fn broker(
    root: &Path,
    native: &Arc<native::Native>,
    certificates: Arc<dyn CertificateAuthority>,
) -> Qualification<Broker> {
    let socket = root.join("control.sock");
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
    let fanout = Arc::new(FanoutState::new());
    let quit = Arc::new(AtomicBool::new(false));
    let services = HostServices {
        workspace_memberships: Some(native.clone()),
        native_certificates: Some(certificates),
        ..Default::default()
    };
    let thread = tilo_action_broker::collab_broker_capnp::spawn(
        socket,
        &fanout,
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::with_optional_owner_commit(None, native.clone()),
        Some(native.clone()),
        services,
        quit.clone(),
    )?;
    Ok(Broker {
        quit,
        thread: Some(thread),
        fanout,
    })
}
fn read_frame(peer: &mut UnixStream, stage: &str) -> Qualification<serde_json::Value> {
    let mut length = [0; 4];
    peer.read_exact(&mut length)?;
    let length = usize::try_from(u32::from_le_bytes(length))?;
    if length > 128 * 1024 {
        return Err("private fixture frame exceeds bound".into());
    }
    let mut bytes = vec![0; length];
    peer.read_exact(&mut bytes)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    if value.get("stage").and_then(serde_json::Value::as_str) != Some(stage) {
        return Err("actual fixture stage changed".into());
    }
    Ok(value)
}

fn queued(
    native: &native::Native,
    certificate: &tilo_user_identity::DeviceCert,
    governor: (&ed25519_dalek::SigningKey, &tilo_user_identity::DeviceCert),
) -> Qualification<String> {
    use tilo_peer_state::workspace_membership::{
        AgentMembershipPreset, WorkspaceMembershipCommand,
    };
    let membership = OwnerCommitRequest {
        room: native.room.to_string(),
        operation: CommitOperation::WorkspaceMembershipCommand {
            invocation: uuid::Uuid::new_v4(),
            caller: certificate.clone(),
            command: WorkspaceMembershipCommand::AddAgent {
                agent: "fixture-agent".into(),
                preset: AgentMembershipPreset::Contributor,
            },
        },
    };
    native
        .owner
        .commit_workspace_membership_command(
            membership.clone(),
            governor.1,
            governor.0,
            SystemTime::now(),
        )?
        .validate_for(&membership)?;
    let task_invocation = uuid::Uuid::new_v4();
    let request = OwnerCommitRequest {
        room: native.room.to_string(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: task_invocation,
            authority: WorkspaceTaskAuthority::Person {
                caller: certificate.clone(),
            },
            command: WorkspaceTaskCommand::Queue {
                title: "Qualify enrolled transport".into(),
                assigned_agent: "fixture-agent".into(),
                route: tilo_desktop_model::WorkspaceTaskRoute::Model,
                resource: None,
            },
        },
    };
    native
        .owner
        .commit_workspace_task_command(request.clone(), SystemTime::now())?
        .validate_for(&request)?;
    let snapshot = native.owner.workspace_task_snapshot(native.room)?;
    let result = snapshot
        .controls
        .values()
        .flat_map(|control| control.receipts.iter())
        .find(|receipt| {
            receipt.invocation == task_invocation && receipt.device == certificate.device_public_key
        })
        .ok_or("actual queue receipt absent")?;
    let tilo_desktop_model::WorkspaceTaskMutationResult::Queued { task_id } = &result.result else {
        return Err("native receipt did not queue task".into());
    };
    Ok(task_id.clone())
}

use tilo_action_broker::{
    managed_host::{
        ManagedHost, ManagedHostCustody, ManagedHostInstallation, ManagedNativeAuthority,
        ManagedServiceConnections,
    },
    peer_actions::{CertificateAuthority, OwnerPolicyClock, PolicyOrigin, PolicyTime},
};
struct Certificates {
    native: Arc<native::Native>,
    origin: PolicyOrigin,
}
impl CertificateAuthority for Certificates {
    fn policy_time(&self) -> Result<Option<PolicyTime>, String> {
        self.origin
            .capture()
            .map(Some)
            .map_err(|error| error.to_string())
    }
    fn authorize(
        &self,
        room: uuid::Uuid,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        self.native.authorize(room, certificate)
    }
    fn authorize_host_worker(
        &self,
        route: &tilo_action_broker::route::CrdtRoute,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        self.authorize_host(&route.cluster_id, certificate)
    }
    fn authorize_join(
        &self,
        room: uuid::Uuid,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        self.native.authorize_join(room, certificate)
    }
}
impl ManagedHostCustody for Certificates {
    fn authorize_host(
        &self,
        room: &str,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        let room = tilo_desktop_model::RoomId::parse(room).ok_or("invalid host room")?;
        self.native.owner.with_current(room, |view| {
            let pinned = match view.host() {
                Some(host) => host == certificate && certificate.capabilities.can_act_as_host,
                None => view.governor() == certificate,
            };
            if !pinned
                || view.device() != certificate
                || (!certificate.capabilities.can_act_as_host
                    && certificate.device_public_key != view.peer().authority().device_public_key)
            {
                return Err("actual native host custody absent".into());
            }
            Ok(())
        })
    }
}
struct Connections {
    certificate: tilo_user_identity::DeviceCert,
    key: ed25519_dalek::SigningKey,
    model_peer: Mutex<Option<UnixStream>>,
}
impl ManagedServiceConnections for Connections {
    fn connect(
        &self,
        binding: &ManagedBinding,
        _: &ManagedProviderSelection,
    ) -> Result<(UnixStream, UnixStream), String> {
        let (identity, mut peer) = UnixStream::pair().map_err(|error| error.to_string())?;
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: binding.room.clone(),
            device_certificate: serde_json::to_vec(&self.certificate)
                .map_err(|error| error.to_string())?,
            proof: self
                .key
                .sign(&runtime_identity::collaboration_join_signing_bytes(
                    &binding.room,
                ))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .map_err(|error| error.to_string())?;
        let bytes = serde_json::to_vec(&serde_json::json!({"channel":tilo_action_broker::route::CrdtRoute::app(binding.app.clone(),binding.room.clone()).channel_key(),"presentation":presentation}))
            .map_err(|error| error.to_string())?;
        peer.write_all(
            &u32::try_from(bytes.len())
                .map_err(|_| "identity frame bound")?
                .to_le_bytes(),
        )
        .and_then(|()| peer.write_all(&bytes))
        .map_err(|error| error.to_string())?;
        let (model, model_peer) = UnixStream::pair().map_err(|error| error.to_string())?;
        model_peer
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .map_err(|error| error.to_string())?;
        *self
            .model_peer
            .lock()
            .map_err(|_| "fixture model peer unavailable")? = Some(model_peer);
        Ok((identity, model))
    }
}
#[test]
fn production_controller_commits_before_actual_signed_spawn_and_admits_exact_task() {
    controller_transport(Admission::Governor).unwrap();
}
#[test]
fn production_controller_accepts_actual_enrolled_delegated_host() {
    controller_transport(Admission::Delegated).unwrap();
}
#[derive(Clone, Copy)]
enum Admission {
    Governor,
    Delegated,
    Member,
}
#[test]
fn actual_enrolled_non_host_member_is_refused_before_any_launch_claim() {
    refuse_non_host().unwrap();
}
fn refuse_non_host() -> Qualification<()> {
    let mut fixture = controller_setup(Admission::Member)?;
    require(
        fixture
            .host
            .launch_task(&fixture.native.room.to_string(), &fixture.task)
            .is_err(),
        "non-host member launched",
    )?;
    let entries = std::fs::read_dir(&fixture.journal)?.collect::<Result<Vec<_>, _>>()?;
    require(
        !entries
            .iter()
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".claim.json")),
        "non-host created a durable launch claim",
    )?;
    fixture.host.shutdown()?;
    drop(fixture);
    Ok(())
}
fn controller_setup(admission: Admission) -> Qualification<RunningController> {
    let root = tempfile::tempdir()?;
    let (native, key, certificate, task) = if matches!(admission, Admission::Governor) {
        let (native, key, certificate) = native::installed(root.path())?;
        let task = queued(&native, &certificate, (&key, &certificate))?;
        (native, key, certificate, task)
    } else {
        let enrolled = if matches!(admission, Admission::Member) {
            native_host::member(root.path())?
        } else {
            native_host::delegated(root.path())?
        };
        (
            enrolled.native,
            enrolled.key,
            enrolled.certificate,
            enrolled.task,
        )
    };
    let certificates = Arc::new(Certificates {
        native: native.clone(),
        origin: OwnerPolicyClock::new()?.new_origin(),
    });
    let broker = broker(root.path(), &native, certificates.clone())?;
    let state = broker.fanout.capnp_state().ok_or("native broker absent")?;
    let (package, trust) = fixture_package(root.path())?;
    let runtime = fixture_runtime(root.path())?;
    let journal = root.path().join("private-managed-journal");
    std::fs::create_dir(&journal)?;
    std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o700))?;
    let connections = Arc::new(Connections {
        certificate,
        key,
        model_peer: Mutex::default(),
    });
    let installation = ManagedHostInstallation {
        package,
        trust,
        runtime,
        journal_root: journal.clone(),
        data_home: root.path().to_path_buf(),
        provider: ManagedProviderSelection {
            profile_id: "signed-qualification".into(),
            model_id: "no-inference".into(),
            protocol: "anthropic".into(),
        },
    };
    let host = ManagedHost::new(
        installation.clone(),
        ManagedNativeAuthority {
            membership: native.clone(),
            tasks: native.clone(),
            certificates: certificates.clone(),
            host_custody: certificates,
        },
        connections.clone(),
        state,
    )?;
    Ok(RunningController {
        host,
        native,
        broker,
        connections,
        journal,
        task,
        installation,
        _root: root,
    })
}
struct RunningController {
    host: ManagedHost,
    native: Arc<native::Native>,
    broker: Broker,
    connections: Arc<Connections>,
    journal: std::path::PathBuf,
    task: String,
    installation: ManagedHostInstallation,
    _root: tempfile::TempDir,
}
fn controller_transport(admission: Admission) -> Qualification<()> {
    let RunningController {
        mut host,
        native,
        broker,
        connections,
        journal,
        task,
        installation: _,
        _root: root,
    } = controller_setup(admission)?;
    let launch = host.launch_task(&native.room.to_string(), &task)?;
    // The real controller has already fsynced a claim even before the process can attest.
    let claims: Vec<_> = std::fs::read_dir(&journal)?.collect::<Result<_, _>>()?;
    require(
        claims
            .iter()
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".claim.json")),
        "no durable pre-spawn claim",
    )?;
    require(
        host.launch_task(&native.room.to_string(), &task).is_err(),
        "same queued epoch launched twice",
    )?;
    let mut peer = take_model_peer(&connections)?;
    let supervisor = observe_started(&mut host)?;
    observe_actual_process(&journal, &launch, supervisor)?;
    let offered = read_frame(&mut peer, "offered")?;
    require(
        offered
            .get("envelope")
            .and_then(|value| value.get("launch_id"))
            .and_then(serde_json::Value::as_str)
            == Some(launch.as_str()),
        "offered launch differs",
    )?;
    peer.write_all(b"A")?;
    read_frame(&mut peer, "accepted")?;
    let snapshot = native.owner.workspace_task_snapshot(native.room)?;
    let control = snapshot.controls.get(&task).ok_or("task control absent")?;
    require(
        control.active_run.is_some() && control.run_epoch == 1,
        "controller did not admit actual native run",
    )?;
    peer.write_all(b"P")?;
    read_frame(&mut peer, "progress")?;
    peer.write_all(b"F")?;
    read_frame(&mut peer, "finished")?;
    host.shutdown()?;
    drop(host);
    drop(broker);
    drop(root);
    Ok(())
}
fn take_model_peer(connections: &Connections) -> Qualification<UnixStream> {
    let mut peer = connections
        .model_peer
        .lock()
        .map_err(|_| "model peer unavailable")?
        .take()
        .ok_or("model peer absent")?;
    let mut length = [0; 4];
    peer.read_exact(&mut length)?;
    let size = usize::try_from(u32::from_le_bytes(length))?;
    require(size <= 4096, "model handoff too large")?;
    let mut handoff = vec![0; size];
    peer.read_exact(&mut handoff)?;
    Ok(peer)
}
fn observe_started(host: &mut ManagedHost) -> Qualification<launcher::managed::ProcessIdentity> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let events = host.poll()?;
        if let Some(process) = events.iter().find_map(|event| match event {
            ManagedLaunchEvent::Started(supervisor) => Some(supervisor.process()),
            _ => None,
        }) {
            return Ok(process);
        }
        require(
            Instant::now() < deadline,
            "actual controller startup deadline",
        )?;
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn observe_actual_process(
    journal: &Path,
    launch: &str,
    supervisor: launcher::managed::ProcessIdentity,
) -> Qualification<()> {
    let entries = std::fs::read_dir(journal)?.collect::<Result<Vec<_>, _>>()?;
    let records = entries
        .iter()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .ends_with(".process.json")
        })
        .map(|entry| -> Qualification<serde_json::Value> {
            Ok(serde_json::from_slice(&std::fs::read(entry.path())?)?)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let record = records
        .iter()
        .find(|record| record.get("launch").and_then(serde_json::Value::as_str) == Some(launch))
        .ok_or("actual launch process record absent")?;
    require(
        record.get("launch").and_then(serde_json::Value::as_str) == Some(launch)
            && record
                .get("supervisor_pid")
                .and_then(serde_json::Value::as_u64)
                == Some(u64::from(supervisor.pid))
            && record
                .get("supervisor_start")
                .and_then(serde_json::Value::as_u64)
                == Some(supervisor.start),
        "durable exact supervisor identity differs",
    )?;
    let agent_pid = u32::try_from(
        record
            .get("agent_pid")
            .and_then(serde_json::Value::as_u64)
            .ok_or("agent PID absent")?,
    )?;
    let actual = launcher::managed::ProcessIdentity::read(agent_pid)?;
    require(
        record
            .get("agent_start")
            .and_then(serde_json::Value::as_u64)
            == Some(actual.start),
        "durable agent identity differs from actual kernel process",
    )?;
    Ok(())
}

fn require(condition: bool, message: &'static str) -> Qualification<()> {
    if !condition {
        return Err(message.into());
    }
    Ok(())
}
fn require_eq<T: PartialEq<U> + std::fmt::Debug, U: std::fmt::Debug>(
    observed: &T,
    expected: U,
) -> Qualification<()> {
    if *observed != expected {
        return Err(format!(
            "qualification mismatch: observed {observed:?}, expected {expected:?}"
        )
        .into());
    }
    Ok(())
}

#[test]
fn lost_startup_custody_refuses_the_same_queued_epoch_after_actual_owner_restart() {
    refuse_restart().unwrap();
}
fn refuse_restart() -> Qualification<()> {
    let RunningController {
        mut host,
        native,
        broker,
        connections,
        journal,
        task,
        installation,
        _root: root,
    } = controller_setup(Admission::Governor)?;
    let launch = host.launch_task(&native.room.to_string(), &task)?;
    let mut peer = take_model_peer(&connections)?;
    let supervisor = observe_started(&mut host)?;
    observe_actual_process(&journal, &launch, supervisor)?;
    read_frame(&mut peer, "offered")?;
    host.shutdown()?;
    drop(host);
    let certificates = Arc::new(Certificates {
        native: native.clone(),
        origin: OwnerPolicyClock::new()?.new_origin(),
    });
    let state = broker.fanout.capnp_state().ok_or("broker absent")?;
    let mut reopened = ManagedHost::new(
        installation,
        ManagedNativeAuthority {
            membership: native.clone(),
            tasks: native.clone(),
            certificates: certificates.clone(),
            host_custody: certificates,
        },
        connections,
        state,
    )?;
    require(
        reopened
            .launch_task(&native.room.to_string(), &task)
            .is_err(),
        "reopened owner retried ambiguous spawn",
    )?;
    require(
        reopened.poll()?.is_empty(),
        "reopened owner spawned from disk observations",
    )?;
    let snapshot = native.owner.workspace_task_snapshot(native.room)?;
    let control = snapshot.controls.get(&task).ok_or("native task absent")?;
    require(
        control.active_run.is_none() && control.run_epoch == 0,
        "startup observation falsely granted native execution",
    )?;
    let entries = std::fs::read_dir(&journal)?.collect::<Result<Vec<_>, _>>()?;
    let claim = entries
        .iter()
        .find(|entry| entry.file_name().to_string_lossy().ends_with(".claim.json"))
        .ok_or("original claim absent")?;
    let claim: serde_json::Value = serde_json::from_slice(&std::fs::read(claim.path())?)?;
    require(
        claim.get("launch").and_then(serde_json::Value::as_str) == Some(launch.as_str()),
        "restart overwrote original attempt",
    )?;
    reopened.shutdown()?;
    drop(reopened);
    drop(broker);
    drop(root);
    Ok(())
}

#[test]
fn one_signed_controller_keeps_two_real_native_task_processes_separate() {
    separate_tasks().unwrap();
}
fn separate_tasks() -> Qualification<()> {
    let mut fixture = controller_setup(Admission::Governor)?;
    let room = fixture.native.room.to_string();
    let first = fixture.host.launch_task(&room, &fixture.task)?;
    let mut first_peer = take_model_peer(&fixture.connections)?;
    let first_process = observe_started(&mut fixture.host)?;
    observe_actual_process(&fixture.journal, &first, first_process)?;
    let first_offer = read_frame(&mut first_peer, "offered")?;
    require(
        first_offer
            .pointer("/envelope/task/task_id")
            .and_then(serde_json::Value::as_str)
            == Some(fixture.task.as_str()),
        "first recipient offered another task",
    )?;
    let second_task = queued(
        &fixture.native,
        &fixture.connections.certificate,
        (&fixture.connections.key, &fixture.connections.certificate),
    )?;
    require(
        second_task != fixture.task,
        "owner did not create separate task",
    )?;
    let second = fixture.host.launch_task(&room, &second_task)?;
    let mut second_peer = take_model_peer(&fixture.connections)?;
    let second_process = observe_started(&mut fixture.host)?;
    observe_actual_process(&fixture.journal, &second, second_process)?;
    require(
        second != first && second_process != first_process,
        "controller reused live process or launch identity",
    )?;
    let second_offer = read_frame(&mut second_peer, "offered")?;
    require(
        second_offer
            .pointer("/envelope/task/task_id")
            .and_then(serde_json::Value::as_str)
            == Some(second_task.as_str()),
        "second recipient offered another task",
    )?;
    complete_recipient(&mut first_peer)?;
    complete_recipient(&mut second_peer)?;
    let snapshot = fixture
        .native
        .owner
        .workspace_task_snapshot(fixture.native.room)?;
    for task in [&fixture.task, &second_task] {
        let control = snapshot
            .controls
            .get(task)
            .ok_or("native task control absent")?;
        let definition = snapshot
            .tasks
            .iter()
            .find(|definition| &definition.id == task)
            .ok_or("native task definition absent")?;
        require(
            control.run_epoch == 1
                && control.active_run.is_none()
                && definition.status == tilo_desktop_model::WorkspaceTaskStatus::Done,
            "native task result crossed process boundaries",
        )?;
    }
    fixture.host.shutdown()?;
    drop(fixture);
    Ok(())
}
fn complete_recipient(peer: &mut UnixStream) -> Qualification<()> {
    peer.write_all(b"A")?;
    read_frame(peer, "accepted")?;
    peer.write_all(b"P")?;
    read_frame(peer, "progress")?;
    peer.write_all(b"F")?;
    read_frame(peer, "finished")?;
    Ok(())
}
