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
}

/// One comment of an issue's thread.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingComment {
    pub author: String,
    /// When it was written, as the service says it (RFC 3339).
    pub at: String,
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
    /// `person:<name>`.
    pub assignee: Option<String>,
    pub comments: Vec<IncomingComment>,
    /// The pages of the issues its relations name: how Tasks finds them, in this run or an
    /// earlier one.
    pub links: Vec<String>,
    /// The page of the issue it is a sub-issue of.
    pub parent: Option<String>,
    /// Its page on the service: the issue's source.
    pub url: String,
}

/// Where a run's items go.
pub trait Landing {
    /// Hand `page` to Notes, durably.
    ///
    /// # Errors
    /// It could not be written.
    fn page(&mut self, page: &IncomingPage) -> io::Result<()>;
    /// Hand `issue` to Tasks, durably.
    ///
    /// # Errors
    /// It could not be written.
    fn issue(&mut self, issue: &IncomingIssue) -> io::Result<()>;
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
        std::fs::create_dir_all(&root)?;
        let digest = blake3::hash(key.as_bytes()).to_hex();
        let name = format!("{source}-{}.json", digest.get(..32).unwrap_or_default());
        write_private(&root.join(name), bytes)
    }
}

impl Landing for InboxLanding {
    fn page(&mut self, page: &IncomingPage) -> io::Result<()> {
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
        };
        let bytes = serde_json::to_vec(&entry).map_err(io::Error::other)?;
        self.write("so.tilo.notes", "pages", &page.source, &page.key, &bytes)
    }

    fn issue(&mut self, issue: &IncomingIssue) -> io::Result<()> {
        let bytes = serde_json::to_vec(&IssueEntry {
            version: INBOX_VERSION,
            issue,
        })
        .map_err(io::Error::other)?;
        self.write("so.tilo.tasks", "issues", &issue.source, &issue.key, &bytes)
    }
}

/// Write `bytes` to `path` owner-only, synced, replacing it whole.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt as _;
    let temporary = path.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options.open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)?;
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}
