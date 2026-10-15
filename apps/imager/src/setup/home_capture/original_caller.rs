//! Required requested original-owner facts. This representation validates
//! shape and binding only; it does not certify source-peer group authority.
//! It must not be synthesized from target NSS or source inode group metadata.
use serde::{Deserialize, Serialize};
use std::io;
const MAX_GROUPS: usize = 65_536;
fn refused(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
/// Included inside the password-authenticated request, never inferred from
/// destination NSS names. A separate trusted source broker must independently
/// certify actual peer credentials before privileged projection is possible.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OriginalCallerFacts {
    schema: u32,
    uid: u32,
    primary_gid: u32,
    supplementary_gids: Vec<u32>,
}
impl OriginalCallerFacts {
    /// Capture only an already admitted original application peer. A request PID,
    /// service worker's NSS credentials, or source inode group is not authority.
    pub(super) fn capture(
        peer: &runtime_portal::process_binding::ExecutingPeer,
    ) -> io::Result<Self> {
        if !peer.current() {
            return Err(refused("original peer exited"));
        }
        let before = super::process::Incarnation::capture(peer.pid(), peer.uid())?;
        let first = read_status(peer)?;
        let second = read_status(peer)?;
        let after = super::process::Incarnation::capture(peer.pid(), peer.uid())?;
        if before != after || !peer.current() || first != second {
            return Err(refused("original peer credentials or incarnation changed"));
        }
        first.validate(peer.uid())?;
        Ok(first)
    }

    pub(crate) fn primary_gid(&self) -> u32 {
        self.primary_gid
    }
    pub(crate) fn supplementary_gids(&self) -> &[u32] {
        &self.supplementary_gids
    }

    pub(crate) fn uid(&self) -> u32 {
        self.uid
    }

    pub(crate) fn validate(&self, original_home_uid: u32) -> io::Result<()> {
        if self.schema != 1
            || self.uid == 0
            || self.uid == u32::MAX
            || self.uid != original_home_uid
            || self.primary_gid == u32::MAX
            || self.supplementary_gids.len() > MAX_GROUPS
            || self.supplementary_gids.contains(&u32::MAX)
            || self
                .supplementary_gids
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
        {
            return Err(refused(
                "original caller facts are not canonical or bound to original Home",
            ));
        }
        Ok(())
    }
}

