//! Writing a person's own files from a process running as root.
//!
//! A privileged enrollment (the nearby-pairing broker approving with a root
//! seed, a headless join) writes into the person's identity directory, which
//! the person owns and can change at any moment. Writing there as root and
//! fixing the owner afterwards lets the person swap a directory on the way
//! for a symlink and have root create, replace or hand them a file wherever
//! the link points. So those writes run with the person's own filesystem
//! identity instead: they reach exactly what the person could reach
//! themselves, and what they create is the person's from the start. Whom
//! they run as comes from the caller, never from the directory: the person
//! can point their directory at one that root owns.

use std::io;
use std::path::Path;

/// Run `write` as the account `person` names: an explicit uid when a root
/// process acts for someone, or `None` for this process's own account.
///
/// `identity_dir`, or its nearest existing ancestor while it is still to be
/// created, must be a real directory (not a symlink to one) owned by that
/// account; anything else is refused before `write` runs. A root process
/// writing for another account does so with that account's filesystem
/// identity (Linux `setfsuid`, which also drops root's filesystem
/// capabilities while it lasts), so a directory swapped after the check
/// leads only where the person could write themselves. It never falls back
/// to writing as root for anyone but root. A process that is not root can
/// write only for itself, and with `None` runs `write` as it is: it can
/// reach nothing it could not reach anyway.
///
/// # Errors
///
/// `PermissionDenied` when the directory is a symlink, not a directory or
/// not the account's, or the account is not this process's own and this
/// process is not root; why the identity could not be assumed; or `write`'s
/// own error.
pub fn as_person<T>(
    person: Option<u32>,
    identity_dir: &Path,
    write: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    #[cfg(target_os = "linux")]
    {
        let process = rustix::process::geteuid().as_raw();
        if process != 0 && person.is_none() {
            return write();
        }
        let uid = person.unwrap_or(process);
        if process != 0 && uid != process {
            return Err(refused("only root writes for another account"));
        }
        let gid = owned_directory(identity_dir, uid)?;
        if uid == process {
            return write();
        }
        let _person = FilesystemIdentity::assume(uid, gid)?;
        write()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (person, identity_dir);
        write()
    }
}

/// Check that `identity_dir`, or its nearest existing ancestor, is a
/// directory `uid` owns, reached without following a symlink at that last
/// component, and return its group.
#[cfg(target_os = "linux")]
fn owned_directory(identity_dir: &Path, uid: u32) -> io::Result<u32> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

    let existing = identity_dir
        .ancestors()
        .find(|ancestor| std::fs::symlink_metadata(ancestor).is_ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no ancestor exists"))?;
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(existing)
        .map_err(|error| match error.raw_os_error() {
            Some(libc::ELOOP | libc::ENOTDIR) => {
                refused("the identity directory is not a directory")
            }
            _ => error,
        })?;
    let metadata = directory.metadata()?;
    if metadata.uid() != uid {
        return Err(refused("the identity directory belongs to another account"));
    }
    Ok(metadata.gid())
}

#[cfg(target_os = "linux")]
fn refused(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, why.to_owned())
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
    fn a_symlinked_or_foreign_identity_directory_is_refused() {
        let own = rustix::process::geteuid().as_raw();
        let home = tempfile::tempdir().unwrap();
        let real = home.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = home.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let mut ran = false;
        for planted in [link.clone(), link.join("not-yet/identity")] {
            let refused = as_person(Some(own), &planted, || {
                ran = true;
                std::fs::write(real.join("fleet.json"), b"{}")
            });
            assert_eq!(refused.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        }
        assert!(!ran);
        assert!(!real.join("fleet.json").exists());

        if own != 0 {
            // Its nearest existing ancestor is root's.
            let foreign = as_person(
                Some(own),
                Path::new("/tilo-no-such-dir/identity"),
                || Ok(()),
            );
            assert_eq!(foreign.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
            let other = as_person(Some(own.wrapping_add(1)), &real, || Ok(()));
            assert_eq!(other.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        }

        as_person(Some(own), &real.join("identity"), || {
            std::fs::create_dir_all(real.join("identity/fleets"))?;
            std::fs::write(real.join("identity/fleets/new.json"), b"{}")
        })
        .unwrap();
    }

    #[test]
    fn root_writes_into_a_persons_directory_only_as_the_person() {
        let person = another_person().filter(|_| rustix::process::geteuid().as_raw() == 0);
        let Some(person_uid) = person else {
            // Not root, or no other person exists here: nothing switches.
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
        let identity = person.join("identity");

        // What it creates is the person's from the start.
        as_person(Some(person_uid), &identity, || {
            std::fs::create_dir_all(identity.join("fleets"))?;
            std::fs::write(identity.join("fleets/new.json"), b"{}")
        })
        .unwrap();
        let written = std::fs::metadata(identity.join("fleets/new.json")).unwrap();
        assert_eq!((written.uid(), written.gid()), (person_uid, person_uid));

        // A directory below it swapped for a link to root's leads nowhere.
        std::fs::remove_dir_all(identity.join("fleets")).unwrap();
        std::os::unix::fs::symlink(&protected, identity.join("fleets")).unwrap();
        let refused = as_person(Some(person_uid), &identity, || {
            std::fs::write(identity.join("fleets/new.json"), b"{}")
        });
        assert_eq!(refused.unwrap_err().kind(), io::ErrorKind::PermissionDenied);

        // Neither does the identity directory itself swapped for one, nor a
        // root-owned directory reached through a swapped ancestor: both are
        // refused before anything is written, even though root owns the
        // target.
        std::fs::remove_dir_all(&identity).unwrap();
        std::os::unix::fs::symlink(&protected, &identity).unwrap();
        let swapped_ancestor = home.path().join("swapped");
        std::os::unix::fs::symlink(home.path(), &swapped_ancestor).unwrap();
        for planted in [identity, swapped_ancestor.join("protected")] {
            let refused = as_person(Some(person_uid), &planted, || {
                std::fs::write(planted.join("new.json"), b"{}")
            });
            assert_eq!(refused.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        }
        assert!(!protected.join("new.json").exists());

        // Root acting for itself refuses the person's directory.
        let own = as_person(None, &person, || std::fs::write(person.join("a"), b"a"));
        assert_eq!(own.unwrap_err().kind(), io::ErrorKind::PermissionDenied);

        // The thread is root's again afterwards.
        std::fs::write(protected.join("root.json"), b"{}").unwrap();
        assert_eq!(current_fsuid(), 0);
    }
}
