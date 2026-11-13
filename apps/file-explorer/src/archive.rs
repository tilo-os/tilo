//! Archives as Files browses them: reading an archive's entries off the window's
//! thread, extracting one into place, and packing a new one.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use ui::lock::MutexRecover as _;

pub use tilo_archive::{Entry, Kind, Observer, Packing};

pub fn is_supported(path: &Path) -> bool {
    tilo_archive::Format::of_path(path).is_some()
}

pub fn location(path: &Path) -> Option<(PathBuf, PathBuf)> {
    for candidate in path.ancestors() {
        if is_supported(candidate) && candidate.is_file() {
            let prefix = path.strip_prefix(candidate).ok()?.to_path_buf();
            return Some((candidate.to_path_buf(), prefix));
        }
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

fn stamp(archive: &Path) -> io::Result<Stamp> {
    let meta = fs::metadata(archive)?;
    Ok(Stamp {
        len: meta.len(),
        modified: meta.modified().ok(),
    })
}

struct Index {
    archive: PathBuf,
    stamp: Stamp,
    entries: Arc<Vec<Entry>>,
}

static INDEX: Mutex<Option<Index>> = Mutex::new(None);

pub fn cached(archive: &Path) -> Option<Arc<Vec<Entry>>> {
    let now = stamp(archive).ok()?;
    let index = INDEX.lock_recover();
    index
        .as_ref()
        .filter(|index| index.archive == archive && index.stamp == now)
        .map(|index| Arc::clone(&index.entries))
}

pub fn index(archive: &Path, observer: &dyn Observer) -> io::Result<Arc<Vec<Entry>>> {
    if let Some(entries) = cached(archive) {
        return Ok(entries);
    }
    let before = stamp(archive)?;
    let entries = Arc::new(tilo_archive::list(archive, observer)?);
    *INDEX.lock_recover() = Some(Index {
        archive: archive.to_path_buf(),
        stamp: before,
        entries: Arc::clone(&entries),
    });
    Ok(entries)
}

pub struct Listing {
    wake: Arc<dyn Fn() + Send + Sync>,
    job: Option<ListingJob>,
}

struct ListingJob {
    archive: PathBuf,
    stop: Arc<std::sync::atomic::AtomicBool>,
    read: Arc<std::sync::atomic::AtomicU64>,
    size: u64,
    done: Arc<Mutex<Option<EntriesRead>>>,
}

type EntriesRead = io::Result<Arc<Vec<Entry>>>;

impl std::fmt::Debug for Listing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Listing")
            .field("archive", &self.job.as_ref().map(|job| &job.archive))
            .finish_non_exhaustive()
    }
}

struct ListingObserver {
    stop: Arc<std::sync::atomic::AtomicBool>,
    read: Arc<std::sync::atomic::AtomicU64>,
}

impl Observer for ListingObserver {
    fn advance(&self, bytes: u64) {
        self.read
            .fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
    }

    fn cancelled(&self) -> bool {
        self.stop.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Listing {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            wake: Arc::new(wake),
            job: None,
        }
    }

    pub fn start(&mut self, archive: &Path) {
        if self.job.as_ref().is_some_and(|job| job.archive == archive) {
            return;
        }
        self.stop();
        let job = ListingJob {
            archive: archive.to_path_buf(),
            stop: Arc::default(),
            read: Arc::default(),
            size: fs::metadata(archive).map_or(0, |meta| meta.len()),
            done: Arc::default(),
        };
        let observer = ListingObserver {
            stop: Arc::clone(&job.stop),
            read: Arc::clone(&job.read),
        };
        let done = Arc::clone(&job.done);
        let wake = Arc::clone(&self.wake);
        let path = job.archive.clone();
        let spawned = std::thread::Builder::new()
            .name("files-archive-list".into())
            .spawn(move || {
                let read = index(&path, &observer);
                if !observer.cancelled() {
                    *done.lock_recover() = Some(read);
                    wake();
                }
            });
        if let Err(error) = spawned {
            *job.done.lock_recover() = Some(Err(error));
        }
        self.job = Some(job);
    }

