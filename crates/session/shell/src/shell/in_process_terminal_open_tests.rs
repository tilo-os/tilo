/// The compositor mounts this same component and drives it on its event thread.
/// Hold the real supervisor protocol reply until after an unrelated system
/// search has processed keyboard input on that thread.
#[cfg(unix)]
#[test]
fn hosted_terminal_delayed_open_keeps_host_keyboard_input_responsive() {
    use std::io::{BufRead, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;

    let (client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let (request_sent, request_received) = mpsc::channel();
    let (release, delayed) = mpsc::channel();
    let supervisor = std::thread::spawn(move || {
        // The persistent-control handshake precedes every open: read the
        // client's hello and answer ready, then read the open request from the
        // same buffered reader.
        let mut reader = std::io::BufReader::new(&server);
        let mut hello = String::new();
        reader.read_line(&mut hello).unwrap();
        assert!(hello.contains("\"op\":\"hello\""), "{hello}");
        (&server)
            .write_all(b"{\"result\":\"ready\",\"protocol\":2}\n")
            .unwrap();
        let mut request = String::new();
        reader.read_line(&mut request).unwrap();
        request_sent.send(request).unwrap();
        // A bound only for a broken test, not simulated startup latency.
        if delayed.recv_timeout(tilo_test_env::hang_bound()).is_ok() {
            (&server)
                .write_all(b"{\"result\":\"no-runtime\",\"reason\":\"delayed readiness\"}\n")
                .unwrap();
        }
    });
    let profile = tempfile::tempdir().unwrap();
    let mut apps = InProcessApps::at(
        profile.path(),
        &profile.path().join("person"),
        "test-person",
    )
    .unwrap();
    let component = apps
        .mount(
            terminal::definition(),
            ROOM,
            TAB,
            RecordId::for_tab(TAB),
            |context| {
                context.lend(terminal::HostedProcessServices { ush: Some(client) });
            },
        )
        .unwrap();
    let mut host = ui::headless::FrameHarness::from_component(
        component,
        (900, 600),
        ui::application::ComponentRunnerInputs::from_native_process(),
    )
    .unwrap();
    host.relayout();
    assert!(tilo_test_env::recv("the open request", &request_received).contains("open-session"));
    let address = ui::bound_dispatch::BoundActionAddress {
        instance_id: TAB,
        address: tilo_actions::ActionAddress::new(terminal::Terminal::KIND, "inspect-session"),
    };
    let inspect = |host: &mut ui::headless::FrameHarness| {
        host.invoke_bound_action(&address, &serde_json::Map::new())
            .unwrap()
            .completed()
            .unwrap()
            .value
            .unwrap()
    };
    assert_eq!(inspect(&mut host)["lifecycle"], "opening");
    assert_eq!(inspect(&mut host)["input_available"], false);
    host.open_global_search_all("").unwrap();
    host.type_text("unrelated keyboard input");
    assert_eq!(host.global_search_query(), Some("unrelated keyboard input"));
    assert_eq!(
        inspect(&mut host)["lifecycle"],
        "opening",
        "startup remains delayed while the host handles input"
    );
    host.key("Escape");
    release.send(()).unwrap();
    tilo_test_env::wait_until("completion to reach the mounted component", || {
        if inspect(&mut host)["lifecycle"] != "opening" {
            return true;
        }
        host.relayout();
        false
    });
    assert_eq!(inspect(&mut host)["lifecycle"], "no_runtime");
    assert_eq!(inspect(&mut host)["startup_reason"], "delayed readiness");
    assert_eq!(inspect(&mut host)["input_available"], false);
    host.close_components();
    supervisor.join().unwrap();
}

#[cfg(unix)]
#[test]
fn deferred_hosted_terminal_grant_reports_missing_ush_through_its_resource() {
    let profile = tempfile::tempdir().unwrap();
    let mut apps = InProcessApps::at(
        profile.path(),
        &profile.path().join("person"),
        "test-person",
    )
    .unwrap();
    let component = apps
        .mount(
            terminal::definition(),
            ROOM,
            TAB,
            RecordId::for_tab(TAB),
            |context| {
                context.lend(terminal::HostedSessionSource);
            },
        )
        .unwrap();
    let mut host = ui::headless::FrameHarness::from_component(
        component,
        (900, 600),
        ui::application::ComponentRunnerInputs::from_native_process(),
    )
    .unwrap();
    let address = ui::bound_dispatch::BoundActionAddress {
        instance_id: TAB,
        address: tilo_actions::ActionAddress::new(terminal::Terminal::KIND, "inspect-session"),
    };
    let inspect = |host: &mut ui::headless::FrameHarness| {
        host.invoke_bound_action(&address, &serde_json::Map::new())
            .unwrap()
            .completed()
            .unwrap()
            .value
            .unwrap()
    };
    tilo_test_env::wait_until("the missing service to complete", || {
        if !matches!(
            inspect(&mut host)["lifecycle"].as_str(),
            Some("unopened" | "opening")
        ) {
            return true;
        }
        host.relayout();
        false
    });
    let state = inspect(&mut host);
    assert_eq!(state["lifecycle"], "no_runtime");
    assert_eq!(state["input_available"], false);
    assert!(state["startup_reason"].as_str().unwrap().contains("ush"));
    host.close_components();
}
