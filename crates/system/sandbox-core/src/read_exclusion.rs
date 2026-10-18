//! Additional child-only read confinement prepared before a multithreaded fork.
//! The ruleset handles only file reads: stacking it never grants authority.
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

const READ_FILE: u64 = 1 << 2;
#[repr(C, packed)]
struct PathRule {
    access: u64,
    parent_fd: i32,
}

/// A fully prepared kernel ruleset. No allocation or path lookup occurs at exec.
#[derive(Debug)]
pub struct PreparedReadExclusion {
    ruleset: OwnedFd,
}

fn error() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "invalid child read exclusion")
}
fn open_child(parent: &File, name: &std::ffi::OsStr) -> io::Result<File> {
    let name = CString::new(name.as_bytes()).map_err(|_| error())?;
    // SAFETY: live directory descriptor and NUL-terminated name; new fd owned below.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: solely own the new successful descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn grant(ruleset: &OwnedFd, object: &File) -> io::Result<()> {
    let rule = PathRule {
        access: READ_FILE,
        parent_fd: object.as_raw_fd(),
    };
    // SAFETY: kernel copies the correctly sized packed path-beneath attribute.
    let result = unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset.as_raw_fd(),
            1,
            &raw const rule,
            0,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
fn namespace_mounts() -> io::Result<Vec<PathBuf>> {
    let mut bytes = Vec::new();
    File::open("/proc/self/mountinfo")?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(error());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| error())?;
    let mut paths = Vec::new();
    for line in text.lines() {
        let encoded = line.split_whitespace().nth(4).ok_or_else(error)?;
        // mountinfo escapes only tab, newline, space and backslash. Decode
        // exactly that grammar; never interpret a mount's source as a grant.
        let path = encoded
            .replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\");
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(error());
        }
        paths.push(path);
        if paths.len() > 4096 {
            return Err(error());
        }
    }
    Ok(paths)
}
fn sibling_names(
    parent: &File,
    location: &Path,
    mounts: &[PathBuf],
) -> io::Result<Vec<std::ffi::OsString>> {
    match fs::read_dir(format!("/proc/self/fd/{}", parent.as_raw_fd())) {
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect(),
        Err(failure) if failure.kind() == io::ErrorKind::PermissionDenied => {
            // A confined app cannot enumerate structural ancestors such as
            // /run. O_PATH still holds them. Its namespace mount table lists
            // every launcher bind beneath those ancestors without granting
            // broader READ_DIR rights to the app.
            let mut names = Vec::new();
            if location == Path::new("/") {
                for name in [
                    "bin", "boot", "dev", "etc", "home", "lib", "lib64", "media", "mnt", "opt",
                    "proc", "root", "run", "sbin", "srv", "sys", "tmp", "usr", "var",
                ] {
                    names.push(std::ffi::OsString::from(name));
                }
            }
            for mount in mounts {
                if let Ok(relative) = mount.strip_prefix(location)
                    && let Some(Component::Normal(name)) = relative.components().next()
                    && !names.iter().any(|present| present == name)
                {
                    names.push(name.to_owned());
                }
            }
            Ok(names)
        }
        Err(failure) => Err(failure),
    }
}

impl PreparedReadExclusion {
    /// Prepare a child-only file-read denial for one existing, non-symlink subtree.
    /// All sibling objects are granted by held descriptors, never by following
    /// aliases. Ancestors themselves are not granted, so no rule covers the
    /// excluded inode even through a proc-fd or symbolic-link alias.
    ///
    /// # Errors
    /// Refuses unsupported kernels, malformed/missing paths, redirecting
    /// ancestors, or any rule that the kernel cannot enforce.
    pub fn prepare(excluded: &Path) -> io::Result<Self> {
        if !excluded.is_absolute() {
            return Err(error());
        }
        let components: Vec<_> = excluded
            .components()
            .skip(1)
            .map(|component| match component {
                Component::Normal(name) => Ok(name),
                _ => Err(error()),
            })
            .collect::<io::Result<_>>()?;
        if components.is_empty() {
            return Err(error());
        }
        // ABI-one READ_FILE is deliberately the only handled right. The
        // inherited application ruleset continues to enforce everything else.
        let handled = READ_FILE;
        // SAFETY: kernel copies one u64 ruleset attribute; no borrowed pointers retained.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                &raw const handled,
                size_of::<u64>(),
                0,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = i32::try_from(fd).map_err(|_| error())?;
        // SAFETY: successful syscall returned a descriptor solely owned here.
        let ruleset = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut parent = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open("/")?;
        let mounts = namespace_mounts()?;
        let mut location = PathBuf::from("/");
        for selected in components {
            if !parent.metadata()?.is_dir() {
                return Err(error());
            }
            for name in sibling_names(&parent, &location, &mounts)? {
                if name == selected {
                    continue;
                }
                let object = match open_child(&parent, &name) {
                    Ok(object) => object,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                };
                let metadata = object.metadata()?;
                if metadata.file_type().is_symlink() {
                    continue;
                }
                // Directories and regular/device files can carry READ_FILE;
                // sockets and pipes are already governed by inherited rights.
                if metadata.is_dir() || metadata.is_file() {
                    grant(&ruleset, &object)?;
                } else {
                    use std::os::unix::fs::FileTypeExt;
                    if metadata.file_type().is_char_device()
                        || metadata.file_type().is_block_device()
                    {
                        grant(&ruleset, &object)?;
                    }
                }
            }
            parent = open_child(&parent, selected)?;
            location.push(selected);
        }
        if !parent.metadata()?.is_dir() {
            return Err(error());
        }
        Ok(Self { ruleset })
    }

