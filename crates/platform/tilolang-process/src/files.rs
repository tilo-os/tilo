//! The device's files, as an app's `file.choose`, `file.save` and `.text()` reach them: the
//! device content store the bytes are kept in, and the person's own chooser.
//!
//! Both arrive with the instance, over connections the session opens as it opens a launched
//! app's: the store is the app's in its workspace, and the chooser is the session's own. A
//! chooser is the desktop host's, never one this process draws: the request goes over that
//! connection and the answer comes back on it. The instance's import is synchronous, so this
//! waits for that answer on the window's thread; the picker itself does its work on the
//! runtime's own threads, so nothing deadlocks, and the person sees a modal chooser over
//! their window while it is open.

use std::time::Duration;

use tilo_content::ContentStore;
use tilo_dialogs::FilePicker;
use tilo_dialogs::portal::{ContentsOutcome, FileFilter, PickRequest, SaveOutcome};
use tilolang_tilo::runtime::blob::{
    FileRequest, FileValue, MAX_CHOSEN, SaveRequest, accepts, media_of,
};

/// What a person asking for a file is told when the session opened no chooser for the app: the
/// truth where nobody is at the computer, and what an action can do instead.
pub const NO_CHOOSER: &str = "nobody is at this computer to choose a file; name the file instead";

/// What a person choosing a file is told on a device that keeps no content store: the file
/// could not be kept, which is what they need to know, not the name of what is missing.
pub const NO_CONTENT_STORE: &str = "this computer cannot keep the file you chose; try again later";

/// The content at `hash`, when this device holds it.
pub fn read(store: &ContentStore, hash: &str) -> Option<Vec<u8>> {
    store.read(hash)
}

/// Ask the person for a file and take it into the content store: what `file.choose` runs.
///
/// # Errors
///
/// Why there is no file: no chooser answers here, the chooser refused, or the content could not
/// be kept.
pub fn choose(
    store: &ContentStore,
    chooser: &dyn FilePicker,
    request: &FileRequest,
) -> Result<Option<FileValue>, String> {
    // The confirm button keeps the chooser's own word (Open): the types it offers name its
    // filter, never what pressing the button does.
    let mut pick = PickRequest::open(&request.title, "")
        .max_contents_bytes(MAX_CHOSEN)
        .thread_name("tilolang-choose-file");
    if !request.accept.is_empty() {
        let mut filter = FileFilter::new(&filter_name(&request.accept));
        for media in &request.accept {
            filter = filter.mimetype(media);
        }
        pick = pick.filter(filter);
    }
    let chosen = match wait(&chooser.pick_contents(pick))? {
        ContentsOutcome::Cancelled => return Ok(None),
        ContentsOutcome::Failed(why) => return Err(why),
        ContentsOutcome::Picked(files) => files,
    };
    let Some(file) = chosen.into_iter().next() else {
        return Ok(None);
    };
    let media = file
        .media_type
        .clone()
        .unwrap_or_else(|| media_of(&file.name));
    if !accepts(&request.accept, &media) || file.bytes.len() > MAX_CHOSEN {
        // The chooser filters by type and reads no more than the limit; a host that answers
        // another file anyhow is answered for here, before anything is kept.
        return Err(format!("`{}` is not a file this asks for", file.name));
    }
    let hash = store
        .put(&file.bytes)
        .map_err(|error| format!("`{}` could not be kept: {error}", file.name))?;
    Ok(Some(FileValue::new(
        file.name,
        media,
        i64::try_from(file.bytes.len()).unwrap_or(i64::MAX),
        hash,
    )))
}

/// Keep `bytes` in the content store as a file called `name`: what a program that holds bytes
/// makes of them, such as the files an archive a person chose holds.
///
/// # Errors
///
/// Why the bytes were not kept: too large a file, or the store's failure.
pub fn keep(
    store: &ContentStore,
    name: &str,
    media: &str,
    bytes: &[u8],
) -> Result<FileValue, String> {
    if bytes.len() > MAX_CHOSEN {
        return Err(format!(
            "`{name}` is {} bytes: more than the {MAX_CHOSEN} a file may be",
            bytes.len()
        ));
    }
    let hash = store
        .put(bytes)
        .map_err(|error| format!("`{name}` could not be kept: {error}"))?;
    Ok(FileValue::new(
        name,
        media,
        i64::try_from(bytes.len()).unwrap_or(i64::MAX),
        hash,
    ))
}

/// Put `content` where the person says: what `file.save` runs.
///
/// # Errors
///
/// Why it was not written: the chooser refused, or the write failed.
pub fn save(chooser: &dyn FilePicker, request: &SaveRequest, content: &[u8]) -> Result<(), String> {
    let title = format!("Save {}", request.name);
    // The confirm button reads Save, not the file's media type.
    let pick = PickRequest::save(&title, "", &request.name).thread_name("tilolang-save-file");
    match wait(&chooser.save_contents(pick, content.to_vec()))? {
        SaveOutcome::Saved | SaveOutcome::Cancelled => Ok(()),
        SaveOutcome::Failed(why) => Err(why),
    }
}

