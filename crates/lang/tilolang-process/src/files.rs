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

use std::fmt::Write as _;
use std::time::Duration;

use tilo_content::ContentStore;
use tilo_dialogs::FilePicker;
use tilo_dialogs::portal::{
    FileFilter, MultiOutcome, OpenFileOutcome, OpenedFile, PickRequest, SaveOutcome,
};
use tilolang_tilo::runtime::blob::{
    FileRequest, FileValue, MAX_CHOSEN, SaveRequest, Saved, accepts, media_of,
};

/// What a person asking for a file is told when the session opened no chooser for the app: the
/// truth where nobody is at the computer, and what an action can do instead.
pub const NO_CHOOSER: &str = "nobody is at this computer to choose a file; name the file instead";

/// What a person is told when a file handed to the app cannot be read on a device that keeps no
/// content store: the file could not be opened, which is what they need to know, not the name of
/// what is missing. A file they choose themselves is kept in the window's own store instead.
pub const NO_CONTENT_STORE: &str = "this computer cannot keep the file you chose; try again later";

/// The content at `hash`, when this device holds it.
pub fn read(store: &ContentStore, hash: &str) -> Option<Vec<u8>> {
    store.read(hash)
}

/// Ask the person for files and take them into the content store: what `file.choose` and
/// `file.choose_folder` run. One file, several, or the files of a folder, as `request` asks;
/// what comes back is an empty list when the person dismissed the chooser.
///
/// # Errors
///
/// Why there are no files: no chooser answers here, the chooser refused, or the content could
/// not be kept.
pub fn choose(
    store: &ContentStore,
    chooser: &dyn FilePicker,
    request: &FileRequest,
) -> Result<Vec<FileValue>, String> {
    // The confirm button keeps the chooser's own word (Open): the types it offers name its
    // filter, never what pressing the button does.
    let filter = (!request.accept.is_empty()).then(|| {
        let mut filter = FileFilter::new(&filter_name(&request.accept));
        for media in &request.accept {
            filter = filter.mimetype(media);
        }
        filter
    });
    let named = |mut pick: PickRequest| {
        if let Some(filter) = filter.clone() {
            pick = pick.filter(filter);
        }
        pick
    };
    let chosen: Vec<(String, Vec<u8>)> = if request.folder {
        let pick =
            named(PickRequest::directory(&request.title, "")).thread_name("tilolang-choose-folder");
        match wait(&chooser.open_handle(pick))? {
            OpenFileOutcome::Cancelled => return Ok(Vec::new()),
            OpenFileOutcome::Failed(why) => return Err(why),
            OpenFileOutcome::Opened(opened) => {
                let folder = opened.into_file().into_std();
                tilo_dialogs::portal::walk_chosen_folder(&folder, MAX_CHOSEN)?
            }
        }
    } else if request.multiple {
        let pick = named(PickRequest::open_multiple(&request.title, ""))
            .thread_name("tilolang-choose-files");
        let references = match wait(&chooser.pick_multiple(pick))? {
            MultiOutcome::Cancelled => return Ok(Vec::new()),
            MultiOutcome::Failed(why) => return Err(why),
            MultiOutcome::Selected(paths) => paths,
        };
        let mut chosen = Vec::with_capacity(references.len());
        for reference in references {
            match wait(&chooser.reopen(&reference, false))? {
                OpenFileOutcome::Cancelled => {}
                OpenFileOutcome::Failed(why) => return Err(why),
                OpenFileOutcome::Opened(opened) => chosen.push(read_opened(opened)?),
            }
        }
        chosen
    } else {
        let pick = named(PickRequest::open(&request.title, "")).thread_name("tilolang-choose-file");
        match wait(&chooser.open_handle(pick))? {
            OpenFileOutcome::Cancelled => return Ok(Vec::new()),
            OpenFileOutcome::Failed(why) => return Err(why),
            OpenFileOutcome::Opened(opened) => vec![read_opened(opened)?],
        }
    };
    let mut kept = Vec::with_capacity(chosen.len());
    for (path, bytes) in chosen {
        // Where the file stands in what the person chose: a path inside a folder they chose, or
        // a file they chose itself. Never a host path, which a sandboxed app has no way to use.
        let name = std::path::Path::new(&path)
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .map_or_else(|| path.clone(), str::to_owned);
        let media = media_of(&name);
        if !accepts(&request.accept, &media) || bytes.len() > MAX_CHOSEN {
            // The chooser filters by type and reads no more than the limit; a host that answers
            // another file anyhow is answered for here, before anything is kept.
            return Err(format!("`{name}` is not a file this asks for"));
        }
        let hash = store
            .put(&bytes)
            .map_err(|error| format!("`{name}` could not be kept: {error}"))?;
        kept.push(
            FileValue::new(
                name,
                media,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash,
            )
            .with_path(path),
        );
    }
    Ok(kept)
}

/// The bytes of the file the chooser answered, read through the descriptor it opened.
fn read_opened(opened: OpenedFile) -> Result<(String, Vec<u8>), String> {
    let name = opened.display_name().to_owned();
    let mut file = opened.into_file();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes)
        .map_err(|error| format!("`{name}` could not be read: {error}"))?;
    Ok((name, bytes))
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

