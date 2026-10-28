//! "New File" **templates**: the files in the user's Templates directory
//! (`$XDG_TEMPLATES_DIR`, else `~/Templates`). Each becomes a "New …" entry in
//! the right-click menu that copies the template into the current directory —
//! the standard Nautilus / Finder "New Document from template" affordance.
//!
//! When the Templates directory has no files the whole submenu is hidden (see
//! [`crate::files_app`]), so a user with no templates never sees an empty menu.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Wake, Waker};
use std::thread::Thread;

use ui::lock::MutexRecover as _;

/// The Templates directory: `$XDG_TEMPLATES_DIR` when set (and non-empty),
/// otherwise `~/Templates`. `None` only when there is no home directory.
pub fn templates_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("XDG_TEMPLATES_DIR") {
        let p = p.trim();
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    tilo_paths::home().map(|h| h.join("Templates"))
}

/// The template files inside `dir`: regular files only (directories and
/// dotfiles are skipped), sorted by name for a stable menu order. An absent or
/// unreadable directory yields an empty list.
pub fn list_templates(dir: &Path) -> Vec<PathBuf> {
    ui::storage::waiting::refused("listing the Templates folder");
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            if entry.file_type().is_ok_and(|t| t.is_file()) {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    out
}

/// The template files of one Templates folder, as the menus and the
/// published rows read them.
///
/// On the compositor's frame loop, which never waits on the disk, a worker
/// thread lists the folder and lists it again when its watch reports a
/// change (or, while the folder does not exist, when one of its ancestors
/// changes), and the window reads the last list from memory. Elsewhere, in a
/// test or a standalone window, the folder is listed when asked.
pub(crate) struct Catalog {
    dir: Option<PathBuf>,
    wake: ui::WakeHandle,
    /// The worker, started the first time the catalog is read on a frame
    /// loop: a window built before its thread became one still reads it
    /// from memory there.
    live: std::cell::OnceCell<Option<Live>>,
}

/// The worker's last list, and how to stop it.
struct Live {
    shared: Arc<Shared>,
    worker: Thread,
}

struct Shared {
    templates: Mutex<Vec<PathBuf>>,
    stopped: AtomicBool,
}

impl Catalog {
    /// The catalog of `dir`, or an empty one for `None`. `wake` wakes the
    /// window when a worker's list changes.
    pub(crate) fn new(dir: Option<PathBuf>, wake: ui::WakeHandle) -> Self {
        let catalog = Self {
            dir,
            wake,
            live: std::cell::OnceCell::new(),
        };
        // On a frame loop the worker starts at once, so the first menu
        // already has the list.
        if ui::storage::waiting::forbidden() {
            let _ = catalog.live();
        }
        catalog
    }

    fn live(&self) -> Option<&Live> {
        self.live
            .get_or_init(|| {
                let dir = self.dir.clone()?;
                Live::start(dir, self.wake.clone())
            })
            .as_ref()
    }

    /// The template files, sorted by name. Listed when asked, except on a
    /// frame loop, where the worker's last list is read.
    pub(crate) fn templates(&self) -> Vec<PathBuf> {
        if ui::storage::waiting::forbidden() || self.live.get().is_some() {
            return self
                .live()
                .map(|live| live.shared.templates.lock_recover().clone())
                .unwrap_or_default();
        }
        self.dir.as_deref().map(list_templates).unwrap_or_default()
    }
}

impl Live {
    fn start(dir: PathBuf, wake: ui::WakeHandle) -> Option<Self> {
        let shared = Arc::new(Shared {
            templates: Mutex::new(Vec::new()),
            stopped: AtomicBool::new(false),
        });
        let kept = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("files-templates".into())
            .spawn(move || keep_listing(&dir, &kept, &wake))
            .inspect_err(|error| tracing::warn!(%error, "templates worker did not start"))
            .ok()?;
        Some(Self {
            shared,
            worker: worker.thread().clone(),
        })
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        // The worker may be listing: it is not waited for, only told to stop.
        self.shared.stopped.store(true, Ordering::Release);
        self.worker.unpark();
    }
}

/// Unparks the worker when its watch has news.
struct Unpark(Thread);

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

fn keep_listing(dir: &Path, shared: &Shared, wake: &ui::WakeHandle) {
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut watch = crate::directory_watch::AwaitedDirectoryWatch::with_waker(waker);
    // Watch before listing, so a change between the two is listed again
    // rather than lost.
    let _ = watch.point_at(Some(dir));
    while !shared.stopped.load(Ordering::Acquire) {
        let listed = list_templates(dir);
        let changed = {
            let mut templates = shared.templates.lock_recover();
            let changed = *templates != listed;
            *templates = listed;
            changed
        };
        if changed {
            wake.wake();
        }
        while !watch.take_changes() {
            if shared.stopped.load(Ordering::Acquire) {
                return;
            }
            std::thread::park();
        }
    }
}

/// The menu label for a template file: `New <stem>` (the extension is dropped
/// from the label but kept when the file is created).
pub fn menu_label(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    crate::i18n::t_args("files-new-from-template", &[crate::i18n::arg("name", stem)])
}

/// Create a new file in `dest_dir` by copying `template` there under a
/// non-colliding name. Returns the created path. Delegates de-duplication to
/// [`crate::fileops::copy_into`] so `Untitled.md` → `Untitled 2.md`.
///
/// # Errors
///
/// Returns an error if `template` has no file name, or the underlying copy
/// fails (see [`crate::fileops::copy_into`]).
pub fn create_from_template(template: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    crate::fileops::copy_into(template, dest_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Templates list sorted files skipping dirs and dotfiles, empty dirs yield nothing, labels drop extensions and creation copies content.
    #[test]
    fn templates_list_label_and_create_files() {
        // --- phase: list_templates_returns_files_sorted_skipping_dirs_and_dotfiles ---
        {
            let d = tempfile::tempdir().unwrap();
            fs::write(d.path().join("Document.md"), b"# ${title}\n").unwrap();
            fs::write(d.path().join("Spreadsheet.csv"), b"a,b\n").unwrap();
            fs::write(d.path().join(".hidden"), b"x").unwrap();
            fs::create_dir(d.path().join("subdir")).unwrap();

            let t = list_templates(d.path());
            let names: Vec<String> = t
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
            assert_eq!(
                names,
                ["Document.md", "Spreadsheet.csv"],
                "files only, sorted, no dotfiles/dirs"
            );
        }

        // --- phase: empty_templates_dir_yields_nothing ---
        {
            let d = tempfile::tempdir().unwrap();
            assert!(list_templates(d.path()).is_empty());
            // A missing directory is also empty, not an error.
            assert!(list_templates(&d.path().join("nope")).is_empty());
        }

        // --- phase: a_catalog_off_the_frame_loop_lists_when_asked ---
        {
            let d = tempfile::tempdir().unwrap();
            let catalog = Catalog::new(Some(d.path().to_path_buf()), ui::WakeHandle::detached());
            assert!(catalog.templates().is_empty());
            fs::write(d.path().join("Note.md"), b"").unwrap();
            assert_eq!(catalog.templates(), [d.path().join("Note.md")]);
        }

        // --- phase: menu_label_drops_extension ---
        {
            assert_eq!(menu_label(Path::new("/t/Document.md")), "New Document");
            assert_eq!(menu_label(Path::new("/t/notes")), "New notes");
        }

        // --- phase: create_from_template_copies_content_into_dest ---
        {
            let d = tempfile::tempdir().unwrap();
            let tmpl = d.path().join("Document.md");
            fs::write(&tmpl, b"hello template").unwrap();
            let dest = d.path().join("dest");
            fs::create_dir(&dest).unwrap();

            let made = create_from_template(&tmpl, &dest).unwrap();
            assert_eq!(made.file_name().unwrap().to_string_lossy(), "Document.md");
            assert_eq!(fs::read(&made).unwrap(), b"hello template");
            // A second create de-duplicates the name.
            let made2 = create_from_template(&tmpl, &dest).unwrap();
            assert_eq!(
                made2.file_name().unwrap().to_string_lossy(),
                "Document 2.md"
            );
        }
    }

    /// On the frame loop, which never waits on the disk, the catalog is listed by its worker, kept current by a watch (here
    /// on a folder created only after the catalog started), and read from
    /// memory.
    #[test]
    fn a_catalog_on_the_frame_loop_is_kept_by_its_worker() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("Templates");
        let catalog = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _loop = ui::storage::waiting::forbid_strictly_on_this_thread();
                    let catalog = Catalog::new(Some(dir.clone()), ui::WakeHandle::detached());
                    // Reading it waits on nothing: this would panic otherwise.
                    assert!(catalog.templates().is_empty());
                    catalog
                })
                .join()
                .unwrap()
        });
        assert!(
            catalog.live.get().is_some_and(Option::is_some),
            "the frame loop's catalog has a worker"
        );
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("Letter.md"), b"").unwrap();
        tilo_test_env::wait_until("the worker to list the new template", || {
            catalog.templates() == [dir.join("Letter.md")]
        });
    }

    /// A window built before its thread became the frame loop's (one
    /// restored at startup) still never lists the folder on that thread.
    #[test]
    fn a_catalog_built_before_the_frame_loop_is_read_there_from_memory() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Memo.md"), b"").unwrap();
        let catalog = Catalog::new(Some(dir.path().to_path_buf()), ui::WakeHandle::detached());
        let catalog = std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    let _loop = ui::storage::waiting::forbid_strictly_on_this_thread();
                    // This would panic were the folder listed here.
                    let _ = catalog.templates();
                    catalog
                })
                .join()
                .unwrap()
        });
        tilo_test_env::wait_until("the worker to list the template", || {
            catalog.templates() == [dir.path().join("Memo.md")]
        });
    }
}
