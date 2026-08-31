use chrono::{DateTime, Local};
use std::path::{Path, PathBuf};

/// The qOS collaborative FUSE mount. Per `fs.service` the daemon mounts
/// at `/var/workspace` (the older `/workspace` was a bug in this app).
pub const WORKSPACE_ROOT: &str = "/var/workspace";

/// Filename infix the collaborative FS uses to materialise a conflicting
/// copy: `<name>.sync-conflict-<first 32 hex of the losing blake3 hash>`.
/// Conflicts are therefore detectable from a plain `readdir` — no special
/// fs API needed.
pub const CONFLICT_MARKER: &str = ".sync-conflict-";

/// Whether `name` is a collaborative-FS conflict copy.
pub fn is_conflict_name(name: &str) -> bool {
    name.contains(CONFLICT_MARKER)
}

/// The extended-attribute the qOS FUSE driver answers with an entry's sync
/// state. Kept in lock-step with `fs::SYNC_XATTR`.
pub const SYNC_XATTR: &str = "user.qos.sync";

/// The qOS FUSE **sync / hydration state** of an entry, read live from the
/// driver via the [`SYNC_XATTR`] extended attribute. The collaborative FS
/// projects a peer's file metadata immediately but only hydrates the bytes on
/// first read, so a workspace file is one of:
///
/// * [`Local`](SyncState::Local) — content is on disk; opening it is instant.
/// * [`Projected`](SyncState::Projected) — metadata only; opening it hydrates
///   from a peer.
/// * [`Downloading`](SyncState::Downloading) — a hydration fetch is in flight.
///
/// `Entry::sync_state` is `None` for anything that isn't on a workspace mount
/// (an ordinary local file carries no such attribute).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncState {
    Local,
    Projected,
    Downloading,
}

impl SyncState {
    /// Parse the xattr value the driver returns.
    pub fn from_xattr(bytes: &[u8]) -> Option<Self> {
        match std::str::from_utf8(bytes).ok()?.trim() {
            "local" => Some(SyncState::Local),
            "projected" => Some(SyncState::Projected),
            "downloading" => Some(SyncState::Downloading),
            _ => None,
        }
    }
}

/// Read the qOS sync state of `path` from its [`SYNC_XATTR`] extended attribute.
/// `None` when the attribute is absent (an ordinary file, off any workspace
/// mount) or unreadable. This is a single `getxattr` — the same call
/// `getfattr -n user.qos.sync <path>` makes.
pub fn read_sync_state(path: &Path) -> Option<SyncState> {
    match xattr::get(path, SYNC_XATTR) {
        Ok(Some(bytes)) => SyncState::from_xattr(&bytes),
        _ => None,
    }
}

/// The extended attribute the qOS FUSE driver answers with a file's **thumbnail**
/// (a small PNG the host generates as metadata, ahead of / independent of the
/// content). Kept in lock-step with `fs::THUMB_XATTR`.
pub const THUMB_XATTR: &str = "user.qos.thumb";

/// Read a host-provided thumbnail (raw PNG bytes) for `path` from the
/// [`THUMB_XATTR`] extended attribute. `None` when the driver has no preview for
/// it (a plain local file off any workspace mount, a non-previewable type, or
/// content not yet local on the host). A single `getxattr`, exactly the metadata
/// channel the FUSE driver exposes — the file manager never re-decodes a file
/// the host already previewed.
pub fn read_thumb_xattr(path: &Path) -> Option<Vec<u8>> {
    match xattr::get(path, THUMB_XATTR) {
        Ok(Some(bytes)) if !bytes.is_empty() => Some(bytes),
        _ => None,
    }
}

/// The qOS workspace roots the FUSE driver mounts (first-class sidebar
/// locations, and the dirs whose entries carry sync state). Normally just
/// [`WORKSPACE_ROOT`]; `QOS_WORKSPACE_ROOTS` (colon-separated) overrides it for
/// tests and alternate mounts. Only roots that currently exist are returned.
pub fn workspace_roots() -> Vec<PathBuf> {
    if let Ok(v) = std::env::var("QOS_WORKSPACE_ROOTS") {
        return parse_workspace_roots(&v);
    }
    let p = PathBuf::from(WORKSPACE_ROOT);
    if p.exists() { vec![p] } else { Vec::new() }
}

