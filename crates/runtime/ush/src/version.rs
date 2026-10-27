//! `version`: the git-shaped verbs an agent already knows, over the history
//! this filesystem already keeps.
//!
//! Every file on a Tilo mount carries its own immutable history: each write
//! appends a version, and the mounted file answers that history as
//! bounded attributes (`fs_protocol::files::FileHistoryReadQueryV1` for the
//! versions and their contents, `CHECKED_HISTORY_*` for a checked restore).
//! The History view in the file manager reads exactly those, and
//! [`apps/file-explorer/src/framework_history.rs`] is the same transport this
//! module speaks. So `version log` and the History view cannot disagree about
//! what a revision is: there is one list, one set of ids, and one restore.
//!
//! # Why this exists
//!
//! An agent asked to change a document reaches for `git`, and `git` wants a
//! repository: an initialised `.git`, a working tree it owns, an index to add
//! to and a commit graph beside the thing it is versioning. None of that is
//! true here. A Tilo file is already versioned, and the person it belongs to
//! can see every version in the History view whether or not anybody committed
//! anything. So the verbs are here, and the store is not:
//!
//! * `status` — whether a file is versioned, and the revision it is at.
//! * `log` — the revisions of one file, oldest first, each with its id, its
//!   parents, its author and its time.
//! * `show` — one revision's bytes.
//! * `diff` — what changed between two revisions, as `+`/`-` lines.
//! * `restore` — put one revision's contents back as a new revision, through
//!   the same checked-restore path the History view uses.
//!
//! Git verbs that are deliberately absent, and why:
//!
//! * `commit`. Writing the file *is* the version: the filesystem appends one on
//!   every write, so a caller that had to `commit` afterwards would be able to
//!   forget, and the person's History view would disagree with the log. A
//!   commit that changed nothing would also have nowhere to put a message — a
//!   [`FileVersionV1`] carries an actor and a time, not prose. Named
//!   checkpoints with descriptions are a manifest over revisions, not a
//!   version, and belong to that.
//! * `branch` and `merge`. Two people editing one Tilo document at once is the
//!   CRDT's own collaboration, which merges without a verb; a `branch` that
//!   really copied a file would be two records, not two lines of one history.
//! * `init`. The mount is the repository.
//!
//! Naming them here would promise behaviour this command does not have.
//!
//! # Machine-readable first
//!
//! Every verb takes `--json` and prints one JSON object. Errors are JSON too
//! on `--json`, with a stable `error` code, so a caller reads a result rather
//! than parsing prose. Exit status is 0 for success and 1 for any refusal,
//! the same as the other file verbs.
//!
//! # What can fail, and what that means
//!
//! A file with no history (never versioned, or a host without the projection)
//! is `no-history`, not an empty log, because "nothing yet" and "nothing ever"
//! are different answers. A stale cursor is reported as `stale` so a caller
//! re-reads instead of acting on a page the frontier has moved past.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs_protocol::files::{
    CHECKED_HISTORY_RESTORE_XATTR, CHECKED_HISTORY_RESULT_XATTR_PREFIX,
    CHECKED_HISTORY_REVIEW_XATTR, CheckedHistoryOperationV1, CheckedHistoryOutcomeV1,
    CheckedHistoryRequestV1, CheckedHistoryResponseV1, CheckedHistoryReviewResponseV1,
    FileHistoryPageV1, FileHistoryReadErrorV1, FileHistoryReadQueryV1, FileHistoryReadResponseV1,
    FileHistoryReadValueV1, FileProvenance, FileRestoreReviewV1, FileVersionId, FileVersionV1,
    MAX_CHECKED_HISTORY_REQUEST_BYTES, MAX_HISTORY_PREVIEW_BYTES, MAX_HISTORY_READ_BYTES,
};
use serde::Serialize;

/// How long a restore waits for the filesystem to download the revision's
/// content before reporting the intent as still pending. The same wait the
/// History view uses, so both give up at the same point.
pub const HYDRATION_WAIT: Duration = Duration::from_secs(10 * 60);

/// A refusal, named so a caller can act on the name instead of the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
    /// The file's history could not be read at all: no projection on this
    /// host, or a path that is not a file on a Tilo mount.
    NoHistory,
    /// The versioned file changed under a cursor the caller was following.
    Stale,
    /// No such revision in this file's history.
    NoSuchRevision,
    /// The revision exists but carries no content — a deletion, or a version
    /// whose bytes are not on this device and could not be fetched.
    NoContent,
    /// The filesystem refused: not visible, read-only, or mid-write.
    Refused,
    /// The reviewed file no longer matches the review a restore was built
    /// from, so the restore would not be the one that was asked for.
    ReviewRequired,
    /// The revision's content is still downloading on this device. Ask again
    /// with the same intent.
    RecoveryPending,
    /// The caller's own arguments did not name one revision, one file, or one
    /// thing to do.
    Usage,
}