/// A preview of `file` no larger than `size` pixels a side, kept in the content store: what
/// `File.thumbnail` answers, generated once by the device's shared cache and byte for byte the
/// same on every call. `None` for a file that has no preview, which is no image (a badge is what
/// its list shows instead).
///
/// # Errors
///
/// Why there is no preview: a file that is not addressed content, or a preview that did not
/// encode or could not be kept.
pub fn thumbnail(
    store: &ContentStore,
    thumbnails: &thumbnail::ThumbnailService,
    file: &FileValue,
    size: u32,
) -> Result<Option<FileValue>, String> {
    let category = thumbnail::category_for_path(std::path::Path::new(&file.name));
    if !category.has_preview() {
        return Ok(None);
    }
    let edge = size.clamp(1, 4096);
    let dimension = thumbnail::ThumbSize::edge(edge);
    let Some(key) = thumbnail::ThumbKey::of_address(&file.hash, dimension) else {
        return Err(format!(
            "`{}` is not the address of content this device holds",
            file.name
        ));
    };
    let state = thumbnails.request(thumbnail::ThumbRequest::load(key, category, {
        let store = store.clone();
        let hash = file.hash.clone();
        move || store.read(&hash)
    }));
    let preview = match state {
        thumbnail::ThumbState::Ready(preview) => Some(preview),
        thumbnail::ThumbState::Badge(_) => None,
        thumbnail::ThumbState::Loading => {
            // The cache generates on its own workers; a preview is ready before the call
            // answers, as a file read is. One that never settles answers no preview.
            thumbnails.block_until_idle(Duration::from_secs(30));
            thumbnails
                .peek(&key)
                .and_then(|state| state.ready().cloned())
        }
    };
    let Some(preview) = preview else {
        return Ok(None);
    };
    let png = thumbnail::encode_png(&preview)
        .ok_or_else(|| format!("the preview of `{}` did not encode", file.name))?;
    let stem = std::path::Path::new(&file.name)
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|stem| !stem.is_empty())
        .unwrap_or("preview");
    keep(store, &format!("{stem}-{edge}.png"), "image/png", &png).map(Some)
}

/// A preview of the device file at `path`, no larger than `size` pixels a side, kept in the content
/// store: what `File.preview` answers, and how a folder of pictures lists with their previews
/// without the window reading every one. The device's shared cache keeps it under the file's path,
/// when it last changed and how large it is, so a file read again costs no decode.
///
/// # Errors
///
/// Why there is no preview: the file is gone or unreadable, or a preview that did not encode or
/// could not be kept.
pub fn preview(
    store: &ContentStore,
    thumbnails: &thumbnail::ThumbnailService,
    path: &std::path::Path,
    size: u32,
) -> Result<Option<FileValue>, String> {
    let category = thumbnail::category_for_path(path);
    if !category.has_preview() {
        return Ok(None);
    }
    let meta =
        std::fs::symlink_metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if !meta.is_file() {
        return Ok(None);
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        });
    let edge = size.clamp(1, 4096);
    let dimension = thumbnail::ThumbSize::edge(edge);
    let key = thumbnail::ThumbKey::of_path_meta(path, mtime, meta.len(), dimension);
    let state = thumbnails.request(thumbnail::ThumbRequest::file(
        key,
        category,
        path.to_path_buf(),
    ));
    let preview = match state {
        thumbnail::ThumbState::Ready(preview) => Some(preview),
        thumbnail::ThumbState::Badge(_) => None,
        thumbnail::ThumbState::Loading => {
            // The cache generates on its own workers; a preview is ready before the call answers,
            // as a file read is. One that never settles answers no preview.
            thumbnails.block_until_idle(Duration::from_secs(30));
            thumbnails
                .peek(&key)
                .and_then(|state| state.ready().cloned())
        }
    };
    let Some(preview) = preview else {
        return Ok(None);
    };
    let png = thumbnail::encode_png(&preview)
        .ok_or_else(|| format!("the preview of `{}` did not encode", path.display()))?;
    let stem = path
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|stem| !stem.is_empty())
        .unwrap_or("preview");
    keep(store, &format!("{stem}-{edge}.png"), "image/png", &png).map(Some)
}

/// Put `content` where the person says: what `file.save` runs, answering where it went or that
/// the chooser was closed.
///
/// # Errors
///
/// Why it was not written: the chooser refused, or the write failed.
pub fn save(
    chooser: &dyn FilePicker,
    request: &SaveRequest,
    content: &[u8],
) -> Result<Saved, String> {
    let title = format!("Save {}", request.name);
    // The confirm button reads Save, not the file's media type.
    let pick = PickRequest::save(&title, "", &request.name).thread_name("tilolang-save-file");
    match wait(&chooser.save_contents(pick, content.to_vec()))? {
        SaveOutcome::Saved { to } => Ok(Saved::At(to)),
        SaveOutcome::Cancelled => Ok(Saved::Cancelled),
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
pub use tilolang_tilo::runtime::files::{Entry, TrashEntry};

/// The entry `path` is, as `host.FileEntry` reads it: what its folder shows, its size, when its
/// content last changed, and whether its name is hidden.
fn entry(path: &std::path::Path) -> Result<Entry, String> {
    let meta = std::fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(described(
        name,
        path.to_string_lossy().into_owned(),
        Facts::of(&meta),
    ))
}

/// What an app's `File.list`, `File.stat`, `File.read` and `File.write` are told where the
/// session opened no chooser for it: the person's files are reached only through what they chose.
pub const NO_CHOSEN_FILES: &str =
    "nobody is at this computer to choose a folder for this app, so it reaches no files";

/// The file or folder at `path`, opened by the person's chooser: a reference the chooser answered
/// for this app, or a path inside a folder it answered. The chooser opens it only while the
/// person's grant to the app stands (a workspace's folder by the grant on its node), so nothing
/// else of this computer is reached, and a path it never answered is refused in its words. The
/// name is what the chooser calls it, which for a chosen folder is the folder's own name rather
/// than its reference.
fn chosen(
    chooser: &dyn FilePicker,
    path: &str,
    write: bool,
) -> Result<(std::fs::File, String), String> {
    match wait(&chooser.reopen(std::path::Path::new(path), write))? {
        OpenFileOutcome::Opened(opened) => {
            let name = opened.display_name().to_owned();
            Ok((opened.into_file().into_std(), name))
        }
        OpenFileOutcome::Failed(why) => Err(why),
        OpenFileOutcome::Cancelled => Err(format!("{path} was not opened")),
    }
}

/// Ask the person for a folder and answer the reference that reaches it again: what
/// `file.choose_folder` fills when the action takes the folder itself, and what `File.list`,
/// `File.read` and `File.watch` take. `None` when they dismissed the chooser.
///
/// # Errors
///
/// Why there is no folder: no chooser answers here, it refused, or it cannot name the folder
/// again.
pub fn choose_folder(
    chooser: &dyn FilePicker,
    request: &FileRequest,
) -> Result<Option<String>, String> {
    let pick = PickRequest::directory(&request.title, "").thread_name("tilolang-choose-folder");
    match wait(&chooser.open_handle(pick))? {
        OpenFileOutcome::Cancelled => Ok(None),
        OpenFileOutcome::Failed(why) => Err(why),
        OpenFileOutcome::Opened(opened) => opened
            .reference()
            .map(|reference| Some(reference.to_string_lossy().into_owned()))
            .ok_or_else(|| "the folder you chose cannot be opened again here".to_owned()),
    }
}

/// What a folder says of one of its entries.
#[derive(Clone, Copy)]
struct Facts {
    folder: bool,
    size: u64,
    /// When its content last changed, in Unix seconds.
    modified: i64,
}

impl Facts {
    fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            folder: meta.is_dir(),
            size: meta.len(),
            modified: meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |since| {
                    i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
                }),
        }
    }
}