/// Parse a `QOS_WORKSPACE_ROOTS` value (colon-separated) into the roots that
/// currently exist. Split out so the parsing is unit-testable without touching
/// the process environment.
fn parse_workspace_roots(val: &str) -> Vec<PathBuf> {
    val.split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .collect()
}

/// Whether `path` sits inside a qOS workspace root (so its entries carry sync
/// state and should be queried for the [`SYNC_XATTR`]).
pub fn is_workspace_path(path: &Path) -> bool {
    path_under_any(path, &workspace_roots())
}

/// Whether `path` is one of `roots` or nested under one. Pure over an explicit
/// root list so the containment logic is testable without the environment.
fn path_under_any(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|r| path == r || path.starts_with(r))
}

/// Which of `roots` contains `path`, if any - the longest match, so a
/// workspace mounted inside another resolves to the inner one.
///
/// [`is_workspace_path`] answers whether a path is in a workspace at all,
/// which is what a sync-state query needs. Naming the workspace needs the
/// root itself, and that is a different question: `/var/workspace` *is*
/// the workspace, while `/var/workspace/notes` is a folder in it. Pure
/// over an explicit root list for the same reason [`path_under_any`] is.
pub fn workspace_root_for<'a>(path: &Path, roots: &'a [PathBuf]) -> Option<&'a Path> {
    roots
        .iter()
        .filter(|r| path == r.as_path() || path.starts_with(r))
        .max_by_key(|r| r.as_os_str().len())
        .map(std::path::PathBuf::as_path)
}

/// What to call a workspace root on screen.
///
/// The mount path is an implementation detail of where the driver put it,
/// and a misleading one: the demo mounts under `/var/tmp`, and a trail
/// that spells it out tells a reader they are looking at scratch space on
/// somebody's laptop - the opposite of a folder that belongs to the room.
/// The mount's own final component is the name the workspace was given.
///
/// One function because the places sidebar and the breadcrumb name the
/// same location, and somebody who clicks the one to reach the other must
/// not find it has been renamed on the way.
pub fn workspace_label(root: &Path) -> String {
    root.file_name().map_or_else(
        || "Workspace".to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<DateTime<Local>>,
    pub mime: String,
    /// The qOS FUSE sync state, when this entry is on a workspace mount.
    pub sync_state: Option<SyncState>,
}

impl Entry {
    /// `true` when this entry is a collaborative-FS conflict copy. Derived
    /// from the name so no extra field (and no construction-site churn) is
    /// needed.
    pub fn is_conflict(&self) -> bool {
        is_conflict_name(&self.name)
    }

    /// Whether this entry's **content** is already on this machine, so
    /// reading it costs a read and nothing else.
    ///
    /// [`SyncState::Projected`] and [`SyncState::Downloading`] say the bytes
    /// are not here: opening such a file hydrates it from a peer, which is
    /// work nobody asked for. Everything else is local — a file the driver
    /// reports as [`SyncState::Local`], and equally a file carrying no sync
    /// attribute at all, which is what an ordinary path (or a workspace root
    /// that is a plain directory rather than a live FUSE mount) looks like.
    pub fn content_is_local(&self) -> bool {
        !matches!(
            self.sync_state,
            Some(SyncState::Projected | SyncState::Downloading)
        )
    }
}

/// Human-readable byte size (e.g. `4.0 kB`). Directories show "—".
pub fn human_size(entry: &Entry) -> String {
    if entry.is_dir {
        "—".to_string()
    } else {
        humansize::format_size(entry.size, humansize::DECIMAL)
    }
}

/// Short modified timestamp (`YYYY-MM-DD HH:MM`), or "—" when unknown.
pub fn format_modified(modified: Option<DateTime<Local>>) -> String {
    match modified {
        Some(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        None => "—".to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Name,
    Size,
    Modified,
    Type,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    List,
    Grid,
}

pub fn sort_entries(sort_by: SortBy, sort_order: SortOrder, entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        let cmp = match sort_by {
            SortBy::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortBy::Size => a.size.cmp(&b.size),
            SortBy::Modified => a.modified.cmp(&b.modified),
            SortBy::Type => a.mime.cmp(&b.mime),
        };
        let final_cmp = if sort_order == SortOrder::Descending {
            cmp.reverse()
        } else {
            cmp
        };
        // Directories always first, regardless of order direction.
        b.is_dir.cmp(&a.is_dir).then(final_cmp)
    });
}

pub fn read_dir_sync(path: &Path) -> Result<Vec<Entry>, String> {
    // Only query the sync xattr when listing a workspace mount — an ordinary
    // directory has no such attribute, so skipping the per-entry `getxattr`
    // there keeps a plain home-folder listing at one syscall per file.
    read_dir_inner(path, is_workspace_path(path))
}

/// The listing body, with the workspace-gating decision passed in (so tests can
/// exercise the sync-state population path without mutating the global
/// `QOS_WORKSPACE_ROOTS` env). Production always routes through
/// [`read_dir_sync`], which derives `in_workspace` from the real environment.
fn read_dir_inner(path: &Path, in_workspace: bool) -> Result<Vec<Entry>, String> {
    let rd = std::fs::read_dir(path).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let meta = entry.metadata().ok();
        let path = entry.path();
        let is_dir = meta.as_ref().is_some_and(std::fs::Metadata::is_dir);
        let mime = mime_guess::from_path(&path)
            .first_or_octet_stream()
            .to_string();
        // Content hydration state only applies to files — a directory's structure
        // is always fully projected — so we don't query (or badge) folders.
        let sync_state = if in_workspace && !is_dir {
            read_sync_state(&path)
        } else {
            None
        };
        out.push(Entry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path,
            is_dir,
            size: meta.as_ref().map_or(0, std::fs::Metadata::len),
            modified: meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .map(DateTime::from),
            mime,
            sync_state,
        });
    }
    Ok(out)
}