impl ErrorCode {
    /// The wording shown to a person for each refusal.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::NoHistory => "this file has no version history here",
            Self::Stale => "this file changed since the revision was read",
            Self::NoSuchRevision => "no such revision of this file",
            Self::NoContent => "this revision has no content",
            Self::Refused => "the filesystem refused this request",
            Self::ReviewRequired => "this file changed; ask for its history again",
            Self::RecoveryPending => "this revision is still downloading; try again",
            Self::Usage => "usage error",
        }
    }
}

/// One thing a `version` line asked for, before anything is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Whether each path is versioned, and the revision it is at.
    Status { paths: Vec<PathBuf> },
    /// Every revision of one file, oldest first.
    Log { path: PathBuf },
    /// One revision's bytes.
    Show { path: PathBuf, revision: Revision },
    /// What changed between two revisions.
    Diff {
        path: PathBuf,
        from: Revision,
        to: Revision,
    },
    /// Put one revision's contents back as a new revision.
    /// Put one revision's contents back as a new revision.
    Restore { path: PathBuf, revision: Revision },
}

/// Which revision a `show`, `diff` or `restore` names.
///
/// Ids are what the History view shows, so they are what a caller uses. A
/// prefix is accepted the way git accepts one, because an id read off a
/// screen is easy to truncate; it must name exactly one revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Revision {
    /// An exact revision id.
    Id(FileVersionId),
    /// A shortest-unique prefix of a revision id, as read off `version log`.
    Prefix(String),
    /// The head the file is at now.
    Head,
}

impl Revision {
    /// Read one revision name off a command line.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        if text == "HEAD" || text == "head" {
            return Self::Head;
        }
        // A full uuid is exact; anything else is a prefix to match.
        match uuid::Uuid::parse_str(text) {
            Ok(id) if !id.is_nil() => Self::Id(FileVersionId(id)),
            _ => Self::Prefix(text.to_ascii_lowercase()),
        }
    }
}

/// Resolve `revision` against `page`, which holds the versions of one file.
///
/// # Errors
///
/// [`ErrorCode::NoSuchRevision`] when the name matches nothing, and
/// [`ErrorCode::Usage`] when a prefix matches more than one revision, because
/// guessing between two is worse than making the caller say which.
pub fn resolve(revision: &Revision, page: &[FileVersionV1]) -> Result<FileVersionId, ErrorCode> {
    match revision {
        Revision::Id(id) => page
            .iter()
            .any(|version| version.id == *id)
            .then_some(*id)
            .ok_or(ErrorCode::NoSuchRevision),
        // `HEAD` is the newest version the file has, which is the last one a
        // page lists.
        Revision::Head => page
            .last()
            .map(|version| version.id)
            .ok_or(ErrorCode::NoSuchRevision),
        Revision::Prefix(prefix) => {
            if prefix.is_empty() {
                return Err(ErrorCode::Usage);
            }
            let matches: Vec<_> = page
                .iter()
                .map(|version| version.id)
                .filter(|id| id.0.simple().to_string().starts_with(prefix.as_str()))
                .collect();
            match matches.as_slice() {
                [only] => Ok(*only),
                [] => Err(ErrorCode::NoSuchRevision),
                _ => Err(ErrorCode::Usage),
            }
        }
    }
}

/// Every revision of one file, oldest first, with the agents the mount
/// attributed to them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    pub versions: Vec<FileVersionV1>,
    pub attributions: Vec<fs_protocol::FileVersionAttributionV1>,
}

impl History {
    /// The lines `log` prints for these revisions.
    #[must_use]
    pub fn lines(&self) -> Vec<RevisionLine> {
        self.versions
            .iter()
            .map(|version| RevisionLine::of(version, &self.attributions))
            .collect()
    }
}

/// Read the history of one mounted file, the way
/// [`apps/file-explorer/src/framework_history.rs`] does.
///
/// A descriptor is held for the whole command so that a later replacement of
/// the path cannot retarget a queued restore: the history a caller reviewed
/// and the file a restore writes are the same kernel object.
pub struct MountedFile {
    file: File,
    path: PathBuf,
}

