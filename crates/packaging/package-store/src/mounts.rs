//! Which installed trees a running app has mounted.
//!
//! The launcher hands an app its payload tree and its runtime bundle as
//! read-only bind mounts (`--ro-bind <tree> <mount>`), so the kernel's mount
//! table is the one record of what a running app uses that every launch path
//! writes, whoever launched it. Each process's `/proc/<pid>/mountinfo` lists
//! the mounts of its namespace with the *root* of each mount: the path of
//! the bound directory inside its filesystem, which keeps naming the tree
//! after the tree is renamed and says `//deleted` once it is gone.
//!
//! A bound tree is recognised by its last two path components
//! (`tilo-runtime/<version>`, `pkgs/<package id>`) anywhere in a mount's
//! root, so a bind of a subdirectory (`<bundle>/lib/fonts`) or a file inside
//! it counts as well. The mount root is relative to its filesystem, which on
//! a split or subvolume layout differs from the path this process sees by a
//! prefix; matching the tail stays right across both. A false match only
//! keeps a tree longer.
//!
//! Only Linux bind-mounts app trees. Elsewhere nothing is mounted, and
//! [`InUse::now`] says so; on Linux a mount table it cannot read makes every
//! tree count as in use, so a collector keeps everything rather than guess.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};

/// The mounts of every mount namespace this process can see.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mounts {
    /// Each mount's root, as path components.
    roots: BTreeSet<Vec<OsString>>,
    /// Each mount's mount point, as the mounting process sees it.
    points: BTreeSet<PathBuf>,
}

impl Mounts {
    /// Read `<proc>/<pid>/mountinfo` for every process under `proc_root`.
    /// A process that exits while it is read, or whose table this user may
    /// not read, is skipped: its mounts are another user's.
    ///
    /// # Errors
    ///
    /// Returns the error from listing `proc_root` itself.
    pub fn scan_proc(proc_root: &Path) -> io::Result<Self> {
        let mut mounts = Self::default();
        let mut seen_namespaces = BTreeSet::new();
        for entry in std::fs::read_dir(proc_root)? {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            if !name
                .to_str()
                .is_some_and(|pid| pid.bytes().all(|b| b.is_ascii_digit()))
            {
                continue;
            }
            let dir = entry.path();
            // Processes of one namespace share one table: read it once.
            if let Ok(namespace) = std::fs::read_link(dir.join("ns/mnt"))
                && !seen_namespaces.insert(namespace)
            {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(dir.join("mountinfo")) {
                mounts.add_mountinfo(&text);
            }
        }
        Ok(mounts)
    }

    /// Parse one `mountinfo` table (proc(5)) into `self`.
    pub fn add_mountinfo(&mut self, text: &str) {
        for line in text.lines() {
            let mut fields = line.split(' ');
            let (Some(root), Some(point)) = (fields.nth(3), fields.next()) else {
                continue;
            };
            let root = PathBuf::from(unescape(root));
            self.roots.insert(
                root.components()
                    .filter_map(|c| match c {
                        Component::Normal(name) => Some(name.to_os_string()),
                        _ => None,
                    })
                    .collect(),
            );
            self.points.insert(PathBuf::from(unescape(point)));
        }
    }

    /// Whether some mount's root is `tree`, or a path inside it, judged by
    /// `tree`'s last two components.
    #[must_use]
    pub fn holds(&self, tree: &Path) -> bool {
        let Some([parent, name]) = tail_of(tree) else {
            return false;
        };
        self.roots.iter().any(|root| {
            root.windows(2)
                .any(|window| matches!(window, [a, b] if *a == parent && *b == name))
        })
    }

    /// Whether some process has a mount at `point`, as it sees it.
    #[must_use]
    pub fn has_mount_point(&self, point: &Path) -> bool {
        self.points.contains(point)
    }
}

/// What the running apps hold, or that it cannot be known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InUse {
    /// The mount table was read.
    Known(Mounts),
    /// The mount table could not be read: everything counts as in use.
    Unknown(String),
}

impl InUse {
    /// The mounts of every process now. Off Linux no app tree is ever
    /// mounted, so the answer is an empty table.
    #[must_use]
    pub fn now() -> Self {
        if cfg!(target_os = "linux") {
            match Mounts::scan_proc(Path::new("/proc")) {
                Ok(mounts) => Self::Known(mounts),
                Err(error) => Self::Unknown(format!("reading /proc: {error}")),
            }
        } else {
            Self::Known(Mounts::default())
        }
    }

    /// Whether `tree` may be in use by a running app.
    #[must_use]
    pub fn holds(&self, tree: &Path) -> bool {
        match self {
            Self::Known(mounts) => mounts.holds(tree),
            Self::Unknown(_) => true,
        }
    }

    /// Whether some process may have a mount at `point`.
    #[must_use]
    pub fn has_mount_point(&self, point: &Path) -> bool {
        match self {
            Self::Known(mounts) => mounts.has_mount_point(point),
            Self::Unknown(_) => true,
        }
    }
}

