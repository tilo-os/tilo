//! Files as a migration: folders on this computer (`home`) and cloud drives, copied into the
//! workspace's files by one engine.
//!
//! A source of files is a [`Tree`]: the folders and files a review lists, each walked to the files
//! under it and copied out whole. Where they go:
//!
//! - **The workspace's folder on this computer** (`~/<workspace>`, `tilo folders`), when it is
//!   mounted: the files are written into it, many at once, as `cp -r` would, and the folder
//!   syncs them. This is the fast path, bound by the disk or the network the folder syncs over.
//!   Each migration notes the files it wrote, so `undo` removes what it brought and nobody
//!   changed since.
//! - **Uploads**, when the workspace has no folder here (a hosted workspace reached with `-w`):
//!   the bytes go up as `tilo files put` sends them, held content skipped and small files sent
//!   together, and the tree takes them in a few `files#open` steps of the migration, which
//!   History undoes.
//!
//! A file the workspace has at its place with the same size is already there: starting again copies
//! only what is new or changed, and a stopped migration resumes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};

use super::size;

use super::Lander;
use super::state::{Item, Place, Review};

pub const HOME: &str = "home";

/// How many files are copied at once.
const COPIES_AT_ONCE: usize = 16;
/// How many files one `files#open` step puts into the tree.
const TREE_STEP_FILES: usize = 2_000;

/// One file under a chosen item.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Its path from the item's parent: `Documents/2026/plan.md`.
    pub path: String,
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// Where the tree finds it again: a path here, a cloud item's id.
    pub id: String,
}

/// A source of files.
pub trait Tree: Sync {
    /// Every file under the reviewed item `key`; what could not be read is answered beside them.
    ///
    /// # Errors
    /// The item could not be read at all.
    fn files(&self, key: &str) -> Result<(Vec<Entry>, Vec<String>)>;

    /// Write `entry`'s bytes into `to`, a new file here.
    ///
    /// # Errors
    /// Its bytes could not be read or written.
    fn copy_to(&self, entry: &Entry, to: &Path) -> Result<()>;

    /// The file here `entry` is, when it is one (a folder on this computer), so an upload reads
    /// it in place rather than from a copy.
    fn local(&self, entry: &Entry) -> Option<PathBuf> {
        let _ = entry;
        None
    }
}

/// Folders on this computer, by the keys a review lists: a name under `home`, or a path.
pub struct Local {
    pub home: PathBuf,
}

impl Local {
    /// The folder `key` names.
    #[must_use]
    pub fn folder(&self, key: &str) -> PathBuf {
        if key.starts_with('/') {
            PathBuf::from(key)
        } else {
            self.home.join(key)
        }
    }
}

impl Tree for Local {
    fn files(&self, key: &str) -> Result<(Vec<Entry>, Vec<String>)> {
        let root = self.folder(key);
        let named = root.file_name().map_or_else(
            || key.to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let mut found = Vec::new();
        let mut unread = Vec::new();
        walk_local(&root, &named, &mut found, &mut unread)?;
        Ok((found, unread))
    }

    fn copy_to(&self, entry: &Entry, to: &Path) -> Result<()> {
        std::fs::copy(&entry.id, to)
            .with_context(|| format!("copying {} to {}", entry.id, to.display()))?;
        Ok(())
    }

    fn local(&self, entry: &Entry) -> Option<PathBuf> {
        Some(PathBuf::from(&entry.id))
    }
}

/// Every file under `dir`, named from `named` down; a link is left alone, as `cp -r` copies what
/// is in the folder and not what a link points at.
fn walk_local(
    dir: &Path,
    named: &str,
    found: &mut Vec<Entry>,
    unread: &mut Vec<String>,
) -> Result<()> {
    let meta =
        std::fs::symlink_metadata(dir).with_context(|| format!("reading {}", dir.display()))?;
    if meta.is_file() {
        found.push(Entry {
            path: named.to_owned(),
            size: meta.len(),
            modified: meta.modified().ok(),
            id: dir.to_string_lossy().into_owned(),
        });
        return Ok(());
    }
    if !meta.is_dir() {
        bail!("{} is not a folder or a file", dir.display());
    }
    let mut walking = vec![(dir.to_path_buf(), named.to_owned())];
    while let Some((dir, named)) = walking.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) => {
                unread.push(format!("{}: {error}", dir.display()));
                continue;
            }
        };
        for entry in entries.filter_map(std::result::Result::ok) {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = format!("{named}/{name}");
            if kind.is_dir() {
                walking.push((entry.path(), path));
            } else if kind.is_file()
                && let Ok(meta) = entry.metadata()
            {
                found.push(Entry {
                    path,
                    size: meta.len(),
                    modified: meta.modified().ok(),
                    id: entry.path().to_string_lossy().into_owned(),
                });
            }
        }
    }
    Ok(())
}

