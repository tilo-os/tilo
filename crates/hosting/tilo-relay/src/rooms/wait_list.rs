//! The requests of people who asked at the door of a room that takes people
//! by invitation, without one: each is an address their invitation goes to
//! ([`JoinRequest::contact`]). Unlike a page's redemption, which waits in
//! memory while its page is open, the person is not online for this: the
//! room keeps it until someone who may invite declines it, so it outlives
//! restarts. One address is one request.
//!
//! Kept beside the room's record, in `<room>.wait-list`, as a versioned
//! format ([`FORMAT`]).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tilo_files::native::migration::Format;
use tilo_relay_protocol::rooms::{JoinRequest, RoomRole};

/// The wait list's on-disk format: a `u32` version, then the entries.
pub(super) const FORMAT: Format = Format {
    name: "room wait list",
    version: 1,
    oldest: 1,
    steps: &[],
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    request_id: [u8; 32],
    contact: String,
    at_ns: u64,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    entries: Vec<Entry>,
}

/// What keeping a request did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitListed {
    Added,
    /// The room kept a request for that address already.
    Already,
    /// The room keeps as many as it takes.
    Full,
}

/// One room's wait list.
#[derive(Debug, Default)]
pub(super) struct WaitList {
    entries: Vec<Entry>,
    path: PathBuf,
}

impl WaitList {
    /// The wait list of the room recorded at `room_path` (its `.room` file).
    pub(super) fn open(room_path: &Path) -> Result<Self, String> {
        let path = room_path.with_extension("wait-list");
        let entries = match std::fs::read(&path) {
            Ok(bytes) => {
                let (stored, _) =
                    postcard::take_from_bytes::<u32>(&bytes).map_err(|error| error.to_string())?;
                let bytes = FORMAT
                    .bring_up(&path, bytes, stored)
                    .map_err(|error| error.to_string())?;
                postcard::from_bytes::<Stored>(&bytes)
                    .map_err(|error| error.to_string())?
                    .entries
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.to_string()),
        };
        Ok(Self { entries, path })
    }

    fn save(&self) -> Result<(), String> {
        let bytes = postcard::to_stdvec(&Stored {
            version: FORMAT.version,
            entries: self.entries.clone(),
        })
        .map_err(|error| error.to_string())?;
        tilo_files::native::atomic::write(&self.path, &bytes).map_err(|error| error.to_string())
    }

    /// Keep the request of `contact` (already a
    /// [`tilo_relay_protocol::rooms::contact_address`]), asked at `now_ns`,
    /// when the room keeps fewer than `max`.
    pub(super) fn keep(
        &mut self,
        contact: &str,
        now_ns: u64,
        max: usize,
    ) -> Result<WaitListed, String> {
        if self.entries.iter().any(|entry| entry.contact == contact) {
            return Ok(WaitListed::Already);
        }
        if self.entries.len() >= max {
            return Ok(WaitListed::Full);
        }
        self.entries.push(Entry {
            request_id: rand::random(),
            contact: contact.to_owned(),
            at_ns: now_ns,
        });
        if let Err(error) = self.save() {
            self.entries.pop();
            return Err(error);
        }
        Ok(WaitListed::Added)
    }

    /// Take out the request `request_id`; whether there was one.
    pub(super) fn decline(&mut self, request_id: [u8; 32]) -> Result<bool, String> {
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.request_id == request_id)
        else {
            return Ok(false);
        };
        let entry = self.entries.remove(index);
        if let Err(error) = self.save() {
            self.entries.insert(index, entry);
            return Err(error);
        }
        Ok(true)
    }

    /// The requests, oldest first, as the room lists its join requests.
    pub(super) fn requests(&self) -> impl Iterator<Item = JoinRequest> + '_ {
        self.entries.iter().map(|entry| JoinRequest {
            request_id: entry.request_id,
            user_public_key: [0; 32],
            device_public_key: [0; 32],
            name: entry.contact.clone(),
            role: RoomRole::Editor,
            requested_at_ns: entry.at_ns,
            contact: Some(entry.contact.clone()),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "in a test these are the assertion")]
mod tests {
    use super::*;

    #[test]
    fn one_address_is_one_request_up_to_the_cap_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let room = dir.path().join("room.room");
        let mut list = WaitList::open(&room).unwrap();
        assert_eq!(list.keep("ada@example.org", 1, 2), Ok(WaitListed::Added));
        assert_eq!(list.keep("ada@example.org", 2, 2), Ok(WaitListed::Already));
        assert_eq!(list.keep("bo@example.org", 3, 2), Ok(WaitListed::Added));
        assert_eq!(list.keep("cy@example.org", 4, 2), Ok(WaitListed::Full));

        let restored = WaitList::open(&room).unwrap();
        let requests: Vec<_> = restored.requests().collect();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].contact.as_deref(), Some("ada@example.org"));
        assert_eq!(requests[0].requested_at_ns, 1);
        assert_eq!(requests[0].user_public_key, [0; 32]);
        assert_ne!(requests[0].request_id, requests[1].request_id);

        let mut list = restored;
        assert_eq!(list.decline(requests[0].request_id), Ok(true));
        assert_eq!(list.decline(requests[0].request_id), Ok(false));
        assert_eq!(list.keep("cy@example.org", 5, 2), Ok(WaitListed::Added));
        let kept: Vec<_> = WaitList::open(&room)
            .unwrap()
            .requests()
            .filter_map(|request| request.contact)
            .collect();
        assert_eq!(kept, ["bo@example.org", "cy@example.org"]);
    }

    #[test]
    fn a_wait_list_a_newer_build_wrote_is_refused_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let room = dir.path().join("room.room");
        let path = room.with_extension("wait-list");
        let newer = postcard::to_stdvec(&Stored {
            version: FORMAT.version + 1,
            entries: Vec::new(),
        })
        .unwrap();
        std::fs::write(&path, &newer).unwrap();
        assert!(WaitList::open(&room).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), newer);
    }
}