fn read_status(
    peer: &runtime_portal::process_binding::ExecutingPeer,
) -> io::Result<OriginalCallerFacts> {
    use std::io::Read;
    if !peer.current() {
        return Err(refused("original peer exited before credential read"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(format!("/proc/{}/status", peer.pid()))?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(refused("original credential status exceeds bound"));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| refused("original credential status is malformed"))?;
    let mut uid = None;
    let mut gid = None;
    let mut groups = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("Uid:") {
            if uid.replace(parse_ids(value, 4)?).is_some() {
                return Err(refused("duplicate original UID"));
            }
        } else if let Some(value) = line.strip_prefix("Gid:") {
            if gid.replace(parse_ids(value, 4)?).is_some() {
                return Err(refused("duplicate original GID"));
            }
        } else if let Some(value) = line.strip_prefix("Groups:") {
            if groups.replace(parse_ids(value, MAX_GROUPS)?).is_some() {
                return Err(refused("duplicate original groups"));
            }
        }
    }
    let uid = uid.ok_or_else(|| refused("original UID missing"))?;
    let gid = gid.ok_or_else(|| refused("original GID missing"))?;
    let mut groups = groups.ok_or_else(|| refused("original groups missing"))?;
    groups.sort_unstable();
    // Kernel setgroups permits duplicates. Multiplicity grants no authority;
    // canonicalize identically to the proof socket's kernel group snapshot.
    groups.dedup();
    // Privileged/changing real/effective/saved/filesystem IDs are not guessed.
    if uid.len() != 4
        || gid.len() != 4
        || uid.iter().any(|id| *id != peer.uid())
        || gid.iter().any(|id| *id != peer.gid())
        || groups != peer.supplementary_groups()?
        || !peer.current()
    {
        return Err(refused("original kernel credentials changed or differ"));
    }
    Ok(OriginalCallerFacts {
        schema: 1,
        uid: peer.uid(),
        primary_gid: peer.gid(),
        supplementary_gids: groups,
    })
}
fn parse_ids(value: &str, maximum: usize) -> io::Result<Vec<u32>> {
    let mut ids = Vec::new();
    for field in value.split_ascii_whitespace() {
        if ids.len() >= maximum {
            return Err(refused("original credentials exceed bound"));
        }
        let id = field
            .parse::<u32>()
            .map_err(|_| refused("original credential ID malformed"))?;
        if id == u32::MAX {
            return Err(refused("original credential ID invalid"));
        }
        ids.push(id);
    }
    Ok(ids)
}

#[cfg(test)]
pub(in crate::setup) fn fixture_source_owner(home: &std::path::Path) -> OriginalCallerFacts {
    use std::os::unix::ffi::OsStrExt;
    // Controlled fixture only: make the Home's actual owner nonroot on root
    // test runners. These facts do not claim authenticated broker provenance.
    // SAFETY: these calls only read the test process's current credentials.
    let actual_uid = rustix::process::geteuid().as_raw();
    let uid = actual_uid.max(1);
    // SAFETY: reads the test process's current effective GID.
    let gid = rustix::process::getegid().as_raw();
    if actual_uid == 0 {
        let path = std::ffi::CString::new(home.as_os_str().as_bytes()).unwrap();
        // SAFETY: NUL-terminated fixture-owned path; no user source is touched.
        assert_eq!(unsafe { libc::chown(path.as_ptr(), uid, gid) }, 0);
    }
    OriginalCallerFacts {
        schema: 1,
        uid,
        primary_gid: gid,
        supplementary_gids: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{net::UnixStream, process::CommandExt},
        },
    };
    const CHILD: &str = "TILO_ORIGINAL_CALLER_FIXTURE_FD";
    const CHILD_TEST: &str =
        "setup::home_capture::original_caller::tests::original_caller_fixture_child";

    #[test]
    fn original_caller_fixture_child() {
        let Ok(value) = std::env::var(CHILD) else {
            return;
        };
        let fd = value.parse::<i32>().unwrap();
        // SAFETY: parent transfers this exclusive fixture socket descriptor.
        let mut socket = unsafe { UnixStream::from_raw_fd(fd) };
        runtime_portal::process_binding::complete(&mut socket).unwrap();
        let mut byte = [0];
        socket.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [7]);
    }

    struct Owned(std::process::Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn original_credentials_come_from_actual_kernel_admitted_peer_and_expire_on_exit() {
        let (host, app) = UnixStream::pair().unwrap();
        let executable = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
        // SAFETY: reads this fixture process's current effective credentials.
        let (uid, gid) = (
            rustix::process::geteuid().as_raw(),
            rustix::process::getegid().as_raw(),
        );
        let groups = if uid == 0 {
            vec![1002u32, 1002, 1003]
        } else {
            current_groups()
        };
        let child_uid = if uid == 0 { 1000 } else { uid };
        let child_gid = if uid == 0 { 1001 } else { gid };
        let descriptors = [app.as_raw_fd(), executable.as_raw_fd()];
        let child_groups = groups.clone();
        // The already held executable avoids relying on a foreign UID traversing
        // the parent's private build directory. No source-home file is touched.
        let mut command =
            std::process::Command::new(format!("/proc/self/fd/{}", executable.as_raw_fd()));
        command
            .args(["--exact", CHILD_TEST, "--nocapture"])
            .env(CHILD, app.as_raw_fd().to_string());
        // SAFETY: child-only pre_exec uses fixed inherited descriptors and
        // async-signal-safe credential syscalls; parent credentials stay intact.
        unsafe {
            command.pre_exec(move || {
                for fd in descriptors {
                    if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                if uid == 0
                    && (libc::setgroups(child_groups.len(), child_groups.as_ptr()) < 0
                        || libc::setgid(child_gid) < 0
                        || libc::setuid(child_uid) < 0)
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = Owned(command.spawn().unwrap());
        let mut channel =
            super::super::transport::HostChannel::authenticate(host, child.0.id(), child_uid)
                .unwrap();
        let facts = channel.original_caller_facts().unwrap();
        assert_eq!(facts.uid, child_uid);
        assert_eq!(facts.primary_gid, child_gid);
        let mut canonical_groups = groups;
        canonical_groups.sort_unstable();
        canonical_groups.dedup();
        assert_eq!(facts.supplementary_gids, canonical_groups);
        facts.validate(child_uid).unwrap();
        assert!(facts.validate(child_uid.checked_add(1).unwrap()).is_err());
        channel.stream().write_all(&[7]).unwrap();
        assert!(child.0.wait().unwrap().success());
        assert!(
            channel.original_caller_facts().is_err(),
            "exited original peer cannot authorize another capture"
        );
    }

    #[test]
    fn optional_source_facts_never_reject_existing_online_root_channel() {
        let (host, mut app) = UnixStream::pair().unwrap();
        let client = std::thread::spawn(move || {
            runtime_portal::process_binding::complete(&mut app).unwrap();
            let mut command = [0];
            app.read_exact(&mut command).unwrap();
            app.write_all(&command).unwrap();
        });
        // SAFETY: reads this fixture's actual effective UID, not request facts.
        let uid = rustix::process::geteuid().as_raw();
        let mut channel =
            super::super::transport::HostChannel::authenticate(host, std::process::id(), uid)
                .unwrap();
        let facts = channel.original_caller_facts();
        if uid == 0 {
            assert!(
                facts.is_err(),
                "root is refused only as original-source certification"
            );
        } else {
            facts.unwrap().validate(uid).unwrap();
        }
        channel.stream().write_all(&[9]).unwrap();
        let mut reply = [0];
        channel.stream().read_exact(&mut reply).unwrap();
        assert_eq!(
            reply,
            [9],
            "online admitted traffic survives absent source certification"
        );
        client.join().unwrap();
    }

    fn current_groups() -> Vec<u32> {
        // SAFETY: queries only this fixture's current supplementary credentials.
        let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
        assert!(count >= 0);
        let mut groups = vec![0; usize::try_from(count).unwrap()];
        // SAFETY: live buffer sized from the kernel count; fail on concurrent drift.
        assert_eq!(
            unsafe { libc::getgroups(count, groups.as_mut_ptr()) },
            count
        );
        groups.sort_unstable();
        groups.dedup();
        groups
    }
}