/// The entry named `name` at `path`, as `facts` describe it.
fn described(name: String, path: String, facts: Facts) -> Entry {
    Entry {
        hidden: name.starts_with('.'),
        name,
        path,
        folder: facts.folder,
        size: if facts.folder {
            0
        } else {
            i64::try_from(facts.size).unwrap_or(i64::MAX)
        },
        modified: facts.modified,
    }
}

/// The last name of `path`, and the path of the folder it stands in.
fn split(path: &str) -> Option<(&str, &str)> {
    let path = path.trim_end_matches('/');
    let (folder, name) = path.rsplit_once('/')?;
    (!name.is_empty() && !folder.is_empty()).then_some((folder, name))
}

/// The entries of the folder `path` the person chose for the app, or of a folder inside it: what
/// `File.list` reads, in the order the disk answers. Each entry's path is the folder's own path
/// with its name after it, which the chooser opens again.
///
/// # Errors
///
/// The person never chose it for the app (or took the choice back), it is not a folder, or it is
/// gone.
pub fn list(chooser: &dyn FilePicker, path: &str) -> Result<Vec<Entry>, String> {
    let (folder, _) = chosen(chooser, path, false)?;
    let base = path.trim_end_matches('/');
    let mut entries = Vec::new();
    for (name, facts) in folder_entries(&folder, path)? {
        let at = format!("{base}/{name}");
        entries.push(described(name, at, facts));
    }
    Ok(entries)
}

/// The entries of the open folder `folder`, read through its descriptor: the app's sandbox has no
/// path to it, and nothing beneath it is opened to read them.
#[cfg(unix)]
fn folder_entries(folder: &std::fs::File, path: &str) -> Result<Vec<(String, Facts)>, String> {
    use std::os::fd::AsRawFd as _;
    if !folder
        .metadata()
        .map_err(|error| format!("{path}: {error}"))?
        .is_dir()
    {
        return Err(format!("{path} is not a folder"));
    }
    Ok(
        tilo_dialogs::portal::chosen_folder_entries(folder.as_raw_fd())?
            .into_iter()
            .map(|entry| {
                let facts = Facts {
                    folder: entry.kind == tilo_dialogs::portal::ChosenKind::Folder,
                    size: entry.size,
                    modified: entry.modified,
                };
                (entry.name, facts)
            })
            .collect(),
    )
}

#[cfg(not(unix))]
fn folder_entries(_folder: &std::fs::File, path: &str) -> Result<Vec<(String, Facts)>, String> {
    Err(format!(
        "{path}: a chosen folder is listed only on Linux and macOS"
    ))
}

/// The entry `path` is, or none when nothing is there: the folder the person chose for the app,
/// or anything inside it.
///
/// # Errors
///
/// The person never chose it, or the folder it stands in, for the app.
pub fn stat(chooser: &dyn FilePicker, path: &str) -> Result<Option<Entry>, String> {
    if let Some((folder, name)) = split(path)
        && let Ok(listed) = list(chooser, folder)
    {
        return Ok(listed.into_iter().find(|entry| entry.name == name));
    }
    // The folder the person chose itself: the folder it stands in was never theirs to give, and
    // the chooser names it as the person knows it (a workspace folder by its name, not its id).
    let (opened, name) = chosen(chooser, path, false)?;
    let meta = opened
        .metadata()
        .map_err(|error| format!("{path}: {error}"))?;
    Ok(Some(described(name, path.to_owned(), Facts::of(&meta))))
}

/// The bytes of the file `path`, one the person chose for the app or one inside a folder they
/// chose: what `File.read` reads.
///
/// # Errors
///
/// The person never chose it for the app, it is a folder, or it is gone.
pub fn read_bytes(chooser: &dyn FilePicker, path: &str) -> Result<Vec<u8>, String> {
    let (mut file, _) = chosen(chooser, path, false)?;
    if file.metadata().is_ok_and(|meta| meta.is_dir()) {
        return Err(format!("{path} is a folder"));
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes)
        .map_err(|error| format!("{path}: {error}"))?;
    Ok(bytes)
}

/// Make the folder `path`, answering the entry it made.
///
/// # Errors
///
/// Why it cannot be made (it exists, its parent is not there, the name is empty).
pub fn mkdir(path: &str) -> Result<Entry, String> {
    let path = std::path::PathBuf::from(path);
    std::fs::create_dir(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    entry(&path)
}

/// Write `bytes` as the new file `path`, in a folder the person chose for the app to change:
/// what `File.write` writes, answering the entry it made.
///
/// # Errors
///
/// It exists, the person never chose its folder for the app to change, or the folder is gone.
pub fn write(chooser: &dyn FilePicker, path: &str, bytes: &[u8]) -> Result<Entry, String> {
    use std::io::Write as _;
    if stat(chooser, path)?.is_some() {
        return Err(format!("{path} already exists"));
    }
    let (mut file, _) = chosen(chooser, path, true)?;
    file.write_all(bytes)
        .map_err(|error| format!("{path}: {error}"))?;
    drop(file);
    stat(chooser, path)?.ok_or_else(|| format!("{path} was not written"))
}

/// Pack `paths`, each with its name and everything in it, into the new archive `to`: what
/// `File.compress` does. The name says the format: `.zip` or `.tar.zst`.
///
/// # Errors
///
/// Why it cannot be packed (the name is taken or names no format, a path cannot be read).
pub fn compress(paths: &[String], to: &str) -> Result<i64, String> {
    if paths.is_empty() {
        return Err("choose an entry to compress".to_owned());
    }
    let to = std::path::PathBuf::from(to);
    let named = to.to_string_lossy().to_ascii_lowercase();
    let packing = if std::path::Path::new(&named)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
    {
        tilo_archive::Packing::Zip
    } else if named.ends_with(".tar.zst") {
        tilo_archive::Packing::TarZst
    } else {
        return Err("name the archive `.zip` or `.tar.zst`".to_owned());
    };
    let sources: Vec<std::path::PathBuf> = paths.iter().map(std::path::PathBuf::from).collect();
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&to)
        .map_err(|error| format!("{}: {error}", to.display()))?;
    match tilo_archive::create(&sources, file, packing, &()) {
        Ok(_) => Ok(i64::try_from(sources.len()).unwrap_or(i64::MAX)),
        Err(error) => {
            // A half-written archive is no archive: the caller sees the refusal, not a file.
            let _ = std::fs::remove_file(&to);
            Err(format!("{}: {error}", to.display()))
        }
    }
}

