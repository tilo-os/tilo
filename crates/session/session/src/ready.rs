//! The record that tells the programs watching a restarted desktop that a person can use it.
//!
//! A live update restarts the desktop in place and rolls back unless the desktop comes back. A
//! compositor process that is merely running is not enough: one that starts, shows nothing and
//! idles looks the same. This session publishes the record once every enabled output has shown
//! its first frame — the lock surface instead, when the session starts locked, because that is
//! what the frame draws — and the windows this device left open are being asked for. The record
//! names this process and its kernel start time, so a record a previous run left is never read
//! as this one's, and the session removes it when it ends.

use std::io;
use std::path::PathBuf;

use tilo_paths::login;

/// This session's ready record: where it goes and what it says.
pub(crate) struct Desktop {
    path: PathBuf,
    record: String,
    published: bool,
}

impl Desktop {
    /// Claim the record for this process. `None` when no login runtime directory names the
    /// session directory, or the system does not describe this process.
    pub(crate) fn claim() -> Option<Self> {
        let path = login::session_dir(std::env::var_os("XDG_RUNTIME_DIR"))?
            .join(login::DESKTOP_READY_FILE);
        Some(Self {
            path,
            record: tilo_process::identity_record(std::process::id())?,
            published: false,
        })
    }

    /// Whether the record is published.
    pub(crate) fn published(&self) -> bool {
        self.published
    }

    /// The desktop is usable: publish the record atomically as this person's private file, so a
    /// reader sees either no record or the whole one.
    ///
    /// # Errors
    /// The session directory or the record could not be written.
    pub(crate) fn publish(&mut self) -> io::Result<()> {
        if let Some(directory) = self.path.parent() {
            tilo_files::native::private::directory(directory)?;
        }
        tilo_files::native::private::write_atomic(&self.path, self.record.as_bytes())?;
        self.published = true;
        Ok(())
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        // Only the record this session wrote: one that a session taking over published says what
        // that session is.
        if std::fs::read(&self.path).is_ok_and(|published| published == self.record.as_bytes()) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    tilo_test_env::scenario! {
        fn the_ready_record_belongs_to_the_session_that_published_it;

        phase a_published_record_names_this_process_and_leaves_with_it;
        {
            let mut environment = tilo_test_env::ScopedEnv::new();
            environment.remove("XDG_RUNTIME_DIR");
            assert!(
                Desktop::claim().is_none(),
                "no runtime directory is no record"
            );
            let runtime = tempfile::tempdir().expect("a runtime directory");
            environment.set("XDG_RUNTIME_DIR", runtime.path().as_os_str());
            let mut desktop = Desktop::claim().expect("the login's record");
            assert!(!desktop.published());
            desktop.publish().expect("the desktop is usable");
            assert!(desktop.published());
            let path = runtime
                .path()
                .join(tilo_paths::login::SESSION_DIR)
                .join(tilo_paths::login::DESKTOP_READY_FILE);
            let record = fs::read_to_string(&path).expect("the published record");
            assert!(tilo_process::identity_names(&record, std::process::id()));
            drop(desktop);
            assert!(!path.exists(), "the record goes with the session");
        }

        phase a_record_another_session_is_left_alone;
        {
            let mut environment = tilo_test_env::ScopedEnv::new();
            let runtime = tempfile::tempdir().expect("a runtime directory");
            environment.set("XDG_RUNTIME_DIR", runtime.path().as_os_str());
            let desktop = Desktop::claim().expect("the login's record");
            let path = runtime
                .path()
                .join(tilo_paths::login::SESSION_DIR)
                .join(tilo_paths::login::DESKTOP_READY_FILE);
            fs::create_dir_all(path.parent().expect("the session directory")).expect("the directory");
            fs::write(&path, b"1 2\n").expect("a record of another session");
            drop(desktop);
            assert!(path.exists(), "another session's record stays");
        }
    }
}
