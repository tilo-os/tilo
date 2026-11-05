//! Where this session's log goes.
//!
//! A session is started by a display manager or by systemd, with no terminal
//! to read it: its runner relays what it writes to stderr into the journal and
//! the flight record (`dist/session/tilo-session-runner`,
//! `crates/session/session-record`). [`install`] makes that one stream the log
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
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            tilo_startup_metrics::log_directives(std::env::var("RUST_LOG").ok().as_deref()),
        ))
        .with_writer(std::io::stderr)
        // The journal, a flight record and a test's pipe read text, not
        // colour: nothing here is a terminal.
        .with_ansi(false)
        // The formatter reports a failed write with `eprintln!`, which panics:
        // a runner or recorder that stopped reading must not end the session.
        .log_internal_errors(false)
        .init();
    tilo_crash_records::install(tilo_crash_records::PANIC_TITLE);
}
