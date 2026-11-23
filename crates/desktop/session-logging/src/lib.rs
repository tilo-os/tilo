//! Where this session's log goes.
//!
//! A session is started by a display manager or by systemd, with no terminal
//! to read it: its runner relays what it writes to stderr into the journal and
//! the flight record (`dist/session/tilo-session-runner`,
//! `crates/desktop/flight-log`). [`install`] makes that one stream the log
//! of everything this process and its crates report — every `tracing::warn!`,
//! `error!` and `info!`, as much of it as `RUST_LOG` asks for, with the
//! launch-timing lines always on ([`tilo_startup_metrics::log_directives`]) —
//! and sends every panic to a crash record with its backtrace before the
//! session ends: a session that disappeared would otherwise leave the person
//! nothing to send back.
//!
//! One process, one log: `tilo-session` installs this in its `main` before it
//! reads its arguments, so a workspace's render service, the same binary in
//! its own mode, logs the same way. A process that installs nothing reports
//! to nobody.
//!
//! A crate of its own so that its test, which finds its own function in a
//! panic's backtrace, alone keeps the symbol table CI strips from test
//! executables (`[profile.test.package.tilo-session-logging]` in the
//! workspace's Cargo.toml), not the session's tests, which link the compositor.

/// The one log of this process: its stderr, filtered as `RUST_LOG` says, and
/// a crash record for every panic.
pub fn install() {
    tracing_subscriber::fmt()
        .with_env_filter(filter(std::env::var("RUST_LOG").ok().as_deref()))
        .with_writer(std::io::stderr)
        // The journal, a flight record and a test's pipe read text, not
        // colour: nothing here is a terminal.
        .with_ansi(false)
        // The formatter reports a failed write with `eprintln!`, which panics:
        // a runner or recorder that stopped reading must not end the session.
        .log_internal_errors(false)
        .init();
    tilo_crash_reports::install(tilo_crash_reports::PANIC_TITLE);
}

/// What the log keeps for a `RUST_LOG` of `rust_log`.
fn filter(rust_log: Option<&str>) -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::new(tilo_startup_metrics::log_directives(rust_log))
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    /// What a log written through [`super::filter`] holds.
    #[derive(Clone, Default)]
    struct Written(Arc<Mutex<Vec<u8>>>);

    impl Write for Written {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| std::io::Error::other("poisoned"))?
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A login sets no `RUST_LOG`: the lines that say what an in-place restart kept of the apps
    /// reach the log anyway, and other information does not.
    #[test]
    fn what_a_restart_kept_is_logged_on_a_login() {
        let written = Written::default();
        let writer = written.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(super::filter(None))
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: tilo_startup_metrics::KEPT_APPS_TARGET, app = "foot", "kept across restart");
            tracing::warn!(target: tilo_startup_metrics::KEPT_APPS_TARGET, app = "gedit", "not adopted: its keeper cannot be written to");
            tracing::info!("an ordinary line");
        });
        let log = String::from_utf8(written.0.lock().expect("the log").clone()).expect("text");
        assert!(log.contains("kept across restart app=\"foot\""), "{log}");
        assert!(
            log.contains("not adopted: its keeper cannot be written to"),
            "{log}"
        );
        assert!(!log.contains("an ordinary line"), "{log}");
    }
}
