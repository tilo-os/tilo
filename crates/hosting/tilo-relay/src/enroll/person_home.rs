//! The approver's access to what lives in the person's home: the device fleet
//! in their identity directory and the room list their session exports.
//!
//! An approval backed by a seed under `/var/lib/tilo` runs as root, but the
//! person (and anything that can write to their home) controls every path in
//! it. Root therefore never opens those paths. Each access runs in a fresh
//! exec of this binary that has dropped to the person's uid and primary group
//! before exec, so a link planted in the home resolves with the person's own
//! permissions and reaches nothing they could not already change.
//!
//! The accessing process, whether that child or an approval already running
//! as the person, also opens every component it touches without following a
//! final symbolic link. Files are reached through directory descriptors
//! opened with `O_NOFOLLOW | O_DIRECTORY` whose owner must be the accessing
//! uid, and replacements are renamed into place inside the descriptor that
//! was checked, so swapping a directory for a link mid-way cannot redirect
//! the write.

use std::ffi::{CStr, CString, OsString};
use std::io::{Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};

/// Largest device fleet an approval reads or writes.
pub const MAX_FLEET_BYTES: usize = 1024 * 1024;
/// Largest exported room list an approval reads.
pub const MAX_ROOMS_BYTES: usize = 64 * 1024;
const FLEETS_DIR: &str = "fleets";
const CHILD_TIMEOUT: Duration = Duration::from_secs(30);

/// Where the person's own files are read and written from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonHome {
    /// This process runs as the person.
    Direct,
    /// This process is root; every access runs in `program` as `person`.
    Delegated { program: PathBuf, person: Person },
}

/// The credentials a delegated access runs with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Person {
    pub uid: libc::uid_t,
    pub gid: libc::gid_t,
}

impl PersonHome {
    /// How this process, running with effective uid `euid`, reaches the home
    /// of `person_uid`, the account the caller approves for.
    ///
    /// Root told whose home it is touches it only as that account. Root
    /// acting for itself reaches only directories and files root owns, never
    /// following a final link, so a person's directory is refused rather than
    /// written as root. A process that is not root can only act for itself.
    pub fn for_process(
        euid: libc::uid_t,
        person_uid: Option<libc::uid_t>,
        lookup_gid: impl FnOnce(libc::uid_t) -> anyhow::Result<libc::gid_t>,
    ) -> anyhow::Result<Self> {
        match (euid, person_uid) {
            (0, Some(uid)) if uid != 0 => Ok(Self::Delegated {
                program: PathBuf::from("/proc/self/exe"),
                person: Person {
                    uid,
                    gid: lookup_gid(uid)?,
                },
            }),
            (euid, Some(uid)) if uid != euid => bail!(
                "--person-uid {uid} is not the account this runs as; run the approval as that \
                 account"
            ),
            (_, _) => Ok(Self::Direct),
        }
    }

    /// Whether every access runs in a child as the person.
    pub const fn is_delegated(&self) -> bool {
        matches!(self, Self::Delegated { .. })
    }

    /// The identity's device fleet in `identity_dir`, or `None` when there is
    /// none yet.
    pub fn read_fleet(
        &self,
        identity_dir: &Path,
        user: [u8; 32],
    ) -> anyhow::Result<Option<Vec<u8>>> {
        match self {
            Self::Direct => Ok(local::read_fleet(identity_dir, user, local::own_uid())?),
            Self::Delegated { program, person } => {
                let output = run_child(
                    program,
                    *person,
                    &child_args(ChildOperation::ReadFleet, identity_dir, Some(user)),
                    None,
                )?;
                decode_read(&output, MAX_FLEET_BYTES)
            }
        }
    }

    /// Replace the identity's device fleet in `identity_dir` with `bytes`.
    pub fn write_fleet(
        &self,
        identity_dir: &Path,
        user: [u8; 32],
        bytes: &[u8],
    ) -> anyhow::Result<()> {
        if bytes.len() > MAX_FLEET_BYTES {
            bail!("device fleet exceeds 1 MiB");
        }
        match self {
            Self::Direct => Ok(local::write_fleet(
                identity_dir,
                user,
                bytes,
                local::own_uid(),
            )?),
            Self::Delegated { program, person } => {
                run_child(
                    program,
                    *person,
                    &child_args(ChildOperation::WriteFleet, identity_dir, Some(user)),
                    Some(bytes),
                )?;
                Ok(())
            }
        }
    }

