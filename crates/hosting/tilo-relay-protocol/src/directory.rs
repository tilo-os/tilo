//! The public workspaces a site lists, and the door each one admits people
//! through.
//!
//! A workspace's owner makes it public in its settings: a short description
//! and the role everyone who joins gets (the default role). The host the
//! workspace lives on then lists it in its directory and admits whoever asks
//! at its door with that role, the way a chat server's invite-free join
//! does. Nobody decides each join again; the owner and the workspace's
//! moderators remove and ban like they do for anyone, and a banned person is
//! refused at the door.
//!
//! JSON over HTTPS, below the host's [`API_PREFIX`](crate::web::API_PREFIX)
//! for a relay, at the site's own path for a site's community workspace (the workspace
//! service). Every route is a `POST` (a relay serves nothing else):
//!
//! | Route | Body | Answer |
//! |---|---|---|
//! | [`DIRECTORY_PATH`] | none | [`DirectoryPage`] |
//! | `<door>join` | [`DoorJoin`] | [`DoorAnswer`] |
//! | `<door>report` | [`DoorReport`] | `204` |
//! | `<door>errors` | [`PageError`] | `204` |
//!
//! `<door>` is a [`DirectoryEntry::door`], resolved against the directory's
//! own address. A site names its directory in its `entry.json`
//! (`directory`, an address), beside its home workspace.

use serde::{Deserialize, Serialize};

use crate::workspaces::WorkspaceRole;

/// The directory of a host, below its API prefix.
pub const DIRECTORY_PATH: &str = "/directory";

/// The longest description a public workspace shows.
pub const MAX_DESCRIPTION_CHARS: usize = 280;

/// People shown as "here now" on a listing at most; the rest are counted.
pub const MAX_FACES: usize = 8;

/// The longest reason a report gives.
pub const MAX_REPORT_CHARS: usize = 500;

/// The door of `workspace` on a relay, relative to its directory: what
/// [`DirectoryEntry::door`] names.
#[must_use]
pub fn door_path(workspace: &str) -> String {
    format!("directory/{workspace}/")
}

/// The workspace a relay door route below the API prefix names, and what is
/// asked of it there: `/directory/<workspace>/join` is `(workspace, "join")`.
#[must_use]
pub fn door_route(route: &str) -> Option<(&str, &str)> {
    let rest = route.strip_prefix(DIRECTORY_PATH)?.strip_prefix('/')?;
    let (workspace, step) = rest.split_once('/')?;
    (!workspace.is_empty() && !step.contains('/')).then_some((workspace, step))
}

/// What the owner decided when they made the workspace public.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Listing {
    /// What the workspace is, in a sentence or two.
    pub description: String,
    /// The role everyone who joins through the directory gets.
    pub default_role: WorkspaceRole,
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
        if self.default_role == WorkspaceRole::Moderator {
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
    /// The workspace's UUID.
    #[serde(rename = "room")]
    pub workspace: String,
    pub name: String,
    /// What it is.
    #[serde(default)]
    pub description: String,
    /// The role people who join get.
    pub default_role: WorkspaceRole,
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

/// Why a door did not let a person in, as it says it: what every caller reads, never a sentence
/// to match.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DoorRefusalCode {
    /// The workspace's people banned them.
    Banned,
    /// They were removed from the workspace within the day: only an invitation brings them back.
    Removed,
    /// The workspace takes people by invitation, and they hold none.
    InvitationOnly,
    /// The door holds as many open grants as the workspace keeps: they come back after the
    /// `Retry-After` the answer carries.
    Full,
    /// Anything else; the sentence says what.
    Refused,
}

/// What a door answers a person it does not let in (HTTP 403, or 503 with `Retry-After` when
/// [`DoorRefusalCode::Full`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoorRefused {
    /// What kind of refusal it is.
    pub code: DoorRefusalCode,
    /// What happened, in plain words.
    pub error: String,
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

/// The largest page error report a door takes.
pub const MAX_PAGE_ERROR_BYTES: usize = 4096;

/// A page's own uncaught error, reported to the door that let the page in:
/// the host logs it for the site's operator, and keeps nothing else.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageError {
    /// What happened: `error`, `unhandledrejection`, or `panic` for a runtime
    /// panic the page heard but the browser could not throw.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub message: String,
    /// The file the error came from, when the browser named one.
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub line: u32,
    #[serde(default)]
    pub column: u32,
    /// The call stack the page held, when it held one.
    #[serde(default)]
    pub stack: String,
    /// The page that failed: its path on the site.
    #[serde(default)]
    pub page: String,
    /// The page's build, as `version.json` names it.
    #[serde(default)]
    pub build: String,
}

