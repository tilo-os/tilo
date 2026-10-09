//! Publish an already sequestered, verified object into the shared home Trash.
//! The standard Files Trash listing and Restore action consume this metadata.
use super::*;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

const PATH_ENCODING: percent_encoding::AsciiSet = percent_encoding::NON_ALPHANUMERIC.remove(b'/');

pub(super) struct TrashSlot {
    files: SecureNearbyDestination,
    path: PathBuf,
}

impl TrashSlot {
    pub(super) fn prepare(original: &Path) -> Result<Self, String> {
        let data = tilo_paths::data_dir().ok_or("home Trash is unavailable")?;
        fs::create_dir_all(&data).map_err(|error| error.to_string())?;
        let data_directory = SecureNearbyDestination::open_beneath(&data, &data)?;
        let trash = private_directory(&data_directory, "Trash")?;
        let files = private_directory(&trash, "files")?;
        let info = private_directory(&trash, "info")?;
        let leaf = original.file_name().ok_or("Trash original has no name")?;
        let mut name = OsString::from_vec(leaf.as_bytes().iter().take(180).copied().collect());
        name.push(format!(".{}", uuid::Uuid::new_v4()));
        let mut info_name = name.clone();
        info_name.push(".trashinfo");
        let encoded =
            percent_encoding::percent_encode(original.as_os_str().as_bytes(), &PATH_ENCODING);
        let contents = format!(
            "[Trash Info]\nPath={encoded}\nDeletionDate={}\n",
            chrono::Local::now().format("%Y-%m-%dT%H:%M:%S")
        );
        let mut metadata = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(info.anchored_root().join(&info_name))
            .map_err(|error| error.to_string())?;
        metadata
            .write_all(contents.as_bytes())
            .and_then(|()| metadata.sync_all())
            .map_err(|error| error.to_string())?;
        sync_directory(&info)?;
        let path = data.join("Trash/files").join(&name);
        Ok(Self { files, path })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn publish_with(
        self,
        recovery: &SecureNearbyDestination,
        _retained: &std::ffi::OsStr,
        publish: impl FnOnce() -> Result<(), String>,
    ) -> Result<PathBuf, String> {
        // A durable cross-device intent may publish on restart. Preserve its
        // standard metadata even when its payload has not reached Trash yet.
        publish()?;
        sync_directory(&self.files)
            .and_then(|()| sync_directory(recovery))
            .map_err(|error| format!("Trash item retained at {}: {error}", self.path.display()))?;
        Ok(self.path)
    }
}

pub(super) fn private_directory(
    parent: &SecureNearbyDestination,
    name: &str,
) -> Result<SecureNearbyDestination, String> {
    let name = std::ffi::OsStr::new(name);
    match parent.private_child(name) {
        Ok(directory) => Ok(directory),
        Err(error) => {
            let entry = parent.entry(name).map_err(|_| error)?;
            let metadata = entry.metadata().map_err(|error| error.to_string())?;
            // SAFETY: geteuid reads the current process's effective uid.
            let owner = unsafe { libc::geteuid() };
            if !metadata.is_dir()
                || metadata.uid() != owner
                || metadata.permissions().mode() & 0o077 != 0
            {
                return Err("Trash directory is not a private directory owned by this user".into());
            }
            let path = parent.anchored_root().join(name);
            let directory = SecureNearbyDestination::open_beneath(&path, &path)?;
            if Identity::of(&directory.metadata()?) != Identity::of(&metadata) {
                return Err("Trash directory changed while opening".into());
            }
            Ok(directory)
        }
    }
}

fn sync_directory(directory: &SecureNearbyDestination) -> Result<(), String> {
    fs::File::open(directory.anchored_root())
        .and_then(|entry| entry.sync_all())
        .map_err(|error| error.to_string())
}

pub(super) fn files_directory() -> Result<SecureNearbyDestination, String> {
    let data = tilo_paths::data_dir().ok_or("home Trash unavailable")?;
    let root = SecureNearbyDestination::open_beneath(&data, &data)?;
    let trash = private_directory(&root, "Trash")?;
    private_directory(&trash, "files")
}

/// Remove only the metadata belonging to a successfully restored Trash leaf.
/// The payload has already moved through its retained no-replace descriptor.
pub(super) fn consume_info(path: &Path) -> Result<(), String> {
    let data = tilo_paths::data_dir().ok_or("home Trash unavailable")?;
    if path.parent() != Some(data.join("Trash/files").as_path()) {
        return Err("restored Trash metadata escaped its scope".into());
    }
    let data_directory = SecureNearbyDestination::open_beneath(&data, &data)?;
    let trash = private_directory(&data_directory, "Trash")?;
    let info = private_directory(&trash, "info")?;
    let mut name = path
        .file_name()
        .ok_or("restored Trash item has no name")?
        .to_os_string();
    name.push(".trashinfo");
    match fs::remove_file(info.anchored_root().join(name)) {
        Ok(()) => sync_directory(&info),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}
