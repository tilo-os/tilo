//! The person's files, found by name and, once the content index has read
//! them, by what they hold ([`crate::files_index`]).
//!
//! Names come from a walk of the home folder on a thread of its own
//! ([`FileNames::scan`]), so opening search never waits for the disk; until
//! the walk is done, search finds what it has reached. A watch on the same
//! folders, armed before the walk, keeps the names current after it: a file
//! or folder made, removed or renamed is found (or no longer found) at the
//! next search. A row opens the file where the host opens files, as its
//! `file://` address.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock, Weak, mpsc};

use tilo_files::ChangeKind;
use tilo_files::native::{ChangeWatch, WatchEvent, Watcher};
use ui::search::FieldWeight;
use ui::search::system::{MAX_PROVIDER_SCORE, ResourceKind, rank_fields};

use crate::files_index::{FilesIndex, MAX_DEPTH, should_skip};
use crate::model::{Hit, Source, Target};

/// The most names kept.
const MAX_NAMES: usize = 200_000;
/// The most rows one query offers.
const MAX_ROWS: usize = 64;
/// A file found by what it holds, not its name: still a match, below any
/// named one.
const CONTENT_SCORE: i32 = 10;

/// The names found, each with whether it is a folder.
type Names = BTreeMap<PathBuf, bool>;

/// The names under the home folder, as far as the walk has come and as the
/// watch has followed them since.
#[derive(Clone, Default)]
pub struct FileNames(Arc<Held>);

#[derive(Default)]
struct Held {
    names: RwLock<Names>,
    /// Ends the thread that keeps the names, and its watch, once the last
    /// handle goes.
    stop: Option<Mutex<mpsc::Sender<Message>>>,
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some(stop) = &self.stop {
            let _ = stop
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .send(Message::Stop);
        }
    }
}

/// What the thread that keeps the names is told.
enum Message {
    /// Changes the watch delivered together, in order.
    Changes(Vec<WatchEvent>),
    /// Every handle is gone.
    Stop,
}

impl FileNames {
    /// Walk `home` on a thread of its own, skipping what the content index
    /// skips (hidden folders, build output, caches), and follow its changes
    /// from then on.
    #[must_use]
    pub fn scan(home: PathBuf) -> Self {
        let (sender, messages) = mpsc::channel();
        let names = Self(Arc::new(Held {
            names: RwLock::default(),
            stop: Some(Mutex::new(sender.clone())),
        }));
        let kept = Arc::downgrade(&names.0);
        let spawned = std::thread::Builder::new()
            .name("tilo-file-names".into())
            .spawn(move || keep(&home, &kept, &sender, &messages));
        if let Err(error) = spawned {
            tracing::warn!(%error, "the file names are not walked");
        }
        names
    }

    /// Names known up front: `(path, is a folder)`.
    #[must_use]
    pub fn from_paths(paths: impl IntoIterator<Item = (PathBuf, bool)>) -> Self {
        Self(Arc::new(Held {
            names: RwLock::new(paths.into_iter().collect()),
            stop: None,
        }))
    }
}

/// The thread that keeps the names: watch `home`, walk it, then take in
/// each change, until the names are dropped.
///
/// The watch is armed first and its changes wait in `messages` until the
/// walk is done, so nothing made or removed meanwhile is missed. Each
/// change says what its path is when it was delivered, later than the walk
/// saw it, so taking the changes in after the walk leaves each name as it
/// is now.
fn keep(
    home: &Path,
    kept: &Weak<Held>,
    sender: &mpsc::Sender<Message>,
    messages: &mpsc::Receiver<Message>,
) {
    let _watch = watch(home, sender.clone());
    let mut batch = Names::new();
    walk(home, home, MAX_NAMES, &mut batch, Some(kept));
    let Some(held) = kept.upgrade() else {
        return;
    };
    held.write().append(&mut batch);
    drop(held);
    while let Ok(Message::Changes(events)) = messages.recv() {
        let Some(held) = kept.upgrade() else {
            return;
        };
        for event in &events {
            if event.kind == ChangeKind::Rescan {
                let mut fresh = Names::new();
                walk(home, home, MAX_NAMES, &mut fresh, None);
                *held.write() = fresh;
            } else {
                take(home, &held, event);
            }
        }
    }
}

/// A watch on every folder the walk reaches, reporting names made, removed
/// and renamed (not writes) to `sender`. Without one the walk's names stay
/// as they were found.
fn watch(home: &Path, sender: mpsc::Sender<Message>) -> Option<ChangeWatch> {
    let skipped = home.to_path_buf();
    Watcher::tree(home)
        .presence()
        // A folder whose entries the walk does not reach is not watched.
        .skip(move |folder| should_skip(folder, &skipped) || depth(folder, &skipped) >= MAX_DEPTH)
        .start(move |events| {
            let _ = sender.send(Message::Changes(events.to_vec()));
        })
        .map_err(|error| tracing::warn!(%error, "the file names do not follow changes"))
        .ok()
}