/// `tilo migrate review home`: the familiar folders and the other visible folders of `home`, and
/// any folder chosen by its path before. The familiar ones are chosen until the person chooses.
pub fn review(home: &Path, kept: Option<&Review>, folders: &BTreeMap<String, PathBuf>) -> Review {
    let mut items = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for folder in crate::known_folders::discover() {
        let Some(path) = folder.path else { continue };
        let key = key_of(home, &path);
        if seen.insert(key.clone()) {
            items.push(item(home, &key, folder.kind.label(), true));
        }
    }
    let workspaces: Vec<&PathBuf> = folders.values().collect();
    if let Ok(entries) = std::fs::read_dir(home) {
        let mut others: Vec<String> = entries
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| !name.starts_with('.'))
            .filter(|name| !workspaces.contains(&&home.join(name)))
            .collect();
        others.sort();
        for name in others {
            if seen.insert(name.clone()) {
                items.push(item(home, &name, &name, false));
            }
        }
    }
    // A folder the person chose by its path stays listed.
    if let Some(kept) = kept {
        for old in &kept.items {
            if old.key.starts_with('/') && seen.insert(old.key.clone()) {
                items.push(item(home, &old.key, &old.title, old.chosen));
            }
        }
        for item in &mut items {
            if let Some(old) = kept.items.iter().find(|old| old.key == item.key) {
                item.chosen = old.chosen;
            }
        }
    }
    Review {
        source: HOME.to_owned(),
        title: format!("Folders in {}", home.display()),
        handle: home.display().to_string(),
        items,
        unreadable: Vec::new(),
    }
}

/// The key a review lists the folder `path` under: its name in `home`, or its whole path.
fn key_of(home: &Path, path: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.components().count() == 1 => rest.to_string_lossy().into_owned(),
        _ => path.to_string_lossy().into_owned(),
    }
}

/// The review's item for the folder `key`, counted.
fn item(home: &Path, key: &str, title: &str, chosen: bool) -> Item {
    let local = Local {
        home: home.to_path_buf(),
    };
    let (files, bytes) = local.files(key).map_or((0, 0), |(found, _)| {
        (found.len(), found.iter().map(|entry| entry.size).sum())
    });
    Item {
        key: key.to_owned(),
        kind: "folder".to_owned(),
        title: title.to_owned(),
        count: files,
        counts: format!("files, {}", size(bytes)),
        within: None,
        chosen,
        note: None,
    }
}

/// Folders `choose` names by their path (`~/code/site`, `/data/photos`) join the review.
///
/// # Errors
/// A path names no folder.
pub fn add_paths(home: &Path, review: &mut Review, keys: &mut [String]) -> Result<()> {
    for key in keys.iter_mut() {
        let path = if let Some(rest) = key.strip_prefix("~/") {
            home.join(rest)
        } else if key.starts_with('/') || key.starts_with("./") || key.starts_with("../") {
            std::fs::canonicalize(&*key).with_context(|| format!("{key} is not a folder here"))?
        } else {
            continue;
        };
        if !path.is_dir() {
            bail!("{} is not a folder", path.display());
        }
        let named = key_of(home, &path);
        if !review.items.iter().any(|item| item.key == named) {
            let title = path
                .file_name()
                .map_or_else(|| named.clone(), |name| name.to_string_lossy().into_owned());
            review.items.push(item(home, &named, &title, false));
        }
        *key = named;
    }
    Ok(())
}

/// What bringing `review` over brings, in a few words.
#[must_use]
pub fn summary(review: &Review, of: &str) -> String {
    let chosen = review.chosen();
    let files: usize = chosen.iter().map(|item| item.count).sum();
    let names: Vec<&str> = chosen.iter().map(|item| item.title.as_str()).collect();
    format!(
        "Bring {} from {of}: {}",
        super::services::counted(files, "files"),
        names.join(", ")
    )
}

/// Where a migration's files go.
pub enum Sink {
    /// The workspace's mounted folder on this computer, and the folder in it they go under.
    Folder(PathBuf),
    /// Uploads into the workspace's tree, under this folder of it.
    Upload(String),
}

/// The folder the workspace `id` has on this computer, among `folders`, when it is mounted.
#[must_use]
pub fn mounted_folder(folders: &BTreeMap<String, PathBuf>, id: &str) -> Option<PathBuf> {
    let folder = folders.get(id)?;
    is_mounted(folder).then(|| folder.clone())
}

