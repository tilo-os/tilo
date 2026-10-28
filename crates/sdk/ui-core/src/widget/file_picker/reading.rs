//! What the chooser asks of the filesystem and the search index, answered
//! on a worker thread: the UI thread lists, checks and searches nothing
//! itself. A folder's rows, Recent and a search come back as [`Row`]s; a
//! path the person is about to give the app (a typed location, a Save name,
//! the selection on Open) comes back as a [`Real`] for each path, and the
//! chooser acts on it when it arrives.

use super::PickerSearch;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::task::Waker;

/// What a path is once links are followed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Kind {
    #[default]
    Nothing,
    /// A link to nothing: the name is taken, but there is nothing to open.
    Dangling,
    File,
    Folder,
    /// A device, socket or pipe.
    Other,
}

/// What a path is, and whether its real path (links followed, `..` gone)
/// lies beneath the real path of one of the chooser's roots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Real {
    pub(super) kind: Kind,
    pub(super) inside: bool,
}

impl Real {
    pub(super) fn is_file_inside(self) -> bool {
        self.kind == Kind::File && self.inside
    }

    /// A folder the chooser may give or go into.
    pub(super) fn is_folder_inside(self) -> bool {
        self.kind == Kind::Folder && self.inside
    }

    /// Something is there, links followed, inside the roots.
    pub(super) fn exists_inside(self) -> bool {
        matches!(self.kind, Kind::File | Kind::Folder | Kind::Other) && self.inside
    }
}

pub(super) struct Row {
    pub(super) path: PathBuf,
    /// A folder to go into. In a folder this is the entry itself (a link
    /// to a folder is not one); for Recent and a search, links followed.
    pub(super) is_dir: bool,
    pub(super) meta: Option<fs::Metadata>,
    pub(super) real: Real,
}

/// What a listing reads.
pub(super) enum Source {
    /// The folder in view.
    Folder(PathBuf),
    /// Paths already known: Recent.
    Paths(Vec<PathBuf>),
    /// A system search for the query.
    Search(PickerSearch, String),
}

/// A listing's answer: its rows, and what the folder in view is.
pub(super) struct Listed {
    pub(super) rows: Vec<Row>,
    pub(super) here: Option<Real>,
}

/// An answer on its way from the worker.
pub(super) struct Pending<T> {
    pub(super) answer: Receiver<T>,
}

/// The worker went away without answering.
#[derive(Debug)]
pub(super) struct Gone;

impl<T> Pending<T> {
    /// The answer, once it is there; [`Gone`] when the worker went away
    /// without one, so the chooser stops waiting.
    pub(super) fn take(&self) -> Option<Result<T, Gone>> {
        match self.answer.try_recv() {
            Ok(answer) => Some(Ok(answer)),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err(Gone)),
        }
    }
}

/// Run `job` on the toolkit's worker and send its answer back, waking the
/// window's wake scope and the host's own waker when it gave one (a
/// compositor has no scope). `None` when no worker could take it.
fn on_worker<T: Send + 'static>(
    name: &'static str,
    waker: Option<Waker>,
    job: impl FnOnce() -> T + Send + 'static,
) -> Option<Pending<T>> {
    let (send, answer) = crate::wake::waking_channel();
    crate::background::Background::current()
        .spawn(name, move || {
            let _ = send.send(job());
            if let Some(waker) = waker {
                waker.wake();
            }
        })
        .ok()
        .map(|()| Pending { answer })
}

pub(super) fn list(
    source: Source,
    roots: Vec<PathBuf>,
    limit: usize,
    waker: Option<Waker>,
) -> Option<Pending<Listed>> {
    on_worker("tilo-file-picker-list", waker, move || {
        let roots = Roots::read(&roots);
        match source {
            Source::Folder(folder) => roots.folder(&folder),
            Source::Paths(paths) => roots.paths(paths, limit),
            Source::Search(search, query) => roots.paths(search(&query), limit),
        }
    })
}

/// Check `paths` on the worker, in order.
pub(super) fn check(
    paths: Vec<PathBuf>,
    roots: Vec<PathBuf>,
    waker: Option<Waker>,
) -> Option<Pending<Vec<Real>>> {
    on_worker("tilo-file-picker-check", waker, move || {
        let roots = Roots::read(&roots);
        paths.iter().map(|path| roots.real(path)).collect()
    })
}

/// The chooser's roots as given and as their real paths, read once per
/// question.
struct Roots {
    given: Vec<PathBuf>,
    real: Vec<PathBuf>,
}

impl Roots {
    fn read(given: &[PathBuf]) -> Self {
        let real = given
            .iter()
            .filter_map(|root| fs::canonicalize(root).ok())
            .filter(|root| root.is_dir())
            .collect();
        Self {
            given: given.to_vec(),
            real,
        }
    }

    fn holds_real(&self, real: &Path) -> bool {
        self.real.iter().any(|root| real.starts_with(root))
    }

    /// What `path` is, links followed, and whether it really lies inside.
    fn real(&self, path: &Path) -> Real {
        let kind = match fs::metadata(path) {
            Ok(meta) if meta.is_dir() => Kind::Folder,
            Ok(meta) if meta.is_file() => Kind::File,
            Ok(_) => Kind::Other,
            Err(_) if fs::symlink_metadata(path).is_ok() => Kind::Dangling,
            Err(_) => Kind::Nothing,
        };
        let inside = kind != Kind::Nothing
            && kind != Kind::Dangling
            && fs::canonicalize(path).is_ok_and(|real| self.holds_real(&real));
        Real { kind, inside }
    }

    /// Every entry of `folder`. An entry that is not a link really lies
    /// where the folder really is, so only links are resolved one by one.
    fn folder(&self, folder: &Path) -> Listed {
        let here = self.real(folder);
        let folder_real = fs::canonicalize(folder).ok();
        let mut rows = Vec::new();
        if let Ok(read) = fs::read_dir(folder) {
            for entry in read.flatten() {
                let path = entry.path();
                let file_type = entry.file_type().ok();
                let is_dir = file_type.is_some_and(|t| t.is_dir());
                let meta = entry.metadata().ok();
                let real = match file_type {
                    Some(t) if !t.is_symlink() => Real {
                        kind: if t.is_dir() {
                            Kind::Folder
                        } else if t.is_file() {
                            Kind::File
                        } else {
                            Kind::Other
                        },
                        inside: folder_real
                            .as_deref()
                            .is_some_and(|real| self.holds_real(real)),
                    },
                    _ => self.real(&path),
                };
                rows.push(Row {
                    path,
                    is_dir,
                    meta,
                    real,
                });
            }
        }
        Listed {
            rows,
            here: Some(here),
        }
    }

    /// The paths Recent or a search named that still exist under a root,
    /// at most twice `limit` of them, so the chooser can still drop some.
    fn paths(&self, paths: Vec<PathBuf>, limit: usize) -> Listed {
        let rows = paths
            .into_iter()
            .filter(|path| self.given.iter().any(|root| path.starts_with(root)))
            .take(limit.saturating_mul(2))
            .filter_map(|path| {
                let meta = fs::metadata(&path).ok()?;
                let real = self.real(&path);
                Some(Row {
                    is_dir: meta.is_dir(),
                    meta: Some(meta),
                    real,
                    path,
                })
            })
            .collect();
        Listed { rows, here: None }
    }
}
