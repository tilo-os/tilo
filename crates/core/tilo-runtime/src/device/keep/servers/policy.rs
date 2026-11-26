//! The update policy a server follows, as its document says.
//!
//! The updates a server follows and its daily maintenance window are settings of
//! the image's update policy, which only root writes
//! (`os/usr/libexec/tilo-update-policy-set`). The server runs unprivileged, so
//! it writes what it wants in its own state root, in the fixed lines below, and a
//! root-run unit reads it and applies it through that helper
//! (`os/usr/libexec/tilo-server-update-policy`). Only values the document's own
//! actions accept are written, and the helper checks them again: nothing it
//! reads is passed on as text to a shell.
//!
//! The window an entry keeps is in minutes after midnight UTC; a server's
//! clock is UTC, so it is written as those hours of the machine's clock.

use std::path::Path;

use tilo_workspace_service::workspaces::servers::{MINUTES_PER_DAY, Server, UPDATES};

/// The file a server writes what its update policy should be, for the
/// root-run helper to apply: in the server's state root, which is
/// `/var/lib/tilo-server` on the image ([`POLICY_REQUEST`]).
pub const POLICY_REQUEST_FILE: &str = "update-policy.request";

/// Where the image's server writes [`POLICY_REQUEST_FILE`], and the root-run
/// helper reads it.
pub const POLICY_REQUEST: &str = "/var/lib/tilo-server/update-policy.request";

/// What a server's entry says its update policy is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// The updates it follows ([`UPDATES`]); none for the ones its image
    /// came with.
    pub updates: Option<String>,
    /// Its daily maintenance window, as `HH:MM-HH:MM`; none for no window.
    pub window: Option<String>,
}

impl Policy {
    /// The policy `server`'s entry says.
    ///
    /// # Errors
    /// The entry names updates or a window this build does not apply.
    pub fn of(server: &Server) -> Result<Self, String> {
        let updates = match server.updates.as_str() {
            "" => None,
            updates if UPDATES.contains(&updates) => Some(updates.to_owned()),
            _ => return Err("unknown updates a server follows".into()),
        };
        if server.maintenance_start >= MINUTES_PER_DAY
            || server.maintenance_minutes > MINUTES_PER_DAY
        {
            return Err("a window starts within a day and lasts at most one".into());
        }
        let window = (server.maintenance_minutes > 0).then(|| {
            // Both are within a day here, so the sum stays within two and its
            // remainder is the minute of the day the window ends.
            let end = server
                .maintenance_start
                .checked_add(server.maintenance_minutes)
                .and_then(|sum| sum.checked_rem(MINUTES_PER_DAY))
                .unwrap_or(server.maintenance_start);
            format!("{}-{}", clock(server.maintenance_start), clock(end))
        });
        Ok(Self { updates, window })
    }

    /// The request the helper reads: one fixed line per setting the entry
    /// names.
    #[must_use]
    pub fn document(&self) -> String {
        use std::fmt::Write as _;
        let mut document = String::new();
        if let Some(updates) = &self.updates {
            let _ = writeln!(document, "updates={updates}");
        }
        if let Some(window) = &self.window {
            let _ = writeln!(document, "window={window}");
        }
        document
    }
}

/// `minutes` after midnight as `HH:MM`.
fn clock(minutes: u32) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// Write what `server`'s entry says its update policy is to `path`, the fixed
/// request the root-run helper applies. Answers whether the request changed:
/// writing the same one again changes nothing.
///
/// # Errors
/// The entry names a policy this build does not apply, or the request could
/// not be written.
pub fn write(server: &Server, path: &Path) -> Result<bool, String> {
    let document = Policy::of(server)?.document();
    if std::fs::read(path).ok().as_deref() == Some(document.as_bytes()) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    tilo_files::native::atomic::write_with_mode(path, document.as_bytes(), 0o600)
        .map_err(|error| error.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(updates: &str, start: u32, minutes: u32) -> Server {
        Server {
            updates: updates.into(),
            maintenance_start: start,
            maintenance_minutes: minutes,
            ..Server::default()
        }
    }

    /// The writer writes exactly the values the entry names, and nothing for
    /// what it names none of; a policy this build cannot apply is refused
    /// before anything is written.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the assertions are the test; its errors are the setup returning them"
    )]
    fn the_request_is_exactly_the_entrys_values() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("update-policy.request");
        assert!(write(&server("stable", 180, 60), &path)?);
        assert_eq!(
            std::fs::read_to_string(&path)?,
            "updates=stable\nwindow=03:00-04:00\n"
        );
        assert!(!write(&server("stable", 180, 60), &path)?, "unchanged");
        assert!(write(&server("dev", 0, 0), &path)?);
        assert_eq!(std::fs::read_to_string(&path)?, "updates=dev\n");
        assert!(write(&Server::default(), &path)?);
        assert_eq!(std::fs::read_to_string(&path)?, "");
        // A window across midnight ends the next day.
        assert!(write(&server("", 22 * 60, 240), &path)?);
        assert_eq!(std::fs::read_to_string(&path)?, "window=22:00-02:00\n");
        let refused = write(&server("nightly", 0, 0), &path);
        assert!(refused.is_err());
        assert_eq!(
            std::fs::read_to_string(&path)?,
            "window=22:00-02:00\n",
            "nothing is written for a policy that is refused"
        );
        let refused = write(&server("stable", 24 * 60, 10), &path);
        assert!(refused.is_err());
        Ok(())
    }
}