/// The name a chooser's filter shows for the types `accept` asks for.
fn filter_name(accept: &[String]) -> String {
    match accept.first().and_then(|media| media.split_once('/')) {
        Some(("image", _)) => "Images".to_owned(),
        Some(("text", _)) => "Text files".to_owned(),
        Some(("audio", _)) => "Audio".to_owned(),
        Some(("video", _)) => "Video".to_owned(),
        Some(_) => "Files".to_owned(),
        None => "Any file".to_owned(),
    }
}

/// How long a chooser may take before the answer is a failure: a person may look at their
/// files for a while, so it is generous, but a picker that never answers must not stop the
/// window for good.
const WAIT: Duration = Duration::from_secs(10 * 60);

/// The picker's answer, or why there is none.
fn wait<T>(answers: &std::sync::mpsc::Receiver<T>) -> Result<T, String> {
    answers
        .recv_timeout(WAIT)
        .map_err(|_| "the chooser did not answer".to_owned())
}

/// One entry of a folder, as `host.FileEntry` reads it.
pub use tilolang_tilo::runtime::files::Entry;

/// The entry `path` is, as `host.FileEntry` reads it: what its folder shows, its size, when its
/// content last changed, and whether its name is hidden.
fn entry(path: &std::path::Path) -> Result<Entry, String> {
    let meta = std::fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        });
    Ok(Entry {
        hidden: name.starts_with('.'),
        name,
        path: path.to_string_lossy().into_owned(),
        folder: meta.is_dir(),
        size: if meta.is_dir() {
            0
        } else {
            i64::try_from(meta.len()).unwrap_or(i64::MAX)
        },
        modified,
    })
}

