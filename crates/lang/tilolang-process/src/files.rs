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

/// This device's home folder: what a `~` at the front of a path names.
#[must_use]
pub fn home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(std::path::PathBuf::from)
}

/// The path a program named as a real one: `~` and `~/…` are this device's home folder, as the
/// shell reads them, and everything else stands as it is.
#[must_use]
pub fn resolve(path: &str) -> std::path::PathBuf {
    if path == "~" {
        return home().unwrap_or_else(|| std::path::PathBuf::from("~"));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = home() {
            return home.join(rest);
        }
    }
    std::path::PathBuf::from(path)
}

/// What a person choosing a file is told on a device that keeps no content store: the file
/// could not be kept, which is what they need to know, not the name of what is missing.
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
    let pick = if request.folder {
        PickRequest::directory(&request.title, "")
    } else if request.multiple {
        PickRequest::open_multiple(&request.title, "")
    } else {
        PickRequest::open(&request.title, "")
    };
    let mut pick = pick
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
        ContentsOutcome::Cancelled => return Ok(Vec::new()),
        ContentsOutcome::Failed(why) => return Err(why),
        ContentsOutcome::Picked(files) => files,
    };
    let mut kept = Vec::with_capacity(chosen.len());
    for file in chosen {
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
        // Where the file stands in what the person chose, and what it is called there. A
        // chooser that answers a host path names only the file: a sandboxed app has no way to
        // use the directory it is in.
        let relative = file
            .location
            .as_ref()
            .and_then(|path| path.to_str())
            .filter(|path| !path.is_empty() && !std::path::Path::new(path).is_absolute());
        let name = relative
            .and_then(|path| std::path::Path::new(path).file_name())
            .and_then(std::ffi::OsStr::to_str)
            .map_or_else(|| file.name.clone(), str::to_owned);
        let path = relative.map_or_else(|| name.clone(), str::to_owned);
        kept.push(
            FileValue::new(
                name,
                media,
                i64::try_from(file.bytes.len()).unwrap_or(i64::MAX),
                hash,
            )
            .with_path(path),
        );
    }
    Ok(kept)
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
pub use tilolang_tilo::runtime::files::{Entry, TrashEntry};

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
    let path = &resolve(&path.to_string_lossy());
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
    let path = &resolve(&path.to_string_lossy());
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
    let path = &resolve(&path.to_string_lossy());
    std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))
}

