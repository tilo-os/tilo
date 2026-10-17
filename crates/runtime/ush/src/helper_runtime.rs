//! The host helper's dynamic loader closure, privately mounted in the guest.
//! Environment images supply tools, not the libraries of the trusted helper.
//! Resolve through the helper's own ELF interpreter without running its entry
//! point; expose individual files under `/run/ush`, never a host library tree.

use std::collections::BTreeMap;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const LIB_DIR: &str = "/run/ush/lib";
pub const LOADER: &str = "/run/ush/lib/ld.so";
pub const EXECUTABLE: &str = "/run/ush/bin/tilo-ush-elf";

#[derive(Debug, Clone)]
pub struct HelperRuntime {
    pub loader: PathBuf,
    pub libraries: BTreeMap<String, PathBuf>,
}

impl HelperRuntime {
    /// Resolve only the installed helper's loader dependencies. A missing
    /// dependency fails before a guest starts, with the loader's explanation.
    ///
    /// # Errors
    /// Returns the native loader or dependency-resolution failure.
    pub fn resolve(binary: &Path) -> io::Result<Option<Self>> {
        let Some(loader) = interpreter(binary)? else {
            return Ok(None);
        };
        let output = std::process::Command::new(&loader)
            .arg("--list")
            .arg(binary)
            .env_clear()
            .output()
            .map_err(|error| {
                io::Error::other(format!("the native host loader could not run: {error}"))
            })?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "the shell helper's libraries could not be resolved: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let libraries = parse_libraries(&String::from_utf8_lossy(&output.stdout), &loader)?;
        Ok(Some(Self { loader, libraries }))
    }

    /// Direct entry before Landlock: no environment shell or script runs
    /// ahead of the trusted helper that installs confinement.
    pub fn entry(&self) -> Vec<String> {
        vec![
            LOADER.into(),
            "--library-path".into(),
            LIB_DIR.into(),
            "--argv0".into(),
            "tilo-ush".into(),
            EXECUTABLE.into(),
        ]
    }

    /// Command shims run after confinement. Keep their original argv[0] for
    /// typed broker dispatch; this library path belongs only to this exec.
    ///
    /// # Errors
    /// Returns an error if the private wrapper cannot be written.
    pub fn write_wrapper(&self, path: &Path) -> io::Result<()> {
        std::fs::write(
            path,
            format!(
                "#!/bin/sh\nexec {LOADER} --library-path {LIB_DIR} --argv0 \"$0\" {EXECUTABLE} \"$@\"\n"
            ),
        )?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o555))
    }
}

fn parse_libraries(output: &str, loader: &Path) -> io::Result<BTreeMap<String, PathBuf>> {
    let mut libraries = BTreeMap::new();
    for line in output.lines() {
        let line = line.trim();
        let Some((name, value)) = line.split_once(" => ") else {
            continue;
        };
        let path = value.split_whitespace().next().unwrap_or_default();
        if !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            || name.is_empty()
            || !path.starts_with('/')
            || !Path::new(path).is_file()
        {
            return Err(io::Error::other(format!("invalid helper library: {line}")));
        }
        if libraries.insert(name.into(), PathBuf::from(path)).is_some() {
            return Err(io::Error::other(format!(
                "duplicate helper library: {name}"
            )));
        }
    }
    let name = loader
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| io::Error::other("invalid helper ELF interpreter"))?;
    libraries.insert(name.into(), loader.into());
    Ok(libraries)
}

