//! Who in-VM builds run as.
//!
//! The daemon runs as root (it manages users, units and the sysext), but a
//! build is ordinary unprivileged work: it compiles into the synced tree's
//! `target/` and talks to the compiler cache. On a VM the build root
//! (`<root>/src`) is root's own directory and nothing else ever touches it.
//! On a laptop that runs a dev image, the build root is a directory of the
//! laptop's owner bind-mounted into place, and the compiler cache is the
//! one that owner's own builds use. A build run as root there leaves
//! root-owned files in `target/` that the owner's next `cargo` cannot
//! overwrite, and starts a root sccache server that the owner's builds
//! then talk to.
//!
//! So a build runs as whoever owns the build root: root on a VM, the
//! owner on a laptop. Only the steps that install into the daemon's own
//! state (laying a component out into staging, importing a layer,
//! installing an app) keep the daemon's privilege.

use std::ffi::{CStr, OsString};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::DaemonError;

/// The account a build runs as when it is not the daemon's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildUser {
    pub uid: u32,
    pub gid: u32,
    pub name: String,
    pub home: PathBuf,
}

impl BuildUser {
    /// The owner of `build_root` when the daemon is root and the owner is
    /// not; `None` when builds run as the daemon itself.
    ///
    /// # Errors
    /// The build root's owner has no account.
    pub fn for_build_root(build_root: &Path) -> Result<Option<Self>, DaemonError> {
        // SAFETY: geteuid takes no arguments and cannot fail.
        let euid = unsafe { libc::geteuid() };
        let Ok(meta) = std::fs::metadata(build_root) else {
            return Ok(None);
        };
        Self::choose(euid, meta.uid(), meta.gid(), lookup)
    }

    /// The decision behind [`Self::for_build_root`], with the account
    /// lookup passed in.
    fn choose(
        euid: u32,
        owner: u32,
        group: u32,
        lookup: impl FnOnce(u32) -> Option<(String, PathBuf)>,
    ) -> Result<Option<Self>, DaemonError> {
        if euid != 0 || owner == 0 {
            return Ok(None);
        }
        let (name, home) = lookup(owner).ok_or_else(|| {
            DaemonError::InvalidParams(format!(
                "the build root belongs to uid {owner}, which has no account to build as"
            ))
        })?;
        Ok(Some(Self {
            uid: owner,
            gid: group,
            name,
            home,
        }))
    }

    /// Run `command` as this account, with its home and its own cargo
    /// home, so nothing it writes belongs to root.
    pub fn apply(&self, command: &mut Command) {
        command
            .uid(self.uid)
            .gid(self.gid)
            .env("HOME", &self.home)
            .env("USER", &self.name)
            .env("LOGNAME", &self.name)
            // Root's cargo home (registry, git checkouts) is not writable
            // for the build user; cargo falls back to `$HOME/.cargo`.
            .env_remove("CARGO_HOME")
            .env_remove("SUDO_USER")
            .env_remove("SUDO_UID")
            .env_remove("SUDO_GID")
            .env_remove("SUDO_COMMAND");
    }

    /// Give `path` (not following a final symlink) to this account.
    ///
    /// # Errors
    /// I/O failures.
    pub fn own(&self, path: &Path) -> Result<(), DaemonError> {
        std::os::unix::fs::lchown(path, Some(self.uid), Some(self.gid)).map_err(DaemonError::io(
            format!("give {} to {}", path.display(), self.name),
        ))
    }

    /// Give every root-owned entry under `root` to this account. Earlier
    /// daemons built as root, and their files would make this account's
    /// builds fail with "permission denied". Returns how many entries
    /// changed hands.
    ///
    /// # Errors
    /// I/O failures.
    pub fn reclaim(&self, root: &Path) -> Result<u64, DaemonError> {
        let mut changed = 0_u64;
        let mut pending = vec![root.to_owned()];
        while let Some(path) = pending.pop() {
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.uid() == 0 {
                self.own(&path)?;
                changed = changed.saturating_add(1);
            }
            if meta.is_dir() {
                let entries = std::fs::read_dir(&path)
                    .map_err(DaemonError::io(format!("read {}", path.display())))?;
                pending.extend(entries.flatten().map(|entry| entry.path()));
            }
        }
        Ok(changed)
    }
}