/// How far below `home` `path` is: its children are 1.
fn depth(path: &Path, home: &Path) -> usize {
    path.strip_prefix(home)
        .map_or(usize::MAX, |below| below.components().count())
}

/// Whether the walk keeps `path`: within reach and not skipped.
fn reached(path: &Path, home: &Path) -> bool {
    (1..=MAX_DEPTH).contains(&depth(path, home)) && !should_skip(path, home)
}

/// Add at most `room` names below `top` (and `top` itself, unless it is
/// `home`) to `names`, as the content index walks them. With `kept`, every
/// thousand names found are handed to the kept names at once, so search
/// finds them while the walk goes on.
fn walk(home: &Path, top: &Path, room: usize, names: &mut Names, kept: Option<&Weak<Held>>) {
    let min_depth = usize::from(top == home);
    let max_depth = MAX_DEPTH.saturating_sub(depth(top, home).min(MAX_DEPTH));
    for entry in walkdir::WalkDir::new(top)
        .min_depth(min_depth)
        .max_depth(max_depth)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !should_skip(entry.path(), home))
        .filter_map(Result::ok)
        .take(room)
    {
        names.insert(entry.path().to_path_buf(), entry.file_type().is_dir());
        if let Some(kept) = kept
            && names.len() >= 1024
        {
            let Some(held) = kept.upgrade() else {
                return;
            };
            held.write().append(names);
        }
    }
}

/// Take in one change the watch reported.
fn take(home: &Path, held: &Arc<Held>, event: &WatchEvent) {
    let path = &event.path;
    if event.kind == ChangeKind::Removed {
        held.forget(path);
        return;
    }
    if !reached(path, home) {
        return;
    }
    // A creation where the platform does not tell one apart arrives as a
    // change: what the path is now decides.
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {
            // A folder moved in brings its names; the watch reports only
            // the folder.
            let room = MAX_NAMES.saturating_sub(held.read().len());
            let mut found = Names::new();
            walk(home, path, room, &mut found, None);
            held.write().append(&mut found);
        }
        Ok(_) => {
            let mut names = held.write();
            if names.len() < MAX_NAMES || names.contains_key(path) {
                names.insert(path.clone(), false);
            }
        }
        Err(_) => held.forget(path),
    }
}

impl Held {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, Names> {
        self.names.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Names> {
        self.names.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Forget `path` and everything below it.
    fn forget(&self, path: &Path) {
        let mut names = self.write();
        let below: Vec<PathBuf> = names
            .range::<Path, _>((std::ops::Bound::Included(path), std::ops::Bound::Unbounded))
            .map(|(name, _)| name)
            .take_while(|name| name.starts_with(path))
            .cloned()
            .collect();
        for name in below {
            names.remove(&name);
        }
    }
}

/// The `file://` address of `path`.
#[must_use]
pub fn file_uri(path: &Path) -> String {
    use std::fmt::Write as _;
    let mut uri = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                uri.push(char::from(byte));
            }
            other => {
                let _ = write!(uri, "%{other:02X}");
            }
        }
    }
    uri
}

pub struct Files {
    home: PathBuf,
    names: FileNames,
    content: Option<Arc<FilesIndex>>,
}

impl Files {
    /// Search `names` under `home`, and what the files hold through
    /// `content` when the host keeps that index.
    #[must_use]
    pub fn new(home: PathBuf, names: FileNames, content: Option<Arc<FilesIndex>>) -> Self {
        Self {
            home,
            names,
            content,
        }
    }

    fn hit(&self, path: &Path, folder: bool, score: i32) -> Hit {
        let title = path
            .file_name()
            .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy())
            .into_owned();
        let place = path
            .parent()
            .map(|parent| parent.strip_prefix(&self.home).unwrap_or(parent))
            .map(|parent| parent.to_string_lossy().into_owned())
            .filter(|place| !place.is_empty());
        Hit {
            kind: if folder {
                ResourceKind::Folder
            } else {
                ResourceKind::File
            },
            provider: self.id().into(),
            id: path.to_string_lossy().into_owned(),
            title,
            subtitle: place,
            facts: Vec::new(),
            score,
            icon: None,
            target: Target::Open {
                uri: file_uri(path),
                fallback: None,
            },
        }
    }
}

