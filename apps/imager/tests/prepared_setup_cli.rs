//! The headless setup binary rejects incomplete requests without publishing output.
#![cfg(target_os = "linux")]

use std::process::{Command, Stdio};

#[test]
fn default_and_selected_setup_modes_reject_incomplete_transport() {
    for arguments in [
        vec![],
        vec!["--package"],
        vec!["--authenticate-install"],
        vec!["--selected-install-generation"],
        vec!["--stream-completed-payload"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_tilo-prepared-setup"))
            .args(&arguments)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        assert_eq!(
            output.stderr, b"Prepared setup could not continue.\n",
            "{arguments:?}"
        );
    }
}

#[test]
fn installed_consumer_modes_accept_no_path_or_receipt_input() {
    for (mode, extra) in [
        ("--verify-home-consumer-capability", "--root"),
        ("--verify-applications-consumer-capability", "--root"),
        ("--verify-account-consumer-capability", "--root"),
        ("--data-consumer-catalog-facts", "--check"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_tilo-prepared-setup"))
            .args([mode, extra, "/tmp"])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{mode}");
        assert!(output.stdout.is_empty(), "{mode}");
        let diagnostic = String::from_utf8(output.stderr).unwrap();
        assert!(diagnostic.contains("accepts"), "{mode}: {diagnostic}");
        assert!(diagnostic.ends_with('\n') && diagnostic.lines().count() == 1);
    }
}
