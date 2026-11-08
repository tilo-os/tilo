//! What this computer holds of its workspaces: the ones it joined, who they have in them, and what
//! their apps recorded before it arrived.
//! A joined workspace is held by the engine beside the desk ([`crate::engine`]). These are what a
//! person and the apps meet of it: which workspaces reached this computer, who is in them, and what
//! they hold.

use crate::Person;
use crate::Session;

impl Session {
    /// The workspaces this device holds: those whose content reached this computer, joined ones
    /// included.
    #[must_use]
    pub fn held_workspaces(&self) -> Vec<String> {
        self.shared
            .engine
            .as_ref()
            .map(|engine| {
                engine
                    .workspaces()
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Who a held workspace has in it, as its governance tells it: every
    /// person the workspace was shared with, by the name it knows them by and
    /// the part they take in it. A workspace joined brings its people with
    /// it.
    ///
    /// # Errors
    /// The workspace is not held here, or its governance cannot be read.
    pub fn people(&self, workspace: &str) -> Result<Vec<Person>, String> {
        self.shared
            .engine
            .as_ref()
            .ok_or_else(|| "this session holds no workspaces".to_owned())?
            .people(workspace)
    }

    /// The devices `workspace` admits, by the publisher each of their
    /// sessions is known by: everyone whose windows this computer shares with
    /// the workspace sees and is seen, and no one else, each until the
    /// certificate the workspace holds of it expires. A device the workspace
    /// removed leaves the roster.
    ///
    /// # Errors
    /// The workspace is not held here, or its governance cannot be read.
    pub fn roster(&self, workspace: &str) -> Result<Vec<String>, String> {
        self.shared
            .engine
            .as_ref()
            .ok_or_else(|| "this session holds no workspaces".to_owned())?
            .roster(workspace)
    }

    /// What a held workspace has of the app `app`'s `record`, as the
    /// workspace last admitted it: what it shows of it, and what it held
    /// before this computer joined. A record the workspace's hosts send only
    /// once asked (an on-demand record, or a part of a chain behind its head)
    /// reads as nothing until it is here, and is asked for on its first read.
    ///
    /// # Errors
    /// The workspace is not held here, or this computer may not read it.
    pub fn read_record(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        self.shared
            .engine
            .as_ref()
            .ok_or_else(|| "this session holds no workspaces".to_owned())?
            .record(workspace, app, record)
    }
}