pub fn format_path(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home {
        if path == home {
            return "~".to_string();
        }
        if let Ok(rel) = path.strip_prefix(home) {
            return format!("~/{}", rel.display());
        }
    }
    path.display().to_string()
}

pub fn initial_path() -> PathBuf {
    // Prefer a configured collaborative workspace mount. An explicit empty
    // override means there is no workspace: test/alternate environments often
    // have an unrelated `/workspace` (for example, the CI checkout mount), and
    // must be able to keep Files from mistaking it for user content.
    if let Some(root) = workspace_roots().into_iter().next() {
        return root;
    }
    // `/workspace` predates `QOS_WORKSPACE_ROOTS`; retain it only when no
    // override was supplied, so old installations keep their start location.
    if std::env::var_os("QOS_WORKSPACE_ROOTS").is_none() {
        let legacy = PathBuf::from("/workspace");
        if legacy.exists() {
            return legacy;
        }
    }
    if let Some(home) = user_home() {
        return home;
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

/// The user's home, which under a sandbox is *not* this process's `HOME`
/// — that is the app's own per-package directory, and opening a file
/// manager on it shows a folder with none of the user's files in it.
/// `QOS_USER_HOME` is present only when a grant actually bound the real
/// home, so preferring it can never point at a directory this process
/// cannot read.
///
/// Every surface that needs to know where home is resolves it here, so
/// the directory the window opens on and the home the breadcrumb
/// abbreviates against are the same answer. When they were two answers
/// the trail rendered as an absolute path from the filesystem root: the
/// window was on the real home and `format_path` was measuring against
/// `/run/qos/app`, which nothing is under.
pub fn user_home() -> Option<PathBuf> {
    ui::sysdirs::user_home_override().or_else(dirs::home_dir)
}

#[cfg(test)]
mod sync_state_tests {
    use super::*;

    #[test]
    fn from_xattr_parses_the_three_states_and_rejects_junk() {
        assert_eq!(SyncState::from_xattr(b"local"), Some(SyncState::Local));
        assert_eq!(
            SyncState::from_xattr(b"projected"),
            Some(SyncState::Projected)
        );
        assert_eq!(
            SyncState::from_xattr(b"downloading"),
            Some(SyncState::Downloading)
        );
        // Trailing whitespace/newline (some xattr tools append one) is tolerated.
        assert_eq!(SyncState::from_xattr(b"local\n"), Some(SyncState::Local));
        assert_eq!(SyncState::from_xattr(b"bogus"), None);
        assert_eq!(SyncState::from_xattr(&[0xff, 0xfe]), None);
    }

    #[test]
    fn read_sync_state_round_trips_a_real_xattr() {
        // A real getxattr against a real `user.qos.sync` attribute — the exact
        // syscall the app makes against the FUSE mount, exercised here on a
        // tmpfile so it doesn't need a live driver.
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("projected.txt");
        std::fs::write(&f, b"x").unwrap();
        // No attribute yet → None.
        assert_eq!(read_sync_state(&f), None, "absent attribute reads as None");
        // Set it the way the driver would and read it back.
        xattr::set(&f, SYNC_XATTR, b"projected").unwrap();
        assert_eq!(read_sync_state(&f), Some(SyncState::Projected));
        xattr::set(&f, SYNC_XATTR, b"local").unwrap();
        assert_eq!(read_sync_state(&f), Some(SyncState::Local));
    }

    #[test]
    fn read_thumb_xattr_round_trips_a_real_host_preview() {
        // The FS-metadata channel: a host writes a preview into `user.qos.thumb`
        // and the file manager reads the exact bytes back — the same `getxattr`
        // the driver answers. Exercised on a tmpfile so it needs no live driver.
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("photo.png");
        std::fs::write(&f, b"content").unwrap();
        assert_eq!(read_thumb_xattr(&f), None, "absent preview reads as None");
        let png = vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4];
        xattr::set(&f, THUMB_XATTR, &png).unwrap();
        assert_eq!(
            read_thumb_xattr(&f),
            Some(png),
            "the host preview bytes come back verbatim"
        );
    }

    #[test]
    fn read_dir_populates_sync_state_only_inside_a_workspace_root() {
        // Tag two files with real sync xattrs, then confirm the real listing
        // path surfaces them on `Entry` when (and only when) the directory is a
        // workspace mount — driven through `read_dir_inner`, the exact function
        // `read_dir_sync` delegates to.
        let ws = tempfile::tempdir().unwrap();
        let downloading = ws.path().join("a-downloading.bin");
        let projected = ws.path().join("b-projected.bin");
        let untagged = ws.path().join("c-plain.bin");
        std::fs::write(&downloading, b"x").unwrap();
        std::fs::write(&projected, b"x").unwrap();
        std::fs::write(&untagged, b"x").unwrap();
        xattr::set(&downloading, SYNC_XATTR, b"downloading").unwrap();
        xattr::set(&projected, SYNC_XATTR, b"projected").unwrap();

        // Not a workspace: sync_state is never queried → all None.
        let outside = read_dir_inner(ws.path(), false).unwrap();
        assert!(
            outside.iter().all(|e| e.sync_state.is_none()),
            "outside a workspace root, no entry carries sync state",
        );

        // Declared a workspace: real xattrs are surfaced on the entries.
        let entries = read_dir_inner(ws.path(), true).unwrap();
        let by = |name: &str| entries.iter().find(|e| e.name == name).unwrap().sync_state;
        assert_eq!(by("a-downloading.bin"), Some(SyncState::Downloading));
        assert_eq!(by("b-projected.bin"), Some(SyncState::Projected));
        assert_eq!(
            by("c-plain.bin"),
            None,
            "an untagged workspace file has no state"
        );
    }

    #[test]
    fn workspace_root_parsing_and_containment() {
        // Colon-separated roots parse, non-existent entries drop out, and
        // containment matches the root itself and any nested path — the exact
        // logic `read_dir_sync` uses to gate the sync-xattr query. Exercised
        // through the pure helpers so no global env is touched.
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let spec = format!(
            "{}:/does/not/exist:{}",
            a.path().display(),
            b.path().display()
        );
        let roots = parse_workspace_roots(&spec);
        assert!(
            roots.contains(&a.path().to_path_buf()),
            "existing root A kept: {roots:?}"
        );
        assert!(
            roots.contains(&b.path().to_path_buf()),
            "existing root B kept"
        );
        assert_eq!(roots.len(), 2, "the non-existent root is filtered out");

        assert!(
            path_under_any(a.path(), &roots),
            "the root itself is a workspace path"
        );
        assert!(
            path_under_any(&a.path().join("deep/file.txt"), &roots),
            "nested path matches"
        );
        assert!(
            !path_under_any(Path::new("/etc/passwd"), &roots),
            "unrelated path does not match"
        );
    }
}