/// Read the native ELF64 `PT_INTERP`, with bounded offsets and lengths. Static
/// helpers need no closure; other executable formats fail closed.
fn interpreter(binary: &Path) -> io::Result<Option<PathBuf>> {
    let mut file = std::fs::File::open(binary)?;
    let mut header = [0_u8; 64];
    file.read_exact(&mut header)?;
    if &header[..6] != b"\x7fELF\x02\x01" {
        return Err(io::Error::other(
            "the shell helper is not a native ELF64 executable",
        ));
    }
    let offset = u64::from_le_bytes(header[32..40].try_into().map_err(io::Error::other)?);
    let size = u16::from_le_bytes(header[54..56].try_into().map_err(io::Error::other)?);
    let count = u16::from_le_bytes(header[56..58].try_into().map_err(io::Error::other)?);
    if size != 56 || count > 512 {
        return Err(io::Error::other("invalid helper ELF program headers"));
    }
    file.seek(SeekFrom::Start(offset))?;
    for _ in 0..count {
        let mut program = [0_u8; 56];
        file.read_exact(&mut program)?;
        if u32::from_le_bytes(program[..4].try_into().map_err(io::Error::other)?) != 3 {
            continue;
        }
        let start = u64::from_le_bytes(program[8..16].try_into().map_err(io::Error::other)?);
        let length = u64::from_le_bytes(program[32..40].try_into().map_err(io::Error::other)?);
        if !(2..=4096).contains(&length) {
            return Err(io::Error::other("invalid helper ELF interpreter length"));
        }
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = vec![0; usize::try_from(length).map_err(io::Error::other)?];
        file.read_exact(&mut bytes)?;
        if bytes.pop() != Some(0) || bytes.contains(&0) {
            return Err(io::Error::other("invalid helper ELF interpreter path"));
        }
        let path = PathBuf::from(String::from_utf8(bytes).map_err(io::Error::other)?);
        let name = path.file_name().and_then(|name| name.to_str());
        let native_loader = if cfg!(target_arch = "aarch64") {
            "ld-linux-aarch64.so.1"
        } else if cfg!(target_arch = "x86_64") {
            "ld-linux-x86-64.so.2"
        } else {
            return Err(io::Error::other("unsupported native helper architecture"));
        };
        let canonical = path.canonicalize()?;
        let system_library = ["/usr/lib", "/usr/lib64", "/lib", "/lib64"]
            .iter()
            .any(|root| canonical.starts_with(root));
        // Never execute an arbitrary interpreter supplied by a development
        // helper. The installed host glibc loader is the only resolution tool.
        let metadata = canonical.metadata()?;
        if name != Some(native_loader)
            || !system_library
            || !metadata.is_file()
            || !trusted_loader_owner(&canonical, &metadata)?
            || metadata.mode() & 0o022 != 0
        {
            return Err(io::Error::other(
                "the shell helper does not use the native host loader",
            ));
        }
        return Ok(Some(path));
    }
    Ok(None)
}

