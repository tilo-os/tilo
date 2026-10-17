//! Closed commands on the existing authenticated root receive gate. No stdin,
//! public socket, UI-only action or caller-provided grant enters this controller.
use super::{AuthenticatedCopyReceive, receiver::InstalledCopyReceiveSession};
use serde::{Deserialize, Serialize};
use std::{
    io,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CopyCommand {
    Prepare {},
    Adopt {},
    RestoreApplications {},
    Retry {},
    Cancel {},
    Finish {},
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "event", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CopyEvent {
    Reviewed {
        operation: String,
        home: bool,
        applications: bool,
    },
    ReadyToAdopt {
        operation: String,
        install_items: usize,
        collisions: usize,
        inactive_custody: usize,
        runtime_sockets: u64,
        recovered: bool,
    },
    Adopting {
        operation: String,
    },
    AdoptionFinished {
        operation: String,
        complete: bool,
        cancelled: bool,
        installed: usize,
        collisions: usize,
        inactive_custody: usize,
        failures: usize,
        verified: bool,
    },
    ApplicationsRestored {
        operation: String,
        restored: usize,
    },
    Complete {
        operation: String,
        home_sha256: Option<[u8; 32]>,
        applications_sha256: Option<[u8; 32]>,
    },
    Cancelled {
        operation: String,
    },
    Failed {
        operation: String,
        message: String,
    },
    Draining {
        operation: String,
    },
}
fn operation(session: &InstalledCopyReceiveSession) -> String {
    session.operation().to_owned()
}
fn prepare(session: &mut InstalledCopyReceiveSession) -> io::Result<()> {
    if let Some(preview) = session.prepare_home()? {
        session.send_event(&CopyEvent::ReadyToAdopt {
            operation: operation(session),
            install_items: preview.install_items,
            collisions: preview.collisions,
            inactive_custody: preview.inactive_custody,
            runtime_sockets: preview.runtime_sockets,
            recovered: preview.recovered,
        })?;
    }
    Ok(())
}
/// The parent's real gate mints the grant and links its EOF/owner fence to
/// `cancel`. Unresolved promotion custody keeps this exact child alive.
pub(super) fn run(grant: AuthenticatedCopyReceive, cancel: Arc<AtomicBool>) -> io::Result<()> {
    let mut session = InstalledCopyReceiveSession::review(grant, cancel)?;
    let result = run_current(&mut session);
    if !session.drain(Duration::from_secs(2)) {
        let _ = session.send_event(&CopyEvent::Draining {
            operation: operation(&session),
        });
        // Each observation is bounded. Never drop the grant/worker on timeout:
        // the root controller must continue owning this actual child until its
        // terminal worker event is observed, even when its channel has closed.
        while !session.drain(Duration::from_millis(250)) {
            std::thread::park_timeout(Duration::from_millis(50));
        }
    }
    result
}
fn run_current(session: &mut InstalledCopyReceiveSession) -> io::Result<()> {
    let (home, applications) = session.selected();
    session.send_event(&CopyEvent::Reviewed {
        operation: operation(session),
        home,
        applications,
    })?;
    loop {
        if let Some(result) = session.poll_adoption()? {
            match result {
                Ok(report) => session.send_event(&CopyEvent::AdoptionFinished {
                    operation: operation(session),
                    complete: report.complete,
                    cancelled: report.cancelled,
                    installed: report.installed,
                    collisions: report.collisions,
                    inactive_custody: report.inactive_custody,
                    failures: report.failures,
                    verified: report.verified,
                })?,
                Err(message) => session.send_event(&CopyEvent::Failed {
                    operation: operation(session),
                    message,
                })?,
            }
        }
        let Some(command) = session.receive_command(Duration::from_millis(100))? else {
            continue;
        };
        match handle_command(session, command, home) {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => session.send_event(&CopyEvent::Failed {
                operation: operation(session),
                message: error.to_string(),
            })?,
        }
    }
}
fn handle_command(
    session: &mut InstalledCopyReceiveSession,
    command: CopyCommand,
    home: bool,
) -> io::Result<bool> {
    match command {
        CopyCommand::Prepare {} => prepare(session)?,
        CopyCommand::Adopt {} => {
            session.start_adoption()?;
            session.send_event(&CopyEvent::Adopting {
                operation: operation(session),
            })?
        }
        CopyCommand::RestoreApplications {} => {
            let restored = session.restore_applications()?;
            session.send_event(&CopyEvent::ApplicationsRestored {
                operation: operation(session),
                restored,
            })?
        }
        CopyCommand::Retry {} => {
            if home {
                prepare(session)?;
                session.start_adoption()?;
                session.send_event(&CopyEvent::Adopting {
                    operation: operation(session),
                })?
            } else {
                let restored = session.restore_applications()?;
                session.send_event(&CopyEvent::ApplicationsRestored {
                    operation: operation(session),
                    restored,
                })?
            }
        }
        CopyCommand::Cancel {} => {
            session.cancel();
            if !session.drain(Duration::from_secs(2)) {
                session.send_event(&CopyEvent::Draining {
                    operation: operation(session),
                })?;
                while !session.drain(Duration::from_millis(250)) {
                    std::thread::park_timeout(Duration::from_millis(50));
                }
            }
            session.send_event(&CopyEvent::Cancelled {
                operation: operation(session),
            })?;
            return Ok(true);
        }
        CopyCommand::Finish {} => {
            let receipt = session.verify_complete()?;
            session.send_event(&receipt)?;
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_vocabulary_is_closed_and_never_accepts_source_or_owner_grants() {
        for bytes in [
            br#"{"command":"prepare","home":"/home/other"}"#.as_slice(),
            br#"{"command":"adopt","uid":0}"#,
            br#"{"command":"unknown"}"#,
        ] {
            assert!(serde_json::from_slice::<CopyCommand>(bytes).is_err());
        }
        assert!(matches!(
            serde_json::from_slice::<CopyCommand>(br#"{"command":"finish"}"#).unwrap(),
            CopyCommand::Finish {}
        ));
    }
}

#[cfg(test)]
mod codec_tests {
    use super::*;
    #[test]
    fn actual_receiver_commands_refuse_extra_and_unknown_fields() {
        for command in [
            "prepare",
            "adopt",
            "restore-applications",
            "retry",
            "cancel",
            "finish",
        ] {
            let valid = serde_json::json!({"command": command});
            assert!(serde_json::from_value::<CopyCommand>(valid).is_ok());
            let extra = serde_json::json!({"command": command, "path": "/not-authority"});
            assert!(serde_json::from_value::<CopyCommand>(extra).is_err());
        }
        assert!(
            serde_json::from_value::<CopyCommand>(serde_json::json!({"command":"replace-source"}))
                .is_err()
        );
    }
}
