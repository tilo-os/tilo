//! Where a service source's items land: in the app that owns them, through that app's import
//! inbox in the destination's personal workspace, as Thunderbird's address books reach
//! Contacts. Notes reads `personal-data-incoming/pages` and Tasks
//! `personal-data-incoming/issues` when they open, and add what they find through their own
//! actions, keyed by the item's source so nothing lands twice.

use std::io;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// The version of the inbox entries below; an app skips an entry of another version.
pub const INBOX_VERSION: u32 = 1;

/// A picture a page shows, by the BLAKE3 digest its Markdown names it by (`asset:<digest>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    pub digest: String,
    pub bytes: Vec<u8>,
}

/// Who wrote an item on the service: their name there, and their handle (an address or a user
/// name) when the service gives one. Not a member of the destination: they need not have joined.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingAuthor {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
}

/// A group an issue belongs to on the service: a Linear team or project.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingGroup {
    /// The service's id for it.
    pub key: String,
    pub name: String,
}

/// How an issue relates to another one: `blocks`, `blocked-by`, `duplicate-of`, `duplicated-by`
/// or `related`, and the other issue's page.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingRelation {
    pub kind: String,
    pub page: String,
}

/// A page for Notes: an Outline collection or document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncomingPage {
    /// The service it came from: `outline`.
    pub source: String,
    /// The service's id for it, unique within the source.
    pub key: String,
    pub title: String,
    /// Its words, pictures as `asset:<digest>` and links to other pages as `[[title]]`.
    pub markdown: String,
    /// The key of the page it sits under; none for a top-level page.
    pub parent: Option<String>,
    /// Where it is read on the service.
    pub url: String,
    pub pictures: Vec<Picture>,
    /// Who wrote it there, when the service says.
    pub author: Option<IncomingAuthor>,
    /// When it was written and last changed there (RFC 3339); empty when the service does not
    /// say.
    pub created_at: String,
    pub updated_at: String,
}

/// One comment of an issue's thread.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingComment {
    pub author: String,
    /// The author's handle there (an address), when the service gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// When it was written, as the service says it (RFC 3339).
    pub at: String,
    /// When it last changed there (RFC 3339); empty when the service does not say.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub updated_at: String,
    /// Its own page on the service, when it has one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    pub text: String,
}

/// An issue for Tasks, as Linear keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingIssue {
    /// The service it came from: `linear`.
    pub source: String,
    /// The service's id for it.
    pub key: String,
    /// How the service names it to people: `ENG-12`.
    pub identifier: String,
    pub title: String,
    /// Its description, as Markdown.
    pub body: String,
    /// The tracker's status: backlog, todo, in-progress, in-review, done or cancelled.
    pub status: String,
    pub labels: Vec<String>,
    /// `outside:<name>`: who it is assigned to there, who need not be a member here.
    pub assignee: Option<String>,
    pub comments: Vec<IncomingComment>,
    /// The pages of the issues its relations name: how Tasks finds them, in this import or an
    /// earlier one.
    pub links: Vec<String>,
    /// The page of the issue it is a sub-issue of.
    pub parent: Option<String>,
    /// Its page on the service: the issue's source.
    pub url: String,
    /// Who opened it there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<IncomingAuthor>,
    /// When it was opened and last changed there (RFC 3339); empty when the service does not
    /// say.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub created_at: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub updated_at: String,
    /// The team it belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<IncomingGroup>,
    /// The project it works towards, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<IncomingGroup>,
    /// How urgent it is: none, urgent, high, medium or low.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    /// How much work it is, in points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<u32>,
    /// When it is due (`2026-11-20`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    /// How it relates to the other reviewed issues, by their pages; `links` names the same
    /// pages without how.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<IncomingRelation>,
}

/// Where an import's items go.
pub trait Landing {
    /// How many items it takes in one hand-over; the import saves its journal after each.
    fn batch(&self) -> usize {
        1
    }
    /// Hand `pages` to Notes, durably: which of them it has now, and which it refused and why.
    ///
    /// # Errors
    /// It could not be reached; what it answered for before stays kept on its side.
    fn pages(&mut self, pages: &[IncomingPage]) -> io::Result<Delivered>;
    /// Hand `issues` to Tasks, durably: which of them it has now, and which it refused and why.
    ///
    /// # Errors
    /// It could not be reached; what it answered for before stays kept on its side.
    fn issues(&mut self, issues: &[IncomingIssue]) -> io::Result<Delivered>;
}

