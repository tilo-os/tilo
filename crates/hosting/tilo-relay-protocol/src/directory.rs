//! The public workspaces a site lists, and the door each one admits people
//! through.
//!
//! A workspace's owner makes it public in its settings: a short description
//! and the role everyone who joins gets (the default role). The host the
//! workspace lives on then lists it in its directory and admits whoever asks
//! at its door with that role, the way a chat server's invite-free join
//! does. Nobody decides each join again; the owner and the room's
//! moderators remove and ban like they do for anyone, and a banned person is
//! refused at the door.
//!
//! JSON over HTTPS, below the host's [`API_PREFIX`](crate::web::API_PREFIX)
//! for a relay, at the site's own path for a site's community room (the room
//! service). Every route is a `POST` (a relay serves nothing else):
//!
//! | Route | Body | Answer |
//! |---|---|---|
//! | [`DIRECTORY_PATH`] | none | [`DirectoryPage`] |
//! | `<door>join` | [`DoorJoin`] | [`DoorAnswer`] |
//! | `<door>report` | [`DoorReport`] | `204` |
//!
//! `<door>` is a [`DirectoryEntry::door`], resolved against the directory's
//! own address. A site names its directory in its `entry.json`
//! (`directory`, an address), beside its home workspace.

use serde::{Deserialize, Serialize};

use crate::rooms::RoomRole;

/// The directory of a host, below its API prefix.
pub const DIRECTORY_PATH: &str = "/directory";

/// The longest description a public workspace shows.
pub const MAX_DESCRIPTION_CHARS: usize = 280;

/// People shown as "here now" on a listing at most; the rest are counted.
pub const MAX_FACES: usize = 8;

/// The longest reason a report gives.
pub const MAX_REPORT_CHARS: usize = 500;

/// The door of `room` on a relay, relative to its directory: what
/// [`DirectoryEntry::door`] names.
#[must_use]
pub fn door_path(room: &str) -> String {
    format!("directory/{room}/")
}

/// The room a relay door route below the API prefix names, and what is
/// asked of it there: `/directory/<room>/join` is `(room, "join")`.
#[must_use]
pub fn door_route(route: &str) -> Option<(&str, &str)> {
    let rest = route.strip_prefix(DIRECTORY_PATH)?.strip_prefix('/')?;
    let (room, step) = rest.split_once('/')?;
    (!room.is_empty() && !step.contains('/')).then_some((room, step))
}

/// What the owner decided when they made the workspace public.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Listing {
    /// What the workspace is, in a sentence or two.
    pub description: String,
    /// The role everyone who joins through the directory gets.
    pub default_role: RoomRole,
}

/// Why a listing cannot be published.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ListingError {
    /// The description is longer than [`MAX_DESCRIPTION_CHARS`] or holds a
    /// control character other than a line break.
    #[error("a description is at most {MAX_DESCRIPTION_CHARS} characters of text")]
    Description,
    /// Moderators are chosen, never handed out at the door.
    #[error("people who join on their own become viewers, commenters or editors")]
    Role,
}

impl Listing {
    /// The listing with its description trimmed, when it may be published.
    pub fn checked(self) -> Result<Self, ListingError> {
        let description = self.description.trim().to_owned();
        if description.chars().count() > MAX_DESCRIPTION_CHARS
            || description.chars().any(|c| c.is_control() && c != '\n')
        {
            return Err(ListingError::Description);
        }
        if self.default_role == RoomRole::Moderator {
            return Err(ListingError::Role);
        }
        Ok(Self {
            description,
            default_role: self.default_role,
        })
    }
}

/// Someone in a public workspace now, as its listing shows them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Face {
    /// Their user root, hex: what their colour is derived from.
    pub user_public_key: String,
    /// The name they gave the workspace.
    pub name: String,
}

/// One public workspace a directory lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryEntry {
    /// The room's UUID.
    pub room: String,
    pub name: String,
    /// What it is.
    #[serde(default)]
    pub description: String,
    /// The role people who join get.
    pub default_role: RoomRole,
    /// Its door, relative to the directory's address (or absolute).
    pub door: String,
    #[serde(default)]
    pub here: Vec<Face>,
    /// How many people are in it now, `here` included.
    #[serde(default)]
    pub here_count: u32,
    /// How many people belong to it.
    #[serde(default)]
    pub members: u32,
    /// When someone last changed something in it (Unix milliseconds), when
    /// the host knows.
    #[serde(default)]
    pub last_active_ms: Option<u64>,
    /// The site's own community workspace, listed first.
    #[serde(default)]
    pub community: bool,
}

/// What a directory answers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryPage {
    /// The public workspaces, in no particular order.
    pub workspaces: Vec<DirectoryEntry>,
}

/// A person asking a door to let them in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DoorJoin {
    /// Their user root, hex.
    pub user_public_key: String,
    /// The name they give the workspace.
    #[serde(default)]
    pub display_name: String,
}