impl MountedFile {
    /// Open `path` for a history read or restore.
    ///
    /// # Errors
    ///
    /// The I/O error from opening `path`.
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            file: File::open(path)?,
            path: path.to_path_buf(),
        })
    }

    /// The path this descriptor was opened on, for messages.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read one attribute of this file. An attribute the mount does not set
    /// is `None`; an attribute it cannot answer at all is `no-history`, which
    /// is what a path without the projection looks like.
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>, ErrorCode> {
        host_fs::attributes::get_open(&self.file, name).map_err(|_| ErrorCode::NoHistory)
    }

    /// Every revision of the file, oldest first, read by following the
    /// page cursor to its end.
    ///
    /// A single page holds at most [`MAX_HISTORY_PAGE_VERSIONS`] versions, and
    /// a real document has more, so a caller that stopped at page one would
    /// silently see a truncated log.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NoHistory`] when the mount does not answer, anything but
    /// a page from a well-formed response, and [`ErrorCode::Stale`] when the
    /// history moved while it was being paged.
    pub fn log(&self) -> Result<History, ErrorCode> {
        let mut versions = Vec::new();
        let mut attributions = Vec::new();
        let mut cursor = None;
        loop {
            let value = self.read(FileHistoryReadQueryV1::Page { cursor })?;
            let FileHistoryReadValueV1::Page(page) = value else {
                return Err(ErrorCode::NoHistory);
            };
            let next = page.next_after;
            // The cursor is only valid against the frontier the page was
            // issued at, so carry this page's own rather than re-reading one
            // that may have moved since.
            let frontier = page.scope.frontier;
            versions.extend(page.versions);
            attributions.extend(page.attributions);
            match next {
                Some(after) => cursor = Some((frontier, after)),
                None => {
                    return Ok(History {
                        versions,
                        attributions,
                    });
                }
            }
        }
    }

    /// The page for one cursor. `None` is the first page, whose scope carries
    /// the frontier a later read or restore is checked against.
    ///
    /// # Errors
    ///
    /// As [`Self::log`].
    pub fn page(
        &self,
        cursor: Option<([u8; 32], FileVersionId)>,
    ) -> Result<FileHistoryPageV1, ErrorCode> {
        match self.read(FileHistoryReadQueryV1::Page { cursor })? {
            FileHistoryReadValueV1::Page(page) => Ok(page),
            FileHistoryReadValueV1::Version { .. } => Err(ErrorCode::NoHistory),
        }
    }

    /// The frontier the file is at now. A latest page carries it in its scope
    /// without needing a version.
    ///
    /// # Errors
    ///
    /// As [`Self::log`].
    pub fn frontier(&self) -> Result<[u8; 32], ErrorCode> {
        match self.read(FileHistoryReadQueryV1::Page { cursor: None })? {
            FileHistoryReadValueV1::Page(page) => Ok(page.scope.frontier),
            FileHistoryReadValueV1::Version { scope, .. } => Ok(scope.frontier),
        }
    }

    /// The bytes of one revision, verified against its recorded hash by the
    /// filesystem before it is returned.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NoContent`] for a revision that carries none, and the
    /// refusal the mount gave for the rest.
    pub fn content(&self, version: FileVersionId) -> Result<Vec<u8>, ErrorCode> {
        let bytes = self.get(&preview_query(self.frontier()?, version).xattr_name())?;
        let bytes = bytes.ok_or(ErrorCode::NoContent)?;
        if bytes.len() > MAX_HISTORY_PREVIEW_BYTES {
            return Err(ErrorCode::NoContent);
        }
        Ok(bytes)
    }

    /// Read one query, turning the mount's answers into named refusals.
    ///
    /// # Errors
    ///
    /// The refusals [`ErrorCode`] names.
    pub fn read(&self, query: FileHistoryReadQueryV1) -> Result<FileHistoryReadValueV1, ErrorCode> {
        let bytes = self.get(&query.xattr_name())?.ok_or(ErrorCode::NoHistory)?;
        if bytes.len() > MAX_HISTORY_READ_BYTES {
            return Err(ErrorCode::NoHistory);
        }
        let response: FileHistoryReadResponseV1 =
            serde_json::from_slice(&bytes).map_err(|_| ErrorCode::NoHistory)?;
        if response.schema_version != 1 {
            return Err(ErrorCode::NoHistory);
        }
        response.result.map_err(read_error)
    }

    /// Ask the filesystem to make `version` the file's contents again, as a
    /// new revision, through the checked-restore path.
    ///
    /// The review is taken immediately before the request so that the frontier
    /// the restore is checked against is the one the caller just read, and a
    /// file that changed in between is refused rather than silently restored
    /// over somebody else's edit.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::ReviewRequired`] when the file moved since the review,
    /// [`ErrorCode::RecoveryPending`] while the content downloads, and the
    /// refusal the mount gave for the rest.
    pub fn restore(&self, version: FileVersionId) -> Result<CheckedHistoryOutcomeV1, ErrorCode> {
        let review = self.review()?;
        let request = CheckedHistoryRequestV1 {
            schema_version: 1,
            intent_id: uuid::Uuid::new_v4(),
            review,
            operation: CheckedHistoryOperationV1::Restore {
                version_id: version,
            },
        };
        if request.validate().is_err() {
            return Err(ErrorCode::Usage);
        }
        let bytes = serde_json::to_vec(&request).map_err(|_| ErrorCode::Usage)?;
        if bytes.len() > MAX_CHECKED_HISTORY_REQUEST_BYTES {
            return Err(ErrorCode::Usage);
        }
        match write_until_hydrated(HYDRATION_WAIT, || {
            host_fs::attributes::set_open(&self.file, CHECKED_HISTORY_RESTORE_XATTR, &bytes)
        }) {
            Ok(()) => Ok(self.outcome(request.intent_id)?),
            // Already the file's content: nothing to do is a success.
            Err(error) if error_code(&error) == Some(host_fs::attributes::ErrorCode::Already) => {
                Ok(CheckedHistoryOutcomeV1::Unchanged {
                    intent_id: request.intent_id,
                    review: request.review,
                })
            }
            Err(error)
                if error_code(&error) == Some(host_fs::attributes::ErrorCode::InProgress) =>
            {
                Ok(CheckedHistoryOutcomeV1::RecoveryPending {
                    intent_id: request.intent_id,
                })
            }
            Err(error) => Err(outcome_error(&error)),
        }
    }

    fn review(&self) -> Result<FileRestoreReviewV1, ErrorCode> {
        let bytes = self
            .get(CHECKED_HISTORY_REVIEW_XATTR)?
            .ok_or(ErrorCode::NoHistory)?;
        if bytes.len() > MAX_HISTORY_READ_BYTES {
            return Err(ErrorCode::NoHistory);
        }
        let response: CheckedHistoryReviewResponseV1 =
            serde_json::from_slice(&bytes).map_err(|_| ErrorCode::NoHistory)?;
        if response.schema_version != 1 {
            return Err(ErrorCode::NoHistory);
        }
        let review = response
            .result
            .map_err(|refused| outcome_refusal(&refused))?;
        review.validate().map_err(|_| ErrorCode::Usage)?;
        Ok(review)
    }

    fn outcome(&self, intent: uuid::Uuid) -> Result<CheckedHistoryOutcomeV1, ErrorCode> {
        let name = format!("{CHECKED_HISTORY_RESULT_XATTR_PREFIX}{intent}");
        let bytes = self.get(&name)?.ok_or(ErrorCode::NoHistory)?;
        if bytes.len() > MAX_HISTORY_READ_BYTES {
            return Err(ErrorCode::NoHistory);
        }
        let response: CheckedHistoryResponseV1 =
            serde_json::from_slice(&bytes).map_err(|_| ErrorCode::NoHistory)?;
        if response.schema_version != 1 {
            return Err(ErrorCode::NoHistory);
        }
        Ok(response.outcome)
    }
}