    /// Install only the prepared ruleset in the child immediately before exec.
    /// Contains no path lookups, allocations, locks or Rust runtime callbacks.
    ///
    /// # Errors
    /// Fails closed if either required kernel operation is refused.
    pub fn install_before_exec(&self) -> io::Result<()> {
        // SAFETY: integer-only process-local kernel calls with a live ruleset fd.
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the fd refers to the already prepared Landlock ruleset.
        if unsafe {
            libc::syscall(
                libc::SYS_landlock_restrict_self,
                self.ruleset.as_raw_fd(),
                0,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    #[test]
    fn exclusion_prepares_inside_outer_policy_without_ancestor_directory_reads() {
        const ROOT: &str = "TILO_TEST_EXCLUSION_OUTER_ROOT";
        if let Some(root) = std::env::var_os(ROOT) {
            use landlock::{AccessFs, CompatLevel};
            let root = PathBuf::from(root);
            crate::seccomp::install(&crate::seccomp::clone3_compat_program().unwrap()).unwrap();
            crate::seccomp::install(
                &crate::seccomp::baseline_program(crate::seccomp::SeccompProfile::TiloWebEngine)
                    .unwrap(),
            )
            .unwrap();
            let mut rules = crate::landlock::access_plan(
                &[
                    "/usr".into(),
                    "/lib".into(),
                    "/lib64".into(),
                    "/etc".into(),
                    "/proc".into(),
                    "/dev/null".into(),
                    root.clone(),
                ],
                &[],
                crate::landlock::FS_ABI,
            );
            for rule in &mut rules {
                rule.access = AccessFs::ReadFile | AccessFs::ReadDir;
            }
            let specification = crate::landlock::RulesetSpec {
                compat: CompatLevel::HardRequirement,
                handled_fs: AccessFs::ReadFile | AccessFs::ReadDir,
                handled_net: landlock::BitFlags::EMPTY,
                connect_ports: Vec::new(),
                rules,
                trim_file_rights: true,
                scoped: landlock::BitFlags::EMPTY,
            };
            let status = crate::landlock::restrict_self(&specification, |_, error| {
                panic!("outer policy object missing: {error}")
            })
            .unwrap();
            assert_eq!(
                crate::landlock::outcome_from_status(&status.ruleset),
                crate::landlock::LandlockOutcome::FullyEnforced
            );
            assert_eq!(
                fs::read_dir("/").unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
            assert_eq!(
                fs::read(root.join("migration/Bookmarks")).unwrap(),
                b"legacy"
            );
            let policy = PreparedReadExclusion::prepare(&root.join("migration")).unwrap();
            let mut command = Command::new("/bin/sh");
            // SAFETY: prepared child callback contains only process-local syscalls.
            unsafe {
                command.pre_exec(move || policy.install_before_exec());
            }
            let output = command
                .stdin(std::process::Stdio::piped())
                .args([
                    "-c",
                    "cat \"$1/migration/Bookmarks\" 2>/dev/null; cat \"$1/allowed\"",
                    "sh",
                ])
                .arg(&root)
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"ordinary engine data");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("migration")).unwrap();
        fs::write(root.path().join("migration/Bookmarks"), b"legacy").unwrap();
        fs::write(root.path().join("allowed"), b"ordinary engine data").unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "read_exclusion::tests::exclusion_prepares_inside_outer_policy_without_ancestor_directory_reads", "--nocapture"])
            .env(ROOT, root.path()).output().unwrap();
        assert!(
            output.status.success(),
            "layered production policy failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("1 passed")
        );
    }
}
