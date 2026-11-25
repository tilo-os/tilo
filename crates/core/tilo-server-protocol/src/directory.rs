//! The public workspaces a site lists, and the entry each one admits people
//! through.
//!
//! A workspace's owner makes it public in its settings: a short description
//! and the role everyone who joins gets (the default role). The host the
//! workspace lives on then lists it in its directory and admits whoever asks
//! at its entry with that role, the way a chat server's invite-free join
//! does. Nobody decides each join again; the owner and the workspace's
//! moderators remove and ban like they do for anyone, and a banned person is
//! refused at the entry.
//!
//! JSON over HTTPS, below the host's [`API_PREFIX`](crate::web::API_PREFIX)
//! for a server, at the site's own path for a site's community workspace (the workspace
//! service). Every route is a `POST` (a server serves nothing else):
//!
//! | Route | Body | Answer |
//! |---|---|---|
//! | [`DIRECTORY_PATH`] | none | [`DirectoryPage`] |
//! | `<entry>report` | [`EntryReport`] | `204` |
//! | `<entry>errors` | [`PageError`] | `204` |
//! | [`WAIT_LIST`] | `{email, language}` | `{email, removal, mail}` |
//! | [`WAIT_LIST_REMOVE`] | `{secret}` | `{}` |
//! | [`WAIT_LIST_CONFIRM`] | `{token}` | `{confirmed}` |
//! | [`WAIT_LIST_STATUS`] | `{secret}` | `{state}` |
//!
//! The wait list is what an entry that takes people by invitation keeps for
//! someone it turns away: an address it may be invited at, confirmed by the
//! link mailed there, taken off by whoever left it. An entry that keeps none
//! answers its four routes `404`.
//!
//! `<entry>` is a [`DirectoryEntry::entry`], resolved against the directory's
//! own address. A site names its directory in its `entry.json`
//! (`directory`, an address), beside its home workspace.
//!
//! Joining is not one of these routes: a device enters through an entry on the
//! session it opens with the entry's host ([`crate::entering`]), whose origin
//! is the entry's own, and the workspace's entry decides there.

use serde::{Deserialize, Serialize};

use crate::workspaces::Role;

/// The directory of a host, below its API prefix.
pub const DIRECTORY_PATH: &str = "/directory";

/// People shown as "here now" on a listing at most; the rest are counted.
pub const MAX_FACES: usize = 8;

/// The longest reason a report gives.
pub const MAX_REPORT_CHARS: usize = 500;

/// The entry of `workspace` on a server, relative to its directory: what
/// [`DirectoryEntry::entry`] names.
#[must_use]
pub fn entry_path(workspace: &str) -> String {
    format!("directory/{workspace}/")
}

/// The workspace a server entry route below the API prefix names, and what is
/// asked of it there: `/directory/<workspace>/report` is `(workspace, "report")`.
/// The wait list's acts are the one step of two words:
/// `…/wait-list/remove` is `(workspace, "wait-list/remove")`.
#[must_use]
pub fn entry_route(route: &str) -> Option<(&str, &str)> {
    let rest = route.strip_prefix(DIRECTORY_PATH)?.strip_prefix('/')?;
    let (workspace, step) = rest.split_once('/')?;
    let two_words = matches!(
        step,
        WAIT_LIST_REMOVE | WAIT_LIST_CONFIRM | WAIT_LIST_STATUS
    );
    (!workspace.is_empty() && (two_words || !step.contains('/'))).then_some((workspace, step))
}

/// The entry's wait list: the acts a person who asked without an invitation
/// leaves an address with, confirms it, takes it off and asks where it
/// stands, each a step after its entry (`<entry>wait-list`, below the API
/// prefix for a server).
pub const WAIT_LIST: &str = "wait-list";
/// The step taking an address off the list with the secret it was handed or
/// mailed.
pub const WAIT_LIST_REMOVE: &str = "wait-list/remove";
/// The step opening the link a confirmation mail carried.
pub const WAIT_LIST_CONFIRM: &str = "wait-list/confirm";
/// The step asking where the address a removal secret belongs to stands.
pub const WAIT_LIST_STATUS: &str = "wait-list/status";

/// What the owner decided when they made the workspace public.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Listing {
    /// What the workspace is, in a sentence or two.
    pub description: String,
    /// The role everyone who joins through the directory gets.
    pub default_role: Role,
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
    pub workspace: String,
    pub name: String,
    /// What it is.
    #[serde(default)]
    pub description: String,
    /// The role people who join get.
    pub default_role: Role,
    /// Its entry, relative to the directory's address (or absolute).
    pub entry: String,
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
    /// Its entry lets in only people invited or already in it: anyone else it
    /// turns away, where [`Self::wait_list`] says how to ask for an invite.
    #[serde(default)]
    pub invitation_only: bool,
    /// The wait list a person without an invitation may leave an address on,
    /// as the entry's refusal names it ([`EntryRefused::wait_list`]): a page
    /// offers it before anyone asks the entry.
    #[serde(default)]
    pub wait_list: Option<EntryWaitList>,
}

/// What a directory answers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryPage {
    /// Who this host is and where it is dialled, as it publishes itself: a device entering one
    /// of these workspaces through its entry reaches the host from this, not by asking it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<crate::web::PageHost>,
    /// The public workspaces, in no particular order.
    pub workspaces: Vec<DirectoryEntry>,
}

/// A person asking an entry to let them in: what a device's entry through the
/// entry asks of it ([`crate::entering::Through::Entry`]), on its session with
/// the entry's host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntryJoin {
    /// Their user root, hex.
    pub user_public_key: String,
    /// The name they give the workspace.
    #[serde(default)]
    pub display_name: String,
}