/// What a landing did with the items handed to it: the keys of those it has now, and those it
/// refused, each with why. An item in neither was not reached and comes again in the next import.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Delivered {
    pub landed: Vec<String>,
    pub refused: Vec<RefusedItem>,
}

/// An item the destination refused: its key, how people know it (`ENG-12`, a page's title), and
/// the destination's reason.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusedItem {
    pub key: String,
    pub name: String,
    pub why: String,
}

#[derive(Serialize)]
struct PageEntry<'a> {
    version: u32,
    source: &'a str,
    key: &'a str,
    title: &'a str,
    markdown: &'a str,
    parent: Option<&'a str>,
    url: &'a str,
    /// Base64 of each picture's bytes.
    pictures: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    author: Option<&'a IncomingAuthor>,
    #[serde(skip_serializing_if = "str::is_empty")]
    created_at: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    updated_at: &'a str,
}

#[derive(Serialize)]
struct IssueEntry<'a> {
    version: u32,
    #[serde(flatten)]
    issue: &'a IncomingIssue,
}

/// The import inboxes of Notes and Tasks in a home's personal workspace.
pub struct InboxLanding {
    home: PathBuf,
}

impl InboxLanding {
    /// The inboxes under `home`: the destination home of the migration.
    #[must_use]
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
        }
    }

    /// The inbox `directory` of the app `app` under this home.
    #[must_use]
    pub fn inbox(&self, app: &str, directory: &str) -> PathBuf {
        self.home
            .join(".local/share/tilo-workspaces/personal")
            .join(app)
            .join(".local/share")
            .join(app)
            .join("personal-data-incoming")
            .join(directory)
    }

    fn write(
        &self,
        app: &str,
        directory: &str,
        source: &str,
        key: &str,
        bytes: &[u8],
    ) -> io::Result<()> {
        let root = self.inbox(app, directory);
        let digest = blake3::hash(key.as_bytes()).to_hex();
        let name = format!("{source}-{}.json", digest.get(..32).unwrap_or_default());
        tilo_files::native::atomic::write_private(&root.join(name), bytes)
    }
}

impl Landing for InboxLanding {
    fn pages(&mut self, pages: &[IncomingPage]) -> io::Result<Delivered> {
        let mut delivered = Delivered::default();
        for page in pages {
            self.page(page)?;
            delivered.landed.push(page.key.clone());
        }
        Ok(delivered)
    }

    fn issues(&mut self, issues: &[IncomingIssue]) -> io::Result<Delivered> {
        let mut delivered = Delivered::default();
        for issue in issues {
            self.issue(issue)?;
            delivered.landed.push(issue.key.clone());
        }
        Ok(delivered)
    }
}

impl InboxLanding {
    fn page(&self, page: &IncomingPage) -> io::Result<()> {
        let engine = base64::engine::general_purpose::STANDARD;
        let entry = PageEntry {
            version: INBOX_VERSION,
            source: &page.source,
            key: &page.key,
            title: &page.title,
            markdown: &page.markdown,
            parent: page.parent.as_deref(),
            url: &page.url,
            pictures: page
                .pictures
                .iter()
                .map(|picture| engine.encode(&picture.bytes))
                .collect(),
            author: page.author.as_ref(),
            created_at: &page.created_at,
            updated_at: &page.updated_at,
        };
        let bytes = serde_json::to_vec(&entry).map_err(io::Error::other)?;
        self.write("so.tilo.notes", "pages", &page.source, &page.key, &bytes)
    }

    fn issue(&self, issue: &IncomingIssue) -> io::Result<()> {
        let bytes = serde_json::to_vec(&IssueEntry {
            version: INBOX_VERSION,
            issue,
        })
        .map_err(io::Error::other)?;
        self.write("so.tilo.tasks", "issues", &issue.source, &issue.key, &bytes)
    }
}
