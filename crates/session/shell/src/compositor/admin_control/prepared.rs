//! First-use access is an actual root installer delivery, never an absent claim.
use serde::Deserialize;
use std::{
    fs::File,
    io::{self, Read},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::{Component, Path},
};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedRecord {
    schema: u32,
    uid: u32,
    gid: u32,
    home: String,
    home_device: u64,
    home_inode: u64,
    sealed_bytes: u64,
    ciphertext_sha256: String,
    identity_public_key: [u8; 32],
    setup_binding: String,
}
fn refusal() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
fn open(parent: &File, name: &std::ffi::OsStr, directory: bool) -> io::Result<File> {
    let name = std::ffi::CString::new(name.as_encoded_bytes()).map_err(|_| refusal())?;
    let flags = libc::O_RDONLY
        | libc::O_NOFOLLOW
        | libc::O_CLOEXEC
        | libc::O_NONBLOCK
        | if directory { libc::O_DIRECTORY } else { 0 };
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn controlled(file: &File) -> io::Result<()> {
    let info = file.metadata()?;
    if !info.is_dir() || info.uid() != 0 || info.mode() & 0o022 != 0 {
        return Err(refusal());
    }
    // This is a read observation of root custody by the target user, not
    // current-user publication authority. Linux ACL write access is reflected
    // in the group mask bits already refused above.
    Ok(())
}
fn root_directory(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    if !path.is_absolute() {
        return Err(refusal());
    }
    let mut parent = File::options()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")?;
    controlled(&parent)?;
    for component in path.components() {
        match component {
            Component::RootDir => (),
            Component::Normal(name) => {
                let child = open(&parent, name, true)?;
                controlled(&child)?;
                parent = child;
            }
            _ => return Err(refusal()),
        }
    }
    Ok(parent)
}
fn stamp(info: &std::fs::Metadata) -> (u64, u64, u64, u32, u32, u32, i64, i64, i64, i64) {
    (
        info.dev(),
        info.ino(),
        info.len(),
        info.uid(),
        info.gid(),
        info.mode(),
        info.mtime(),
        info.mtime_nsec(),
        info.ctime(),
        info.ctime_nsec(),
    )
}
fn bounded(file: &mut File, limit: u64) -> io::Result<Vec<u8>> {
    let before = file.metadata()?;
    if !before.is_file()
        || before.nlink() != 1
        || before.len() > limit
        || before.mode() & 0o022 != 0
    {
        return Err(refusal());
    }
    let mut bytes = Vec::new();
    Read::by_ref(file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != before.len() || stamp(&before) != stamp(&file.metadata()?) {
        return Err(refusal());
    }
    Ok(bytes)
}
/// The home directory and group of account `uid`, from the password
/// database.
fn account(uid: u32) -> io::Result<(std::path::PathBuf, u32)> {
    let mut pwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0u8; 16384];
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            pwd.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &raw mut result,
        )
    };
    if status != 0 || result.is_null() {
        return Err(refusal());
    }
    let pwd = unsafe { pwd.assume_init() };
    let home = Path::new(
        unsafe { std::ffi::CStr::from_ptr(pwd.pw_dir) }
            .to_str()
            .map_err(|_| refusal())?,
    )
    .to_owned();
    Ok((home, pwd.pw_gid))
}
/// This fixed root-controlled record was issued by firstboot after the real
/// account received the exact ciphertext authenticated by the native installer.
pub(super) fn check(identity_dir: &Path) -> io::Result<()> {
    let uid = rustix::process::geteuid().as_raw();
    let (home, gid) = account(uid)?;
    let system = root_directory(Path::new("/var/lib/tilo/prepared-first-use"))?;
    let name = format!("{uid}.json");
    let mut record = open(&system, std::ffi::OsStr::new(&name), false)?;
    if record.metadata()?.uid() != 0 {
        return Err(refusal());
    }
    let record_before = record.metadata()?;
    let body: PreparedRecord =
        serde_json::from_slice(&bounded(&mut record, 8192)?).map_err(|_| refusal())?;
    if body.schema != 1
        || body.uid != uid
        || body.gid != gid
        || Path::new(&body.home) != home
        || !identity_dir.starts_with(&home)
        || body.identity_public_key == [0; 32]
        || body.setup_binding.is_empty()
        || body.setup_binding.len() > 256
        || body.setup_binding.chars().any(char::is_control)
        || !(48..=33_570_816).contains(&body.sealed_bytes)
    {
        return Err(refusal());
    }
    // Public metadata may be imported before its certificate worker finishes.
    // It must already name the root installation's same person. A corrupt or
    // dangling selection is refused, never interpreted as no local identity.
    if let Some(record) = runtime_identity::resolve_active_identity(identity_dir) {
        let metadata = runtime_identity::load_metadata_from_path(&record).map_err(|_| refusal())?;
        if metadata.group_public_key != body.identity_public_key {
            return Err(refusal());
        }
    } else {
        for name in ["active", "primary.json"] {
            match std::fs::symlink_metadata(identity_dir.join(name)) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                _ => return Err(refusal()),
            }
        }
    }
    let pending = crate::onboarding_prepared::Pending::open(&home)?;
    if hex::encode(pending.digest()) != body.ciphertext_sha256 {
        return Err(refusal());
    }
    let info = std::fs::symlink_metadata(&home)?;
    if !info.is_dir()
        || info.uid() != uid
        || info.gid() != body.gid
        || info.mode() & 0o022 != 0
        || info.dev() != body.home_device
        || info.ino() != body.home_inode
    {
        return Err(refusal());
    }
    // A claimed or corrupt existing device cannot be made unclaimed by missing
    // session identity. This protected delivery names exactly one intended person.
    let paths = tilo_user_identity::persist::IdentityPaths::default_system();
    let owner = public_file(&paths.user_pubkey, 32)?;
    if owner
        .as_ref()
        .is_some_and(|key| key.as_slice() != body.identity_public_key)
    {
        return Err(refusal());
    }
    let certificate = public_file(&paths.device_cert, 65536)?;
    if let Some(bytes) = certificate {
        let cert: tilo_user_identity::DeviceCert =
            tilo_user_identity::canonical::from_canonical_bytes(&bytes).map_err(|_| refusal())?;
        tilo_user_identity::verify_device_cert(&cert).map_err(|_| refusal())?;
        let key = tilo_user_identity::persist::load_device_key(&paths).map_err(|_| refusal())?;
        if cert.user_public_key != body.identity_public_key
            || cert.device_public_key != key.verifying_key().to_bytes()
        {
            return Err(refusal());
        }
    } else if owner.is_some() {
        return Err(refusal());
    }
    pending.validate()?;
    if stamp(&info) != stamp(&std::fs::symlink_metadata(&home)?) {
        return Err(refusal());
    }
    let named = open(&system, std::ffi::OsStr::new(&name), false)?;
    if stamp(&named.metadata()?) != stamp(&record_before) {
        return Err(refusal());
    }
    controlled(&system)
}
fn public_file(path: &Path, limit: u64) -> io::Result<Option<Vec<u8>>> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = match File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    bounded(&mut file, limit).map(Some)
}