/// Extract the archive `from` into the new folder `to`: what `File.extract` does. Nothing is
/// written outside `to`, and an entry that would be is refused whole.
///
/// # Errors
///
/// Why it cannot be extracted (the folder is taken, the archive cannot be made one).
pub fn extract(from: &str, to: &str) -> Result<i64, String> {
    let from = std::path::PathBuf::from(from);
    let to = std::path::PathBuf::from(to);
    if to.symlink_metadata().is_ok() {
        return Err(format!("{} already exists", to.display()));
    }
    std::fs::create_dir(&to).map_err(|error| format!("{}: {error}", to.display()))?;
    match tilo_archive::extract_into(&from, &to, &()) {
        Ok(done) => {
            let entries = done.files.saturating_add(done.directories);
            Ok(i64::try_from(entries).unwrap_or(i64::MAX))
        }
        Err(error) => {
            let _ = std::fs::remove_dir_all(&to);
            Err(format!("{}: {error}", from.display()))
        }
    }
}

/// Rename the entry at `path` to `name` in its own folder.
///
/// # Errors
///
/// Why it cannot be renamed (the name is taken, it has no folder, it is gone).
pub fn rename(path: &str, name: &str) -> Result<Entry, String> {
    let path = std::path::PathBuf::from(path);
    let folder = path
        .parent()
        .ok_or_else(|| format!("{} has no folder", path.display()))?;
    let to = folder.join(name);
    if to.symlink_metadata().is_ok() {
        return Err(format!("{} already exists", to.display()));
    }
    std::fs::rename(&path, &to).map_err(|error| format!("{}: {error}", path.display()))?;
    entry(&to)
}

/// The entry at `path` moved into `folder`, or copied there when `copy` is set.
fn place(path: &str, folder: &str, copy: bool) -> Result<Entry, String> {
    let from = std::path::PathBuf::from(path);
    let folder = std::path::PathBuf::from(folder);
    let name = from
        .file_name()
        .ok_or_else(|| format!("{} has no name", from.display()))?
        .to_owned();
    let to = folder.join(name);
    if to.symlink_metadata().is_ok() {
        return Err(format!("{} already exists", to.display()));
    }
    if copy {
        copy_tree(&from, &to)?;
    } else if std::fs::rename(&from, &to).is_err() {
        // Across devices a move is a copy and a removal of what it left.
        copy_tree(&from, &to)?;
        remove_tree(&from)?;
    }
    entry(&to)
}

/// Copy the file or folder at `from` to `to`, which must not exist.
fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> Result<(), String> {
    copy_tree_told(from, to, &mut |_| true)
}

/// What [`copy_tree_told`] says when the one it tells asked it to stop.
pub(crate) const STOPPED: &str = "stopped";

/// How much a copy reads and writes at a time between telling how far it has got.
const COPY_CHUNK: usize = 1 << 20;

/// Copy the file or folder at `from` to `to`, which must not exist, telling `copied` each run of
/// bytes it wrote. `copied` answers whether to go on: when it says no the copy stops between
/// chunks with [`STOPPED`], leaving what it wrote so far for the caller to take away.
pub(crate) fn copy_tree_told(
    from: &std::path::Path,
    to: &std::path::Path,
    copied: &mut dyn FnMut(u64) -> bool,
) -> Result<(), String> {
    let meta = from
        .symlink_metadata()
        .map_err(|error| format!("{}: {error}", from.display()))?;
    if meta.is_dir() {
        std::fs::create_dir(to).map_err(|error| format!("{}: {error}", to.display()))?;
        let reading =
            std::fs::read_dir(from).map_err(|error| format!("{}: {error}", from.display()))?;
        for item in reading {
            let item = item.map_err(|error| format!("{}: {error}", from.display()))?;
            copy_tree_told(&item.path(), &to.join(item.file_name()), copied)?;
        }
        Ok(())
    } else if meta.file_type().is_symlink() {
        let target =
            std::fs::read_link(from).map_err(|error| format!("{}: {error}", from.display()))?;
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(target, to);
        // Windows links a folder and a file each its own way: the link points as the original did.
        #[cfg(windows)]
        let linked = if std::fs::metadata(from).is_ok_and(|meta| meta.is_dir()) {
            std::os::windows::fs::symlink_dir(target, to)
        } else {
            std::os::windows::fs::symlink_file(target, to)
        };
        linked.map_err(|error| format!("{}: {error}", to.display()))
    } else {
        copy_file_told(from, to, meta.permissions(), copied)
    }
}

/// Copy one file's bytes a chunk at a time, telling `copied` after each.
fn copy_file_told(
    from: &std::path::Path,
    to: &std::path::Path,
    permissions: std::fs::Permissions,
    copied: &mut dyn FnMut(u64) -> bool,
) -> Result<(), String> {
    use std::io::{Read, Write};
    let mut source =
        std::fs::File::open(from).map_err(|error| format!("{}: {error}", from.display()))?;
    let mut target = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(|error| format!("{}: {error}", to.display()))?;
    let mut chunk = vec![0_u8; COPY_CHUNK];
    loop {
        let read = match source.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("{}: {error}", from.display())),
        };
        target
            .write_all(chunk.get(..read).unwrap_or_default())
            .map_err(|error| format!("{}: {error}", to.display()))?;
        if !copied(read as u64) {
            return Err(STOPPED.to_owned());
        }
    }
    target
        .set_permissions(permissions)
        .map_err(|error| format!("{}: {error}", to.display()))
}