    /// The exported room list at `path`, or `None` when there is none.
    pub fn read_rooms(&self, path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
        match self {
            Self::Direct => Ok(local::read_rooms(path, local::own_uid())?),
            Self::Delegated { program, person } => {
                let output = run_child(
                    program,
                    *person,
                    &child_args(ChildOperation::ReadRooms, path, None),
                    None,
                )?;
                decode_read(&output, MAX_ROOMS_BYTES)
            }
        }
    }
}

/// An access the delegated child performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ChildOperation {
    ReadFleet,
    WriteFleet,
    ReadRooms,
}

/// The name of the hidden subcommand the delegated child runs.
pub const CHILD_COMMAND: &str = "person-home";

fn child_args(operation: ChildOperation, path: &Path, user: Option<[u8; 32]>) -> Vec<OsString> {
    let operation = match operation {
        ChildOperation::ReadFleet => "read-fleet",
        ChildOperation::WriteFleet => "write-fleet",
        ChildOperation::ReadRooms => "read-rooms",
    };
    let mut args: Vec<OsString> = vec![
        CHILD_COMMAND.into(),
        operation.into(),
        "--path".into(),
        path.into(),
    ];
    if let Some(user) = user {
        args.push("--user".into());
        args.push(hex::encode(user).into());
    }
    args
}

/// The child's command line, environment and credentials. The environment is
/// empty, the working directory is `/`, and before exec the child gives up
/// root for good and marks every inherited descriptor above stderr
/// close-on-exec, so it holds nothing of the approving process but its
/// stdin, stdout and stderr.
fn child_command(program: &Path, person: Person, args: &[OsString]) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command.args(args).env_clear().current_dir("/");
    let Person { uid, gid } = person;
    // SAFETY: the closure runs in the forked child before exec and calls only
    // async-signal-safe syscalls on values copied before the fork.
    unsafe {
        command.pre_exec(move || drop_to(uid, gid));
    }
    command
}

/// Give up root for `uid`/`gid` irrevocably. Runs between fork and exec.
fn drop_to(uid: libc::uid_t, gid: libc::gid_t) -> std::io::Result<()> {
    const CLOSE_RANGE_CLOEXEC: libc::c_uint = 1 << 2;
    // SAFETY: plain syscalls with value arguments; `groups` outlives the call.
    unsafe {
        if libc::syscall(
            libc::SYS_close_range,
            3 as libc::c_uint,
            libc::c_uint::MAX,
            CLOSE_RANGE_CLOEXEC,
        ) != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let groups = [gid];
        if libc::setgroups(1, groups.as_ptr()) != 0
            || libc::setresgid(gid, gid, gid) != 0
            || libc::setresuid(uid, uid, uid) != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        if libc::getuid() != uid
            || libc::geteuid() != uid
            || libc::getgid() != gid
            || libc::getegid() != gid
            || libc::setuid(0) == 0
        {
            return Err(std::io::Error::from_raw_os_error(libc::EPERM));
        }
    }
    Ok(())
}

fn run_child(
    program: &Path,
    person: Person,
    args: &[OsString],
    input: Option<&[u8]>,
) -> anyhow::Result<Vec<u8>> {
    let command = child_command(program, person, args);
    let output = bounded_command::run_prepared_with_input_limit(
        command,
        input,
        MAX_FLEET_BYTES,
        MAX_FLEET_BYTES + 1,
        16 * 1024,
        CHILD_TIMEOUT,
    )
    .context("read the person's identity files as the person")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{}", stderr.trim());
    }
    Ok(output.stdout)
}

/// A read's answer on the child's stdout: `0` when the file is absent, `1`
/// followed by its bytes otherwise.
fn decode_read(output: &[u8], limit: usize) -> anyhow::Result<Option<Vec<u8>>> {
    match output.split_first() {
        Some((b'0', [])) => Ok(None),
        Some((b'1', bytes)) if bytes.len() <= limit => Ok(Some(bytes.to_vec())),
        Some((b'1', _)) => bail!("the person's file exceeds {limit} bytes"),
        _ => bail!("the person's identity file reader answered malformed output"),
    }
}

