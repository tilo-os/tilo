//! What the desk holds of its rooms: the workspaces this computer joined,
//! who they have in them, and what their apps recorded before it arrived.
//!
//! A workspace is its room, and a joined workspace's room is held by the
//! engine beside the desk ([`crate::engine`]). These are what a person and
//! the apps meet of it: which rooms reached this computer, who is in them,
//! and what they hold.

use crate::Person;
use crate::Session;

impl Session {
    /// The rooms this device holds: the workspaces whose content reached
    /// this computer, joined ones included.
    #[must_use]
    pub fn held_rooms(&self) -> Vec<String> {
        self.shared
            .engine
            .as_ref()
            .map(|engine| {
                engine
                    .rooms()
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Who a held room has in it, as its governance tells it: every person
    /// the workspace was shared with, by the name it knows them by and the
    /// part they take in it. A workspace joined brings its people with it.
    ///
    /// # Errors
    /// The room is not held here, or its governance cannot be read.
    pub fn people(&self, room: &str) -> Result<Vec<Person>, String> {
        self.shared
            .engine
            .as_ref()
            .ok_or_else(|| "this session holds no rooms".to_owned())?
            .people(room)
    }

    /// The devices `room` admits, by the publisher each of their sessions is
    /// known by: everyone whose windows this computer shares with the room
    /// sees and is seen, and no one else, each until the certificate the room
    /// holds of it expires. A device the room removed leaves the roster.
    ///
    /// # Errors
    /// The room is not held here, or its governance cannot be read.
    pub fn roster(&self, room: &str) -> Result<Vec<String>, String> {
        self.shared
            .engine
            .as_ref()
            .ok_or_else(|| "this session holds no rooms".to_owned())?
            .roster(room)
    }

    /// What a held room has of the app `app`'s `record`, as the room last
    /// admitted it: what a workspace shows of it, and what it held before
    /// this computer joined.
    ///
    /// # Errors
    /// The room is not held here, or this computer may not read it.
    pub fn read_record(
        &self,
        room: &str,
        app: &str,
        record: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        self.shared
            .engine
            .as_ref()
            .ok_or_else(|| "this session holds no rooms".to_owned())?
            .record(room, app, record)
    }
}
