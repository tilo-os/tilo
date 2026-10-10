//! The actual launcher-authenticated host channel, not a replaced App stream.
//! Owned D-Bus source and sandbox only; no system bus, display or credentials.
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use launcher::capture::{
    CaptureCancellation, CapturedResourcePlan, CapturedRunOutput, CapturedTermination,
};
use launcher::run::{IdentitySource, RunOptions};
use shell::compositor::floating_service::{
    AppearanceDelivery, FloatingServiceEvent, FloatingSurfaceService,
};
use shell::shell::status::StatusBridge;
use ui::floating_surface::{
    DeviceShellOperation, FloatingSurfaceClientMessage, FloatingSurfaceHostMessage,
};
use zbus::connection::Builder;
use zbus::zvariant::OwnedObjectPath;
use zbus::{Connection, Guid};

struct OwnedNm {
    state: u32,
    connectivity: u32,
    primary: OwnedObjectPath,
    primary_type: String,
    metered: u32,
    checks: Arc<AtomicUsize>,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager")]
impl OwnedNm {
    #[zbus(property)]
    fn state(&self) -> u32 {
        self.state
    }
    #[zbus(property)]
    fn connectivity(&self) -> u32 {
        self.connectivity
    }
    #[zbus(property)]
    fn metered(&self) -> u32 {
        self.metered
    }
    #[zbus(property)]
    fn primary_connection(&self) -> OwnedObjectPath {
        self.primary.clone()
    }
    #[zbus(property)]
    fn primary_connection_type(&self) -> String {
        self.primary_type.clone()
    }
    fn check_connectivity(&self) -> u32 {
        self.checks.fetch_add(1, Ordering::SeqCst);
        4
    }
}

async fn owned_source(checks: Arc<AtomicUsize>) -> (Connection, tilo_nm::NmClient) {
    let (server, client) = tokio::net::UnixStream::pair().unwrap();
    let server = Builder::unix_stream(server)
        .server(Guid::generate())
        .unwrap()
        .p2p()
        .serve_at(
            tilo_nm::ROOT_PATH,
            OwnedNm {
                state: 70,
                connectivity: 4,
                primary: OwnedObjectPath::try_from("/").unwrap(),
                primary_type: String::new(),
                metered: 0,
                checks,
            },
        )
        .unwrap()
        .build();
    let client = Builder::unix_stream(client).p2p().build();
    let (server, client) = tokio::join!(server, client);
    (server.unwrap(), tilo_nm::NmClient::new(client.unwrap()))
}

fn wait_for(subject: &str, mut ready: impl FnMut() -> bool) {
    let started = Instant::now();
    while !ready() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "native boundary did not reach expected state: {subject}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn set_cost(
    runtime: &tokio::runtime::Runtime,
    server: &Connection,
    status: &StatusBridge,
    cost: u32,
) {
    runtime.block_on(async {
        let iface = server
            .object_server()
            .interface::<_, OwnedNm>(tilo_nm::ROOT_PATH)
            .await
            .unwrap();
        iface.get_mut().await.metered = cost;
        iface
            .get()
            .await
            .metered_changed(iface.signal_emitter())
            .await
            .unwrap();
    });
    let expected = match cost {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    };
    wait_for("owned NM follower projection", || {
        status.network_status().available == Some(true)
            && status.network_status().metered == expected
    });
}

fn launch(root: &std::path::Path, mode: &str) -> std::thread::JoinHandle<CapturedRunOutput> {
    let manifest_path = root.join("manifest.toml");
    std::fs::write(
        &manifest_path,
        format!(
            r#"
[package]
id = "so.tilo.network-boundary"
version = "0.0.0"
[entrypoint]
bin = "bin/network-boundary-fixture"
type = "gui"
[sandbox.net]
mode = "{mode}"
[sandbox.services]
shell-ui = "required@1"
"#
        ),
    )
    .unwrap();
    let manifest = launcher::manifest::read_manifest(&manifest_path).unwrap();
    let payload = root.join("payload");
    let opts = RunOptions {
        identity: IdentitySource::None,
        minimal_libs: true,
        data_home: Some(root.join("data")),
        sandbox_shim_bin: Some(env!("CARGO_BIN_EXE_network-boundary-fixture").into()),
        ..RunOptions::default()
    };
    std::thread::spawn(move || {
        let output = launcher::run::run_with_captured_bounded(
            &manifest,
            &payload,
            &opts,
            CapturedResourcePlan {
                cpu_seconds: 30,
                memory_bytes: 2 * 1024 * 1024 * 1024,
                output_bytes: 1024 * 1024,
                wall_time: Duration::from_secs(25),
            },
            CaptureCancellation::default(),
        )
        .unwrap();
        assert!(
            matches!(output.termination, CapturedTermination::Exited(code) if code.success()),
            "sandbox failed: {:?}: {}",
            output.termination,
            String::from_utf8_lossy(&output.stderr)
        );
        for line in String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| line.starts_with("SDK "))
        {
            println!("{line}");
        }
        output
    })
}

fn connected(service: &FloatingSurfaceService, allowed: bool) -> u64 {
    let mut connection = None;
    wait_for("launcher-authenticated connection", || {
        for event in service.drain() {
            if let FloatingServiceEvent::Connected {
                connection: id,
                app_id,
                network_status_allowed,
            } = event
            {
                assert_eq!(app_id, "so.tilo.network-boundary");
                assert_eq!(network_status_allowed, allowed);
                connection = Some(id);
            }
        }
        connection.is_some()
    });
    connection.unwrap()
}