fn encode_read(bytes: Option<&[u8]>) -> Vec<u8> {
    bytes.map_or_else(
        || b"0".to_vec(),
        |bytes| {
            let mut out = Vec::with_capacity(bytes.len().saturating_add(1));
            out.push(b'1');
            out.extend_from_slice(bytes);
            out
        },
    )
}

/// The delegated child: perform `operation` as this process's own uid and
/// answer on stdout.
pub fn serve_child(
    operation: ChildOperation,
    path: &Path,
    user: Option<&str>,
) -> anyhow::Result<()> {
    let uid = local::own_uid();
    let user = || -> anyhow::Result<[u8; 32]> {
        hex::decode(user.context("--user is required")?)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .context("--user must be a 32-byte hex identity")
    };
    let mut stdout = std::io::stdout().lock();
    match operation {
        ChildOperation::ReadFleet => {
            let bytes = local::read_fleet(path, user()?, uid)?;
            stdout.write_all(&encode_read(bytes.as_deref()))?;
        }
        ChildOperation::ReadRooms => {
            let bytes = local::read_rooms(path, uid)?;
            stdout.write_all(&encode_read(bytes.as_deref()))?;
        }
        ChildOperation::WriteFleet => {
            let mut bytes = Vec::new();
            std::io::stdin()
                .lock()
                .take((MAX_FLEET_BYTES as u64).saturating_add(1))
                .read_to_end(&mut bytes)?;
            if bytes.len() > MAX_FLEET_BYTES {
                bail!("device fleet exceeds 1 MiB");
            }
            local::write_fleet(path, user()?, &bytes, uid)?;
        }
    }
    stdout.flush()?;
    Ok(())
}

/// The person's primary group, from the account database.
pub fn primary_gid(uid: libc::uid_t) -> anyhow::Result<libc::gid_t> {
    let mut buffer = vec![0_u8; 16 * 1024];
    // SAFETY: an all-zero passwd is a valid out-parameter.
    let mut record: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is valid for the call and `buffer` outlives it.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            &raw mut record,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &raw mut found,
        )
    };
    if status != 0 || found.is_null() {
        bail!("account {uid} was not found");
    }
    Ok(record.pw_gid)
}

/// No-follow, owner-checked file access for the uid this process runs as.
pub mod local {
    use super::{CStr, CString, FLEETS_DIR, MAX_FLEET_BYTES, MAX_ROOMS_BYTES, OwnedFd, Path};
    use std::io::{self, Read as _, Write as _};
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::os::unix::ffi::OsStrExt as _;