    pub fn stop(&mut self) {
        if let Some(job) = self.job.take() {
            job.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    pub fn is_reading(&self, archive: &Path) -> bool {
        self.job.as_ref().is_some_and(|job| job.archive == archive)
    }

    pub fn progress(&self) -> Option<(u64, u64)> {
        self.job.as_ref().map(|job| {
            (
                job.read.load(std::sync::atomic::Ordering::Relaxed),
                job.size,
            )
        })
    }

    pub fn take(&mut self) -> Option<(PathBuf, EntriesRead)> {
        let read = self.job.as_ref()?.done.lock_recover().take()?;
        let job = self.job.take()?;
        Some((job.archive, read))
    }
}

pub fn children_of(entries: &[Entry], prefix: &Path) -> io::Result<Vec<Entry>> {
    let prefix = if prefix.as_os_str().is_empty() {
        PathBuf::new()
    } else {
        tilo_archive::safe_relative(prefix)?.unwrap_or_default()
    };
    Ok(tilo_archive::children(entries, &prefix))
}

#[cfg(test)]
pub fn list_directory(archive: &Path, prefix: &Path) -> io::Result<Vec<Entry>> {
    children_of(&index(archive, &())?, prefix)
}

#[cfg(test)]
pub fn is_directory(archive: &Path, member: &Path) -> io::Result<bool> {
    let entries = index(archive, &())?;
    Ok(tilo_archive::has_directory(&entries, member))
}

#[cfg(test)]
pub fn list(archive: &Path) -> io::Result<Vec<Entry>> {
    Ok(index(archive, &())?.as_ref().clone())
}

pub fn extract_member(archive: &Path, member: &Path, output: &Path) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    if let Err(error) = tilo_archive::read_member(archive, member, &mut file, &()) {
        drop(file);
        let _ = fs::remove_file(output);
        return Err(error);
    }
    let mut permissions = file.metadata()?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(output, permissions)
}

pub fn stem(archive: &Path) -> String {
    let name = archive
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = tilo_archive::stem(&name);
    if stem.is_empty() {
        name
    } else {
        stem.to_owned()
    }
}

pub fn likely_name(archive: &Path) -> String {
    let mut top: Option<(PathBuf, bool)> = None;
    let mut single = true;
    let streamed = matches!(
        tilo_archive::Format::of_path(archive),
        Some(tilo_archive::Format::Tar(_)) | None
    );
    let walked = tilo_archive::walk(archive, &(), &mut |entry, _| {
        let mut parts = entry.path.components();
        let first = parts.next().map(|part| PathBuf::from(part.as_os_str()));
        let folder = parts.next().is_some() || entry.kind == Kind::Directory;
        match (&top, first) {
            (None, Some(first)) => top = Some((first, folder)),
            (Some((name, _)), Some(first)) if *name != first => single = false,
            _ => {}
        }
        Ok(if streamed || !single {
            tilo_archive::Flow::Stop
        } else {
            tilo_archive::Flow::Continue
        })
    });
    match (walked, top) {
        (Ok(()), Some((name, true))) if single => name.to_string_lossy().into_owned(),
        _ => stem(archive),
    }
}

pub struct Staging {
    path: PathBuf,
    kept: bool,
}

impl Staging {
    pub fn result(&self) -> io::Result<(PathBuf, bool)> {
        let mut children = fs::read_dir(&self.path)?;
        if let (Some(only), None) = (children.next().transpose()?, children.next())
            && only.file_type()?.is_dir()
        {
            return Ok((only.path(), true));
        }
        Ok((self.path.clone(), false))
    }

    pub fn keep(mut self) {
        self.kept = true;
        if self.path.exists() {
            let _ = fs::remove_dir(&self.path);
        }
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if !self.kept {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

pub fn extract(archive: &Path, parent: &Path, observer: &dyn Observer) -> io::Result<Staging> {
    let staging = Staging {
        path: parent.join(format!(".tilo-extract-{}", uuid::Uuid::new_v4())),
        kept: false,
    };
    fs::create_dir(&staging.path)?;
    tilo_archive::extract_into(archive, &staging.path, observer)?;
    Ok(staging)
}

struct PartialArchive<'a> {
    path: &'a Path,
    committed: bool,
}

impl Drop for PartialArchive<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(self.path);
        }
    }
}

pub fn create(
    paths: &[PathBuf],
    output: &Path,
    packing: Packing,
    observer: &dyn Observer,
) -> io::Result<()> {
    let parent = output.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "archive has no parent folder")
    })?;
    // Reserve private scratch storage without truncating a neighbour or
    // following a link left at a predictable .part name.
    let temporary = parent.join(format!(".tilo-archive-{}", uuid::Uuid::new_v4()));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let mut cleanup = PartialArchive {
        path: &temporary,
        committed: false,
    };
    let file = tilo_archive::create(paths, file, packing, observer)?;
    file.sync_all()?;
    drop(file);
    host_fs::rename_noreplace(&temporary, output)?;
    cleanup.committed = true;
    Ok(())
}

#[cfg(test)]
pub fn create_zip(paths: &[PathBuf], output: &Path) -> io::Result<()> {
    create(paths, output, Packing::Zip, &())
}