fn preview_query(frontier: [u8; 32], version: FileVersionId) -> FileHistoryReadQueryV1 {
    FileHistoryReadQueryV1::Preview {
        frontier,
        version_id: version,
    }
}

fn error_code(error: &io::Error) -> Option<host_fs::attributes::ErrorCode> {
    host_fs::attributes::ErrorCode::of(error)
}

const fn read_error(error: FileHistoryReadErrorV1) -> ErrorCode {
    match error {
        FileHistoryReadErrorV1::Stale => ErrorCode::Stale,
        FileHistoryReadErrorV1::MissingVersion => ErrorCode::NoSuchRevision,
        FileHistoryReadErrorV1::NoContent => ErrorCode::NoContent,
        FileHistoryReadErrorV1::NotVisible
        | FileHistoryReadErrorV1::InvalidRequest
        | FileHistoryReadErrorV1::Unavailable
        | FileHistoryReadErrorV1::TooLarge => ErrorCode::Refused,
    }
}

fn outcome_refusal(outcome: &CheckedHistoryOutcomeV1) -> ErrorCode {
    match outcome {
        CheckedHistoryOutcomeV1::NotVisible
        | CheckedHistoryOutcomeV1::ReadOnly
        | CheckedHistoryOutcomeV1::PendingWrites
        | CheckedHistoryOutcomeV1::UnsupportedOwner { .. }
        | CheckedHistoryOutcomeV1::Unavailable => ErrorCode::Refused,
        CheckedHistoryOutcomeV1::InvalidRequest => ErrorCode::Usage,
        CheckedHistoryOutcomeV1::ReviewRequired => ErrorCode::ReviewRequired,
        CheckedHistoryOutcomeV1::RecoveryPending { .. } => ErrorCode::RecoveryPending,
        // A review this command built is never a completed restore.
        CheckedHistoryOutcomeV1::Applied { .. } | CheckedHistoryOutcomeV1::Unchanged { .. } => {
            ErrorCode::Refused
        }
    }
}

