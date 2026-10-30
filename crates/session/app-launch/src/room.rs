//! The room an app is launched into.
//!
//! A workspace is a collaboration room: its id is the cluster id every
//! change an app makes is addressed to, and what the person granted apps in
//! it is what the launcher lets them reach. Both travel with the launch —
//! the id as `--workspace-id`, the grants as a JSON file under the per-user
//! runtime directory that the launcher reads and the app never sees
//! ([`Room::write_grants`]).
//!
//! An app launched with no room gets the manifest's sandbox and nothing
//! else; that is the safe default, so a room is only ever passed when the
//! desktop knows which workspace the launch belongs to.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use collab_types::caps::LaunchGrants;

/// One workspace's room, as a launch into it needs it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Room {
    /// The room's id: the workspace's, or empty for a launch outside any
    /// workspace.
    pub id: String,
    /// What the person granted apps in this room.
    pub grants: LaunchGrants,
    /// Where the session keeps a launch's private files, under the per-user
    /// runtime directory.
    pub directory: PathBuf,
}

impl Room {
    /// A room with no grants: an app gets exactly what its manifest
    /// declares.
    #[must_use]
    pub fn new(id: impl Into<String>, directory: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            grants: LaunchGrants::default(),
            directory: directory.into(),
        }
    }

    /// The room apps are launched into by default: `room`'s id, the
    /// launcher's capability directory under `runtime`.
    #[must_use]
    pub fn under(runtime: &Path, id: impl Into<String>) -> Self {
        Self::new(id, runtime.join("tilo").join("capabilities"))
    }

    /// Whether a launch into this room names a workspace at all.
    #[must_use]
    pub fn is_workspace(&self) -> bool {
        !self.id.is_empty()
    }

    /// Write the grants where `tilo-launcher --capabilities` reads them, and
    /// return that path: `None` when nothing was granted, so the launch
    /// carries no capability flag at all.
    ///
    /// The file is created owner-only before anything is written to it, and
    /// is left for the launcher to read rather than deleted here: a launch
    /// that fails to start must not take the file with it, and the runtime
    /// directory does not outlive the session.
    ///
    /// # Errors
    /// The directory or the file could not be written.
    pub fn write_grants(&self, app: &str) -> std::io::Result<Option<PathBuf>> {
        // Reverse-DNS in principle, but this arrives from a registry file:
        // refuse anything that could escape the directory.
        if app.is_empty() || app.contains('/') || app.contains("..") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("refusing to write a capability file for app {app:?}"),
            ));
        }
        // Only what this room's grants hand this app: a capability naming a
        // person, a room or another app never reaches this app's file
        // ([`LaunchGrants::for_app`]).
        let grants = self.grants.for_app(app);
        if grants.is_empty() {
            return Ok(None);
        }
        {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&self.directory)?;
        }
        let json = serde_json::to_vec(&grants).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("serialising capabilities: {error}"),
            )
        })?;
        let path = self.directory.join(format!("launch-{app}.json"));
        // Created at 0600 rather than written and chmod-ed after: the
        // window between the two is exactly when another process could open
        // it. Not synced: the launcher reads it through the page cache, and
        // this runs on the session's loop, where a sync under I/O pressure
        // freezes the desktop.
        let mut file = std::fs::File::create(&path).and_then(|file| {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            Ok(file)
        })?;
        file.write_all(&json)?;
        Ok(Some(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::caps::{Access, CapOverride, Scope, Subject};

    fn granted(room: &Room, app: &str, path: &str) -> LaunchGrants {
        let mut grants = room.grants.clone();
        grants.sandbox.push(CapOverride::grant(
            Subject::app(app),
            Scope::FsPath(path.into()),
            Access::Read,
        ));
        grants
    }

    #[test]
    fn a_launch_writes_the_grants_the_launcher_reads_and_names_nothing_else() {
        let directory = tempfile::tempdir().expect("a directory");
        let mut room = Room::under(directory.path(), "room-7");
        assert_eq!(room.id, "room-7");
        assert!(room.is_workspace());
        assert_eq!(
            room.write_grants("so.tilo.notes").expect("written"),
            None,
            "nothing granted, no capability file and no flag"
        );

        room.grants = granted(&room, "so.tilo.notes", "/home/you/notes");
        let path = room
            .write_grants("so.tilo.notes")
            .expect("written")
            .expect("a capability file");
        assert!(path.starts_with(directory.path().join("tilo").join("capabilities")));
        let read: LaunchGrants =
            serde_json::from_slice(&std::fs::read(&path).expect("readable")).expect("JSON");
        assert_eq!(
            read, room.grants,
            "the launcher reads back what was granted"
        );
        let mode = {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::metadata(&path)
                .expect("the file")
                .permissions()
                .mode()
        };
        assert_eq!(mode & 0o777, 0o600, "the person's grants are owner-only");
    }

    #[test]
    fn a_launch_reads_back_only_what_this_app_was_granted() {
        let directory = tempfile::tempdir().expect("a directory");
        let mut room = Room::under(directory.path(), "room-7");
        room.grants = granted(&room, "so.tilo.notes", "/home/you/notes");
        room.grants = {
            let mut grants = room.grants.clone();
            grants.sandbox.push(CapOverride::grant(
                Subject::app("so.tilo.mail"),
                Scope::FsPath("/home/you/mail".into()),
                Access::Read,
            ));
            grants.authors.push("so.tilo.notes".into());
            grants
        };
        let path = room
            .write_grants("so.tilo.notes")
            .expect("written")
            .expect("a capability file");
        let read: LaunchGrants =
            serde_json::from_slice(&std::fs::read(&path).expect("readable")).expect("JSON");
        assert_eq!(
            read,
            room.grants.for_app("so.tilo.notes"),
            "the launcher reads what was granted to this app, not the room's other grants"
        );
        assert_eq!(read.sandbox.len(), 1);
        assert_eq!(read.authors, vec!["so.tilo.notes".to_string()]);
    }

    #[test]
    fn a_room_refuses_a_launch_whose_app_id_could_leave_its_directory() {
        let directory = tempfile::tempdir().expect("a directory");
        let mut room = Room::under(directory.path(), "room");
        room.grants = granted(&room, "so.tilo.notes", "/home/you/notes");
        for app in ["", "../escape", "so.tilo/notes"] {
            assert!(
                room.write_grants(app).is_err(),
                "{app:?} must not name a path"
            );
        }
        assert!(
            !Room::under(directory.path(), "").is_workspace(),
            "a launch outside a workspace names no room"
        );
    }
}
