//! Where the shells an app's connection carries come from on this device
//! ([`tilo_action_services::line::shells`]): Linux's shell runtime, which runs the workspace's
//! environments and this computer's own shell alike; elsewhere this computer's own login shell
//! on a pty (`ConPTY` on Windows), the one target a computer without that runtime has.

use std::sync::Arc;

use tilo_action_services::line::shells::ShellSource;

/// This device's shells.
#[cfg(target_os = "linux")]
pub(crate) fn source() -> Arc<dyn ShellSource> {
    Arc::new(ush::app_shells::Runtime(super::ush_line::here()))
}

/// This device's shells.
#[cfg(not(target_os = "linux"))]
pub(crate) fn source() -> Arc<dyn ShellSource> {
    Arc::new(platform::OwnShell)
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use serde_json::Value;
    use tilo_action_services::line::shells::{OpenedShell, ShellSource};
    use tilo_actions::remote::ActionCallError;
    use tilo_actions::remote::shell::ShellOpen;

    /// This computer's own login shell, for the person whose computer it is.
    pub(super) struct OwnShell;

    impl ShellSource for OwnShell {
        fn open(&self, _workspace: &str, open: &ShellOpen) -> Result<OpenedShell, ActionCallError> {
            if open.target.get("kind").and_then(Value::as_str) != Some("host") {
                return Err(ActionCallError::Refused {
                    reason: "Environments run on Linux, which this computer is not: open this \
                             computer's own shell, or an environment on a Linux device of the \
                             workspace"
                        .into(),
                });
            }
            if open.agent {
                return Err(ActionCallError::Refused {
                    reason: "Agents may not open this computer's own shell".into(),
                });
            }
            pty(open)
        }
    }

    /// The login shell, or the command `open` names, on a pty of `open`'s size.
    #[cfg(any(target_os = "macos", windows))]
    fn pty(open: &ShellOpen) -> Result<OpenedShell, ActionCallError> {
        use portable_pty::{CommandBuilder, PtySize, native_pty_system};

        let failed = |error: &dyn std::fmt::Display| ActionCallError::Unavailable {
            reason: format!("this computer's shell did not start: {error}"),
        };
        let size = |cols: u16, rows: u16| PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = native_pty_system()
            .openpty(size(open.cols, open.rows))
            .map_err(|error| failed(&error))?;
        let mut command = match open.command.split_first() {
            Some((program, args)) => {
                let mut command = CommandBuilder::new(program);
                command.args(args);
                command
            }
            // The account's own shell, as a login shell (`$SHELL`; `%ComSpec%` on Windows).
            None => CommandBuilder::new_default_prog(),
        };
        // The terminal reads the SGR and DEC sequences an xterm takes.
        command.env("TERM", "xterm-256color");
        if let Some(home) = tilo_paths::home() {
            command.cwd(home);
        }
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| failed(&error))?;
        drop(pair.slave);
        let output = pair
            .master
            .try_clone_reader()
            .map_err(|error| failed(&error))?;
        let input = pair.master.take_writer().map_err(|error| failed(&error))?;
        let master = std::sync::Arc::new(std::sync::Mutex::new(pair.master));
        let window = std::sync::Arc::clone(&master);
        Ok(OpenedShell {
            input,
            output,
            resize: Box::new(move |cols, rows| {
                let master = window
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let _ = master.resize(size(cols, rows));
            }),
            report: Box::new(String::new),
            // Letting the pty go hangs the shell up; the child is ended with it.
            keep: Box::new(Kept {
                _master: master,
                child,
            }),
            name: "host".into(),
            relay_session: None,
            commands: false,
        })
    }

    /// A device that is neither Linux, macOS nor Windows opens no shell of its own.
    #[cfg(not(any(target_os = "macos", windows)))]
    fn pty(_open: &ShellOpen) -> Result<OpenedShell, ActionCallError> {
        Err(ActionCallError::Refused {
            reason: "This device opens no shells".into(),
        })
    }

    /// An open shell's pty and process: the process is ended as the shell is let go.
    #[cfg(any(target_os = "macos", windows))]
    struct Kept {
        _master: std::sync::Arc<std::sync::Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
        child: Box<dyn portable_pty::Child + Send + Sync>,
    }

    #[cfg(any(target_os = "macos", windows))]
    impl Drop for Kept {
        fn drop(&mut self) {
            let _ = self.child.kill();
        }
    }
}