/// Folders on this computer.
pub struct Folders;

impl super::Source for Folders {
    fn here(&self, _: &super::Cx<'_>, row: &'static super::registry::Row) -> super::Here {
        super::Here {
            next: Some(format!("tilo migrate review {}", row.key)),
            ..super::Here::default()
        }
    }

    fn review(
        &self,
        cx: &super::Cx<'_>,
        row: &'static super::registry::Row,
        _: Option<&str>,
    ) -> Result<Review> {
        Ok(review(
            cx.home,
            cx.place.review(row.key)?.as_ref(),
            cx.folders,
        ))
    }

    fn summary(&self, row: &'static super::registry::Row, review: &Review) -> String {
        summary(review, row.title)
    }

    fn files_under(&self, _: &'static super::registry::Row) -> Option<&'static str> {
        Some("")
    }

    fn land(
        &self,
        cx: &super::Cx<'_>,
        _: &'static super::registry::Row,
        review: &Review,
        sink: Option<&Sink>,
        lander: &mut Lander<'_>,
    ) -> Result<()> {
        let sink = sink.ok_or_else(|| anyhow!("files have nowhere to go"))?;
        land(
            &Local {
                home: cx.home.to_path_buf(),
            },
            review,
            sink,
            cx.place,
            lander,
        )
    }

    fn landed(
        &self,
        _: &super::Cx<'_>,
        _: &'static super::registry::Row,
        journal: &super::state::Journal,
    ) -> Result<usize> {
        // Files are counted as the last run found them: brought, or there already.
        Ok(journal
            .brought
            .values()
            .sum::<usize>()
            .saturating_add(journal.already_there))
    }
}

/// Whether `path` is a mount point: its device differs from its parent's.
fn is_mounted(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let (Ok(here), Some(Ok(parent))) = (
        std::fs::metadata(path),
        path.parent().map(std::fs::metadata),
    ) else {
        return false;
    };
    here.dev() != parent.dev()
}

/// How a migration's copying stands, shared by the threads that copy.
#[derive(Default)]
struct Counts {
    copied: AtomicUsize,
    already: AtomicUsize,
    bytes: AtomicU64,
    failed: Mutex<Vec<String>>,
}

/// Bring the chosen items of `review` from `tree` into `sink`, as the lander's invocation.
///
/// # Errors
/// The migration was stopped, the workspace refused, or files could not be copied.
pub fn land(
    tree: &dyn Tree,
    review: &Review,
    sink: &Sink,
    place: &Place,
    lander: &mut Lander<'_>,
) -> Result<()> {
    let started = Instant::now();
    let mut total_bytes = 0_u64;
    let mut total_files = 0_usize;
    for item in review
        .chosen()
        .into_iter()
        .filter(|item| item.within.is_none())
    {
        if lander.stopping() {
            return Err(super::Stopped.into());
        }
        let (entries, unread) = tree
            .files(&item.key)
            .with_context(|| format!("reading {}", item.title))?;
        for problem in unread {
            lander.say(&format!("left out (unreadable): {problem}"));
        }
        let counts = match sink {
            Sink::Folder(root) => copy_into(tree, &entries, root, place, lander)?,
            Sink::Upload(under) => upload(tree, &entries, under, lander)?,
        };
        let failed = std::mem::take(
            &mut *counts
                .failed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        total_bytes = total_bytes.saturating_add(counts.bytes.load(Ordering::Relaxed));
        total_files = total_files.saturating_add(counts.copied.load(Ordering::Relaxed));
        if !failed.is_empty() {
            bail!(
                "{} of {}'s files did not come over (start again to retry them):\n  {}",
                failed.len(),
                item.title,
                failed
                    .iter()
                    .take(20)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n  ")
            );
        }
        lander.note([format!("folder:{}", item.key)])?;
    }
    let elapsed = started.elapsed();
    // The rate in whole bytes a second: a size to read, not a float's rounding of one.
    let millis = elapsed.as_millis().max(1);
    let per_second = u128::from(total_bytes)
        .saturating_mul(1000)
        .checked_div(millis)
        .and_then(|rate| u64::try_from(rate).ok())
        .unwrap_or(u64::MAX);
    lander.say(&format!(
        "{total_files} files, {} in {:.1} s ({}/s)",
        size(total_bytes),
        elapsed.as_secs_f64(),
        size(per_second)
    ));
    Ok(())
}

/// What an invocation wrote into a mounted folder, so `undo` removes it: the folder, and each
/// file with the size it wrote.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Written {
    root: PathBuf,
    files: Vec<(u64, PathBuf)>,
}

/// The field an invocation notes what it wrote in.
fn written_field(invocation: &str) -> String {
    format!("{WRITTEN}{invocation}")
}

const WRITTEN: &str = "written-";

/// The files every other invocation of `source` wrote, which undoing one leaves alone.
fn written_by_others(place: &Place, source: &str, invocation: &str) -> Result<BTreeSet<PathBuf>> {
    let own = written_field(invocation);
    let mut paths = BTreeSet::new();
    for field in place.fields(source) {
        if field == own || !field.starts_with(WRITTEN) {
            continue;
        }
        if let Some(written) = place.field::<Written>(source, &field)? {
            paths.extend(written.files.into_iter().map(|(_, path)| path));
        }
    }
    Ok(paths)
}

/// Copy `entries` into the mounted folder `root`, many at once.
fn copy_into(
    tree: &dyn Tree,
    entries: &[Entry],
    root: &Path,
    place: &Place,
    lander: &mut Lander<'_>,
) -> Result<Counts> {
    let counts = Counts::default();
    let (source, field) = (
        lander.source().to_owned(),
        written_field(&lander.invocation().id),
    );
    // Undo empties folders up to `root` and no further.
    let written = Mutex::new(place.field::<Written>(&source, &field)?.unwrap_or(Written {
        root: root.to_path_buf(),
        files: Vec::new(),
    }));
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let mut reported = (0_usize, 0_usize);
    std::thread::scope(|scope| -> Result<()> {
        for _ in 0..COPIES_AT_ONCE.min(entries.len().max(1)) {
            scope.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    let at = next.fetch_add(1, Ordering::Relaxed);
                    let Some(entry) = entries.get(at) else { break };
                    copy_one(tree, entry, root, &counts, &written);
                }
            });
        }
        // This thread tells how far the copy is and hears a stop.
        loop {
            let done = next.load(Ordering::Relaxed) >= entries.len();
            let now = (
                counts.copied.load(Ordering::Relaxed),
                counts.already.load(Ordering::Relaxed),
            );
            if now != reported {
                lander.count(
                    "files",
                    now.0.saturating_sub(reported.0),
                    now.1.saturating_sub(reported.1),
                )?;
                reported = now;
            }
            if done {
                break;
            }
            if lander.stopping() {
                stop.store(true, Ordering::Relaxed);
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        Ok(())
    })?;
    let now = (
        counts.copied.load(Ordering::Relaxed),
        counts.already.load(Ordering::Relaxed),
    );
    lander.count(
        "files",
        now.0.saturating_sub(reported.0),
        now.1.saturating_sub(reported.1),
    )?;
    place.keep_field(
        &source,
        &field,
        &written
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )?;
    if stop.load(Ordering::Relaxed) {
        return Err(super::Stopped.into());
    }
    Ok(counts)
}

