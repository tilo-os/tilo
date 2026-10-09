//! Compile-time native acceptance app. It uses the product shim and normal
//! App construction, with an explicit service pump in place of a GPU loop.
#[cfg(target_os = "linux")]
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if let Some((command, arguments)) = args.split_first()
        && command == "__sandbox-exec"
    {
        launcher::landlock_shim::run_sandbox_exec(arguments);
    }
    let mut app =
        ui::App::from_factory(ui::State::new(), || Box::new(ui::widget::Container::new()));
    let started = std::time::Instant::now();
    let mut last = None;
    loop {
        let connected = app.pump_native_host_for_test();
        let status = tilo_net::status::current();
        let observation = (connected, status);
        if last != Some(observation) {
            println!(
                "SDK {}",
                serde_json::json!({
                    "connected": connected,
                    "metered": status.metered,
                    "online": status.online(),
                })
            );
            last = Some(observation);
        }
        if !connected {
            // Loss is terminal for this launch. A second pump must stay local.
            assert!(!app.pump_native_host_for_test());
            assert_ne!(tilo_net::status::current().metered, Some(false));
            break;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(15),
            "host fixture failed to finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    panic!("the sandbox network boundary fixture requires Linux");
}
