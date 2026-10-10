//! Actual signed sandbox/process and Capnp qualification; unavailable subjects fail.
tilo_automerge_isolate::test_worker!();

use launcher::managed::{InstalledManagedPackage, ManagedRuntime, PackageTrust};
use std::{
    fs::{File, OpenOptions},
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
use ed25519_dalek::Signer as _;
use launcher::{
    managed::{
        ManagedBinding, ManagedLaunchAuthority, ManagedLaunchEvent, ManagedLaunchScope,
        ManagedLauncher, VerifiedManagedProcess, VerifiedManagedSupervisor,
        channel::{ManagedProcessAuthority, PendingManagedConnection},
    },
    services::{EnrolledServiceContext, ManagedProviderSelection},
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
struct Authority {
    binding: ManagedBinding,
    claimed: AtomicBool,
    native: Arc<native::Native>,
    path: std::path::PathBuf,
}
impl ManagedLaunchAuthority for Authority {
    fn claim_launch(&self, binding: &ManagedBinding) -> Result<(), String> {
        self.revalidate_launch(binding)?;
        if self.claimed.swap(true, Ordering::SeqCst) {
            return Err("qualification launch already claimed".into());
        }
        Ok(())
    }
    fn revalidate_launch(&self, binding: &ManagedBinding) -> Result<(), String> {
        if binding != &self.binding {
            return Err("qualification actual launch binding changed".into());
        }
        self.native.owner.health()?;
        let ManagedLaunchScope::Task { task, .. } = &binding.scope else {
            return Err("task binding absent".into());
        };
        let snapshot = self
            .native
            .owner
            .workspace_task_snapshot(self.native.room)?;
        if !snapshot
            .tasks
            .iter()
            .any(|t| &t.id == task && t.assigned_agent.as_deref() == Some(&binding.agent))
        {
            return Err("actual native task binding absent".into());
        }
        Ok(())
    }
}
impl ManagedProcessAuthority for Authority {
    fn bind_process(&self, process: &VerifiedManagedProcess) -> Result<(), String> {
        self.revalidate_launch(process.binding())?;
        let facts = process.facts()?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&self.path)
            .map_err(|e| e.to_string())?;
        let bytes = serde_json::to_vec(&serde_json::json!({"actual_pid":facts.process.pid,"actual_start":facts.process.start,"launch":facts.binding.launch,"runtime_epoch":facts.runtime.runtime_epoch,"channel_id":facts.channel_id})).map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        File::open(self.path.parent().ok_or("private process parent absent")?)
            .and_then(|d| d.sync_all())
            .map_err(|e| e.to_string())
    }
}

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
fn broker(root: &Path, native: &Arc<native::Native>) -> Qualification<Broker> {
    let socket = root.join("control.sock");
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
    let fanout = Arc::new(FanoutState::new());
    let quit = Arc::new(AtomicBool::new(false));
    let services = HostServices {
        workspace_memberships: Some(native.clone()),
        native_certificates: Some(native.clone()),
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
fn started(launcher: &mut ManagedLauncher) -> Qualification<VerifiedManagedSupervisor> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(event) = launcher.drain_events()?.into_iter().next() {
            match event {
                ManagedLaunchEvent::Started(proof) => return Ok(proof),
                ManagedLaunchEvent::Failed { reason, .. } => return Err(reason.into()),
                ManagedLaunchEvent::Finished { .. } => {
                    return Err("fixture exited before actual process admission".into());
                }
            }
        }
        if Instant::now() >= deadline {
            return Err("actual managed startup deadline".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
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
    key: &ed25519_dalek::SigningKey,
    certificate: &tilo_user_identity::DeviceCert,
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
            certificate,
            key,
            SystemTime::now(),
        )?
        .validate_for(&membership)?;
    let request = OwnerCommitRequest {
        room: native.room.to_string(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: uuid::Uuid::new_v4(),
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
    Ok(native
        .owner
        .workspace_task_snapshot(native.room)?
        .tasks
        .into_iter()
        .next()
        .ok_or("actual queued native task absent")?
        .id)
}

#[test]
fn signed_kernel_recipient_accepts_and_reports_through_actual_capnp_and_native_owner() {
    actual_enrolled_transport(true).unwrap();
}

#[test]
fn signed_enrolled_recipient_acknowledges_only_the_actual_direction_revision() -> Qualification<()>
{
    let mut fixture = running_fixture()?;
    fixture.accept_and_progress()?;
    let request = OwnerCommitRequest {
        room: fixture.native.room.to_string(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: uuid::Uuid::new_v4(),
            authority: WorkspaceTaskAuthority::Person {
                caller: fixture.certificate.clone(),
            },
            command: WorkspaceTaskCommand::Direction {
                task_id: fixture.task.clone(),
                text: "Use the reviewed native task direction".into(),
            },
        },
    };
    fixture
        .native
        .owner
        .commit_workspace_task_command(request.clone(), SystemTime::now())?
        .validate_for(&request)?;
    let before = fixture
        .native
        .owner
        .workspace_task_snapshot(fixture.native.room)?;
    let (_, control) = task_view(&before, &fixture.task)?;
    require_eq(
        &control
            .directions
            .last()
            .map(|direction| direction.revision),
        Some(1),
    )?;
    require(
        control.directions[0].applied.is_none(),
        "direction already acknowledged",
    )?;

    fixture.model_peer.write_all(b"D")?;
    read_frame(&mut fixture.model_peer, "direction-applied")?;
    let after = fixture
        .native
        .owner
        .workspace_task_snapshot(fixture.native.room)?;
    let (_, control) = task_view(&after, &fixture.task)?;
    let applied = control.directions[0]
        .applied
        .as_ref()
        .ok_or("actual direction acknowledgement absent")?;
    require_eq(&(applied.agent_id.as_str()), "fixture-agent")?;
    require_eq(&(applied.run_epoch), 1)?;
    let run = control.active_run.as_ref().ok_or("active run absent")?;
    require_eq(&(run.applied_direction_revision), 1)?;
    require_eq(
        &(control
            .execution
            .as_ref()
            .ok_or("execution absent")?
            .sequence),
        1,
    )?;
    fixture.finish()?;
    fixture.stop()?;
    Ok(())
}

#[test]
fn disconnected_signed_recipient_abandons_only_its_actual_native_epoch() {
    actual_enrolled_transport(false).unwrap();
}

#[test]
fn revoked_native_membership_retires_the_idle_signed_recipient() {
    let mut fixture = running_fixture().unwrap();
    fixture.accept_and_progress().unwrap();
    fixture.revoke().unwrap();
    fixture.stop().unwrap();
}

#[test]
fn policy_changed_after_delivery_never_grants_agent_acceptance() {
    let mut fixture = running_fixture().unwrap();
    read_frame(&mut fixture.model_peer, "offered").unwrap();
    let budget = collab_types::task_budget::TaskExecutionBudget {
        turn_ms: 60_000,
        total_ms: 600_000,
    };
    let request = OwnerCommitRequest {
        room: fixture.native.room.to_string(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: uuid::Uuid::new_v4(),
            authority: WorkspaceTaskAuthority::Person {
                caller: fixture.certificate.clone(),
            },
            command: WorkspaceTaskCommand::SetExecutionBudget {
                task_id: fixture.task.clone(),
                expected_run_epoch: 0,
                expected_budget: None,
                budget: Some(budget),
            },
        },
    };
    fixture
        .native
        .owner
        .commit_workspace_task_command(request.clone(), SystemTime::now())
        .unwrap()
        .validate_for(&request)
        .unwrap();
    fixture.model_peer.write_all(b"A").unwrap();
    let refused = read_frame(&mut fixture.model_peer, "refused").unwrap();
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("native task execution budget changed after delivery")
    );
    let snapshot = fixture
        .native
        .owner
        .workspace_task_snapshot(fixture.native.room)
        .unwrap();
    let (_, control) = task_view(&snapshot, &fixture.task).unwrap();
    assert_eq!(control.run_epoch, 1);
    assert_eq!(control.execution_budget, Some(budget));
    assert!(
        control.execution.is_none(),
        "no Agent phase is committed under stale policy"
    );
    fixture.model_peer.write_all(b"E").unwrap();
    fixture.retire().unwrap();
    fixture.stop().unwrap();
}

#[test]
fn certified_person_channel_cannot_subscribe_to_managed_task_custody() {
    let root = tempfile::tempdir().unwrap();
    let (native, key, certificate) = native::installed(root.path()).unwrap();
    let broker = broker(root.path(), &native).unwrap();
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id: native.room.to_string(),
        device_certificate: serde_json::to_vec(&certificate).unwrap(),
        proof: key
            .sign(&runtime_identity::collaboration_join_signing_bytes(
                &native.room.to_string(),
            ))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(tokio::task::LocalSet::new().run_until(async {
        let (service, driver) = tilo_action_broker::collab_broker_capnp::dial_capnp_for_test(
            root.path().join("control.sock"),
        )
        .await
        .unwrap();
        let mut request = service.join_channel_request();
        request.get().set_channel_id(
            tilo_action_broker::route::CrdtRoute::cluster(native.room.to_string()).channel_key(),
        );
        request.get().set_identity(&presentation);
        request.get().set_participant_kind("person");
        request.get().set_participant_handle("Fixture owner");
        let response = request.send().promise.await.unwrap();
        let channel = response.get().unwrap().get_handle().unwrap();
        assert!(
            channel
                .subscribe_enrolled_tasks_request()
                .send()
                .promise
                .await
                .is_err(),
            "genuine ordinary Person admission must not substitute for a kernel-admitted descriptor"
        );
        driver.abort();
    }));
    drop(broker);
}

fn running_fixture() -> Qualification<RunningFixture> {
    let root = tempfile::tempdir()?;
    let (native, key, certificate) = native::installed(root.path())?;
    let task = queued(&native, &key, &certificate)?;
    let broker = broker(root.path(), &native)?;
    let state = broker
        .fanout
        .capnp_state()
        .ok_or("actual broker state absent")?;
    let (package, trust) = fixture_package(root.path())?;
    let runtime = fixture_runtime(root.path())?;
    let binding = ManagedBinding {
        room: native.room.to_string(),
        app: "so.tilo.agent".into(),
        agent: "fixture-agent".into(),
        scope: ManagedLaunchScope::Task {
            task: task.clone(),
            run: uuid::Uuid::new_v4().to_string(),
        },
        launch: uuid::Uuid::new_v4().to_string(),
        generation: 1,
    };
    let authority = Arc::new(Authority {
        binding: binding.clone(),
        claimed: AtomicBool::new(false),
        native: native.clone(),
        path: root.path().join("actual-process.json"),
    });
    let mut launcher = ManagedLauncher::new(package, trust, runtime, authority.clone())?;
    let facts = launcher.runtime_lease().revalidate()?;
    let mut permit = launcher.authorize(binding.clone())?;
    let (pending, collab_child) = PendingManagedConnection::pair(&mut permit)?;
    let (identity_child, mut identity_peer) = UnixStream::pair()?;
    let (model_child, mut model_peer) = UnixStream::pair()?;
    model_peer.set_read_timeout(Some(tilo_test_env::hang_bound()))?;
    let services = EnrolledServiceContext::new(
        binding,
        root.path().to_path_buf(),
        facts.runtime_epoch,
        pending.channel_id().into(),
        ManagedProviderSelection {
            profile_id: "qualification-only".into(),
            model_id: "no-inference".into(),
            protocol: "anthropic".into(),
        },
        identity_child,
        collab_child,
        model_child,
    )?;
    let mut length = [0; 4];
    model_peer.read_exact(&mut length)?;
    let length = usize::try_from(u32::from_le_bytes(length))?;
    require(length <= 4096, "qualification condition failed")?;
    let mut handoff = vec![0; length];
    model_peer.read_exact(&mut handoff)?;
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id: native.room.to_string(),
        device_certificate: serde_json::to_vec(&certificate)?,
        proof: key
            .sign(&runtime_identity::collaboration_join_signing_bytes(
                &native.room.to_string(),
            ))
            .to_bytes()
            .to_vec(),
    }
    .encode()?;
    let presentation = serde_json::to_vec(&serde_json::json!({
        "channel": tilo_action_broker::route::CrdtRoute::app("so.tilo.agent", native.room.to_string()).channel_key(),
        "presentation": presentation,
    }))?;
    identity_peer.write_all(&u32::try_from(presentation.len())?.to_le_bytes())?;
    identity_peer.write_all(&presentation)?;
    launcher.launch(permit, services)?;
    let supervisor = started(&mut launcher)?;
    let admitted = pending.admit_connection(&supervisor, authority.as_ref())?;
    let process = admitted.process().clone();
    state.connect_enrolled_worker(admitted, &certificate)?;
    Ok(RunningFixture {
        launcher,
        broker: Some(broker),
        native,
        process,
        state,
        model_peer,
        task,
        certificate,
        key,
        _root: root,
    })
}

struct RunningFixture {
    launcher: ManagedLauncher,
    broker: Option<Broker>,
    native: Arc<native::Native>,
    process: VerifiedManagedProcess,
    state: Arc<tilo_action_broker::collab_broker_capnp::CapnpBrokerState>,
    model_peer: UnixStream,
    task: String,
    certificate: tilo_user_identity::DeviceCert,
    key: ed25519_dalek::SigningKey,
    _root: tempfile::TempDir,
}
impl RunningFixture {
    fn accept_and_progress(&mut self) -> Qualification<()> {
        let offered = read_frame(&mut self.model_peer, "offered")?;
        require_eq(
            &(offered
                .pointer("/envelope/task_id")
                .and_then(serde_json::Value::as_str)),
            Some(self.task.as_str()),
        )?;
        let queued = self
            .native
            .owner
            .workspace_task_snapshot(self.native.room)?;
        let (task, control) = task_view(&queued, &self.task)?;
        require_eq(
            &(task.status),
            tilo_desktop_model::WorkspaceTaskStatus::Queued,
        )?;
        require(
            control.active_run.is_none(),
            "qualification condition failed",
        )?;
        require_eq(&(control.run_epoch), 0)?;
        self.model_peer.write_all(b"A")?;
        read_frame(&mut self.model_peer, "accepted")?;
        let accepted = self
            .native
            .owner
            .workspace_task_snapshot(self.native.room)?;
        let (task, control) = task_view(&accepted, &self.task)?;
        require_eq(
            &(task.status),
            tilo_desktop_model::WorkspaceTaskStatus::InProgress,
        )?;
        require_eq(
            &(control
                .active_run
                .as_ref()
                .ok_or("actual native run absent")?
                .epoch),
            1,
        )?;
        require_eq(
            &(control
                .execution
                .as_ref()
                .ok_or("actual execution absent")?
                .phase),
            tilo_desktop_model::WorkspaceTaskExecutionPhase::Accepted,
        )?;
        self.model_peer.write_all(b"P")?;
        read_frame(&mut self.model_peer, "progress")?;
        let progressed = self
            .native
            .owner
            .workspace_task_snapshot(self.native.room)?;
        let (_, control) = task_view(&progressed, &self.task)?;
        let execution = control
            .execution
            .as_ref()
            .ok_or("actual execution absent")?;
        require_eq(
            &(execution.phase),
            tilo_desktop_model::WorkspaceTaskExecutionPhase::Accepted,
        )?;
        require_eq(&(execution.sequence), 1)?;
        require(
            !serde_json::to_string(&(&progressed.tasks, &progressed.controls))?
                .contains("Private fixture progress"),
            "qualification condition failed",
        )?;
        Ok(())
    }
    fn retire(&mut self) -> Qualification<()> {
        self.launcher.shutdown()?;
        require(
            self.process.facts().is_err(),
            "qualification condition failed",
        )?;
        self.wait_retired()?;
        Ok(())
    }
    fn revoke(&self) -> Qualification<()> {
        let request = OwnerCommitRequest {
            room: self.native.room.to_string(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: self.certificate.clone(),
                command:
                    tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::RemoveAgent {
                        agent: "fixture-agent".into(),
                    },
            },
        };
        self.native
            .owner
            .commit_workspace_membership_command(
                request.clone(),
                &self.certificate,
                &self.key,
                SystemTime::now(),
            )?
            .validate_for(&request)?;
        self.wait_retired()?;
        require(
            self.process.facts().is_ok(),
            "idle recipient is still a real live process; revoked membership alone must retire its custody",
        )?;
        Ok(())
    }
    fn wait_retired(&self) -> Qualification<()> {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let retired = self
                .native
                .owner
                .workspace_task_snapshot(self.native.room)?;
            let (task, control) = task_view(&retired, &self.task)?;
            if control.active_run.is_none() {
                require_eq(
                    &(task.status),
                    tilo_desktop_model::WorkspaceTaskStatus::Waiting,
                )?;
                require_eq(&(control.run_epoch), 1)?;
                let audit = task.audit.last().ok_or("actual native audit absent")?;
                require_eq(&(audit.actor.as_str()), "host")?;
                require_eq(
                    &(audit.summary.as_str()),
                    "Managed recipient connection retired",
                )?;
                break;
            }
            require(
                Instant::now() < deadline,
                "actual owner retirement not observed",
            )?;
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
    fn finish(&mut self) -> Qualification<()> {
        self.model_peer.write_all(b"F")?;
        read_frame(&mut self.model_peer, "finished")?;
        let finished = self
            .native
            .owner
            .workspace_task_snapshot(self.native.room)?;
        let (task, control) = task_view(&finished, &self.task)?;
        require_eq(
            &(task.status),
            tilo_desktop_model::WorkspaceTaskStatus::Done,
        )?;
        require(
            control.active_run.is_none(),
            "qualification condition failed",
        )?;
        let audit = task.audit.last().ok_or("actual native audit absent")?;
        require_eq(&(audit.actor.as_str()), "agent:fixture-agent")?;
        require_eq(&(audit.summary.as_str()), "Fixture complete")?;
        self.model_peer.write_all(b"E")?;
        Ok(())
    }
    fn stop(&mut self) -> Qualification<()> {
        self.launcher.shutdown()?;
        require(
            self.process.facts().is_err(),
            "actual managed process custody retired",
        )?;
        drop(self.broker.take());
        require(
            self.state
                .connect_local_worker(
                    tilo_action_broker::route::CrdtRoute::app(
                        "so.tilo.agent",
                        self.native.room.to_string(),
                    ),
                    &self.certificate,
                )
                .is_err(),
            "stopped broker cannot admit a replacement",
        )?;
        Ok(())
    }
}

fn actual_enrolled_transport(finish: bool) -> Qualification<()> {
    let mut fixture = running_fixture()?;
    fixture.accept_and_progress()?;
    if finish {
        fixture.finish()?;
    } else {
        fixture.retire()?;
    }
    fixture.stop()?;
    drop(fixture);
    Ok(())
}

fn task_view<'a>(
    snapshot: &'a tilo_peer_state::workspace_task::WorkspaceTaskSnapshot,
    id: &str,
) -> Qualification<(
    &'a tilo_desktop_model::WorkspaceTask,
    &'a tilo_desktop_model::WorkspaceTaskControl,
)> {
    Ok((
        snapshot
            .tasks
            .iter()
            .find(|task| task.id == id)
            .ok_or("actual native task absent")?,
        snapshot
            .controls
            .get(id)
            .ok_or("actual native control absent")?,
    ))
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