/// Why an entry did not let a person in, as it says it: what every caller reads, never a sentence
/// to match.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryRefusalCode {
    /// The workspace's people banned them.
    Banned,
    /// They were removed from the workspace within the day: only an invitation brings them back.
    Removed,
    /// The workspace takes people by invitation, and they hold none.
    InvitationOnly,
    /// The entry holds as many open grants as the workspace keeps: they come back after the
    /// `Retry-After` the answer carries.
    Full,
    /// Anything else; the sentence says what.
    #[default]
    Refused,
}

/// What an entry answers a person it does not let in (HTTP 403, or 503 with `Retry-After` when
/// [`EntryRefusalCode::Full`]).
///
/// It also travels in a session's entry ([`crate::entering::EnterRefusal::Entry`]), whose frames
/// are postcard: a format that names no fields, so every field is written whatever its value. A
/// field left out when empty shifts the rest, and the device can read no refusal at all.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryRefused {
    /// What kind of refusal it is.
    pub code: EntryRefusalCode,
    /// What happened, in plain words.
    pub error: String,
    /// The workspace takes people by invitation and keeps a wait list
    /// ([`EntryWaitList`]): the person may leave an address to be invited at.
    #[serde(default)]
    pub wait_list: bool,
    /// Where the person asks to be taken off that list, when whoever runs the
    /// entry named a contact.
    #[serde(default)]
    pub contact: Option<String>,
    /// The entry mails the address a link that confirms it: without, it lists
    /// an address at once.
    #[serde(default)]
    pub mail: bool,
}

impl EntryRefused {
    /// The refusal, naming the wait list `entry` keeps for a workspace taking
    /// people by invitation.
    #[must_use]
    pub fn with_wait_list(mut self, entry: Option<&EntryWaitList>) -> Self {
        if self.code == EntryRefusalCode::InvitationOnly
            && let Some(entry) = entry
        {
            self.wait_list = true;
            self.contact.clone_from(&entry.contact);
            self.mail = entry.mail;
        }
        self
    }

    /// The refusal as a host action's refusal reason: its JSON, which
    /// [`Self::from_reason`] reads back typed. A host refuses its own
    /// operations this way when the person asking was never let in through
    /// its site's entry, so whoever asked hears the entry's answer, wait list
    /// included.
    #[must_use]
    pub fn to_reason(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| self.error.clone())
    }

    /// The entry's refusal an action's refusal reason carries, when it carries
    /// one ([`Self::to_reason`]).
    #[must_use]
    pub fn from_reason(reason: &str) -> Option<Self> {
        serde_json::from_str(reason).ok()
    }
}

/// The wait list an entry keeps for a workspace that takes people by
/// invitation: what its page offers someone it turns away, in the refusal's
/// [`EntryRefused::wait_list`] (`workspaces::Workspaces::waiting_offer`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryWaitList {
    /// Where someone asks to be taken off the list, as the entry names it.
    pub contact: Option<String>,
    /// The entry mails the address a link that confirms it.
    pub mail: bool,
}

/// A report of a public workspace to the people who run it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntryReport {
    /// Who reports, hex user root.
    pub reporter_user_public_key: String,
    /// What is wrong with it.
    pub reason: String,
}

impl EntryReport {
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

/// The largest page error report an entry takes.
pub const MAX_PAGE_ERROR_BYTES: usize = 4096;

/// A page's own uncaught error, reported to the entry that let the page in:
/// the host logs it for the site's operator, and keeps nothing else.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageError {
    /// What happened: `error`, `unhandledrejection`, `panic` for a runtime
    /// panic the page heard but the browser could not throw, or `failed`
    /// when the page stopped on its failure screen (`message` then starts
    /// with the cause it showed, `source` names the host it was reaching).
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
    /// entry takes here.
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
            "error" | "unhandledrejection" | "panic" | "failed" => self.kind.as_str(),
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
            default_role: Role::Commenter,
            entry: entry_path("6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71"),
            here: Vec::new(),
            here_count: 0,
            members: 0,
            last_active_ms: None,
            community: false,
            invitation_only: false,
            wait_list: None,
        }
    }

    tilo_test_env::scenario! {
        fn an_entry_names_its_steps_and_reads_its_pages_error_reports;

        phase an_entry_route_names_its_workspace_and_step;
        {
            let workspace = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
            assert_eq!(
                entry_route(&format!("/{}errors", entry_path(workspace))),
                Some((workspace, "errors"))
            );
            assert_eq!(
                entry_route(&format!("/directory/{workspace}/report")),
                Some((workspace, "report"))
            );
            assert_eq!(entry_route("/directory"), None);
            assert_eq!(entry_route("/directory//report"), None);
            assert_eq!(
                entry_route(&format!("/directory/{workspace}/report/more")),
                None
            );
            assert_eq!(entry_route("/workspaces"), None);
        }

        phase a_page_error_report_is_what_the_page_sends;
        {
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
    }

    tilo_test_env::scenario! {
        fn a_directory_lists_workspaces_as_text_activity_and_camel_case_json;

        phase activity_says_who_is_here_before_how_recent;
        {
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

        phase the_wire_is_camel_case_json_with_optional_fields;
        {
            let page: DirectoryPage = serde_json::from_value(serde_json::json!({
                "workspaces": [{
                    "workspace": "r", "name": "Coffee", "defaultRole": "editor", "entry": "https://tilo.sh/",
                    "community": true, "hereCount": 12
                }]
            }))
            .unwrap();
            let listed = &page.workspaces[0];
            assert!(listed.community);
            assert_eq!(listed.default_role, Role::Editor);
            assert_eq!(listed.here_count, 12);
            assert!(listed.here.is_empty());
        }
    }
}