    pub fn own_uid() -> libc::uid_t {
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() }
    }

    fn invalid(message: impl Into<String>) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, message.into())
    }

    fn c_path(path: &Path) -> io::Result<CString> {
        CString::new(path.as_os_str().as_bytes()).map_err(|_| invalid("path contains NUL"))
    }

    fn c_name(name: &str) -> io::Result<CString> {
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(invalid(format!("{name:?} is not a single file name")));
        }
        CString::new(name).map_err(|_| invalid("name contains NUL"))
    }

    fn fstat(fd: &OwnedFd) -> io::Result<libc::stat> {
        // SAFETY: an all-zero stat is a valid out-parameter.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is open and `stat` is writable.
        if unsafe { libc::fstat(fd.as_raw_fd(), &raw mut stat) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(stat)
    }

    fn checked(
        fd: OwnedFd,
        kind: libc::mode_t,
        owner: libc::uid_t,
        what: &str,
    ) -> io::Result<OwnedFd> {
        let stat = fstat(&fd)?;
        if stat.st_mode & libc::S_IFMT != kind {
            return Err(invalid(format!("{what} is not a {}", kind_name(kind))));
        }
        if stat.st_uid != owner {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{what} belongs to uid {}, not {owner}", stat.st_uid),
            ));
        }
        Ok(fd)
    }

    const fn kind_name(kind: libc::mode_t) -> &'static str {
        if kind == libc::S_IFDIR {
            "directory"
        } else {
            "regular file"
        }
    }

    fn from_raw(fd: libc::c_int) -> io::Result<OwnedFd> {
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` was just returned open by the kernel and is owned here.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    const DIR_FLAGS: libc::c_int =
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    const READ_FLAGS: libc::c_int =
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC;

    /// Open the directory at `path` without following a final link; it must
    /// belong to `owner`.
    pub fn open_owned_dir(path: &Path, owner: libc::uid_t) -> io::Result<OwnedFd> {
        let c = c_path(path)?;
        // SAFETY: `c` is NUL-terminated.
        let fd = from_raw(unsafe { libc::open(c.as_ptr(), DIR_FLAGS) })?;
        checked(fd, libc::S_IFDIR, owner, &path.display().to_string())
    }

    /// Open `name` inside `dir` as a directory without following a link;
    /// `None` when it does not exist and `create` is false.
    pub fn open_owned_subdir(
        dir: &OwnedFd,
        name: &str,
        owner: libc::uid_t,
        create: bool,
    ) -> io::Result<Option<OwnedFd>> {
        let c = c_name(name)?;
        loop {
            // SAFETY: `dir` is open and `c` is NUL-terminated.
            let fd = unsafe { libc::openat(dir.as_raw_fd(), c.as_ptr(), DIR_FLAGS) };
            match from_raw(fd) {
                Ok(fd) => return checked(fd, libc::S_IFDIR, owner, name).map(Some),
                Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
                    // SAFETY: `dir` is open and `c` is NUL-terminated.
                    if unsafe { libc::mkdirat(dir.as_raw_fd(), c.as_ptr(), 0o777) } != 0 {
                        let error = io::Error::last_os_error();
                        if error.kind() != io::ErrorKind::AlreadyExists {
                            return Err(error);
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            }
        }
    }

    /// Read `name` inside `dir`: a regular file belonging to `owner`, opened
    /// without following a link or blocking on a FIFO, at most `limit`
    /// bytes. `None` when it does not exist.
    pub fn read_owned_file(
        dir: &OwnedFd,
        name: &str,
        owner: libc::uid_t,
        limit: usize,
    ) -> io::Result<Option<Vec<u8>>> {
        let c = c_name(name)?;
        // SAFETY: `dir` is open and `c` is NUL-terminated.
        let fd = match from_raw(unsafe { libc::openat(dir.as_raw_fd(), c.as_ptr(), READ_FLAGS) }) {
            Ok(fd) => fd,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let fd = checked(fd, libc::S_IFREG, owner, name)?;
        let mut bytes = Vec::new();
        std::fs::File::from(fd)
            .take((limit as u64).saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(invalid(format!("{name} exceeds {limit} bytes")));
        }
        Ok(Some(bytes))
    }

    fn stat_at(dir: &OwnedFd, name: &CStr) -> io::Result<Option<libc::stat>> {
        // SAFETY: an all-zero stat is a valid out-parameter.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `dir` is open, `name` NUL-terminated, `stat` writable.
        let status = unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                name.as_ptr(),
                &raw mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if status == 0 {
            return Ok(Some(stat));
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(error)
        }
    }

    /// Atomically replace `name` inside `dir` with `bytes`. An existing entry
    /// must be a regular file belonging to `owner`; its permission bits and
    /// group carry over. The new file is created exclusively inside `dir` and
    /// renamed over `name` there, so neither step resolves a link.
    pub fn replace_owned_file(
        dir: &OwnedFd,
        name: &str,
        bytes: &[u8],
        owner: libc::uid_t,
    ) -> io::Result<()> {
        let target = c_name(name)?;
        let existing = stat_at(dir, &target)?;
        if let Some(stat) = &existing
            && (stat.st_mode & libc::S_IFMT != libc::S_IFREG || stat.st_uid != owner)
        {
            return Err(invalid(format!(
                "{name} is not a regular file belonging to uid {owner}"
            )));
        }
        let mode = existing.map_or(0o644, |stat| stat.st_mode & 0o777);
        let mut random = [0_u8; 8];
        getrandom::rand_core::Rng::fill_bytes(
            &mut getrandom::rand_core::UnwrapErr(getrandom::SysRng),
            &mut random,
        );
        let temp = c_name(&format!(".{name}.{}.tmp", hex::encode(random)))?;
        // SAFETY: `dir` is open and `temp` is NUL-terminated.
        let fd = from_raw(unsafe {
            libc::openat(
                dir.as_raw_fd(),
                temp.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                libc::c_uint::from(mode),
            )
        })?;
        let result = (|| {
            if let Some(stat) = &existing {
                // A group this account cannot use is left as the new file's
                // default; the replacement never depends on it.
                // SAFETY: `fd` is open.
                let _ = unsafe { libc::fchown(fd.as_raw_fd(), u32::MAX, stat.st_gid) };
                // SAFETY: `fd` is open.
                if unsafe { libc::fchmod(fd.as_raw_fd(), mode) } != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            let mut file = std::fs::File::from(fd);
            file.write_all(bytes)?;
            file.sync_all()?;
            // SAFETY: `dir` is open and both names are NUL-terminated.
            if unsafe {
                libc::renameat(
                    dir.as_raw_fd(),
                    temp.as_ptr(),
                    dir.as_raw_fd(),
                    target.as_ptr(),
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `dir` is open.
            unsafe { libc::fsync(dir.as_raw_fd()) };
            Ok(())
        })();
        if result.is_err() {
            // SAFETY: `dir` is open and `temp` is NUL-terminated.
            unsafe { libc::unlinkat(dir.as_raw_fd(), temp.as_ptr(), 0) };
        }
        result
    }

    fn fleet_name(user: [u8; 32]) -> String {
        format!("{}.json", hex::encode(user))
    }

    /// The fleet of `user` in `identity_dir`, read as `owner`.
    pub fn read_fleet(
        identity_dir: &Path,
        user: [u8; 32],
        owner: libc::uid_t,
    ) -> io::Result<Option<Vec<u8>>> {
        let dir = open_owned_dir(identity_dir, owner)?;
        read_fleet_at(&dir, user, owner)
    }

    /// The fleet of `user` in the already-checked identity directory `dir`.
    pub fn read_fleet_at(
        dir: &OwnedFd,
        user: [u8; 32],
        owner: libc::uid_t,
    ) -> io::Result<Option<Vec<u8>>> {
        let Some(fleets) = open_owned_subdir(dir, FLEETS_DIR, owner, false)? else {
            return Ok(None);
        };
        read_owned_file(&fleets, &fleet_name(user), owner, MAX_FLEET_BYTES)
    }

    /// Replace the fleet of `user` in `identity_dir`, as `owner`.
    pub fn write_fleet(
        identity_dir: &Path,
        user: [u8; 32],
        bytes: &[u8],
        owner: libc::uid_t,
    ) -> io::Result<()> {
        let dir = open_owned_dir(identity_dir, owner)?;
        write_fleet_at(&dir, user, bytes, owner)
    }

    /// Replace the fleet of `user` in the already-checked identity directory.
    pub fn write_fleet_at(
        dir: &OwnedFd,
        user: [u8; 32],
        bytes: &[u8],
        owner: libc::uid_t,
    ) -> io::Result<()> {
        let fleets = open_owned_subdir(dir, FLEETS_DIR, owner, true)?
            .ok_or_else(|| invalid("fleet directory could not be created"))?;
        replace_owned_file(&fleets, &fleet_name(user), bytes, owner)
    }

    /// The exported room list at `path`, read as `owner`: its directory and
    /// the file both belong to `owner` and neither is a link.
    pub fn read_rooms(path: &Path, owner: libc::uid_t) -> io::Result<Option<Vec<u8>>> {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
        else {
            return Err(invalid("room list path has no directory and file name"));
        };
        let dir = match open_owned_dir(parent, owner) {
            Ok(dir) => dir,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        read_owned_file(&dir, name, owner, MAX_ROOMS_BYTES)
    }
}

#[cfg(test)]
mod tests {
    use super::local::*;
    use super::*;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{MetadataExt as _, symlink};

    const USER: [u8; 32] = [7; 32];

    fn fleet_file(identity: &Path) -> PathBuf {
        identity
            .join(FLEETS_DIR)
            .join(format!("{}.json", hex::encode(USER)))
    }

    fn me() -> libc::uid_t {
        own_uid()
    }

    #[test]
    fn root_only_ever_reaches_the_home_as_the_named_person() {
        let gid = |_| Ok(4242);
        assert_eq!(
            PersonHome::for_process(0, Some(1000), gid).unwrap(),
            PersonHome::Delegated {
                program: PathBuf::from("/proc/self/exe"),
                person: Person {
                    uid: 1000,
                    gid: 4242
                },
            }
        );
        assert_eq!(
            PersonHome::for_process(0, None, gid).unwrap(),
            PersonHome::Direct
        );
        assert_eq!(
            PersonHome::for_process(0, Some(0), gid).unwrap(),
            PersonHome::Direct
        );
        assert!(PersonHome::for_process(0, Some(1000), |_| bail!("no account")).is_err());
        assert_eq!(
            PersonHome::for_process(1000, Some(1000), gid).unwrap(),
            PersonHome::Direct
        );
        assert_eq!(
            PersonHome::for_process(1000, None, gid).unwrap(),
            PersonHome::Direct
        );
        assert!(PersonHome::for_process(1000, Some(1001), gid).is_err());
    }

    #[test]
    fn the_child_runs_this_binary_with_only_its_operation() {
        let args = child_args(
            ChildOperation::WriteFleet,
            Path::new("/home/p/.local/share/tilo/identities"),
            Some(USER),
        );
        let command = child_command(
            Path::new("/proc/self/exe"),
            Person {
                uid: 1000,
                gid: 1000,
            },
            &args,
        );
        assert_eq!(command.get_program(), "/proc/self/exe");
        let shown: Vec<_> = command.get_args().collect();
        assert_eq!(
            shown,
            [
                CHILD_COMMAND,
                "write-fleet",
                "--path",
                "/home/p/.local/share/tilo/identities",
                "--user",
                &hex::encode(USER),
            ]
        );
        assert_eq!(command.get_current_dir(), Some(Path::new("/")));
    }

    #[test]
    fn child_answers_round_trip_and_stay_bounded() {
        assert_eq!(decode_read(&encode_read(None), 4).unwrap(), None);
        assert_eq!(
            decode_read(&encode_read(Some(b"abc")), 4).unwrap(),
            Some(b"abc".to_vec())
        );
        assert_eq!(
            decode_read(&encode_read(Some(b"")), 4).unwrap(),
            Some(Vec::new())
        );
        assert!(decode_read(&encode_read(Some(b"abcde")), 4).is_err());
        assert!(decode_read(b"", 4).is_err());
        assert!(decode_read(b"0junk", 4).is_err());
        assert!(decode_read(b"x", 4).is_err());
    }

    #[test]
    fn fleet_reads_and_writes_stay_inside_the_person_owned_directory() {
        let scope = tempfile::tempdir().unwrap();
        let identity = scope.path().join("identities");
        std::fs::create_dir(&identity).unwrap();
        assert_eq!(read_fleet(&identity, USER, me()).unwrap(), None);
        write_fleet(&identity, USER, b"{\"v\":1}", me()).unwrap();
        assert_eq!(
            read_fleet(&identity, USER, me()).unwrap().unwrap(),
            b"{\"v\":1}"
        );
        write_fleet(&identity, USER, b"{\"v\":2}", me()).unwrap();
        assert_eq!(std::fs::read(fleet_file(&identity)).unwrap(), b"{\"v\":2}");
        // No temporary file is left behind.
        assert_eq!(
            std::fs::read_dir(identity.join(FLEETS_DIR))
                .unwrap()
                .count(),
            1
        );
        // Another account's directory is refused outright.
        let other = me().wrapping_add(1);
        assert!(read_fleet(&identity, USER, other).is_err());
        assert!(write_fleet(&identity, USER, b"x", other).is_err());
        assert_eq!(std::fs::read(fleet_file(&identity)).unwrap(), b"{\"v\":2}");
    }

    #[test]
    fn a_symlinked_identity_directory_is_never_followed() {
        let scope = tempfile::tempdir().unwrap();
        let elsewhere = scope.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let identity = scope.path().join("identities");
        symlink(&elsewhere, &identity).unwrap();

        assert!(read_fleet(&identity, USER, me()).is_err());
        assert!(write_fleet(&identity, USER, b"planted", me()).is_err());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    #[test]
    fn a_symlinked_fleet_directory_is_never_followed() {
        let scope = tempfile::tempdir().unwrap();
        let elsewhere = scope.path().join("etc");
        std::fs::create_dir(&elsewhere).unwrap();
        let identity = scope.path().join("identities");
        std::fs::create_dir(&identity).unwrap();
        symlink(&elsewhere, identity.join(FLEETS_DIR)).unwrap();

        assert!(read_fleet(&identity, USER, me()).is_err());
        assert!(write_fleet(&identity, USER, b"planted", me()).is_err());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    #[test]
    fn a_symlinked_or_hardlinked_fleet_file_is_never_written_through() {
        let scope = tempfile::tempdir().unwrap();
        let identity = scope.path().join("identities");
        std::fs::create_dir_all(identity.join(FLEETS_DIR)).unwrap();
        let target = scope.path().join("shadow");
        std::fs::write(&target, b"root:secret").unwrap();

        symlink(&target, fleet_file(&identity)).unwrap();
        assert!(read_fleet(&identity, USER, me()).is_err());
        assert!(write_fleet(&identity, USER, b"planted", me()).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"root:secret");
        assert!(
            std::fs::symlink_metadata(fleet_file(&identity))
                .unwrap()
                .file_type()
                .is_symlink()
        );

        // A hard link is replaced as a directory entry; the other name keeps
        // its bytes.
        std::fs::remove_file(fleet_file(&identity)).unwrap();
        std::fs::hard_link(&target, fleet_file(&identity)).unwrap();
        write_fleet(&identity, USER, b"fleet", me()).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"root:secret");
        assert_eq!(std::fs::read(fleet_file(&identity)).unwrap(), b"fleet");
        assert_eq!(std::fs::metadata(&target).unwrap().nlink(), 1);
    }

    #[test]
    fn a_directory_swapped_for_a_link_after_the_check_does_not_redirect_the_write() {
        let scope = tempfile::tempdir().unwrap();
        let identity = scope.path().join("identities");
        std::fs::create_dir_all(identity.join(FLEETS_DIR)).unwrap();
        let elsewhere = scope.path().join("elsewhere");
        std::fs::create_dir_all(elsewhere.join(FLEETS_DIR)).unwrap();

        let checked = open_owned_dir(&identity, me()).unwrap();
        let moved = scope.path().join("moved");
        std::fs::rename(&identity, &moved).unwrap();
        symlink(&elsewhere, &identity).unwrap();

        write_fleet_at(&checked, USER, b"fleet", me()).unwrap();
        assert_eq!(std::fs::read(fleet_file(&moved)).unwrap(), b"fleet");
        assert_eq!(
            std::fs::read_dir(elsewhere.join(FLEETS_DIR))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            read_fleet_at(&checked, USER, me()).unwrap().unwrap(),
            b"fleet"
        );
    }

    #[test]
    fn a_planted_room_export_is_not_read() {
        let scope = tempfile::tempdir().unwrap();
        let state = scope.path().join("state/tilo");
        std::fs::create_dir_all(&state).unwrap();
        let rooms = state.join("personal-rooms.json");
        assert_eq!(read_rooms(&rooms, me()).unwrap(), None);
        assert_eq!(
            read_rooms(&scope.path().join("missing/personal-rooms.json"), me()).unwrap(),
            None
        );

        std::fs::write(&rooms, b"[]").unwrap();
        assert_eq!(read_rooms(&rooms, me()).unwrap().unwrap(), b"[]");
        assert!(read_rooms(&rooms, me().wrapping_add(1)).is_err());

        // A link to a file only root may read.
        let secret = scope.path().join("secret.json");
        std::fs::write(&secret, b"[\"secret\"]").unwrap();
        std::fs::remove_file(&rooms).unwrap();
        symlink(&secret, &rooms).unwrap();
        assert!(read_rooms(&rooms, me()).is_err());

        // A FIFO is refused without blocking.
        std::fs::remove_file(&rooms).unwrap();
        let fifo = CString::new(rooms.as_os_str().as_bytes()).unwrap();
        // SAFETY: `fifo` is NUL-terminated.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(read_rooms(&rooms, me()).is_err());

        // A linked directory is refused as well.
        let elsewhere = scope.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("personal-rooms.json"), b"[]").unwrap();
        let linked = scope.path().join("linked");
        symlink(&elsewhere, &linked).unwrap();
        assert!(read_rooms(&linked.join("personal-rooms.json"), me()).is_err());
    }

    #[test]
    fn direct_access_reads_and_writes_the_fleet_within_bounds() {
        let scope = tempfile::tempdir().unwrap();
        let home = PersonHome::Direct;
        assert_eq!(home.read_fleet(scope.path(), USER).unwrap(), None);
        home.write_fleet(scope.path(), USER, b"[]").unwrap();
        assert_eq!(home.read_fleet(scope.path(), USER).unwrap().unwrap(), b"[]");
        assert!(
            home.write_fleet(scope.path(), USER, &vec![0; MAX_FLEET_BYTES + 1])
                .is_err()
        );
    }
}