fn outcome_error(error: &io::Error) -> ErrorCode {
    match error_code(error) {
        Some(host_fs::attributes::ErrorCode::Busy) => ErrorCode::ReviewRequired,
        Some(host_fs::attributes::ErrorCode::InProgress) => ErrorCode::RecoveryPending,
        Some(host_fs::attributes::ErrorCode::InvalidArgument) => ErrorCode::Usage,
        _ => ErrorCode::Refused,
    }
}

/// Write a history request, repeating exactly it while the filesystem answers
/// that the content it needs is still downloading.
///
/// The same rule the History view follows: the filesystem downloads off its
/// request path, so repeating the same idempotent request commits once the
/// bytes are local, and after `wait` the last answer is the answer.
fn write_until_hydrated(
    wait: Duration,
    mut write: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    let start = Instant::now();
    let mut pause = Duration::from_millis(20);
    loop {
        match write() {
            Err(error)
                if error_code(&error) == Some(host_fs::attributes::ErrorCode::InProgress)
                    && start.elapsed() < wait =>
            {
                let left = wait.saturating_sub(start.elapsed());
                std::thread::sleep(pause.min(left));
                pause = pause.saturating_mul(2).min(Duration::from_millis(500));
            }
            result => return result,
        }
    }
}

/// One `+`/`-`/` ` line of a diff between two revisions of a text file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffLine {
    /// `+`, `-` or a space for context.
    pub op: char,
    /// The line, without its terminator.
    pub text: String,
    /// The line's number in the old revision, when it is in it.
    pub old_line: Option<usize>,
    /// The line's number in the new revision, when it is in it.
    pub new_line: Option<usize>,
}

/// A whole file's worth of change, with counts for a header line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Diff {
    pub lines: Vec<DiffLine>,
    pub added: usize,
    pub removed: usize,
}

/// Diff two texts line by line.
///
/// The alignment is [`tilo_resource::diff`]'s, the same one a document uses.
/// Two lines are the same item when the text is the same, so an edit that
/// leaves a repeated line in place pairs the copies in order instead of
/// pairing every copy with the first.
#[must_use]
pub fn diff_text(old: &str, new: &str) -> Diff {
    let old_lines = split_lines(old);
    let new_lines = split_lines(new);
    let steps = tilo_resource::diff::align(&old_lines, &new_lines);

    let mut diff = Diff::default();
    let (mut seen_old, mut seen_new) = (0usize, 0usize);
    for step in steps {
        // Each line's number is how many lines of its own side precede it,
        // plus one: a removed line still counts on the old side and an added
        // one on the new, so numbers name the revision the line came from.
        let old_no = seen_old.saturating_add(1);
        let new_no = seen_new.saturating_add(1);
        let (op, line, old_line, new_line) = match step {
            tilo_resource::diff::Step::Both(line) => {
                seen_old = old_no;
                seen_new = new_no;
                (' ', line, Some(old_no), Some(new_no))
            }
            tilo_resource::diff::Step::OldOnly(line) => {
                seen_old = old_no;
                diff.removed = diff.removed.saturating_add(1);
                ('-', line, Some(old_no), None)
            }
            tilo_resource::diff::Step::NewOnly(line) => {
                seen_new = new_no;
                diff.added = diff.added.saturating_add(1);
                ('+', line, None, Some(new_no))
            }
        };
        diff.lines.push(DiffLine {
            op,
            text: line.to_string(),
            old_line,
            new_line,
        });
    }
    diff
}

/// A text's lines without their terminators. A trailing newline is a
/// terminator, not an empty last line, so it does not show up as a change.
fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let body = text.strip_suffix('\n').unwrap_or(text);
    body.split('\n').collect()
}