/// Remove the file or folder at `path` for good.
pub(crate) fn remove_tree(path: &std::path::Path) -> Result<(), String> {
    let meta = path
        .symlink_metadata()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
    .map_err(|error| format!("{}: {error}", path.display()))
}

/// Move the entry at `path` into `folder`.
///
/// # Errors
///
/// Why it cannot be moved (a name is taken, the folder is gone).
pub fn move_entry(path: &str, folder: &str) -> Result<Entry, String> {
    place(path, folder, false)
}

/// Copy the entry at `path` into `folder`, folder and all.
///
/// # Errors
///
/// Why it cannot be copied (a name is taken, the folder is gone).
pub fn copy(path: &str, folder: &str) -> Result<Entry, String> {
    place(path, folder, true)
}

/// Copy the entry at `path` to `to`, a path named whole: what `File.copy_to` does, so a paste
/// beside an entry whose name is taken keeps both.
///
/// # Errors
///
/// Why it cannot be copied (the name is taken, the folder is gone).
pub fn copy_to(path: &str, to: &str) -> Result<Entry, String> {
    let from = std::path::PathBuf::from(path);
    let to = std::path::PathBuf::from(to);
    if to.symlink_metadata().is_ok() {
        return Err(format!("{} already exists", to.display()));
    }
    copy_tree(&from, &to)?;
    entry(&to)
}

/// Remove the entry at `path` for good, answering whether anything was there.
///
/// # Errors
///
/// Why it cannot be removed (it is in use, its parent is not writable).
pub fn remove(path: &str) -> Result<bool, String> {
    let path = std::path::PathBuf::from(path);
    match path.symlink_metadata() {
        Ok(_) => {
            remove_tree(&path)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// This device's trash: the folders its files and their receipts stand in.
fn trash_dirs() -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|data| !data.is_empty())
        .map(std::path::PathBuf::from)
        .ok_or_else(|| "this device has no data folder".to_owned())?;
    let root = data.join("Trash");
    let files = root.join("files");
    let info = root.join("info");
    for folder in [&files, &info] {
        std::fs::create_dir_all(folder)
            .map_err(|error| format!("{}: {error}", folder.display()))?;
    }
    Ok((files, info))
}

/// The date the freedesktop trash writes: the instant as `YYYY-MM-DDThh:mm:ss`, in UTC.
fn trash_date(seconds: i64) -> String {
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// The days since 1970-01-01 as a calendar date (Howard Hinnant's civil calendar).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe
        .saturating_sub(doe / 1460)
        .saturating_add(doe / 36_524)
        .saturating_sub(doe / 146_096))
        / 365;
    let y = yoe.saturating_add(era.saturating_mul(400));
    let doy = doe.saturating_sub(
        yoe.saturating_mul(365)
            .saturating_add(yoe / 4)
            .saturating_sub(yoe / 100),
    );
    let mp = doy.saturating_mul(5).saturating_add(2) / 153;
    let d = doy
        .saturating_sub(mp.saturating_mul(153).saturating_add(2) / 5)
        .saturating_add(1);
    let m = if mp < 10 {
        mp.saturating_add(3)
    } else {
        mp.saturating_sub(9)
    };
    (y.saturating_add(i64::from(m <= 2)), m, d)
}

/// The days since 1970-01-01 of a calendar date, as [`civil_from_days`] reads back.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y.saturating_sub(1) } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 {
        m.saturating_sub(3)
    } else {
        m.saturating_add(9)
    };
    let doy = (mp.saturating_mul(153).saturating_add(2) / 5)
        .saturating_add(d)
        .saturating_sub(1);
    let doe = yoe
        .saturating_mul(365)
        .saturating_add(yoe / 4)
        .saturating_sub(yoe / 100)
        .saturating_add(doy);
    era.saturating_mul(146_097)
        .saturating_add(doe)
        .saturating_sub(719_468)
}