pub fn packing_of(name: &str) -> Packing {
    if name.to_ascii_lowercase().ends_with(".tar.zst") {
        Packing::TarZst
    } else {
        Packing::Zip
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn selected_directory_symlinks_are_not_archived() {
        let temp = tempfile::tempdir().unwrap();
        let private = temp.path().join("private");
        fs::create_dir(&private).unwrap();
        fs::write(private.join("secret.txt"), b"not selected").unwrap();
        let link = temp.path().join("shortcut");
        std::os::unix::fs::symlink(&private, &link).unwrap();
        let selected = temp.path().join("selected.txt");
        fs::write(&selected, b"selected").unwrap();
        let output = temp.path().join("selection.zip");

        create_zip(&[link, selected], &output).unwrap();

        let entries = list(&output).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, Path::new("selected.txt"));
    }

    #[test]
    fn creating_zip_preserves_neighbours_and_occupied_output() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("notes.txt");
        fs::write(&source, b"notes").unwrap();
        let output = temp.path().join("bundle.zip");
        let neighbour = temp.path().join("bundle.zip.part");
        fs::write(&neighbour, b"unrelated draft").unwrap();
        create_zip(std::slice::from_ref(&source), &output).unwrap();
        assert_eq!(fs::read(&neighbour).unwrap(), b"unrelated draft");
        let original = fs::read(&output).unwrap();
        fs::write(&source, b"changed").unwrap();
        assert!(create_zip(&[source], &output).is_err());
        assert_eq!(fs::read(&output).unwrap(), original);
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 3);
    }

    #[test]
    fn archive_paths_listings_and_index() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("bundle.zip");
        fs::write(&archive, b"").unwrap();
        assert_eq!(location(&archive), Some((archive.clone(), PathBuf::new())));
        assert_eq!(
            location(&archive.join("Photos/Trips")),
            Some((archive.clone(), PathBuf::from("Photos/Trips")))
        );
        assert_eq!(location(&temp.path().join("ordinary")), None);
        assert!(
            location(&temp.path().join("game.tar.xz")).is_none(),
            "missing file"
        );

        let source = temp.path().join("source");
        fs::create_dir_all(source.join("Photos")).unwrap();
        fs::write(source.join("Photos/one.jpg"), b"one").unwrap();
        fs::write(source.join("root.txt"), b"root").unwrap();
        fs::remove_file(&archive).unwrap();
        create_zip(&[source.join("Photos"), source.join("root.txt")], &archive).unwrap();
        let root = list_directory(&archive, Path::new("")).unwrap();
        assert_eq!(root.len(), 2);
        assert!(
            root.iter()
                .any(|entry| entry.path == Path::new("Photos") && entry.kind == Kind::Directory)
        );
        assert!(cached(&archive).is_some(), "browsing keeps the entries");
        let photos = list_directory(&archive, Path::new("Photos")).unwrap();
        assert_eq!(photos.len(), 1);
        assert_eq!(photos[0].path, Path::new("Photos/one.jpg"));
        assert!(is_directory(&archive, Path::new("Photos")).unwrap());
        assert!(!is_directory(&archive, Path::new("root.txt")).unwrap());

        fs::remove_file(&archive).unwrap();
        create_zip(&[source.join("root.txt")], &archive).unwrap();
        assert_eq!(list_directory(&archive, Path::new("")).unwrap().len(), 1);
    }

    #[test]
    fn an_extraction_takes_the_archives_own_folder_or_its_name() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("Project");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("a.txt"), b"a").unwrap();
        let loose = temp.path().join("loose.txt");
        fs::write(&loose, b"l").unwrap();

        let one = temp.path().join("one.tar.zst");
        create(std::slice::from_ref(&folder), &one, Packing::TarZst, &()).unwrap();
        assert_eq!(likely_name(&one), "Project");
        let staging = extract(&one, temp.path(), &()).unwrap();
        let (result, own) = staging.result().unwrap();
        assert!(own);
        assert_eq!(result.file_name().unwrap(), "Project");
        drop(staging);

        let many = temp.path().join("many.zip");
        create_zip(&[folder, loose], &many).unwrap();
        assert_eq!(likely_name(&many), "many");
        let staging = extract(&many, temp.path(), &()).unwrap();
        assert!(!staging.result().unwrap().1);
        drop(staging);
        assert!(
            fs::read_dir(temp.path()).unwrap().all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".tilo-extract")),
            "a dropped staging folder leaves nothing"
        );
        assert_eq!(stem(Path::new("/x/game_2.1.tar.xz")), "game_2.1");
    }
}
