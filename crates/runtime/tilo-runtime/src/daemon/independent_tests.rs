//! The actual private native endpoint followed by same-PID app exec and SDK join.
use super::*;
use crate::standalone::Standalone;

const TEST: &str =
    "daemon::independent_tests::private_registration_then_run_app_exec_admits_the_signed_sdk";
const HELPER: &str = "TILO_DAEMON_SIGNED_SDK_HELPER";
const APP: &str = "so.tilo.daemon-sdk";
const KIND: &str = "daemon.document";

#[test]
fn private_registration_then_run_app_exec_admits_the_signed_sdk() {
    let root = std::env::var_os("TILO_STANDALONE_TEST_ROOT").map(PathBuf::from);
    if let Ok(mode) = std::env::var(HELPER) {
        let root = root.unwrap();
        if mode == "exec" {
            let mut environment = tilo_test_env::ScopedEnv::new();
            environment.set(HELPER, "attached");
            let executable = std::env::current_exe().unwrap().into_os_string();
            run_app(
                &root,
                APP,
                None,
                &executable,
                &[
                    OsString::from("--exact"),
                    OsString::from(TEST),
                    OsString::from("--nocapture"),
                ],
            )
            .unwrap();
            panic!("run_app must replace this exact registering process");
        }
        assert_eq!(mode, "attached");
        let checkout = ui::record::RecordCheckout::new(
            ui::record::RecordId::for_file_record(uuid::Uuid::nil()),
            ui::record::RecordKind::new(KIND),
        );
        let mut manager = ui::collaboration::CollaborationManager::for_native_record(&checkout);
        tilo_test_env::wait_until("daemon-attached SDK connection", || {
            let _ = manager.poll();
            manager.state() != ui::collaboration::CollaborationState::Connecting
        });
        assert!(manager.is_connected(), "{:?}", manager.state());
        return;
    }
    let Some(root) = crate::standalone::isolated_test_root(TEST) else {
        return;
    };
    let store = crate::apps::independent_source_fixture(&root, APP, KIND);
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("TILO_APPS_DATA_DIR", store);
    environment.remove("TILO_APPS_SYSTEM_DIR");
    let session = Standalone::start_reporting(&root, &mut |_| {}).unwrap();
    let before = std::fs::read(crate::standalone::test_owner_path(&root)).unwrap();
    let listener = UnixListener::bind(root.join("native.sock")).unwrap();
    let host = session.action_host().unwrap();
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
    let launcher = session.native_launcher_services().unwrap();
    // Exactly the status probe and registration request made by run_app.
    let serving = std::thread::spawn(move || {
        for _ in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            serve_connection(stream, &server, &status, None, &search, Some(&launcher)).unwrap();
        }
    });
    let log_path = root.join("daemon-sdk.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--nocapture"])
        .env(HELPER, "exec")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .spawn()
        .unwrap();
    let result = child.wait().unwrap();
    assert!(
        result.success(),
        "{}",
        std::fs::read_to_string(log_path).unwrap()
    );
    serving.join().unwrap();
    assert_ne!(
        std::fs::read(crate::standalone::test_owner_path(&root)).unwrap(),
        before,
        "the daemon-attached SDK must create its typed record through the durable owner"
    );
    drop(session);
}
