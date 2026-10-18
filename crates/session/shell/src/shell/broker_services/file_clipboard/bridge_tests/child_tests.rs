//! Production sandbox recipe, Landlock, launch ancestry, real sockets and
//! cross-process selected descriptors. Mounted SDK/history is proven separately.
use super::*;
use serde::{Deserialize, Serialize};

const TEST: &str = "shell::broker_services::file_clipboard::bridge_tests::child_tests::clipboard_bridge_runs_in_a_real_sandboxed_child";
const CHILD: &str = "TILO_CLIPBOARD_PROOF_CHILD";
const INFO: &str = "TILO_CLIPBOARD_PROOF_INFO";

#[derive(Serialize, Deserialize)]
struct ChildInfo {
    socket: PathBuf,
    root: PathBuf,
    room: String,
    identity: Vec<u8>,
    protected: PathBuf,
    sandbox_argv: Vec<String>,
}

#[test]
fn clipboard_bridge_runs_in_a_real_sandboxed_child() {
    if let Ok(mode) = std::env::var(CHILD) {
        let info_path = PathBuf::from(std::env::var_os(INFO).expect("test launch metadata"));
        let info: ChildInfo =
            serde_json::from_slice(&fs::read(&info_path).expect("test launch metadata"))
                .expect("valid metadata");
        if mode == "launcher" {
            await_registration();
            let status = run_sandbox(&info);

            assert!(status.success(), "real sandboxed child failed: {status}");
            return;
        }
        assert_eq!(mode, "sandbox");
        apply_landlock(&info);
        assert!(
            fs::read(&info.protected).is_err(),
            "an ungranted host file cannot be read by the child"
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("child runtime");
        tokio::task::LocalSet::new().block_on(&runtime, child_journey(&info));
        fs::write(
            info.root.join("child-completed"),
            b"actual sandboxed clipboard journey complete",
        )
        .expect("proof completion");
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("host runtime");
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let fixture = Fixture::new();
        let protected = tempfile::tempdir().expect("ungranted control");
        let protected_path = protected.path().join("private");
        fs::write(&protected_path, b"ungranted control bytes").expect("control bytes");
        fs::write(
            fixture.directory.path().join("source"),
            b"actual sandboxed source",
        )
        .expect("source");
        fs::create_dir(fixture.directory.path().join("receiving")).expect("receiving folder");
        start_child(&fixture, protected_path.clone());
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(30))
            .expect("bounded child proof");
        loop {
            if fixture.directory.path().join("child-completed").exists() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "actual sandboxed child did not complete; launch events: {:?}",
                crate::shell::launcher::drain_events()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            fs::read(fixture.directory.path().join("source")).expect("restored source"),
            b"actual sandboxed source"
        );
        assert_eq!(
            fs::read_dir(fixture.directory.path().join("receiving"))
                .expect("receiving entries")
                .count(),
            0
        );
        assert_eq!(
            fs::read(protected_path).expect("ungranted host control"),
            b"ungranted control bytes"
        );
    });
}

fn await_registration() {
    assert_eq!(
        std::env::var(launcher::registration_gate::LAUNCH_REGISTRATION_STDIN_ENV).as_deref(),
        Ok("1"),
        "production registration gate"
    );
    launcher::registration_gate::wait_on_stdin()
        .expect("production launch registration precedes child connection");
}
fn apply_landlock(info: &ChildInfo) {
    let mut ro = launcher::profile::landlock_ro_paths(false);
    ro.push(launcher::sandbox::payload_mount_for(APP));
    let mut rw = launcher::profile::landlock_rw_device_paths();
    rw.push(info.root.clone());
    let status = launcher::landlock_shim::apply(&launcher::landlock_shim::ShimArgs {
        ro,
        rw,
        net_restricted: true,
        ..Default::default()
    });
    assert_eq!(
        status,
        launcher::landlock::LandlockOutcome::FullyEnforced,
        "a sandbox proof must fail if the actual kernel cannot enforce its policy"
    );
}

#[expect(
    clippy::future_not_send,
    reason = "Cap'n Proto runs on the disposable sandbox child's LocalSet"
)]
async fn child_journey(info: &ChildInfo) {
    let source = info.root.join("source");
    let receiving = info.root.join("receiving");
    let a = inherited_connection(info, "sandbox-source").await;
    let b = inherited_connection(info, "sandbox-receiving").await;
    for cut in [false, true] {
        let ticket = a.capture(&source, false).await;
        let offer = a.publish(ticket, cut).await;
        let destination = b.capture(&receiving, true).await;
        let operation = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            b.command(Command::Paste {
                offer,
                destination,
                operation: operation.clone(),
                conflict: runtime_collab::file_clipboard::Conflict::KeepBoth,
            })
            .await
            .expect("sandboxed native Paste"),
            Reply::Running {
                operation: operation.clone()
            }
        );
        assert!(matches!(
            b.settled(&operation).await,
            Reply::Settled {
                completed: 1,
                error: None,
                undone: false,
                ..
            }
        ));
        assert_eq!(
            fs::read(receiving.join("source")).expect("host-published bytes"),
            b"actual sandboxed source"
        );
        assert_eq!(source.exists(), !cut);
        let undone = b
            .compensate(Command::Undo { operation })
            .await
            .expect("sandboxed native Undo");
        assert!(
            matches!(
                undone,
                Reply::Settled {
                    completed: 1,
                    error: None,
                    undone: true,
                    ..
                }
            ),
            "sandboxed native Undo settled as {undone:?}"
        );
        assert_eq!(
            fs::read(&source).expect("source remains or is restored"),
            b"actual sandboxed source"
        );
    }
}