/// The name and home directory of `uid`, from the account database.
fn lookup(uid: u32) -> Option<(String, PathBuf)> {
    let mut buffer = vec![0_u8; 16 * 1024];
    // SAFETY: an all-zero passwd is a valid out-parameter for getpwuid_r.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is valid for the call and the buffer's length
    // is passed with it; on success `result` points at `entry`, whose
    // strings live in `buffer`.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            &raw mut entry,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &raw mut result,
        )
    };
    if status != 0 || result.is_null() || entry.pw_name.is_null() || entry.pw_dir.is_null() {
        return None;
    }
    // SAFETY: getpwuid_r succeeded, so both are NUL-terminated strings in `buffer`.
    let (name, home) = unsafe { (CStr::from_ptr(entry.pw_name), CStr::from_ptr(entry.pw_dir)) };
    Some((
        name.to_string_lossy().into_owned(),
        PathBuf::from(OsString::from_vec(home.to_bytes().to_vec())),
    ))
}

/// The local TCP port the compiler cache's server listens on.
pub fn sccache_port() -> u16 {
    std::env::var("SCCACHE_SERVER_PORT")
        .ok()
        .and_then(|port| port.parse().ok())
        .unwrap_or(4226)
}

/// Whether a socket owned by root listens on local `port`, from the
/// kernel's socket tables (`/proc/net/tcp` and `tcp6`).
pub fn root_listens_on(port: u16) -> bool {
    ["/proc/net/tcp", "/proc/net/tcp6"]
        .into_iter()
        .filter_map(|table| std::fs::read_to_string(table).ok())
        .any(|table| table_has_root_listener(&table, port))
}

/// Whether one `/proc/net/tcp*` table lists a root-owned listener on `port`.
fn table_has_root_listener(table: &str, port: u16) -> bool {
    // Columns: sl local_address rem_address st tx:rx tr:when retrnsmt uid ...
    // `st` 0A is LISTEN; the local address ends in `:<port in hex>`.
    table.lines().skip(1).any(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (Some(local), Some(state), Some(uid)) = (fields.get(1), fields.get(3), fields.get(7))
        else {
            return false;
        };
        let listens_on_port = local
            .rsplit_once(':')
            .and_then(|(_, hex)| u16::from_str_radix(hex, 16).ok())
            == Some(port);
        listens_on_port && *state == "0A" && *uid == "0"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(uid: u32) -> Option<(String, PathBuf)> {
        (uid == 1000).then(|| ("bob".to_owned(), PathBuf::from("/var/home/user")))
    }

    /// A VM's build root is root's: builds run as the daemon. A laptop's
    /// build root is its owner's: builds run as the owner. A daemon that is
    /// not root never switches.
    #[test]
    fn builds_run_as_the_build_roots_owner() {
        assert_eq!(BuildUser::choose(0, 0, 0, account).unwrap(), None);
        assert_eq!(
            BuildUser::choose(0, 1000, 1000, account).unwrap(),
            Some(BuildUser {
                uid: 1000,
                gid: 1000,
                name: "bob".into(),
                home: "/var/home/user".into(),
            })
        );
        assert_eq!(BuildUser::choose(1000, 1000, 1000, account).unwrap(), None);
        assert!(BuildUser::choose(0, 4242, 4242, account).is_err());
    }

    #[test]
    fn a_build_users_command_carries_its_home_and_no_root_cargo_home() {
        let user = BuildUser {
            uid: 1000,
            gid: 1000,
            name: "bob".into(),
            home: "/var/home/user".into(),
        };
        let mut command = Command::new("cargo");
        command.env("CARGO_HOME", "/root/.cargo");
        user.apply(&mut command);
        let envs: Vec<_> = command.get_envs().collect();
        assert!(envs.contains(&("HOME".as_ref(), Some("/var/home/user".as_ref()))));
        assert!(envs.contains(&("CARGO_HOME".as_ref(), None)));
    }

    #[test]
    fn only_a_root_listener_on_the_port_counts() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:1082 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1 1\n\
   1: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 2 1\n\
   2: 0100007F:1F91 0100007F:9C40 01 00000000:00000000 00:00000000 00000000     0        0 3 1\n";
        assert!(table_has_root_listener(table, 4226));
        assert!(!table_has_root_listener(table, 8080), "a user's listener");
        assert!(
            !table_has_root_listener(table, 8081),
            "an established connection"
        );
        assert!(!table_has_root_listener(table, 9999));
    }
}
