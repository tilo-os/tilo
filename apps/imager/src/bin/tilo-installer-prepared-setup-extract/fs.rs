use super::{Result, invalid};
use serde_json::Value;
use std::ffi::CString;
use std::fs::{File, Metadata};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;

pub const FILE: &str = "prepared-setup.sealed";
pub const RECEIPT: &str = "receipt.json";
pub const DIRECTORY: &str = ".tilo-installer";
pub const MAX: u64 = 32 * 1024 * 1024 + 16384;
pub fn uid() -> u32 {
    rustix::process::geteuid().as_raw()
}
pub fn gid() -> u32 {
    rustix::process::getegid().as_raw()
}
fn c(s: &str) -> Result<CString> {
    CString::new(s).map_err(|_| invalid())
}
fn checked(fd: i32) -> Result<File> {
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful open transfers a new descriptor to File.
    Ok(unsafe { File::from_raw_fd(fd) })
}
pub fn open(parent: &File, name: &str, flags: i32, mode: u32) -> Result<File> {
    if name.is_empty() || name.contains('/') || matches!(name, "." | "..") {
        return Err(invalid());
    }
    let name = c(name)?;
    // SAFETY: CString and parent remain live across openat.
    checked(unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            mode,
        )
    })
}
pub fn child(parent: &File, name: &str) -> Result<File> {
    open(parent, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)
}
pub fn directory(path: &str) -> Result<File> {
    if !path.starts_with('/') {
        return Err(invalid());
    }
    let mut fd = checked(unsafe {
        libc::open(
            c("/")?.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    })?;
    // Normalize separators and current-directory components like pathlib,
    // but never resolve traversal or symlinks on the filesystem.
    for part in path
        .split('/')
        .skip(1)
        .filter(|p| !p.is_empty() && *p != ".")
    {
        fd = child(&fd, part)?;
    }
    Ok(fd)
}
pub fn protected(fd: &File, owner: u32, private: bool) -> Result<Metadata> {
    let m = fd.metadata()?;
    if m.uid() != owner || m.mode() & if private { 0o077 } else { 0o022 } != 0 {
        return Err(invalid());
    }
    Ok(m)
}
pub fn snapshot(m: &Metadata) -> Vec<i128> {
    // Each timestamp component is i64, so nanosecond scaling is exact in i128.
    vec![
        i128::from(m.dev()),
        i128::from(m.ino()),
        i128::from(m.len()),
        i128::from(m.mtime())
            .saturating_mul(1_000_000_000)
            .saturating_add(i128::from(m.mtime_nsec())),
        i128::from(m.ctime())
            .saturating_mul(1_000_000_000)
            .saturating_add(i128::from(m.ctime_nsec())),
    ]
}
pub fn identity(m: &Metadata) -> (u64, u64) {
    (m.dev(), m.ino())
}
pub fn owner(fd: &File, uid: u32, gid: u32, mode: u32) -> Result<()> {
    if unsafe { libc::fchown(fd.as_raw_fd(), uid, gid) } != 0
        || unsafe { libc::fchmod(fd.as_raw_fd(), mode) } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
pub fn private_child(parent: &File, name: &str, uid: u32, gid: u32, private: bool) -> Result<File> {
    let n = c(name)?;
    let created = unsafe { libc::mkdirat(parent.as_raw_fd(), n.as_ptr(), 0o700) } == 0;
    if !created && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists {
        return Err(std::io::Error::last_os_error());
    }
    let fd = child(parent, name)?;
    if created {
        owner(&fd, uid, gid, 0o700)?;
        parent.sync_all()?;
    }
    protected(&fd, uid, private)?;
    Ok(fd)
}
pub fn exists(parent: &File, name: &str) -> Result<bool> {
    let name = c(name)?;
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            st.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        return Ok(true);
    }
    let e = std::io::Error::last_os_error();
    if e.kind() == std::io::ErrorKind::NotFound {
        Ok(false)
    } else {
        Err(e)
    }
}
pub fn read(
    parent: &File,
    name: &str,
    bound: u64,
    own: Option<u32>,
    private: bool,
) -> Result<Vec<u8>> {
    let fd = open(parent, name, libc::O_RDONLY, 0)?;
    read_fd(&fd, bound, own, if private { 0o077 } else { 0 })
}
pub fn read_fd(fd: &File, bound: u64, own: Option<u32>, forbidden_mode: u32) -> Result<Vec<u8>> {
    let m = fd.metadata()?;
    if !m.is_file()
        || m.len() > bound
        || own.is_some_and(|o| m.uid() != o)
        || m.mode() & forbidden_mode != 0
    {
        return Err(invalid());
    }
    let mut data = Vec::new();
    fd.take(bound.checked_add(1).ok_or_else(invalid)?)
        .read_to_end(&mut data)?;
    if data.len() as u64 != m.len() || snapshot(&m) != snapshot(&fd.metadata()?) {
        return Err(invalid());
    }
    Ok(data)
}
pub fn json(
    parent: &File,
    name: &str,
    bound: u64,
    own: Option<u32>,
    private: bool,
    padded: bool,
    reserved: bool,
) -> Result<Option<Value>> {
    let data = read(parent, name, bound, own, private)?;
    if reserved && data.len() != 4096 {
        return Err(invalid());
    }
    if padded && data.len() == 4096 && data.iter().all(|b| *b == b' ') {
        return Ok(None);
    }
    if padded {
        if data.first().is_none_or(u8::is_ascii_whitespace) {
            return Err(invalid());
        }
        let mut parser = serde_json::Deserializer::from_slice(&data).into_iter::<Value>();
        let v = parser.next().ok_or_else(invalid)?.map_err(|_| invalid())?;
        if data
            .get(parser.byte_offset()..)
            .ok_or_else(invalid)?
            .iter()
            .any(|b| *b != b' ')
        {
            return Err(invalid());
        }
        Ok(Some(v))
    } else {
        Ok(Some(serde_json::from_slice(&data).map_err(|_| invalid())?))
    }
}
pub fn json_required(
    parent: &File,
    name: &str,
    bound: u64,
    own: Option<u32>,
    private: bool,
) -> Result<Value> {
    json(parent, name, bound, own, private, false, false)?.ok_or_else(invalid)
}
pub fn receipt(parent: &File, own: Option<u32>, private: bool) -> Result<Option<Value>> {
    let data = read(parent, RECEIPT, 1024, own, private)?;
    if data.len() == 1024 && data.iter().all(|b| *b == b' ') {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&data).map_err(|_| invalid())?))
}
pub fn ciphertext(parent: &File, own: u32) -> Result<Vec<u8>> {
    let data = read(parent, FILE, MAX, Some(own), true)?;
    if data.len() < 48 || !data.starts_with(b"TILOMIG1") {
        return Err(invalid());
    }
    Ok(data)
}
pub fn link(from: &File, name: &str, to: &File, target: &str) -> Result<()> {
    if unsafe {
        libc::linkat(
            from.as_raw_fd(),
            c(name)?.as_ptr(),
            to.as_raw_fd(),
            c(target)?.as_ptr(),
            0,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
pub fn unlink(parent: &File, name: &str) -> Result<()> {
    if unsafe { libc::unlinkat(parent.as_raw_fd(), c(name)?.as_ptr(), 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
pub struct Temporary<'a> {
    pub parent: &'a File,
    pub name: String,
    pub fd: File,
}
impl<'a> Temporary<'a> {
    pub fn new(parent: &'a File, uid: u32, gid: u32) -> Result<Self> {
        // tempfile supplies collision-resistant names; keep the publication itself fd-relative.
        let random = tempfile::Builder::new().prefix("prepared-").tempfile()?;
        let name = format!(
            ".{}",
            random
                .path()
                .file_name()
                .ok_or_else(invalid)?
                .to_str()
                .ok_or_else(invalid)?
        );
        let fd = open(
            parent,
            &name,
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )?;
        let temp = Self { parent, name, fd };
        owner(&temp.fd, uid, gid, 0o600)?;
        Ok(temp)
    }
    pub fn publish(&self, name: &str) -> Result<()> {
        self.fd.sync_all()?;
        link(self.parent, &self.name, self.parent, name)?;
        self.parent.sync_all()
    }
}
impl Drop for Temporary<'_> {
    fn drop(&mut self) {
        let _ = unlink(self.parent, &self.name);
    }
}
pub fn publish_json(parent: &File, name: &str, value: &Value, uid: u32, gid: u32) -> Result<()> {
    let data = serde_json::to_vec(value).map_err(|_| invalid())?;
    if data.len() > 32768 {
        return Err(invalid());
    }
    if exists(parent, name)? {
        if json_required(parent, name, 32768, Some(uid), true)? != *value {
            return Err(invalid());
        }
        return Ok(());
    }
    let temp = Temporary::new(parent, uid, gid)?;
    (&temp.fd).write_all(&data)?;
    temp.publish(name)
}
pub fn publish_cipher(parent: &File, data: &[u8], uid: u32, gid: u32) -> Result<()> {
    if exists(parent, FILE)? {
        if ciphertext(parent, uid)? != data {
            return Err(invalid());
        }
        return Ok(());
    }
    let temp = Temporary::new(parent, uid, gid)?;
    (&temp.fd).write_all(data)?;
    temp.publish(FILE)?;
    if ciphertext(parent, uid)? != data {
        return Err(invalid());
    }
    Ok(())
}
pub fn chain(root: &str, components: &[&str], uid: u32, gid: u32) -> Result<File> {
    let mut fd = directory(root)?;
    protected(&fd, uid, false)?;
    for (i, c) in components.iter().enumerate() {
        fd = private_child(&fd, c, uid, gid, Some(i) == components.len().checked_sub(1))?;
    }
    Ok(fd)
}
pub fn verify_chain(root: &str, components: &[&str], expected: &File, uid: u32) -> Result<()> {
    let mut fd = directory(root)?;
    protected(&fd, uid, false)?;
    for c in components {
        fd = child(&fd, c)?;
        protected(&fd, uid, false)?;
    }
    protected(&fd, uid, true)?;
    if identity(&fd.metadata()?) != identity(&expected.metadata()?) {
        return Err(invalid());
    }
    Ok(())
}
pub fn capacity(fd: &File) -> Result<(u64, bool)> {
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::fstatvfs(fd.as_raw_fd(), st.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let st = unsafe { st.assume_init() };
    Ok((
        st.f_bavail.saturating_mul(st.f_frsize),
        st.f_flag & libc::ST_RDONLY != 0,
    ))
}