/// systemd user-service sandboxes map only the signed-in uid, so even the
/// installed root-owned loader appears as the kernel's overflow uid. This
/// exception is restricted to the immutable installed filesystem: a mutable
/// file with an unmapped owner never becomes a trusted resolution program.
fn trusted_loader_owner(path: &Path, metadata: &std::fs::Metadata) -> io::Result<bool> {
    if metadata.uid() == 0 {
        return Ok(true);
    }
    let overflow = std::fs::read_to_string("/proc/sys/kernel/overflowuid")?
        .trim()
        .parse::<u32>()
        .map_err(io::Error::other)?;
    if metadata.uid() != overflow {
        return Ok(false);
    }
    let mapping = std::fs::read_to_string("/proc/self/uid_map")?;
    for row in mapping.lines() {
        let ids = row
            .split_whitespace()
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(io::Error::other)?;
        let [inside, _, count] = ids.as_slice() else {
            return Err(io::Error::other("invalid kernel uid mapping"));
        };
        if *inside == 0 && *count != 0 {
            return Ok(false);
        }
    }
    let file = std::fs::File::open(path)?;
    let mut filesystem = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: the descriptor is live and statvfs writes exactly this structure.
    if unsafe { libc::fstatvfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatvfs initialized every field of the structure.
    let filesystem = unsafe { filesystem.assume_init() };
    if filesystem.f_flag & libc::ST_RDONLY == 0 {
        return Ok(false);
    }
    let installed = std::fs::File::open("/usr")?;
    let mount = mount_id(&file)?;
    if mount != mount_id(&installed)? {
        return Ok(false);
    }
    // A readonly bind is insufficient: its underlying filesystem can still
    // be changed by the file owner through another writable mount. Require
    // the installed /usr filesystem itself to be readonly as well.
    let mounts = std::fs::read_to_string("/proc/self/mountinfo")?;
    Ok(mounts.lines().any(|row| {
        let Some((mount_fields, filesystem_fields)) = row.split_once(" - ") else {
            return false;
        };
        mount_fields.split_whitespace().next() == Some(mount.to_string().as_str())
            && filesystem_fields
                .split_whitespace()
                .nth(2)
                .is_some_and(|options| options.split(',').any(|option| option == "ro"))
    }))
}

fn mount_id(file: &std::fs::File) -> io::Result<u64> {
    let mut metadata = std::mem::MaybeUninit::<libc::statx>::uninit();
    // SAFETY: AT_EMPTY_PATH selects the live descriptor; statx initializes
    // precisely this structure, and the empty path is nul-terminated.
    if unsafe {
        libc::statx(
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            libc::STATX_MNT_ID,
            metadata.as_mut_ptr(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful statx initialized the structure.
    let metadata = unsafe { metadata.assume_init() };
    if metadata.stx_mask & libc::STATX_MNT_ID == 0 {
        return Err(io::Error::other("kernel did not identify the loader mount"));
    }
    Ok(metadata.stx_mnt_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_nonabsolute_dependencies_fail_before_guest_launch() {
        let loader = std::env::current_exe().unwrap();
        for output in [
            "libmissing.so => not found",
            "libbad.so => relative (0x1)",
            "../../outside => /proc/self/exe (0x1)",
        ] {
            assert!(parse_libraries(output, &loader).is_err());
        }
    }

    #[test]
    fn installed_loader_in_systemd_user_namespace_requires_immutable_filesystem() {
        const CHILD: &str = "TILO_USH_LOADER_NAMESPACE_TEST";
        if let Ok(case) = std::env::var(CHILD) {
            probe_namespace_loader(&case);
            return;
        }
        let executable = std::env::current_exe().unwrap();
        let loader = interpreter(Path::new("/usr/bin/bash")).unwrap().unwrap();
        let canonical = loader.canonicalize().unwrap();
        // The uid-only child must traverse every ancestor of the fixture,
        // and $TMPDIR may sit under a root-only directory; /tmp never does.
        let fixture = tempfile::tempdir_in("/tmp").unwrap();
        std::fs::set_permissions(fixture.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        materialize_test_loader(fixture.path());
        let _mount = mount_immutable_fixture(fixture.path());
        let attacker = fixture.path().join("attacker-loader");
        std::fs::copy(&canonical, &attacker).unwrap();
        // Native test containers run as root; the child deliberately matches
        // systemd's unprivileged uid-only map instead of mapping host root.
        let caller = rustix::process::getuid().as_raw();
        let uid = if caller == 0 { 1000 } else { caller };
        if caller == 0 {
            let path = std::ffi::CString::new(attacker.as_os_str().as_encoded_bytes()).unwrap();
            // SAFETY: path is nul-terminated and chown borrows it for the call.
            assert_eq!(unsafe { libc::chown(path.as_ptr(), uid, uid) }, 0);
        }
        let unmapped = fixture.path().join("unmapped-loader");
        std::fs::copy(&canonical, &unmapped).unwrap();
        assert_eq!(
            caller, 0,
            "native container must provide disposable fixture ownership"
        );
        let path = std::ffi::CString::new(unmapped.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: path is nul-terminated and chown borrows it for the call.
        assert_eq!(unsafe { libc::chown(path.as_ptr(), 2000, 2000) }, 0);
        for case in [
            "immutable",
            "mutable",
            "readonly-bind",
            "attacker",
            "attacker-unmapped",
        ] {
            run_namespace_case(case, fixture.path(), &canonical, &executable, uid);
        }
    }

    fn run_namespace_case(
        case: &str,
        fixture: &Path,
        canonical: &Path,
        executable: &Path,
        uid: u32,
    ) {
        use std::os::unix::process::CommandExt;
        let test = "helper_runtime::tests::installed_loader_in_systemd_user_namespace_requires_immutable_filesystem";
        let mut command = std::process::Command::new("bwrap");
        command.args([
            "--unshare-user",
            "--unshare-pid",
            "--uid",
            &uid.to_string(),
            "--gid",
            &uid.to_string(),
            "--ro-bind",
            "/",
            "/",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
        ]);
        if case == "immutable" {
            command
                .arg("--ro-bind")
                .arg(fixture.join("installed"))
                .arg("/usr");
        } else if case == "mutable" {
            command.args(["--bind", "/usr", "/usr"]);
        } else if case.starts_with("attacker") {
            let source = fixture.join(if case == "attacker" {
                "attacker-loader"
            } else {
                "unmapped-loader"
            });
            command.arg("--ro-bind").arg(source).arg(canonical);
        }
        command
            .arg("--")
            .arg(executable)
            .args([test, "--exact", "--nocapture"]);
        command.env("TILO_USH_LOADER_NAMESPACE_TEST", case);
        {
            // SAFETY: only async-signal-safe credential syscalls execute
            // between fork and exec; the parent's credentials stay intact.
            unsafe {
                command.pre_exec(move || {
                    if libc::setgid(uid) != 0 || libc::setuid(uid) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{case}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn probe_namespace_loader(case: &str) {
        let binary = Path::new("/usr/bin/bash");
        if !case.starts_with("attacker") {
            let loader = if cfg!(target_arch = "aarch64") {
                "/usr/lib/ld-linux-aarch64.so.1"
            } else {
                "/usr/lib64/ld-linux-x86-64.so.2"
            };
            let overflow = std::fs::read_to_string("/proc/sys/kernel/overflowuid")
                .unwrap()
                .trim()
                .parse::<u32>()
                .unwrap();
            assert_eq!(std::fs::metadata(loader).unwrap().uid(), overflow);
        }
        let result = HelperRuntime::resolve(binary);
        if case == "immutable" {
            assert!(result.unwrap().is_some(), "real loader closure is resolved");
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("native host loader"),
                "{case} ownership must not become trusted"
            );
        }
    }

    struct ImmutableMount(PathBuf);

    impl Drop for ImmutableMount {
        fn drop(&mut self) {
            let target = std::ffi::CString::new(self.0.as_os_str().as_encoded_bytes()).unwrap();
            // SAFETY: target is our disposable mount's nul-terminated path.
            assert_eq!(
                unsafe { libc::umount2(target.as_ptr(), libc::MNT_DETACH) },
                0,
                "{}",
                io::Error::last_os_error()
            );
        }
    }

    fn mount_immutable_fixture(root: &Path) -> ImmutableMount {
        // Make a genuine readonly filesystem before dropping to the uid-only
        // namespace. A readonly bind of the mutable builder filesystem is
        // deliberately insufficient for production trust.
        let installed = root.join("installed");
        std::fs::create_dir(&installed).unwrap();
        let target = std::ffi::CString::new(installed.as_os_str().as_encoded_bytes()).unwrap();
        let options = std::ffi::CString::new(format!(
            "lowerdir={}/empty:{}/usr",
            root.display(),
            root.display()
        ))
        .unwrap();
        // SAFETY: mount arguments are valid nul-terminated strings.
        assert_eq!(
            unsafe {
                libc::mount(
                    c"installed-test".as_ptr(),
                    target.as_ptr(),
                    c"overlay".as_ptr(),
                    libc::MS_RDONLY | libc::MS_NODEV,
                    options.as_ptr().cast(),
                )
            },
            0,
            "{}",
            io::Error::last_os_error()
        );
        ImmutableMount(installed)
    }

    fn materialize_test_loader(root: &Path) {
        let executable = std::env::current_exe().unwrap();
        let mut files = Vec::new();
        for binary in [Path::new("/usr/bin/bash"), executable.as_path()] {
            let closure = HelperRuntime::resolve(binary).unwrap().unwrap();
            files.push(closure.loader);
            files.extend(closure.libraries.into_values());
        }
        files.push(PathBuf::from("/usr/bin/bash"));
        for source in files {
            let relative = source.strip_prefix("/").unwrap();
            let relative = if relative.starts_with("lib") || relative.starts_with("lib64") {
                Path::new("usr").join(relative)
            } else {
                relative.to_path_buf()
            };
            let destination = root.join(relative);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::copy(source, destination).unwrap();
        }
        std::fs::create_dir(root.join("empty")).unwrap();
    }
}