/// Whether `bytes` is text this command can diff: valid UTF-8 with no NUL.
/// A binary revision is shown by `show`, never diffed into nonsense.
#[must_use]
pub fn is_text(bytes: &[u8]) -> bool {
    !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

/// What a `version` line asked for, and how it wants the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub command: Command,
    /// Print one JSON object instead of lines meant for a person.
    pub json: bool,
}

/// The help text, as `version --help` prints it and as `version` with no
/// verb prints it.
pub const USAGE: &str = "\
version — the history Tilo already keeps, with the verbs you know.

  version status [PATH…]              is this file versioned, and at which revision
  version log PATH                    every revision, oldest first
  version show PATH REVISION          one revision's contents
  version diff PATH REVISION [REVISION]   what changed, the second to the first
  version restore PATH REVISION       put one revision back as a new revision

Every verb takes --json. A REVISION is a revision id from `version log`, an
unambiguous prefix of one, or HEAD for the newest.";

/// Read `args` (the words after `version`) as one invocation.
///
/// # Errors
///
/// [`ErrorCode::Usage`] for an unknown verb, a missing operand, an extra one,
/// or a flag this command does not have.
pub fn parse(args: &[String]) -> Result<Option<Invocation>, ErrorCode> {
    let json = args.iter().any(|arg| arg == "--json");
    let mut words: Vec<&String> = args.iter().filter(|arg| *arg != "--json").collect();
    // A lone `--` asks for nothing more; anything shaped like a flag is
    // refused rather than read as a path, so a typo cannot become a filename.
    if let Some(flag) = words.iter().find(|word| word.starts_with('-')) {
        let _ = flag;
        return Err(ErrorCode::Usage);
    }
    if words.is_empty() {
        return Ok(None);
    }
    let verb = words.remove(0).clone();
    let path = |words: &mut Vec<&String>| -> Result<PathBuf, ErrorCode> {
        let first = words.first().ok_or(ErrorCode::Usage)?;
        let path = PathBuf::from(first.as_str());
        words.remove(0);
        Ok(path)
    };
    let revision = |words: &mut Vec<&String>| -> Result<Revision, ErrorCode> {
        let first = words.first().ok_or(ErrorCode::Usage)?;
        let revision = Revision::parse(first);
        words.remove(0);
        Ok(revision)
    };
    let command = match verb.as_str() {
        "status" => {
            let paths = words
                .iter()
                .map(|word| PathBuf::from(word.as_str()))
                .collect();
            // Every word was a path, so there is nothing left over to refuse.
            words.clear();
            Command::Status { paths }
        }
        "log" => Command::Log {
            path: path(&mut words)?,
        },
        "show" => Command::Show {
            path: path(&mut words)?,
            revision: revision(&mut words)?,
        },
        // Two revisions read as `from` then `to`; one reads as `from` to the
        // file's head, which is the common question ("what have I changed").
        "diff" => {
            let path = path(&mut words)?;
            let from = revision(&mut words)?;
            let to = if words.is_empty() {
                Revision::Head
            } else {
                revision(&mut words)?
            };
            Command::Diff { path, from, to }
        }
        "restore" => Command::Restore {
            path: path(&mut words)?,
            revision: revision(&mut words)?,
        },
        _ => return Err(ErrorCode::Usage),
    };
    if !words.is_empty() {
        return Err(ErrorCode::Usage);
    }
    Ok(Some(Invocation { command, json }))
}

/// Run `args` and return the process exit code: 0 for an answer, 1 for a
/// refusal. The refusal is printed before this returns, in the caller's
/// chosen shape.
#[must_use]
pub fn run(args: &[String]) -> i32 {
    let json = args.iter().any(|arg| arg == "--json");
    let result = parse(args).and_then(|parsed| {
        if let Some(invocation) = parsed {
            return execute(&invocation);
        }
        // No verb: the usage is the answer, and it is not a failure.
        println!("{USAGE}");
        Ok(())
    });
    match result {
        Ok(()) => 0,
        Err(error) => {
            refuse(error, json);
            1
        }
    }
}

/// Print one refusal, as JSON when the caller asked for JSON so that a
/// machine reads a code rather than a sentence.
fn refuse(error: ErrorCode, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({"error": error, "message": error.message()})
        );
    } else {
        eprintln!("version: {}", error.message());
    }
}

/// One version of one file, as `log` and `status` report it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RevisionLine {
    pub id: String,
    pub parents: Vec<String>,
    pub time_ms: u64,
    /// The change this revision records, in words a person reads.
    pub change: String,
    /// The agent behind it, when an agent made it, as the person it acted for.
    pub agent: Option<String>,
    /// The revision a restore took its content from, for a restore.
    pub restored_from: Option<String>,
}

