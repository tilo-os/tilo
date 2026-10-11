//! Disposable proof of the difference between native custody and confinement.
use super::*;
use crate::standalone::Standalone;
use std::os::unix::fs::OpenOptionsExt as _;

const TEST: &str =
    "daemon::development_proof_tests::uninstalled_native_run_app_can_read_a_private_profile_canary";
const HELPER: &str = "TILO_DEVELOPMENT_CONFINEMENT_PROOF";
const APP: &str = "dev.tilo.confinement-proof";
const CANARY: &str = "owner-only-canary";
const CONTENT: &[u8] = b"disposable non-secret confinement probe";

fn serve_native(
    session: &Standalone,
    root: &Path,
) -> io::Result<std::thread::JoinHandle<io::Result<()>>> {
    let listener = UnixListener::bind(root.join("native.sock"))?;
    let host = session.action_host()?;
    let search = tilo_os_search::OsSearch::new(
        Arc::new(host.clone()),
        tilo_actions::remote::Caller::local_person(session.actor()).in_room(session.room()),
    );
    let server = Arc::new(McpServer::new(
        host,
        session.room().into(),
        session.actor().into(),
    ));
    let status = Status {
        version: 1,
        pid: std::process::id(),
        room: session.room().into(),
    };
    let launcher = session
        .native_launcher_services()
        .ok_or_else(|| invalid("proof launcher unavailable"))?;
    std::thread::Builder::new()
        .name("sdk-proof-native".into())
        .spawn(move || {
            for _ in 0..2 {
                let (stream, _) = listener.accept()?;
                serve_connection(stream, &server, &status, None, &search, Some(&launcher))?;
            }
            Ok(())
        })
}

#[test]
fn uninstalled_native_run_app_can_read_a_private_profile_canary() {
    if let Ok(mode) = std::env::var(HELPER) {
        let root = PathBuf::from(std::env::var_os("TILO_STANDALONE_TEST_ROOT").unwrap());
        if mode == "exec" {
            let mut environment = tilo_test_env::ScopedEnv::new();
            environment.set(HELPER, "attached");
            run_app(
                &root,
                APP,
                None,
                &std::env::current_exe().unwrap().into_os_string(),
                &[
                    OsString::from("--exact"),
                    OsString::from(TEST),
                    OsString::from("--nocapture"),
                ],
            )
            .unwrap();
            panic!("run_app must exec the actual uninstalled SDK child");
        }
        assert_eq!(mode, "attached");
        let checkout = ui::record::RecordCheckout::new(
            ui::record::RecordId::for_file_record(uuid::Uuid::nil()),
            ui::record::RecordKind::new("local.dev"),
        );
        let mut manager = ui::collaboration::CollaborationManager::for_native_record(&checkout);
        tilo_test_env::wait_until("uninstalled native SDK attaches", || {
            let _ = manager.poll();
            manager.state() != ui::collaboration::CollaborationState::Connecting
        });
        assert!(manager.is_connected(), "{:?}", manager.state());
        assert_eq!(
            std::fs::read(root.join(CANARY)).unwrap(),
            CONTENT,
            "native process/source custody alone is not a filesystem confinement boundary"
        );
        return;
    }
    let Some(root) = crate::standalone::isolated_test_root(TEST) else {
        return;
    };
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("TILO_APPS_DATA_DIR", root.join("empty-store"));
    environment.remove("TILO_APPS_SYSTEM_DIR");
    let session = Standalone::start_in_dev_mode(&root, &mut |_| {}).unwrap();
    let mut canary = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(root.join(CANARY))
        .unwrap();
    canary.write_all(CONTENT).unwrap();
    drop(canary);
    let serving = serve_native(&session, &root).unwrap();
    let log_path = root.join("confinement-proof.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--nocapture"])
        .env(HELPER, "exec")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .status()
        .unwrap();
    assert!(
        status.success(),
        "{}",
        std::fs::read_to_string(log_path).unwrap()
    );
    serving.join().unwrap().unwrap();
    drop(session);
}