fn run_sandbox(info: &ChildInfo) -> std::process::ExitStatus {
    use std::os::unix::process::CommandExt as _;
    let mut source =
        std::os::unix::net::UnixStream::connect(&info.socket).expect("launcher service socket");
    launcher::services::write_collab_executing_handoff(&mut source, APP, &info.room)
        .expect("production launcher handoff");
    let descriptors = [source.as_raw_fd()];
    let mut command = std::process::Command::new("bwrap");
    let mut argv = info.sandbox_argv.clone();
    let entry = argv
        .iter()
        .position(|arg| arg == "--exact")
        .and_then(|index| index.checked_sub(1))
        .expect("sandbox executable entry");
    argv.splice(
        entry..entry,
        [
            "--setenv".into(),
            "TILO_SVC_COLLAB_FD".into(),
            descriptors[0].to_string(),
            "--setenv".into(),
            runtime_portal::process_binding::ENV.into(),
            "1".into(),
        ],
    );
    command.args(argv);
    // SAFETY: this disposable launcher's child performs only async-signal-safe
    // fcntl calls, preserving exactly the two owned service sockets for exec.
    unsafe {
        command.pre_exec(move || {
            for fd in descriptors {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    command.status().expect("production sandbox must execute")
}
#[expect(
    clippy::future_not_send,
    reason = "Cap'n Proto runs on the disposable child LocalSet"
)]
async fn inherited_connection(info: &ChildInfo, record: &str) -> Connection {
    // Exercise the production SDK inherited-service hub, including its one-time
    // executing-task handshake, rather than completing that protocol in a helper.
    let stream = std::os::unix::net::UnixStream::from(
        runtime_portal::connect_service_fd("collab").expect("production inherited SDK connection"),
    );
    stream.set_nonblocking(true).expect("async socket");
    let stream = tokio::net::UnixStream::from_std(stream).expect("async service descriptor");
    // Selected objects ride the inherited connection's own descriptor lane,
    // which the production hub writes; this private hub connection has none.
    let lane = runtime_portal::service_descriptors("collab").expect("inherited descriptor lane");
    connect_stream(stream, lane, &info.room, &info.identity, record).await
}

fn start_child(fixture: &Fixture, protected_path: PathBuf) {
    let executable = std::env::current_exe().expect("test payload");
    let payload = executable.parent().expect("payload directory");
    let manifest = launcher::manifest::read_manifest(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../apps/file-explorer/tilo-app.toml"),
    )
    .expect("the actual Files manifest");
    let plan = launcher::capabilities::CapabilityPlan::for_app(
        APP,
        &fixture.tree.read().workspaces[0].sandbox,
        None,
    );
    assert_eq!(plan.rw_paths, vec![fixture.directory.path().to_path_buf()]);
    let mut argv = launcher::sandbox::build_bwrap_argv_for_entry(
        &manifest,
        payload,
        None,
        &[],
        None,
        None,
        executable
            .file_name()
            .expect("test payload name")
            .to_str()
            .expect("UTF-8 executable"),
        &plan,
    );
    let entry = argv.pop().expect("production sandbox entry");
    let info_path = fixture.directory.path().join("child-info.json");
    argv.extend([
        "--setenv".into(),
        CHILD.into(),
        "sandbox".into(),
        "--setenv".into(),
        INFO.into(),
        info_path.to_string_lossy().into_owned(),
        "--setenv".into(),
        "LD_LIBRARY_PATH".into(),
        launcher::sandbox::payload_mount_for(APP)
            .to_string_lossy()
            .into_owned(),
        entry,
        "--exact".into(),
        TEST.into(),
        "--nocapture".into(),
    ]);
    let info = ChildInfo {
        socket: fixture.socket.clone(),
        root: fixture.directory.path().into(),
        room: fixture.room.clone(),
        identity: fixture.identity.clone(),
        protected: protected_path,
        sandbox_argv: argv,
    };
    fs::write(
        &info_path,
        serde_json::to_vec(&info).expect("test launch metadata"),
    )
    .expect("metadata");
    crate::shell::launcher::spawn_launch(
        APP.into(),
        "Files clipboard proof".into(),
        crate::shell::launcher::LaunchCommand {
            program: executable,
            args: vec!["--exact".into(), TEST.into(), "--nocapture".into()],
            env: vec![
                (CHILD.into(), "launcher".into()),
                (INFO.into(), info_path.to_string_lossy().into_owned()),
            ],
            service_identity: Some(crate::shell::launcher::ServiceLaunchIdentity {
                app_id: APP.into(),
                workspace_id: fixture.room.clone(),
                allowed_compat_entry_ids: vec![],
                compat_entry_generation: 0,
                application_creation: None,
            }),
            portal_workspace_id: None,
            managed_agent_id: None,
        },
    )
    .expect("production launcher supervision");
}
