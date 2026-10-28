//! Plymouth, while it draws the boot: the splash before and after the
//! greeter, and the volume's own questions after "Can't sign in"
//! ([`crate::relay`]). The sign-in itself is the greeter's
//! (docs/security/sign-in.md §3.4): Plymouth gives the display up to it and
//! takes it back, or leaves it to the console when the greeter can't start
//! ([`crate::screen`]).

use std::path::PathBuf;
use std::process::{Command, Stdio};

pub struct Plymouth {
    pub program: PathBuf,
}

impl Plymouth {
    /// Plymouth, if it is running.
    pub fn running(program: PathBuf) -> Option<Self> {
        let pinged = Command::new(&program)
            .arg("--ping")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        pinged.then_some(Self { program })
    }

    pub fn message(&self, text: &str) {
        self.command(&["display-message", &format!("--text={text}")]);
    }

    /// Let go of the display, so another program may take it.
    pub fn deactivate(&self) {
        self.command(&["deactivate"]);
    }

    /// Take the display back and draw the splash again.
    pub fn reactivate(&self) {
        self.command(&["reactivate"]);
    }

    /// End the splash for this boot and give the console back. systemd's
    /// console password agent asks only while Plymouth is not running.
    pub fn quit(&self) {
        self.command(&["quit"]);
    }

    fn command(&self, args: &[&str]) {
        let _ = Command::new(&self.program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