impl RevisionLine {
    /// One revision as a line, with the agents the page attributed to it.
    ///
    /// The attribution is the page's own, which the filesystem took from the
    /// version's signed record: a version is attributed only when its own
    /// signing device recorded the agent, never because a reader guessed.
    fn of(version: &FileVersionV1, attributions: &[fs_protocol::FileVersionAttributionV1]) -> Self {
        let (change, restored_from) = change_of(&version.provenance);
        Self {
            id: version.id.0.simple().to_string(),
            parents: version
                .parents
                .iter()
                .map(|parent| parent.0.simple().to_string())
                .collect(),
            time_ms: version.timestamp_ms,
            change,
            agent: attributions
                .iter()
                .find(|attributed| attributed.version_id == version.id)
                .map(|attributed| attributed.attribution.label.clone()),
            restored_from,
        }
    }
}

/// What a version records, and for a restore, the revision it came from.
fn change_of(provenance: &FileProvenance) -> (String, Option<String>) {
    match provenance {
        FileProvenance::Create => ("created".into(), None),
        FileProvenance::Edit => ("edited".into(), None),
        FileProvenance::Rename => ("renamed".into(), None),
        FileProvenance::Move => ("moved".into(), None),
        FileProvenance::Delete => ("deleted".into(), None),
        FileProvenance::Restore { from } => ("restored".into(), Some(from.0.simple().to_string())),
        FileProvenance::ConflictResolution => ("conflict resolved".into(), None),
    }
}

/// Carry out one invocation, printing the answer.
///
/// # Errors
///
/// Whatever the mount refused, named as an [`ErrorCode`].
fn execute(invocation: &Invocation) -> Result<(), ErrorCode> {
    match &invocation.command {
        Command::Status { paths } => status(paths, invocation.json),
        Command::Log { path } => log(path, invocation.json),
        Command::Show { path, revision } => show(path, revision, invocation.json),
        Command::Diff { path, from, to } => diff(path, from, to, invocation.json),
        Command::Restore { path, revision } => restore(path, revision, invocation.json),
    }
}

/// `version status`: for each path, whether it is versioned and where it is.
fn status(paths: &[PathBuf], json: bool) -> Result<(), ErrorCode> {
    let paths: Vec<PathBuf> = if paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        paths.to_vec()
    };
    let mut rows = Vec::new();
    for path in &paths {
        let row = match MountedFile::open(path) {
            // A file the mount cannot describe has no history here; that is
            // an answer about this path, not a failure of the whole command.
            Ok(file) => match file.log() {
                Ok(history) => StatusRow {
                    path: path.display().to_string(),
                    versioned: true,
                    revisions: history.versions.len(),
                    head: history.lines().last().map(|line| line.id.clone()),
                    at: history.lines().last().map(|line| line.change.clone()),
                },
                Err(_) => StatusRow::unversioned(path),
            },
            Err(_) => StatusRow::unversioned(path),
        };
        rows.push(row);
    }
    if json {
        print_json(&rows)?;
        return Ok(());
    }
    for row in &rows {
        match (&row.head, &row.at) {
            (Some(head), Some(at)) => println!(
                "{}  {} ({} revision{}), last {at}",
                short(head),
                row.path,
                row.revisions,
                if row.revisions == 1 { "" } else { "s" }
            ),
            _ => println!("{}  no version history here", row.path),
        }
    }
    Ok(())
}

/// One line of `status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusRow {
    pub path: String,
    pub versioned: bool,
    pub revisions: usize,
    pub head: Option<String>,
    pub at: Option<String>,
}

impl StatusRow {
    fn unversioned(path: &Path) -> Self {
        Self {
            path: path.display().to_string(),
            versioned: false,
            revisions: 0,
            head: None,
            at: None,
        }
    }
}

/// `version log`: every revision of one file.
fn log(path: &Path, json: bool) -> Result<(), ErrorCode> {
    let file = MountedFile::open(path).map_err(|_| ErrorCode::NoHistory)?;
    let history = file.log()?;
    let lines = history.lines();
    if json {
        print_json(&lines)?;
        return Ok(());
    }
    if lines.is_empty() {
        println!("{} has no revisions", path.display());
        return Ok(());
    }
    for line in &lines {
        let agent = match &line.agent {
            Some(agent) => format!(", by {agent}"),
            None => String::new(),
        };
        match &line.restored_from {
            Some(from) => println!(
                "{}  {}  {}{agent}, content from {}",
                short(&line.id),
                line.time_ms,
                line.change,
                short(from)
            ),
            None => println!(
                "{}  {}  {}{agent}",
                short(&line.id),
                line.time_ms,
                line.change
            ),
        }
    }
    Ok(())
}