impl PageError {
    /// The report a page sent, when it is one this host logs.
    ///
    /// # Errors
    ///
    /// A body larger than [`MAX_PAGE_ERROR_BYTES`], or one that is not a
    /// report: a page's own code, not another site's identity, is what a
    /// door takes here.
    pub fn parse(body: &[u8]) -> Result<Self, &'static str> {
        if body.len() > MAX_PAGE_ERROR_BYTES {
            return Err("a page error report is at most 4 KiB");
        }
        serde_json::from_slice(body).map_err(|_| "not a page error report")
    }

    /// The kind as a host logs it: a kind this version does not know logs as
    /// `other` rather than being dropped.
    #[must_use]
    pub fn kind(&self) -> &str {
        match self.kind.as_str() {
            "error" | "unhandledrejection" => self.kind.as_str(),
            _ => "other",
        }
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
            workspace: "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71".into(),
            name: "Coffee".into(),
            description: String::new(),
            default_role: WorkspaceRole::Commenter,
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
        let workspace = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
        assert_eq!(
            door_route(&format!("/{}join", door_path(workspace))),
            Some((workspace, "join"))
        );
        assert_eq!(
            door_route(&format!("/directory/{workspace}/report")),
            Some((workspace, "report"))
        );
        assert_eq!(door_route("/directory"), None);
        assert_eq!(door_route("/directory//join"), None);
        assert_eq!(
            door_route(&format!("/directory/{workspace}/join/more")),
            None
        );
        assert_eq!(door_route("/rooms"), None);
    }

    #[test]
    fn a_listing_is_text_and_hands_out_no_moderators() {
        let listing = Listing {
            description: "  A place for coffee.\nAnd tea.  ".into(),
            default_role: WorkspaceRole::Commenter,
        };
        assert_eq!(
            listing.checked().map(|listing| listing.description),
            Ok("A place for coffee.\nAnd tea.".to_owned())
        );
        let moderators = Listing {
            description: String::new(),
            default_role: WorkspaceRole::Moderator,
        };
        assert_eq!(moderators.checked(), Err(ListingError::Role));
        let long = Listing {
            description: "x".repeat(MAX_DESCRIPTION_CHARS + 1),
            default_role: WorkspaceRole::Viewer,
        };
        assert_eq!(long.checked(), Err(ListingError::Description));
        let control = Listing {
            description: "bell\u{7}".into(),
            default_role: WorkspaceRole::Viewer,
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
    fn a_page_error_report_is_what_the_page_sends() {
        let body = serde_json::json!({
            "kind": "error", "message": "x is not a function", "source": "https://site.example/v/1/page.js",
            "line": 12, "column": 4, "stack": "at page.js:12", "page": "/", "build": "1.2.3",
        })
        .to_string();
        let error = PageError::parse(body.as_bytes()).unwrap();
        assert_eq!(error.kind(), "error");
        assert_eq!(error.line, 12);
        assert_eq!(error.build, "1.2.3");
        // A report without the optional parts is still a report.
        let rejection =
            PageError::parse(br#"{"kind": "unhandledrejection", "message": "gone"}"#).unwrap();
        assert_eq!(rejection.kind(), "unhandledrejection");
        assert_eq!(rejection.source, "");
        // A kind this version does not know is logged as one, not refused.
        assert_eq!(
            PageError::parse(br#"{"kind": "console", "message": "hi"}"#)
                .unwrap()
                .kind(),
            "other"
        );
        assert!(PageError::parse(b"not json").is_err());
        assert!(PageError::parse(&vec![b' '; MAX_PAGE_ERROR_BYTES + 1]).is_err());
    }

    #[test]
    fn the_wire_is_camel_case_json_with_optional_fields() {
        let page: DirectoryPage = serde_json::from_value(serde_json::json!({
            "workspaces": [{
                "room": "r", "name": "Coffee", "defaultRole": "Editor", "door": "https://tilo.sh/",
                "community": true, "hereCount": 12
            }]
        }))
        .unwrap();
        let listed = &page.workspaces[0];
        assert!(listed.community);
        assert_eq!(listed.default_role, WorkspaceRole::Editor);
        assert_eq!(listed.here_count, 12);
        assert!(listed.here.is_empty());
        let answer: DoorAnswer = serde_json::from_value(serde_json::json!({
            "invitation": "abc", "admissionToken": "t"
        }))
        .unwrap();
        assert_eq!(answer.origin, None);
    }
}