/// What a door answers a person it lets in: an invitation only they can
/// redeem and the one-time token that admits them at once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoorAnswer {
    /// The invitation bound to the person, URL-safe base64 without padding.
    pub invitation: String,
    /// The token their redemption presents.
    pub admission_token: String,
    /// Where to redeem it; the door's own origin when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// A report of a public workspace to the people who run it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DoorReport {
    /// Who reports, hex user root.
    pub reporter_user_public_key: String,
    /// What is wrong with it.
    pub reason: String,
}

impl DoorReport {
    /// The reason trimmed, when it is one.
    ///
    /// # Errors
    ///
    /// An empty reason, or one longer than [`MAX_REPORT_CHARS`].
    pub fn reason(&self) -> Result<&str, &'static str> {
        let reason = self.reason.trim();
        if reason.is_empty() || reason.chars().count() > MAX_REPORT_CHARS {
            return Err("a reason is 1 to 500 characters");
        }
        Ok(reason)
    }
}

/// How lively a public workspace is, as its listing says in a word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    HereNow(u32),
    /// Someone changed something today (the last 24 hours).
    ActiveToday,
    /// Someone changed something this week.
    ActiveThisWeek,
    /// Nothing recent the host knows of.
    Quiet,
}

impl DirectoryEntry {
    /// How lively it is at `now_ms`.
    #[must_use]
    pub fn activity(&self, now_ms: u64) -> Activity {
        const DAY_MS: u64 = 24 * 60 * 60 * 1000;
        let here = self
            .here_count
            .max(u32::try_from(self.here.len()).unwrap_or(u32::MAX));
        if here > 0 {
            return Activity::HereNow(here);
        }
        match self.last_active_ms.map(|at| now_ms.saturating_sub(at)) {
            Some(ago) if ago < DAY_MS => Activity::ActiveToday,
            Some(ago) if ago < 7 * DAY_MS => Activity::ActiveThisWeek,
            _ => Activity::Quiet,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> DirectoryEntry {
        DirectoryEntry {
            room: "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71".into(),
            name: "Coffee".into(),
            description: String::new(),
            default_role: RoomRole::Commenter,
            door: door_path("6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71"),
            here: Vec::new(),
            here_count: 0,
            members: 0,
            last_active_ms: None,
            community: false,
        }
    }

    #[test]
    fn a_door_route_names_its_room_and_step() {
        let room = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
        assert_eq!(
            door_route(&format!("/{}join", door_path(room))),
            Some((room, "join"))
        );
        assert_eq!(
            door_route(&format!("/directory/{room}/report")),
            Some((room, "report"))
        );
        assert_eq!(door_route("/directory"), None);
        assert_eq!(door_route("/directory//join"), None);
        assert_eq!(door_route(&format!("/directory/{room}/join/more")), None);
        assert_eq!(door_route("/rooms"), None);
    }

    #[test]
    fn a_listing_is_text_and_hands_out_no_moderators() {
        let listing = Listing {
            description: "  A place for coffee.\nAnd tea.  ".into(),
            default_role: RoomRole::Commenter,
        };
        assert_eq!(
            listing.checked().map(|listing| listing.description),
            Ok("A place for coffee.\nAnd tea.".to_owned())
        );
        let moderators = Listing {
            description: String::new(),
            default_role: RoomRole::Moderator,
        };
        assert_eq!(moderators.checked(), Err(ListingError::Role));
        let long = Listing {
            description: "x".repeat(MAX_DESCRIPTION_CHARS + 1),
            default_role: RoomRole::Viewer,
        };
        assert_eq!(long.checked(), Err(ListingError::Description));
        let control = Listing {
            description: "bell\u{7}".into(),
            default_role: RoomRole::Viewer,
        };
        assert_eq!(control.checked(), Err(ListingError::Description));
    }

    #[test]
    fn activity_says_who_is_here_before_how_recent() {
        const DAY_MS: u64 = 24 * 60 * 60 * 1000;
        let now = 100 * DAY_MS;
        let mut listed = entry();
        assert_eq!(listed.activity(now), Activity::Quiet);
        listed.last_active_ms = Some(now - DAY_MS / 2);
        assert_eq!(listed.activity(now), Activity::ActiveToday);
        listed.last_active_ms = Some(now - 3 * DAY_MS);
        assert_eq!(listed.activity(now), Activity::ActiveThisWeek);
        listed.last_active_ms = Some(now - 30 * DAY_MS);
        assert_eq!(listed.activity(now), Activity::Quiet);
        listed.here_count = 4;
        assert_eq!(listed.activity(now), Activity::HereNow(4));
    }

    #[test]
    fn the_wire_is_camel_case_json_with_optional_fields() {
        let page: DirectoryPage = serde_json::from_value(serde_json::json!({
            "workspaces": [{
                "room": "r", "name": "Coffee", "defaultRole": "Editor", "door": "https://tilo.coffee/",
                "community": true, "hereCount": 12
            }]
        }))
        .unwrap();
        let listed = &page.workspaces[0];
        assert!(listed.community);
        assert_eq!(listed.default_role, RoomRole::Editor);
        assert_eq!(listed.here_count, 12);
        assert!(listed.here.is_empty());
        let answer: DoorAnswer = serde_json::from_value(serde_json::json!({
            "invitation": "abc", "admissionToken": "t"
        }))
        .unwrap();
        assert_eq!(answer.origin, None);
    }
}
