//! New File templates: the files in the person's Templates directory.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Wake, Waker};
use std::thread::Thread;

use ui::lock::MutexRecover as _;

pub fn templates_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("XDG_TEMPLATES_DIR") {
        let p = p.trim();
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    tilo_paths::home().map(|h| h.join("Templates"))
}

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

pub(crate) struct Catalog {
    dir: Option<PathBuf>,
    wake: ui::WakeHandle,
    live: std::cell::OnceCell<Option<Live>>,
}

struct Live {
    shared: Arc<Shared>,
    worker: Thread,
}

struct Shared {
    templates: Mutex<Vec<PathBuf>>,
    stopped: AtomicBool,
}

impl Catalog {
    pub(crate) fn new(dir: Option<PathBuf>, wake: ui::WakeHandle) -> Self {
        let catalog = Self {
            dir,
            wake,
            live: std::cell::OnceCell::new(),
        };
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
        self.shared.stopped.store(true, Ordering::Release);
        self.worker.unpark();
    }
}

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

pub fn menu_label(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    crate::i18n::t_args("files-new-from-template", &[crate::i18n::arg("name", stem)])
}

pub fn create_from_template(template: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    crate::fileops::copy_into(template, dest_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn templates_list_label_and_create_files() {
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

        {
            let d = tempfile::tempdir().unwrap();
            assert!(list_templates(d.path()).is_empty());
            assert!(list_templates(&d.path().join("nope")).is_empty());
        }

        {
            let d = tempfile::tempdir().unwrap();
            let catalog = Catalog::new(Some(d.path().to_path_buf()), ui::WakeHandle::detached());
            assert!(catalog.templates().is_empty());
            fs::write(d.path().join("Note.md"), b"").unwrap();
            assert_eq!(catalog.templates(), [d.path().join("Note.md")]);
        }

        {
            assert_eq!(menu_label(Path::new("/t/Document.md")), "New Document");
            assert_eq!(menu_label(Path::new("/t/notes")), "New notes");
        }

        {
            let d = tempfile::tempdir().unwrap();
            let tmpl = d.path().join("Document.md");
            fs::write(&tmpl, b"hello template").unwrap();
            let dest = d.path().join("dest");
            fs::create_dir(&dest).unwrap();

            let made = create_from_template(&tmpl, &dest).unwrap();
            assert_eq!(made.file_name().unwrap().to_string_lossy(), "Document.md");
            assert_eq!(fs::read(&made).unwrap(), b"hello template");
            let made2 = create_from_template(&tmpl, &dest).unwrap();
            assert_eq!(
                made2.file_name().unwrap().to_string_lossy(),
                "Document 2.md"
            );
        }
    }

    #[test]
    fn a_catalog_on_the_frame_loop_is_kept_by_its_worker() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("Templates");
        let catalog = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _loop = ui::storage::waiting::forbid_strictly_on_this_thread();
                    let catalog = Catalog::new(Some(dir.clone()), ui::WakeHandle::detached());
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

    #[test]
    fn a_catalog_built_before_the_frame_loop_is_read_there_from_memory() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Memo.md"), b"").unwrap();
        let catalog = Catalog::new(Some(dir.path().to_path_buf()), ui::WakeHandle::detached());
        let catalog = std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    let _loop = ui::storage::waiting::forbid_strictly_on_this_thread();
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