/// `tree`'s parent directory name and its own name.
fn tail_of(tree: &Path) -> Option<[OsString; 2]> {
    let name = tree.file_name()?;
    let parent = tree.parent()?.file_name()?;
    Some([parent.to_os_string(), name.to_os_string()])
}

/// Undo mountinfo's octal escapes (`\040` for a space, `\134` for a
/// backslash, …).
fn unescape(field: &str) -> String {
    let mut out = Vec::with_capacity(field.len());
    let mut rest = field.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'\\'
            && let Some((code, after)) = tail.split_first_chunk::<3>()
            && let Some(value) = octal_byte(*code)
        {
            out.push(value);
            rest = after;
            continue;
        }
        out.push(byte);
        rest = tail;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Three octal digits as one byte, when they are octal digits that fit one.
fn octal_byte(code: [u8; 3]) -> Option<u8> {
    code.iter().try_fold(0u8, |acc, digit| {
        let value = digit.checked_sub(b'0').filter(|value| *value < 8)?;
        acc.checked_mul(8)?.checked_add(value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SANDBOX: &str = "\
612 590 0:31 /var/home/ada/.local/share/tilo-runtime/2026.07+native.ab /run/tilo-runtime/2026.07+native.ab ro,nosuid master:1 - btrfs /dev/vda3 rw
613 590 0:31 /var/home/ada/.local/share/tilo-runtime/2026.07+native.ab/lib/fonts /usr/share/fonts/tilo ro - btrfs /dev/vda3 rw
614 590 0:31 /var/home/ada/.local/share/tilo-apps/pkgs/7f7f /usr/share/tilo-apps/so.tilo.notes ro - btrfs /dev/vda3 rw
615 590 0:31 /var/home/ada/My\\040Files /home/ada/My\\040Files rw - btrfs /dev/vda3 rw
";

    #[test]
    fn a_sandbox_mount_table_names_the_trees_it_binds() {
        let mut mounts = Mounts::default();
        mounts.add_mountinfo(SANDBOX);
        // The home seen by this process may differ by a prefix.
        assert!(mounts.holds(Path::new(
            "/home/ada/.local/share/tilo-runtime/2026.07+native.ab"
        )));
        assert!(mounts.holds(Path::new("/srv/tilo-apps/pkgs/7f7f")));
        assert!(!mounts.holds(Path::new(
            "/home/ada/.local/share/tilo-runtime/2026.07+native.cd"
        )));
        assert!(!mounts.holds(Path::new("/home/ada/.local/share/tilo-apps/pkgs/8e8e")));
        assert!(mounts.has_mount_point(Path::new("/run/tilo-runtime/2026.07+native.ab")));
        assert!(
            mounts.holds(Path::new("/var/home/ada/My Files")),
            "escapes are undone"
        );
    }

    #[test]
    fn a_renamed_tree_is_found_under_its_new_name() {
        let mut mounts = Mounts::default();
        mounts.add_mountinfo(
            "700 1 0:31 /home/ada/.local/share/tilo-runtime/.trash/v1.42.7 /run/tilo-runtime/v1 ro - ext4 /dev/sda1 rw\n",
        );
        assert!(mounts.holds(Path::new(
            "/home/ada/.local/share/tilo-runtime/.trash/v1.42.7"
        )));
        assert!(!mounts.holds(Path::new("/home/ada/.local/share/tilo-runtime/v1")));
        assert!(mounts.has_mount_point(Path::new("/run/tilo-runtime/v1")));
    }

    #[test]
    fn an_unreadable_table_holds_everything() {
        let unknown = InUse::Unknown("no /proc".into());
        assert!(unknown.holds(Path::new("/anything/at/all")));
        assert!(unknown.has_mount_point(Path::new("/run/tilo-runtime/x")));
        assert!(!InUse::Known(Mounts::default()).holds(Path::new("/a/b")));
    }

    #[test]
    fn scanning_a_proc_tree_reads_every_process() {
        let proc_root = tempfile::tempdir().unwrap();
        for (pid, table) in [("1", SANDBOX), ("22", ""), ("self", SANDBOX)] {
            std::fs::create_dir_all(proc_root.path().join(pid)).unwrap();
            std::fs::write(proc_root.path().join(pid).join("mountinfo"), table).unwrap();
        }
        let mounts = Mounts::scan_proc(proc_root.path()).unwrap();
        assert!(mounts.holds(Path::new("/x/pkgs/7f7f")));
        assert!(Mounts::scan_proc(&proc_root.path().join("missing")).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn this_process_sees_its_own_root_mount() {
        // The production path: a real /proc lists at least the root mount.
        match InUse::now() {
            InUse::Known(mounts) => assert!(mounts.has_mount_point(Path::new("/"))),
            InUse::Unknown(why) => panic!("the mount table must be readable here: {why}"),
        }
    }
}