impl Source for Files {
    fn id(&self) -> &'static str {
        "files"
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        if needle.trim().is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<Hit> = self
            .names
            .0
            .read()
            .iter()
            .filter_map(|(path, folder)| {
                let file = path.file_name()?.to_string_lossy();
                let score = rank_fields(needle, &[(&file, FieldWeight::TITLE)])?;
                Some(self.hit(path, *folder, score))
            })
            .collect();
        hits.sort_by_key(|hit| std::cmp::Reverse(hit.score));
        hits.truncate(MAX_ROWS);
        if let Some(content) = &self.content {
            for found in content.query(needle, MAX_ROWS) {
                let id = found.path.to_string_lossy();
                if hits.iter().any(|hit| hit.id == id) {
                    continue;
                }
                hits.push(self.hit(&found.path, false, CONTENT_SCORE.min(MAX_PROVIDER_SCORE)));
            }
        }
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_found_by_name_and_opens_by_its_address() {
        let home = PathBuf::from("/home/mara");
        let names = FileNames::from_paths([
            (home.join("Documents/Budget 2026.ods"), false),
            (home.join("Documents"), true),
        ]);
        let files = Files::new(home, names, None);
        let hits = files.query("budget");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].subtitle.as_deref(), Some("Documents"));
        assert_eq!(
            hits[0].target,
            Target::Open {
                uri: "file:///home/mara/Documents/Budget%202026.ods".into(),
                fallback: None
            }
        );
        assert_eq!(files.query("documents")[0].kind, ResourceKind::Folder);
        assert!(files.query("").is_empty());
    }

    /// The titles `files` finds for `needle` once `done` holds of them, at
    /// most as long as a loaded machine takes to report a change.
    fn found_when(files: &Files, needle: &str, done: impl Fn(&[&str]) -> bool) -> Vec<String> {
        let started = std::time::Instant::now();
        loop {
            let titles: Vec<String> = files
                .query(needle)
                .into_iter()
                .map(|hit| hit.title)
                .collect();
            let borrowed: Vec<&str> = titles.iter().map(String::as_str).collect();
            if done(&borrowed) || started.elapsed() > std::time::Duration::from_secs(10) {
                return titles;
            }
            std::thread::park_timeout(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn the_walk_finds_names_without_hidden_folders() {
        let home = tempfile::tempdir().expect("a home");
        std::fs::create_dir_all(home.path().join("Notes")).expect("a folder");
        std::fs::create_dir_all(home.path().join(".cache")).expect("a cache");
        std::fs::write(home.path().join("Notes/plan.txt"), "x").expect("a file");
        std::fs::write(home.path().join(".cache/plan-cache.txt"), "x").expect("a file");
        let names = FileNames::scan(home.path().to_path_buf());
        let files = Files::new(home.path().to_path_buf(), names, None);
        let titles = found_when(&files, "plan", |titles| !titles.is_empty());
        assert_eq!(titles, ["plan.txt"]);
    }

    #[test]
    fn names_follow_files_made_renamed_and_removed_after_the_walk() {
        let home = tempfile::tempdir().expect("a home");
        std::fs::create_dir_all(home.path().join("Notes")).expect("a folder");
        std::fs::write(home.path().join("Notes/plan.txt"), "x").expect("a file");
        let names = FileNames::scan(home.path().to_path_buf());
        let files = Files::new(home.path().to_path_buf(), names, None);
        assert_eq!(found_when(&files, "plan", |t| !t.is_empty()), ["plan.txt"]);

        std::fs::write(home.path().join("Notes/budget.ods"), "x").expect("a new file");
        assert_eq!(
            found_when(&files, "budget", |t| !t.is_empty()),
            ["budget.ods"]
        );

        // A folder moved in from one search skips brings what it holds;
        // what stays in the skipped one is not kept.
        let staging = home.path().join(".staging");
        std::fs::create_dir_all(staging.join("Trips/2026")).expect("a folder");
        std::fs::write(staging.join("Trips/2026/itinerary.md"), "x").expect("a file");
        std::fs::write(staging.join("itinerary-old.md"), "x").expect("a file");
        std::fs::rename(staging.join("Trips"), home.path().join("Trips")).expect("a move");
        assert_eq!(
            found_when(&files, "itinerary", |t| !t.is_empty()),
            ["itinerary.md"]
        );

        std::fs::rename(
            home.path().join("Notes/plan.txt"),
            home.path().join("Notes/schedule.txt"),
        )
        .expect("a rename");
        assert!(found_when(&files, "plan", |t| t.is_empty()).is_empty());
        assert_eq!(
            found_when(&files, "schedule", |t| !t.is_empty()),
            ["schedule.txt"]
        );

        std::fs::remove_dir_all(home.path().join("Trips")).expect("a folder removed");
        assert!(found_when(&files, "itinerary", |t| t.is_empty()).is_empty());
        assert!(found_when(&files, "2026", |t| t.is_empty()).is_empty());
    }
}