/// The entries of the folder `path`: what `File.list` reads, in the order the disk answers.
///
/// # Errors
///
/// Why it cannot be read (it is not a folder, the sandbox refuses it, it is gone).
pub fn list(path: &std::path::Path) -> Result<Vec<Entry>, String> {
    let reading =
        std::fs::read_dir(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut entries = Vec::new();
    for item in reading {
        // A name the reader cannot take is one entry's loss, never the folder's.
        let Ok(item) = item else {
            continue;
        };
        if let Ok(entry) = entry(&item.path()) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// The entry `path` is, or none when nothing is there.
///
/// # Errors
///
/// Why the disk could not be asked (the sandbox refuses it, it is unreadable).
pub fn stat(path: &std::path::Path) -> Result<Option<Entry>, String> {
    match std::fs::metadata(path) {
        Ok(_) => entry(path).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// The bytes `path` holds: what `File.read` reads.
///
/// # Errors
///
/// Why they cannot be read (it is a folder, the sandbox refuses it, it is gone).
pub fn read_bytes(path: &std::path::Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))
}

/// The folders this instance watches, and the action each runs when one changes.
///
/// A watch is the platform's own: `inotify` on Linux, `FSEvents` on macOS and
/// `ReadDirectoryChangesW` on Windows, through `notify`; a mount whose entries change without
/// passing through this kernel (FUSE, NFS, SMB) is polled, and so is a folder whose watcher cannot
/// be made. The handler runs on the watcher's thread: it records that the folder changed and wakes
/// the host, and the frame that follows runs the action. Nothing is polled or woken while nothing
/// changes.
pub struct Watches {
    inner: std::sync::Mutex<Inner>,
    /// What wakes the host when a watched folder changes.
    wake: tilolang_tilo::background::Waker,
}

#[derive(Default)]
struct Inner {
    next: u64,
    watches: std::collections::BTreeMap<String, Watched>,
}

/// The registry's state, read even if a thread panicked while holding it: a watch that failed
/// somewhere else must not make every later read fail.
fn locked<T>(inner: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    inner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct Watched {
    /// The action run when it changes, by its name in the package.
    run: String,
    /// The member it runs for.
    actor: String,
    /// Set from the watcher's thread when the folder changed, and taken by the frame that runs
    /// the action.
    changed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// What watches the folder; dropping it stops the watch.
    _watcher: Box<dyn notify::Watcher + Send>,
}

impl Watches {
    /// Watches waking `wake` when a folder they watch changes.
    #[must_use]
    pub fn new(wake: tilolang_tilo::background::Waker) -> Self {
        Self {
            inner: std::sync::Mutex::new(Inner::default()),
            wake,
        }
    }

    /// Watch `path` for the action `run`, run as `actor`, and answer the token that names the
    /// watch.
    ///
    /// # Errors
    ///
    /// Why it cannot be watched (it is not a folder, the sandbox refuses it, no watcher can be
    /// made for it).
    pub fn watch(&self, path: &str, run: &str, actor: &str) -> Result<String, String> {
        let folder = std::path::Path::new(path);
        let meta = std::fs::metadata(folder).map_err(|error| format!("{path}: {error}"))?;
        if !meta.is_dir() {
            return Err(format!("{path} is not a folder"));
        }
        let changed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watcher = watcher_for(
            folder,
            std::sync::Arc::clone(&changed),
            std::sync::Arc::clone(&self.wake),
        )?;
        let mut inner = locked(&self.inner);
        inner.next = inner.next.saturating_add(1);
        let token = format!("watch-{}", inner.next);
        inner.watches.insert(
            token.clone(),
            Watched {
                run: run.to_owned(),
                actor: actor.to_owned(),
                changed,
                _watcher: watcher,
            },
        );
        drop(inner);
        Ok(token)
    }

    /// Stop the watch `token` names.
    pub fn unwatch(&self, token: &str) {
        let mut inner = locked(&self.inner);
        inner.watches.remove(token);
    }

    /// Whether anything is watched.
    #[must_use]
    pub fn watching(&self) -> bool {
        let inner = locked(&self.inner);
        !inner.watches.is_empty()
    }

    /// The watches whose folder changed since they last ran, each with the action to run and the
    /// member it runs for, their changes taken up to now.
    #[must_use]
    pub fn take_changed(&self) -> Vec<(String, String)> {
        let inner = locked(&self.inner);
        let mut due = Vec::new();
        for watched in inner.watches.values() {
            if watched
                .changed
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                due.push((watched.run.clone(), watched.actor.clone()));
            }
        }
        drop(inner);
        due
    }
}

/// The watcher for `folder`: the platform's own, or a poll for a mount whose entries change
/// without passing through this kernel, or where the platform's watcher cannot be made.
fn watcher_for(
    folder: &std::path::Path,
    changed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    wake: tilolang_tilo::background::Waker,
) -> Result<Box<dyn notify::Watcher + Send>, String> {
    // A read of an entry's metadata is not a change of the folder's listing.
    let handler = move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else {
            return;
        };
        if matches!(event.kind, notify::EventKind::Access(_)) {
            return;
        }
        changed.store(true, std::sync::atomic::Ordering::SeqCst);
        wake();
    };
    let mut watcher: Box<dyn notify::Watcher + Send> =
        if tilo_files::native::watch_sees_every_change(folder) {
            Box::new(notify::recommended_watcher(handler).map_err(|error| error.to_string())?)
        } else {
            Box::new(
                notify::PollWatcher::new(
                    handler,
                    notify::Config::default().with_poll_interval(std::time::Duration::from_secs(2)),
                )
                .map_err(|error| error.to_string())?,
            )
        };
    watcher
        .watch(folder, notify::RecursiveMode::NonRecursive)
        .map_err(|error| error.to_string())?;
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use tilo_dialogs::portal::{
        ContentsChannel, LaunchChannel, LaunchRequest, MultiChannel, OpenFileChannel, PickRequest,
        PickedContents, PortalChannel,
    };

    use super::*;

    /// A channel nothing will answer: the requests a test never makes.
    fn unanswered<T>() -> std::sync::mpsc::Receiver<T> {
        std::sync::mpsc::channel().1
    }

    #[test]
    fn a_choosers_filter_is_named_for_what_it_offers() {
        assert_eq!(filter_name(&[]), "Any file");
        assert_eq!(filter_name(&["image/png".to_owned()]), "Images");
        assert_eq!(filter_name(&["text/markdown".to_owned()]), "Text files");
        assert_eq!(filter_name(&["application/pdf".to_owned()]), "Files");
    }

    /// A chooser that answers every contents pick with `name` and `bytes`: the person picking a
    /// file, without a desktop.
    struct Picks(
        &'static str,
        &'static [u8],
        std::sync::Mutex<Option<PickRequest>>,
    );

    impl Picks {
        fn new(name: &'static str, bytes: &'static [u8]) -> Self {
            Self(name, bytes, std::sync::Mutex::new(None))
        }
    }

    impl FilePicker for Picks {
        fn pick(&self, _: PickRequest) -> PortalChannel {
            unanswered()
        }

        fn pick_multiple(&self, _: PickRequest) -> MultiChannel {
            unanswered()
        }

        fn open_file(&self, _: PickRequest) -> OpenFileChannel {
            unanswered()
        }

        fn launch(&self, _: LaunchRequest) -> LaunchChannel {
            unanswered()
        }

        fn pick_contents(&self, request: PickRequest) -> ContentsChannel {
            *self.2.lock().unwrap() = Some(request);
            let (sender, receiver) = std::sync::mpsc::channel();
            let _ = sender.send(ContentsOutcome::Picked(vec![PickedContents {
                name: self.0.to_owned(),
                media_type: Some(media_of(self.0)),
                bytes: self.1.to_vec(),
                location: None,
            }]));
            receiver
        }
    }

    #[test]
    fn a_chosen_file_is_kept_in_the_store_the_instance_holds() {
        let store = ContentStore::memory();
        let chooser = Picks::new("Lisbon.md", b"# Lisbon in May\n");
        let chosen = choose(
            &store,
            &chooser,
            &FileRequest {
                title: "Import Markdown".to_owned(),
                name: String::new(),
                accept: vec!["text/markdown".to_owned()],
            },
        )
        .expect("the chooser answered")
        .expect("a file was chosen");
        assert_eq!(chosen.name, "Lisbon.md");
        assert_eq!(chosen.media, "text/markdown");
        let asked = chooser
            .2
            .lock()
            .unwrap()
            .take()
            .expect("the chooser was asked");
        assert_eq!(
            (asked.title(), asked.accept_label(), asked.filter_names()),
            ("Import Markdown", "", vec!["Text files"]),
            "the types name the filter; the confirm button keeps the chooser's own Open"
        );
        assert_eq!(
            store.read(&chosen.hash).as_deref(),
            Some(&b"# Lisbon in May\n"[..]),
            "the bytes the person chose are in the instance's own store"
        );
    }

    #[test]
    fn a_file_another_type_is_refused_before_it_is_kept() {
        let store = ContentStore::memory();
        let chooser = Picks::new("photo.png", b"\x89PNG");
        let refused = choose(
            &store,
            &chooser,
            &FileRequest {
                title: "Import Markdown".to_owned(),
                name: String::new(),
                accept: vec!["text/markdown".to_owned()],
            },
        );
        assert_eq!(
            refused,
            Err("`photo.png` is not a file this asks for".to_owned())
        );
        assert!(
            !store.has(&tilo_content::hash_bytes(b"\x89PNG")),
            "nothing was kept"
        );
    }

    #[test]
    fn a_folder_lists_its_entries_and_reads_their_bytes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("books")).unwrap();
        std::fs::write(dir.path().join("a.epub"), b"EPUB").unwrap();
        let mut listed: Vec<(String, bool)> = list(dir.path())
            .unwrap()
            .into_iter()
            .map(|entry| (entry.name, entry.folder))
            .collect();
        listed.sort();
        assert_eq!(
            listed,
            vec![("a.epub".to_owned(), false), ("books".to_owned(), true)]
        );
        let found = stat(&dir.path().join("a.epub")).unwrap().unwrap();
        assert_eq!(found.size, 4, "its bytes' length");
        assert_eq!(
            read_bytes(&dir.path().join("a.epub")).unwrap(),
            b"EPUB",
            "what File.read reads"
        );
        assert!(stat(&dir.path().join("gone")).unwrap().is_none());
        assert!(read_bytes(dir.path()).is_err(), "a folder is not a file");
    }

    #[test]
    fn a_watched_folder_that_gains_an_entry_wakes_and_runs_its_action() {
        let dir = tempfile::tempdir().unwrap();
        let (woken, woke) = std::sync::mpsc::channel::<()>();
        let woken = std::sync::Mutex::new(woken);
        let watches = Watches::new(std::sync::Arc::new(move || {
            let _ = woken.lock().unwrap().send(());
        }));
        let token = watches
            .watch(dir.path().to_str().unwrap(), "scan", "person")
            .expect("the folder is watched");
        assert!(watches.watching());
        // Nothing changed: nothing wakes the app, and no action is due.
        assert!(
            woke.recv_timeout(std::time::Duration::from_millis(300))
                .is_err(),
            "an idle watch does not wake the app"
        );
        assert!(watches.take_changed().is_empty(), "nothing changed yet");
        std::fs::write(dir.path().join("new.epub"), b"EPUB").unwrap();
        woke.recv_timeout(std::time::Duration::from_secs(10))
            .expect("the new entry wakes the app");
        assert_eq!(
            watches.take_changed(),
            vec![("scan".to_owned(), "person".to_owned())],
            "the action runs for the member"
        );
        assert!(
            watches.take_changed().is_empty(),
            "a change runs its action once"
        );
        watches.unwatch(&token);
        assert!(!watches.watching());
    }

    #[test]
    fn bytes_a_program_makes_are_kept_too() {
        let store = ContentStore::memory();
        let kept = keep(&store, "note.md", "text/markdown", b"hello").expect("kept");
        assert_eq!(store.read(&kept.hash).as_deref(), Some(&b"hello"[..]));
        let oversized = vec![0_u8; MAX_CHOSEN + 1];
        assert!(keep(&store, "big.zip", "application/zip", &oversized).is_err());
    }
}