fn publish(
    service: &FloatingSurfaceService,
    delivery: &mut AppearanceDelivery,
    status: &StatusBridge,
    target: (u64, u64),
    allowed: bool,
) {
    let (connection, phase) = target;
    delivery.publish(
        |_| ui::WindowAppearance::default(),
        &ui::Theme::default(),
        status.network_status(),
        |id, message| {
            let frame = serde_json::to_value(message).unwrap();
            // The host never sends connection names, addresses or rich NM state.
            assert_eq!(
                frame.get("network_status").is_some(),
                allowed,
                "resolved launcher policy must gate the wire hint"
            );
            let text = frame.to_string();
            for private in [
                "primary_connection",
                "ssid",
                "interface",
                "connectivity_check_uri",
            ] {
                assert!(!text.contains(private));
            }
            service.send(id, message);
        },
    );
    // Existing semantic inspection provides a same-channel ordering barrier.
    service.send(
        connection,
        &FloatingSurfaceHostMessage::DeviceShellOperation {
            request: phase,
            operation: DeviceShellOperation::Inspect,
        },
    );
    wait_for("App SDK pump acknowledgement", || {
        service.drain().into_iter().any(|event| matches!(event,
        FloatingServiceEvent::Message { message: FloatingSurfaceClientMessage::DeviceShellOutcome { request, .. }, .. } if request == phase))
    });
}

fn observations(job: std::thread::JoinHandle<CapturedRunOutput>) -> Vec<serde_json::Value> {
    let output = job.join().unwrap();
    assert!(
        matches!(output.termination, CapturedTermination::Exited(code) if code.success()),
        "sandbox failed: {:?}: {}",
        output.termination,
        String::from_utf8_lossy(&output.stderr)
    );
    let values = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| {
            line.strip_prefix("SDK ")
                .map(|json| serde_json::from_str::<serde_json::Value>(json).unwrap())
        })
        .collect::<Vec<_>>();
    assert!(!values.is_empty(), "sandbox SDK was not observed");
    assert_eq!(values.last().unwrap()["connected"], false);
    assert!(values.last().unwrap()["metered"].is_null());
    values
}

#[test]
#[serial_test::serial(process_env)]
fn real_launch_cost_hints_stop_on_loss_relaunch_recovers_and_denied_apps_stay_local() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let runtime_dir = root.join("run");
    std::fs::create_dir_all(&runtime_dir).unwrap();
    let payload_bin = root.join("payload/bin");
    std::fs::create_dir_all(&payload_bin).unwrap();
    std::fs::copy(
        env!("CARGO_BIN_EXE_network-boundary-fixture"),
        payload_bin.join("network-boundary-fixture"),
    )
    .unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("HOME", root)
        .set("XDG_CONFIG_HOME", root.join("config"))
        .set("XDG_CACHE_HOME", root.join("cache"))
        .set("XDG_RUNTIME_DIR", &runtime_dir)
        .set("XDG_DATA_HOME", root.join("data"));
    env.remove("DBUS_SESSION_BUS_ADDRESS")
        .remove("TILO_IDENTITY_ROOT");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let checks = Arc::new(AtomicUsize::new(0));
    let (server, client) = runtime.block_on(owned_source(Arc::clone(&checks)));
    assert_eq!(
        runtime.block_on(client.state()).unwrap().state,
        tilo_nm::NmState::ConnectedGlobal
    );
    let status = StatusBridge::new();
    let follower_status = status.clone();
    let follower = runtime.spawn(async move {
        follower_status
            .follow_owned_network_for_test(client)
            .await
            .unwrap();
        panic!("owned host follower stopped before fixture completion");
    });
    set_cost(&runtime, &server, &status, 0);
    for (allowed, costs, expected) in [
        (true, &[0, 1, 2][..], &[true, false][..]),
        // Fresh normal launch receives the current unmetered first snapshot.
        (true, &[2][..], &[false][..]),
        (false, &[0, 1, 2][..], &[][..]),
    ] {
        let service = FloatingSurfaceService::start().unwrap();
        let job = launch(root, if allowed { "host" } else { "off" });
        let connection = connected(&service, allowed);
        let mut delivery = AppearanceDelivery::default();
        delivery.connected(connection, "so.tilo.network-boundary", allowed);
        for &cost in costs {
            let phase = u64::from(cost) + 1;
            set_cost(&runtime, &server, &status, cost);
            publish(
                &service,
                &mut delivery,
                &status,
                (connection, phase),
                allowed,
            );
        }
        let retained_reply = service.reply_writer(connection).unwrap();
        drop(service);
        let values = observations(job);
        if allowed {
            let costs = values
                .iter()
                .filter_map(|v| v["metered"].as_bool())
                .collect::<Vec<_>>();
            assert_eq!(
                costs, expected,
                "metering must traverse the real native SDK"
            );
        } else {
            assert!(
                values.iter().all(|v| v["metered"].is_null()),
                "denied app received a cost hint"
            );
        }
        drop(retained_reply);
    }
    assert_eq!(
        checks.load(Ordering::SeqCst),
        0,
        "following must never probe connectivity"
    );
    follower.abort();
}