/// Percent-encode a path as the freedesktop trash's `Path=` holds it.
fn trash_escape(path: &str) -> String {
    let mut out = String::new();
    for byte in path.bytes() {
        let keep = byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~');
        if keep {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// The byte a hex digit `digit` names, when it is one.
fn hex_digit(digit: u8) -> Option<u8> {
    char::from(digit)
        .to_digit(16)
        .and_then(|digit| u8::try_from(digit).ok())
}

/// The byte a percent-escaped pair `high` and `low` names, when both are hex digits.
fn hex_byte(high: u8, low: u8) -> Option<u8> {
    let high = hex_digit(high)?;
    let low = hex_digit(low)?;
    high.checked_mul(16).and_then(|high| high.checked_add(low))
}

/// Percent-decode the trash's `Path=`.
fn trash_unescape(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        if byte == b'%' {
            // The byte the two digits after the `%` name, when both are there and are digits.
            let pair = bytes.get(at.saturating_add(1)..at.saturating_add(3));
            let decoded = pair.and_then(|pair| {
                let high = *pair.first()?;
                let low = *pair.get(1)?;
                hex_byte(high, low)
            });
            if let Some(decoded) = decoded {
                out.push(decoded);
                at = at.saturating_add(3);
                continue;
            }
        }
        out.push(byte);
        at = at.saturating_add(1);
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The instant a trash receipt says the entry was deleted at, as seconds since the epoch; 0
/// when the receipt says nothing this reads.
fn deleted_at(receipt: &str) -> i64 {
    let Some(stamp) = receipt
        .lines()
        .find_map(|line| line.strip_prefix("DeletionDate="))
        .map(str::trim)
    else {
        return 0;
    };
    let Some((date, time)) = stamp.split_once('T') else {
        return 0;
    };
    let mut parts = date.split('-').map(str::parse::<i64>);
    let (Some(Ok(y)), Some(Ok(m)), Some(Ok(d))) = (parts.next(), parts.next(), parts.next()) else {
        return 0;
    };
    let mut clock = time.split(':').map(str::parse::<i64>);
    let (Some(Ok(hours)), Some(Ok(minutes)), Some(Ok(seconds))) =
        (clock.next(), clock.next(), clock.next())
    else {
        return 0;
    };
    days_from_civil(y, m, d)
        .saturating_mul(86_400)
        .saturating_add(hours.saturating_mul(3600))
        .saturating_add(minutes.saturating_mul(60))
        .saturating_add(seconds)
}

/// The receipt of the trashed entry `name`: where it came from and when.
fn receipt(info: &std::path::Path, name: &str) -> Option<(String, i64)> {
    let text = std::fs::read_to_string(info.join(format!("{name}.trashinfo"))).ok()?;
    let original = text
        .lines()
        .find_map(|line| line.strip_prefix("Path="))
        .map(trash_unescape)?;
    Some((original, deleted_at(&text)))
}

/// The trashed entry `name` in `files`, `info` beside it.
fn trash_entry(files: &std::path::Path, info: &std::path::Path, name: &str) -> Option<TrashEntry> {
    let path = files.join(name);
    if path.symlink_metadata().is_err() {
        return None;
    }
    let (original, deleted) = receipt(info, name).unwrap_or_default();
    Some(TrashEntry {
        name: name.to_owned(),
        path: path.to_string_lossy().into_owned(),
        original,
        deleted,
    })
}

/// Put the entry at `path` in this device's trash, answering where it went.
///
/// # Errors
///
/// Why it cannot be trashed (it is gone, the trash cannot be reached, its receipt cannot be
/// written).
pub fn trash(path: &str) -> Result<TrashEntry, String> {
    let from = std::path::PathBuf::from(path);
    let name = from
        .file_name()
        .ok_or_else(|| format!("{} has no name", from.display()))?
        .to_string_lossy()
        .into_owned();
    let (files, info) = trash_dirs()?;
    let mut candidate = name.clone();
    let mut suffix = 1_u32;
    while files.join(&candidate).symlink_metadata().is_ok()
        || info.join(format!("{candidate}.trashinfo")).exists()
    {
        candidate = format!("{name}.{suffix}");
        suffix = suffix.saturating_add(1);
    }
    let original = from.to_string_lossy().into_owned();
    // The receipt is written before the entry moves, so an entry is never in the trash without
    // the place to put it back.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        });
    std::fs::write(
        info.join(format!("{candidate}.trashinfo")),
        format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            trash_escape(&original),
            trash_date(now)
        ),
    )
    .map_err(|error| error.to_string())?;
    if std::fs::rename(&from, files.join(&candidate)).is_err() {
        copy_tree(&from, &files.join(&candidate))?;
        remove_tree(&from)?;
    }
    Ok(TrashEntry {
        name: candidate.clone(),
        path: files.join(&candidate).to_string_lossy().into_owned(),
        original,
        deleted: now,
    })
}

/// What this device's trash holds, newest first.
///
/// # Errors
///
/// Why it cannot be read.
pub fn trash_entries() -> Result<Vec<TrashEntry>, String> {
    let (files, info) = trash_dirs()?;
    let reading =
        std::fs::read_dir(&info).map_err(|error| format!("{}: {error}", info.display()))?;
    let mut entries = Vec::new();
    for item in reading {
        let Ok(item) = item else {
            continue;
        };
        let name = item.file_name().to_string_lossy().into_owned();
        let Some(name) = name.strip_suffix(".trashinfo") else {
            continue;
        };
        if let Some(entry) = trash_entry(&files, &info, name) {
            entries.push(entry);
        }
    }
    entries.sort_by(|a, b| b.deleted.cmp(&a.deleted).then_with(|| a.name.cmp(&b.name)));
    Ok(entries)
}

/// Empties this device's trash for good: every entry it holds, and the receipt that would put it
/// back. What it removes nothing brings back. Answers how many entries it emptied.
///
/// # Errors
///
/// Why the trash cannot be read, or one of its entries cannot be removed.
pub fn empty_trash() -> Result<usize, String> {
    let (files, info) = trash_dirs()?;
    let emptied = trash_entries()?;
    for entry in &emptied {
        remove(&entry.path)?;
        let receipt = info.join(format!("{}.trashinfo", entry.name));
        if receipt.exists() {
            remove(&receipt.to_string_lossy())?;
        }
    }
    // A file the trash keeps with no receipt is emptied too: it is nothing a person can restore.
    if let Ok(reading) = std::fs::read_dir(&files) {
        for item in reading.flatten() {
            let left = &item.path();
            let name = item.file_name().to_string_lossy().into_owned();
            if !info.join(format!("{name}.trashinfo")).exists() {
                remove(&left.to_string_lossy())?;
            }
        }
    }
    Ok(emptied.len())
}

/// Restore the trashed entry `name` to where it came from, answering the entry it made there.
///
/// # Errors
///
/// Why it cannot be restored (it is gone, where it stood is taken, its receipt is unreadable).
pub fn restore(name: &str) -> Result<Entry, String> {
    let (files, info) = trash_dirs()?;
    let name = name.strip_suffix(".trashinfo").unwrap_or(name);
    let (original, _) =
        receipt(&info, name).ok_or_else(|| format!("{name} is not in this device's trash"))?;
    let to = std::path::PathBuf::from(&original);
    if to.symlink_metadata().is_ok() {
        return Err(format!("{} already exists", to.display()));
    }
    let from = files.join(name);
    if std::fs::rename(&from, &to).is_err() {
        copy_tree(&from, &to)?;
        remove_tree(&from)?;
    }
    let _ = std::fs::remove_file(info.join(format!("{name}.trashinfo")));
    entry(&to)
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
        let folder = std::path::PathBuf::from(path);
        let folder = folder.as_path();
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
        LaunchChannel, LaunchRequest, MultiChannel, OpenFileChannel, PickRequest, PortalChannel,
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

    /// A chooser that answers with files on disk, at the descriptors a real chooser opens: the
    /// person picking a file, several, or a folder, without a desktop.
    struct Picks {
        root: tempfile::TempDir,
        /// The files in the root, by their path inside it.
        files: Vec<(&'static str, &'static [u8])>,
        asked: std::sync::Mutex<Option<PickRequest>>,
    }

    impl Picks {
        fn new(name: &'static str, bytes: &'static [u8]) -> Self {
            Self::several(vec![(name, bytes)])
        }

        fn several(files: Vec<(&'static str, &'static [u8])>) -> Self {
            let root = tempfile::tempdir().expect("a folder to choose from");
            for (name, bytes) in &files {
                let path = root.path().join(name);
                if let Some(folder) = path.parent() {
                    std::fs::create_dir_all(folder).expect("the file's folder");
                }
                std::fs::write(path, bytes).expect("the file a person might choose");
            }
            Self {
                root,
                files,
                asked: std::sync::Mutex::new(None),
            }
        }

        fn path(&self, name: &str) -> std::path::PathBuf {
            self.root.path().join(name)
        }
    }

    impl FilePicker for Picks {
        fn pick(&self, _: PickRequest) -> PortalChannel {
            unanswered()
        }

        fn pick_multiple(&self, request: PickRequest) -> MultiChannel {
            *self.asked.lock().unwrap() = Some(request);
            let (sender, receiver) = std::sync::mpsc::channel();
            let paths = self.files.iter().map(|(name, _)| self.path(name)).collect();
            let _ = sender.send(MultiOutcome::Selected(paths));
            receiver
        }

        fn open_handle(&self, request: PickRequest) -> OpenFileChannel {
            *self.asked.lock().unwrap() = Some(request.clone());
            let (sender, receiver) = std::sync::mpsc::channel();
            let answered = if request.is_directory() {
                match tilo_dialogs::portal::open_directory_handle(self.root.path()) {
                    Ok(folder) => OpenFileOutcome::Opened(OpenedFile::at(folder, self.root.path())),
                    Err(error) => OpenFileOutcome::Failed(error.to_string()),
                }
            } else {
                let path = self.path(self.files[0].0);
                match std::fs::File::open(&path) {
                    Ok(file) => OpenFileOutcome::Opened(OpenedFile::at(file, &path)),
                    Err(error) => OpenFileOutcome::Failed(error.to_string()),
                }
            };
            let _ = sender.send(answered);
            receiver
        }

        fn reopen(&self, reference: &std::path::Path, _write: bool) -> OpenFileChannel {
            let (sender, receiver) = std::sync::mpsc::channel();
            let answered = match std::fs::File::open(reference) {
                Ok(file) => OpenFileOutcome::Opened(OpenedFile::at(file, reference)),
                Err(error) => OpenFileOutcome::Failed(error.to_string()),
            };
            let _ = sender.send(answered);
            receiver
        }

        fn launch(&self, _: LaunchRequest) -> LaunchChannel {
            unanswered()
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
                ..FileRequest::default()
            },
        )
        .expect("the chooser answered");
        assert_eq!(chosen.len(), 1, "one file was chosen");
        let chosen = &chosen[0];
        assert_eq!(chosen.name, "Lisbon.md");
        assert_eq!(chosen.path, "Lisbon.md", "the file's own name is its path");
        assert_eq!(chosen.media, "text/markdown");
        let asked = chooser
            .asked
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
                ..FileRequest::default()
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
    fn several_files_and_a_folders_files_are_kept_with_their_paths() {
        let store = ContentStore::memory();
        let chooser = Picks::several(vec![
            ("trip/one.md", b"# One\n"),
            ("trip/two.md", b"# Two\n"),
        ]);
        let chosen = choose(
            &store,
            &chooser,
            &FileRequest {
                title: "Import notes".to_owned(),
                accept: vec!["text/markdown".to_owned()],
                multiple: true,
                ..FileRequest::default()
            },
        )
        .expect("the chooser answered");
        assert_eq!(
            chosen
                .iter()
                .map(|file| (file.name.as_str(), file.path.as_str()))
                .collect::<Vec<_>>(),
            [("one.md", "one.md"), ("two.md", "two.md")],
            "each file keeps its name"
        );
        let asked = chooser.asked.lock().unwrap().take().expect("asked");
        assert!(asked.is_multiple(), "the person may choose several");

        let chooser = Picks::several(vec![("trip/photo.png", b"png")]);
        let chosen = choose(
            &store,
            &chooser,
            &FileRequest {
                title: "Import a folder".to_owned(),
                accept: vec!["image/*".to_owned()],
                folder: true,
                ..FileRequest::default()
            },
        )
        .expect("the chooser answered");
        assert_eq!(chosen.len(), 1);
        let asked = chooser.asked.lock().unwrap().take().expect("asked");
        assert!(asked.is_directory(), "the person chooses a folder");
    }

    #[test]
    fn a_preview_comes_from_the_devices_cache_and_keeps_a_file_of_its_own() {
        let image = thumbnail::RgbaThumb {
            rgba: [20_u8, 40, 60, 255].repeat(32 * 16),
            width: 32,
            height: 16,
        };
        let png = thumbnail::encode_png(&image).expect("the fixture encodes");
        let store = ContentStore::memory();
        let hash = store.put(&png).expect("the fixture is kept");
        let file = FileValue::new(
            "photo.png",
            "image/png",
            i64::try_from(png.len()).unwrap_or(i64::MAX),
            hash,
        );
        let service = thumbnail::ThumbnailService::ephemeral(1);
        let preview = thumbnail(&store, &service, &file, 16)
            .expect("the preview generated")
            .expect("an image has a preview");
        assert_eq!(preview.name, "photo-16.png");
        assert_eq!(preview.media, "image/png");
        let bytes = store.read(&preview.hash).expect("the preview is kept");
        let decoded = thumbnail::decode_png(&bytes).expect("the preview is a PNG");
        assert_eq!(decoded.width.max(decoded.height), 16);

        // A file that is no image has none: its list shows a badge instead.
        let text = FileValue::new(
            "plan.md",
            "text/markdown",
            4,
            store.put(b"# P\n").expect("the text is kept"),
        );
        assert!(
            thumbnail(&store, &service, &text, 16)
                .expect("no preview is no error")
                .is_none()
        );
    }

    /// The person at the computer, choosing the folder the test put in front of them.
    #[cfg(unix)]
    struct ChoosesFolder(std::path::PathBuf);

    #[cfg(unix)]
    impl tilo_dialogs::chooser_service::Person for ChoosesFolder {
        #[expect(
            clippy::panic_in_result_fn,
            reason = "the stand-in person fails the test when the app asks the wrong question"
        )]
        fn choose(
            &self,
            _caller: &tilo_dialogs::chooser_service::Caller,
            _handle: String,
            question: &tilo_dialogs::chooser_service::Question,
        ) -> Result<Vec<std::path::PathBuf>, tilo_dialogs::chooser_service::Asked> {
            assert!(question.directory, "the app asked for a folder");
            Ok(vec![self.0.clone()])
        }
    }

    /// The person's grants as a host keeps them: each choice is the app's that asked for it, and
    /// a reference opens again only for that app.
    #[cfg(unix)]
    #[derive(Default)]
    struct GrantsByApp(std::sync::Mutex<Vec<(String, tilo_dialogs::chooser_service::Granted)>>);

    #[cfg(unix)]
    impl tilo_dialogs::chooser_service::Grants for GrantsByApp {
        fn grant(
            &self,
            caller: &tilo_dialogs::chooser_service::Caller,
            chosen: &std::path::Path,
            choice: tilo_dialogs::chooser_service::Choice,
        ) -> Result<tilo_dialogs::chooser_service::Granted, String> {
            let name = chosen
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .ok_or_else(|| "the choice names no folder".to_owned())?;
            let granted = tilo_dialogs::chooser_service::Granted {
                reference: std::path::Path::new(tilo_dialogs::chosen::CHOSEN_ROOT)
                    .join("records/token")
                    .join(&name),
                name,
                host: chosen.to_path_buf(),
                choice,
                save_sibling: None,
            };
            self.0
                .lock()
                .unwrap()
                .push((caller.app.clone(), granted.clone()));
            Ok(granted)
        }

        fn document(
            &self,
            caller: &tilo_dialogs::chooser_service::Caller,
            reference: &std::path::Path,
        ) -> Option<tilo_dialogs::chooser_service::Granted> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .find(|(app, granted)| {
                    *app == caller.app && reference.starts_with(&granted.reference)
                })
                .map(|(_, granted)| granted.clone())
        }
    }

    #[cfg(unix)]
    struct OpensNothing;

    #[cfg(unix)]
    impl tilo_dialogs::chooser_service::Opens for OpensNothing {
        fn open(&self, _workspace: &str, _uri: &str) -> Result<Option<()>, String> {
            Ok(None)
        }
    }

    /// An app lists, reads and writes the folder the person chose for it, through the chooser
    /// service that holds the person's grant, and through nothing else: no home folder, no host
    /// path, nothing above the folder, and nothing of it for an app it was not chosen for.
    #[cfg(unix)]
    #[test]
    fn an_app_reaches_the_folder_chosen_for_it_and_no_other_app_does() {
        use tilo_dialogs::chooser_service::{NOT_CHOSEN, Serving, pair};
        let root = tempfile::tempdir().unwrap();
        let projects = root.path().join("Projects");
        std::fs::create_dir_all(projects.join("site")).unwrap();
        std::fs::write(projects.join("plan.md"), b"Spring planting").unwrap();
        std::fs::write(root.path().join("secret.txt"), b"never chosen").unwrap();
        let grants = std::sync::Arc::new(GrantsByApp::default());
        let picker = |app: &str| {
            let serving = Serving {
                person: Some(std::sync::Arc::new(ChoosesFolder(projects.clone()))),
                grants: std::sync::Arc::clone(&grants) as _,
                opens: std::sync::Arc::new(OpensNothing),
            };
            let stream = pair(serving, app, "5ba25f73-87c5-447f-a5a6-880dc4249a81").unwrap();
            tilo_dialogs::shell::picker(stream, tilo_dialogs::unavailable_picker()).unwrap()
        };
        let git = picker("so.tilo.git-client");
        let notes = picker("so.tilo.notes");

        // Nothing is reached before the person chooses: not a home folder, not a host path.
        let host = root.path().to_str().unwrap().to_owned();
        for path in ["~", "~/Projects", "/etc", host.as_str()] {
            assert_eq!(list(&*git, path).unwrap_err(), NOT_CHOSEN, "{path}");
        }

        // The person chooses the folder for the git client, to change too.
        let OpenFileOutcome::Opened(opened) = git
            .open_handle(PickRequest::writable_directory("Open a repository", ""))
            .recv_timeout(tilo_test_env::hang_bound())
            .unwrap()
        else {
            panic!("the chosen folder was not handed over");
        };
        let folder = opened.reference().unwrap().to_str().unwrap().to_owned();
        let plan = format!("{folder}/plan.md");

        let mut listed: Vec<(String, String, bool)> = list(&*git, &folder)
            .unwrap()
            .into_iter()
            .map(|entry| (entry.name, entry.path, entry.folder))
            .collect();
        listed.sort();
        assert_eq!(
            listed,
            vec![
                ("plan.md".to_owned(), plan.clone(), false),
                ("site".to_owned(), format!("{folder}/site"), true),
            ],
            "the folder's entries, each by a path the chooser opens again"
        );
        assert_eq!(read_bytes(&*git, &plan).unwrap(), b"Spring planting");
        assert_eq!(stat(&*git, &plan).unwrap().unwrap().size, 15);
        assert!(stat(&*git, &folder).unwrap().unwrap().folder);
        assert!(stat(&*git, &format!("{folder}/gone")).unwrap().is_none());
        assert!(list(&*git, &format!("{folder}/site")).unwrap().is_empty());
        assert!(
            read_bytes(&*git, &folder).is_err(),
            "a folder is not a file"
        );
        assert!(
            read_bytes(&*git, &format!("{folder}/../secret.txt")).is_err(),
            "nothing above the chosen folder"
        );

        let page = format!("{folder}/site/index.html");
        let written = write(&*git, &page, b"<h1>Hello</h1>").unwrap();
        assert_eq!((written.name.as_str(), written.size), ("index.html", 14));
        assert_eq!(
            std::fs::read(projects.join("site/index.html")).unwrap(),
            b"<h1>Hello</h1>"
        );
        assert!(
            write(&*git, &page, b"again").is_err(),
            "a new file, never over one"
        );

        // The folder is the git client's: another app is refused every bit of it.
        assert_eq!(list(&*notes, &folder).unwrap_err(), NOT_CHOSEN);
        assert_eq!(read_bytes(&*notes, &plan).unwrap_err(), NOT_CHOSEN);
        assert!(stat(&*notes, &plan).is_err());
        assert!(write(&*notes, &format!("{folder}/note.md"), b"x").is_err());
        assert!(!projects.join("note.md").exists());
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