/// Copy `entry` under `root`, unless a file of its size is there already, and note in `written`
/// what it wrote so `undo` removes it.
fn copy_one(
    tree: &dyn Tree,
    entry: &Entry,
    root: &Path,
    counts: &Counts,
    written: &Mutex<Written>,
) {
    let to = root.join(&entry.path);
    if std::fs::metadata(&to).is_ok_and(|meta| meta.len() == entry.size) {
        counts.already.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let copied = to
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .map_err(anyhow::Error::from)
        .and_then(|()| tree.copy_to(entry, &to));
    match copied {
        Ok(()) => {
            if let Some(modified) = entry.modified
                && let Ok(file) = std::fs::File::options().write(true).open(&to)
            {
                let _ = file.set_modified(modified);
            }
            if let Ok(mut written) = written.lock() {
                written.files.push((entry.size, to.clone()));
            }
            counts.copied.fetch_add(1, Ordering::Relaxed);
            counts.bytes.fetch_add(entry.size, Ordering::Relaxed);
        }
        Err(error) => {
            let _ = std::fs::remove_file(&to);
            if let Ok(mut failed) = counts.failed.lock() {
                failed.push(format!("{}: {error:#}", entry.path));
            }
        }
    }
}

/// Upload `entries` into the workspace's tree under `under`: bytes first, then the tree takes
/// them in steps of the invocation.
fn upload(
    tree: &dyn Tree,
    entries: &[Entry],
    under: &str,
    lander: &mut Lander<'_>,
) -> Result<Counts> {
    let counts = Counts::default();
    // A file that is not on this computer is fetched into a staging folder first, which goes
    // once the workspace took its files.
    let staging = lander.scratch().join(lander.source()).join("staging");
    let mut files = Vec::with_capacity(entries.len());
    for entry in entries {
        if lander.stopping() {
            return Err(super::Stopped.into());
        }
        let path = if let Some(path) = tree.local(entry) {
            path
        } else {
            let to = staging.join(&entry.path);
            if !std::fs::metadata(&to).is_ok_and(|meta| meta.len() == entry.size) {
                if let Some(dir) = to.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                if let Err(error) = tree.copy_to(entry, &to) {
                    if let Ok(mut failed) = counts.failed.lock() {
                        failed.push(format!("{}: {error:#}", entry.path));
                    }
                    continue;
                }
            }
            to
        };
        files.push((path, entry.path.clone()));
    }
    for step in files.chunks(TREE_STEP_FILES) {
        if lander.stopping() {
            return Err(super::Stopped.into());
        }
        let (workspace, invocation) = lander.workspace()?;
        let contents = workspace.put_files(step)?;
        let listed: Vec<Value> = step
            .iter()
            .zip(&contents)
            .map(|((_, name), content)| {
                let (dir, base) = name.rsplit_once('/').unwrap_or(("", name));
                let dir = [under, dir]
                    .iter()
                    .filter(|part| !part.is_empty())
                    .copied()
                    .collect::<Vec<_>>()
                    .join("/");
                json!({"content": content, "name": base, "dir": dir})
            })
            .collect();
        let list = workspace.put(&serde_json::to_vec(&listed)?)?;
        let answer = workspace.act(invocation, "files#open", json!({"files": list}))?;
        let put = answer
            .result()
            .and_then(|result| result.get("count"))
            .and_then(Value::as_u64)
            .map_or(step.len(), |count| usize::try_from(count).unwrap_or(0));
        let bytes: u64 = step
            .iter()
            .filter_map(|(path, _)| std::fs::metadata(path).ok())
            .map(|meta| meta.len())
            .sum();
        counts.copied.fetch_add(put, Ordering::Relaxed);
        counts.bytes.fetch_add(bytes, Ordering::Relaxed);
        lander.count("files", put, 0)?;
    }
    let _ = std::fs::remove_dir_all(&staging);
    Ok(counts)
}

/// Take back what `invocation` wrote into a mounted folder: each file it wrote that still has
/// the size it wrote, then the folders that left empty. How many files went, or `None` when it
/// wrote into no folder here.
///
/// # Errors
/// Its list could not be read.
pub fn undo_written(place: &Place, source: &str, invocation: &str) -> Result<Option<usize>> {
    let field = written_field(invocation);
    let Some(written) = place.field::<Written>(source, &field)? else {
        return Ok(None);
    };
    let others = written_by_others(place, source, invocation)?;
    let root = written.root;
    let mut removed = 0_usize;
    let mut folders = BTreeSet::new();
    for (size, path) in written.files {
        // A later migration wrote it again: it is that one's now.
        if others.contains(&path) {
            continue;
        }
        let unchanged = std::fs::metadata(&path).is_ok_and(|meta| meta.len() == size);
        if unchanged && std::fs::remove_file(&path).is_ok() {
            removed = removed.saturating_add(1);
            let mut dir = path.parent().map(Path::to_path_buf);
            while let Some(at) = dir {
                if !at.starts_with(&root) || at == root {
                    break;
                }
                folders.insert((
                    usize::MAX.saturating_sub(at.components().count()),
                    at.clone(),
                ));
                dir = at.parent().map(Path::to_path_buf);
            }
        }
    }
    // Deepest first, so a folder whose folders emptied goes too; a folder that holds anything
    // else stays.
    for (_, dir) in folders {
        let _ = std::fs::remove_dir(&dir);
    }
    place.forget_field(source, &field)?;
    Ok(Some(removed))
}

/// Where a migration of `source` lands: the workspace's folder here when it is mounted, uploads
/// otherwise, or what to do when neither reaches it.
///
/// # Errors
/// It lands in this computer's workspace, which has no folder mounted here.
pub fn sink(
    folders: &BTreeMap<String, PathBuf>,
    workspace: &str,
    hosted: bool,
    under: &str,
) -> Result<Sink> {
    if let Some(folder) = mounted_folder(folders, workspace) {
        return Ok(Sink::Folder(if under.is_empty() {
            folder
        } else {
            folder.join(under)
        }));
    }
    if hosted {
        return Ok(Sink::Upload(under.to_owned()));
    }
    Err(anyhow!(
        "the workspace's folder isn't on this computer, so there is nowhere to copy the files to. \
         See it with `tilo folders`, give it one with `tilo folders add <workspace>`, then start again"
    ))
}
