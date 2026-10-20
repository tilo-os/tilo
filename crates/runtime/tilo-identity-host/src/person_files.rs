//! Writing a person's own files from a process running as root.
//!
//! A privileged enrollment (the nearby-pairing broker approving with a root
//! seed, a headless join) writes into the person's identity directory, which
//! the person owns and can change at any moment. Writing there as root and
//! fixing the owner afterwards lets the person swap a directory on the way
//! for a symlink and have root create, replace or hand them a file wherever
//! the link points. So those writes run with the person's own filesystem
//! identity instead: they reach exactly what the person could reach
//! themselves, and what they create is the person's from the start.

use std::io;
use std::path::Path;

/// Run `write` as the owner of `path`, or of its nearest existing ancestor,
/// when this process is root and that owner is not. Only the calling
/// thread's filesystem identity changes (Linux `setfsuid`, which also drops
/// root's filesystem capabilities while it lasts), and it is restored when
/// `write` returns or unwinds. Anywhere else `write` runs as it is.
///
/// # Errors
///
/// Why the owner could not be read or assumed, or `write`'s own error.
pub fn as_owner_of<T>(path: &Path, write: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt as _;

        if rustix::process::geteuid().as_raw() != 0 {
            return write();
        }
        let owner = path
            .ancestors()
            .find_map(|ancestor| std::fs::metadata(ancestor).ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no ancestor exists"))?;
        if owner.uid() == 0 {
            return write();
        }
        let _person = FilesystemIdentity::assume(owner.uid(), owner.gid())?;
        write()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        write()
    }
}

/// The calling thread's filesystem identity, switched to a person's until
/// dropped.
#[cfg(target_os = "linux")]
struct FilesystemIdentity {
    uid: u32,
    gid: u32,
    groups: Vec<libc::gid_t>,
}

#[cfg(target_os = "linux")]
impl FilesystemIdentity {
    fn assume(uid: u32, gid: u32) -> io::Result<Self> {
        let previous = Self {
            uid: current_fsuid(),
            gid: current_fsgid(),
            groups: thread_groups()?,
        };
        // The supplementary groups go too, so root's groups grant nothing.
        // The raw call changes this thread only; libc's wrapper changes them
        // for every thread of the process.
        set_thread_groups(&[])?;
        // SAFETY: plain credential syscalls; they return the previous id,
        // so success is checked by reading the ids back.
        unsafe {
            libc::setfsgid(gid);
            libc::setfsuid(uid);
        }
        if current_fsuid() != uid || current_fsgid() != gid {
            drop(previous);
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "could not take the person's filesystem identity",
            ));
        }
        Ok(previous)
    }
}

#[cfg(target_os = "linux")]
impl Drop for FilesystemIdentity {
    fn drop(&mut self) {
        // SAFETY: as in `assume`, restoring the ids read before it.
        unsafe {
            libc::setfsuid(self.uid);
            libc::setfsgid(self.gid);
        }
        let restored = set_thread_groups(&self.groups).is_ok()
            && current_fsuid() == self.uid
            && current_fsgid() == self.gid;
        // A thread left with another identity must not go on as root's.
        assert!(restored, "could not restore the filesystem identity");
    }
}

#[cfg(target_os = "linux")]
fn current_fsuid() -> u32 {
    // SAFETY: the invalid all-ones id is never installed; the call reports
    // the current one.
    unsafe { libc::setfsuid(u32::MAX) }.cast_unsigned()
}

#[cfg(target_os = "linux")]
fn current_fsgid() -> u32 {
    // SAFETY: as in `current_fsuid`.
    unsafe { libc::setfsgid(u32::MAX) }.cast_unsigned()
}

#[cfg(target_os = "linux")]
fn thread_groups() -> io::Result<Vec<libc::gid_t>> {
    // SAFETY: a zero count asks for the length and writes nothing.
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    let mut groups = vec![0; usize::try_from(count).map_err(|_| io::Error::last_os_error())?];
    // SAFETY: the buffer holds exactly `count` entries.
    if unsafe { libc::getgroups(count, groups.as_mut_ptr()) } != count {
        return Err(io::Error::last_os_error());
    }
    Ok(groups)
}

#[cfg(target_os = "linux")]
fn set_thread_groups(groups: &[libc::gid_t]) -> io::Result<()> {
    // SAFETY: `groups` is a valid buffer of `groups.len()` ids.
    if unsafe { libc::syscall(libc::SYS_setgroups, groups.len(), groups.as_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    use super::*;

    /// A user id other than root's that this process can hand files to: the
    /// first one its user namespace maps, if any does.
    fn another_person() -> Option<u32> {
        let map = std::fs::read_to_string("/proc/self/uid_map").ok()?;
        map.lines().find_map(|line| {
            let mut fields = line.split_whitespace().map(str::parse::<u32>);
            let (Some(Ok(inside)), Some(Ok(_)), Some(Ok(count))) =
                (fields.next(), fields.next(), fields.next())
            else {
                return None;
            };
            (inside..inside.saturating_add(count)).find(|uid| *uid != 0)
        })
    }

    #[test]
    fn root_writes_into_a_persons_directory_only_as_the_person() {
        let person = another_person().filter(|_| rustix::process::geteuid().as_raw() == 0);
        let Some(person_uid) = person else {
            // Not root, or no other person exists here: nothing switches.
            let dir = tempfile::tempdir().unwrap();
            as_owner_of(dir.path(), || std::fs::write(dir.path().join("a"), b"a")).unwrap();
            return;
        };
        let home = tempfile::tempdir().unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let person = home.path().join("person");
        std::fs::create_dir(&person).unwrap();
        if std::os::unix::fs::chown(&person, Some(person_uid), Some(person_uid)).is_err() {
            // A root without the capability to hand files to anyone (a
            // capability-stripped sandbox) cannot switch either.
            return;
        }
        let protected = home.path().join("protected");
        std::fs::create_dir(&protected).unwrap();

        // What it creates is the person's from the start.
        as_owner_of(&person.join("fleets/new.json"), || {
            std::fs::create_dir_all(person.join("fleets"))?;
            std::fs::write(person.join("fleets/new.json"), b"{}")
        })
        .unwrap();
        let written = std::fs::metadata(person.join("fleets/new.json")).unwrap();
        assert_eq!((written.uid(), written.gid()), (person_uid, person_uid));

        // A directory the person swapped for a link to root's leads nowhere.
        std::fs::remove_dir_all(person.join("fleets")).unwrap();
        std::os::unix::fs::symlink(&protected, person.join("fleets")).unwrap();
        let refused = as_owner_of(&person, || {
            std::fs::write(person.join("fleets/new.json"), b"{}")
        });
        assert_eq!(refused.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert!(!protected.join("new.json").exists());

        // The thread is root's again afterwards.
        std::fs::write(protected.join("root.json"), b"{}").unwrap();
        assert_eq!(current_fsuid(), 0);
    }
}