/// `version show`: one revision's contents, byte for byte.
fn show(path: &Path, revision: &Revision, json: bool) -> Result<(), ErrorCode> {
    let file = MountedFile::open(path).map_err(|_| ErrorCode::NoHistory)?;
    let history = file.log()?;
    let id = resolve(revision, &history.versions)?;
    let bytes = file.content(id)?;
    if json {
        // Bytes that are not text are reported as their length rather than
        // as a string that would be mangled by the encoding.
        let value = if is_text(&bytes) {
            serde_json::json!({
                "path": path.display().to_string(),
                "revision": id.0.simple().to_string(),
                "text": String::from_utf8_lossy(&bytes),
            })
        } else {
            serde_json::json!({
                "path": path.display().to_string(),
                "revision": id.0.simple().to_string(),
                "bytes": bytes.len(),
            })
        };
        println!("{value}");
        return Ok(());
    }
    if !is_text(&bytes) {
        eprintln!(
            "version: this revision is {} bytes and not text",
            bytes.len()
        );
        return Ok(());
    }
    // Straight through, so a caller can pipe it: no added newline.
    print!("{}", String::from_utf8_lossy(&bytes));
    Ok(())
}

/// `version diff`: what changed between two revisions.
fn diff(path: &Path, from: &Revision, to: &Revision, json: bool) -> Result<(), ErrorCode> {
    let file = MountedFile::open(path).map_err(|_| ErrorCode::NoHistory)?;
    let history = file.log()?;
    let from = resolve(from, &history.versions)?;
    let to = resolve(to, &history.versions)?;
    let before = file.content(from)?;
    let after = file.content(to)?;
    // A revision with no text has no lines to compare; saying so is better
    // than inventing a diff of replacement characters.
    if !is_text(&before) || !is_text(&after) {
        return Err(ErrorCode::NoContent);
    }
    let result = diff_text(
        &String::from_utf8_lossy(&before),
        &String::from_utf8_lossy(&after),
    );
    if json {
        println!(
            "{}",
            serde_json::json!({
                "path": path.display().to_string(),
                "from": from.0.simple().to_string(),
                "to": to.0.simple().to_string(),
                "added": result.added,
                "removed": result.removed,
                "lines": result.lines,
            })
        );
        return Ok(());
    }
    if result.lines.is_empty() {
        println!("these revisions have the same contents");
        return Ok(());
    }
    for line in &result.lines {
        println!("{}{}", line.op, line.text);
    }
    println!("\n{} added, {} removed", result.added, result.removed);
    Ok(())
}

/// `version restore`: put one revision's contents back, as a new revision.
fn restore(path: &Path, revision: &Revision, json: bool) -> Result<(), ErrorCode> {
    let file = MountedFile::open(path).map_err(|_| ErrorCode::NoHistory)?;
    let history = file.log()?;
    let id = resolve(revision, &history.versions)?;
    let outcome = file.restore(id)?;
    match outcome {
        CheckedHistoryOutcomeV1::Applied { receipt } => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "path": path.display().to_string(),
                        "restored": id.0.simple().to_string(),
                        "revision": receipt.restored_revision.version_id.0.simple().to_string(),
                    })
                );
            } else {
                println!(
                    "{} is back to {} as a new revision",
                    path.display(),
                    short(&id.0.simple().to_string())
                );
            }
            Ok(())
        }
        // The file already had these contents: nothing was written, which is
        // the answer the caller wanted either way.
        CheckedHistoryOutcomeV1::Unchanged { .. } => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "path": path.display().to_string(),
                        "restored": id.0.simple().to_string(),
                        "unchanged": true,
                    })
                );
            } else {
                println!("{} already had these contents", path.display());
            }
            Ok(())
        }
        CheckedHistoryOutcomeV1::RecoveryPending { .. } => Err(ErrorCode::RecoveryPending),
        CheckedHistoryOutcomeV1::ReviewRequired => Err(ErrorCode::ReviewRequired),
        other => Err(outcome_refusal(&other)),
    }
}

fn print_json<T: Serialize>(value: &T) -> Result<(), ErrorCode> {
    let text = serde_json::to_string_pretty(value).map_err(|_| ErrorCode::Refused)?;
    println!("{text}");
    Ok(())
}

/// The first eight characters of a revision id: enough to name one in a
/// message, and the length a caller copies into the next command.
fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

#[cfg(test)]
mod tests;