/// Make the folder `path`, answering the entry it made.
///
/// # Errors
///
/// Why it cannot be made (it exists, its parent is not there, the name is empty).
pub fn mkdir(path: &str) -> Result<Entry, String> {
    let path = resolve(path);
    std::fs::create_dir(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    entry(&path)
}

/// Write `bytes` as the new file `path`, answering the entry it made.
///
/// # Errors
///
/// Why it cannot be written (it exists, its folder is gone, the name is empty).
pub fn write(path: &str, bytes: &[u8]) -> Result<Entry, String> {
    use std::io::Write as _;
    let path = resolve(path);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    entry(&path)
}

/// Rename the entry at `path` to `name` in its own folder.
///
/// # Errors
///
/// Why it cannot be renamed (the name is taken, it has no folder, it is gone).
pub fn rename(path: &str, name: &str) -> Result<Entry, String> {
    let path = resolve(path);
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
    let from = resolve(path);
    let folder = resolve(folder);
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
    let meta = from
        .symlink_metadata()
        .map_err(|error| format!("{}: {error}", from.display()))?;
    if meta.is_dir() {
        std::fs::create_dir(to).map_err(|error| format!("{}: {error}", to.display()))?;
        let reading =
            std::fs::read_dir(from).map_err(|error| format!("{}: {error}", from.display()))?;
        for item in reading {
            let item = item.map_err(|error| format!("{}: {error}", from.display()))?;
            copy_tree(&item.path(), &to.join(item.file_name()))?;
        }
        Ok(())
    } else if meta.file_type().is_symlink() {
        let target =
            std::fs::read_link(from).map_err(|error| format!("{}: {error}", from.display()))?;
        std::os::unix::fs::symlink(target, to).map_err(|error| format!("{}: {error}", to.display()))
    } else {
        std::fs::copy(from, to)
            .map(|_| ())
            .map_err(|error| format!("{}: {error}", from.display()))
    }
}

/// Remove the file or folder at `path` for good.
fn remove_tree(path: &std::path::Path) -> Result<(), String> {
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

/// Remove the entry at `path` for good, answering whether anything was there.
///
/// # Errors
///
/// Why it cannot be removed (it is in use, its parent is not writable).
pub fn remove(path: &str) -> Result<bool, String> {
    let path = resolve(path);
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
        .or_else(|| home().map(|home| home.join(".local/share")))
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
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + i64::from(m <= 2), m, d)
}

/// The days since 1970-01-01 of a calendar date, as [`civil_from_days`] reads back.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Percent-encode a path as the freedesktop trash's `Path=` holds it.
fn trash_escape(path: &str) -> String {
    let mut out = String::new();
    for byte in path.bytes() {
        let keep = byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~');
        if keep {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Percent-decode the trash's `Path=`.
fn trash_unescape(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&path[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
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
    let mut parts = date.split('-').filter_map(|part| part.parse::<i64>().ok());
    let (Some(y), Some(m), Some(d)) = (parts.next(), parts.next(), parts.next()) else {
        return 0;
    };
    let mut clock = time.split(':').filter_map(|part| part.parse::<i64>().ok());
    let (hours, minutes, seconds) = (
        clock.next().unwrap_or(0),
        clock.next().unwrap_or(0),
        clock.next().unwrap_or(0),
    );
    days_from_civil(y, m, d) * 86_400 + hours * 3600 + minutes * 60 + seconds
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
    let from = resolve(path);
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
        .map_or(0, |since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX));
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
        name: candidate,
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
    let reading = std::fs::read_dir(&info).map_err(|error| format!("{}: {error}", info.display()))?;
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

/// Restore the trashed entry `name` to where it came from, answering the entry it made there.
///
/// # Errors
///
/// Why it cannot be restored (it is gone, where it stood is taken, its receipt is unreadable).
pub fn restore(name: &str) -> Result<Entry, String> {
    let (files, info) = trash_dirs()?;
    let name = name.strip_suffix(".trashinfo").unwrap_or(name);
    let (original, _) = receipt(&info, name)
        .ok_or_else(|| format!("{} is not in this device's trash", name))?;
    let to = resolve(&original);
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
        let folder = resolve(path);
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
        Vec<(&'static str, &'static [u8])>,
        std::sync::Mutex<Option<PickRequest>>,
    );

    impl Picks {
        fn new(name: &'static str, bytes: &'static [u8]) -> Self {
            Self(vec![(name, bytes)], std::sync::Mutex::new(None))
        }

        fn several(files: Vec<(&'static str, &'static [u8])>) -> Self {
            Self(files, std::sync::Mutex::new(None))
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
            *self.1.lock().unwrap() = Some(request);
            let (sender, receiver) = std::sync::mpsc::channel();
            let files = self
                .0
                .iter()
                .map(|(name, bytes)| PickedContents {
                    name: (*name).to_owned(),
                    media_type: Some(media_of(name)),
                    bytes: bytes.to_vec(),
                    location: Some(std::path::PathBuf::from(name)),
                })
                .collect();
            let _ = sender.send(ContentsOutcome::Picked(files));
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
            .1
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
            [("one.md", "trip/one.md"), ("two.md", "trip/two.md")],
            "each file keeps its name and its path inside what the person chose"
        );
        let asked = chooser.1.lock().unwrap().take().expect("asked");
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
        let asked = chooser.1.lock().unwrap().take().expect("asked");
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
