//! Every window reaches the shell, whatever its manifest names.

use super::*;
use std::io::{BufRead as _, BufReader};

tilo_test_env::scenario! {
    fn every_window_reaches_the_shell;

    phase a_window_gets_the_shell_without_naming_it_and_a_headless_launch_does_not;
    // A window describes its controls to the shell over this connection, so every window has one
    // without its manifest naming it; a command, a service or a connector opens no window.
    {
        let dir = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", dir.path());
        let listener = std::os::unix::net::UnixListener::bind(
            crate::services::ensure_services_root().unwrap().join("shell-ui.sock"),
        )
        .unwrap();
        assert!(opens_window(&LaunchMode::Gui, None));
        for (mode, allowed) in [("off", false), ("host", true)] {
            let egress = crate::egress_proxy::EgressDecision::resolve(mode, &[], &[], true);
            let bind = resolve_shell_ui_bind("so.tilo.activity", &egress, false)
                .unwrap()
                .expect("a window gets the shell-ui connection");
            assert!(crate::services::RESERVED_CHILD_FDS.contains(&bind.child_fd));
            let mut line = String::new();
            BufReader::new(listener.accept().unwrap().0).read_line(&mut line).unwrap();
            let handshake: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(handshake["app_id"], "so.tilo.activity");
            assert_eq!(handshake["network_status_allowed"], allowed, "{mode}");
        }
        let egress = crate::egress_proxy::EgressDecision::resolve("off", &[], &[], true);
        assert!(resolve_shell_ui_bind("so.tilo.activity", &egress, true).unwrap().is_none());

        let service = LaunchMode::Service { passthrough_argv: Vec::new() };
        let command = LaunchMode::Cli { command: "index".to_owned(), passthrough_argv: Vec::new() };
        assert!(!opens_window(&service, None));
        assert!(!opens_window(&command, None));
        assert!(!opens_window(&LaunchMode::Gui, Some("imap")), "a connector opens no window");
    }
}
