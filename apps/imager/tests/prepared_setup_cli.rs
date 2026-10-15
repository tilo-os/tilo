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
