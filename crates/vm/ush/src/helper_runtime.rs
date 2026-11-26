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

    /// Command shims run after confinement. Keep their original `argv[0]` for
    /// typed broker dispatch; this library path belongs only to this exec.
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

/// Whether a guest can start here: the runtime binary's loader must be one the
/// helper trusts. A nested test runner that maps the host's root-owned files to
/// the caller is root-equivalent over them, so the helper refuses every host
/// loader there and guest tests skip through the `TrustedLoader` capability.
///
/// # Errors
///
/// Returns the helper's own refusal, or whatever resolving the runtime
/// binary's closure reports.
pub fn native_loader_trusted() -> io::Result<()> {
    HelperRuntime::resolve(&crate::supervisor::runtime_binary()?).map(drop)
}

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
    if unsafe { libc::fstatvfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
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
    let metadata = unsafe { metadata.assume_init() };
    if metadata.stx_mask & libc::STATX_MNT_ID == 0 {
        return Err(io::Error::other("kernel did not identify the loader mount"));
    }
    Ok(metadata.stx_mnt_id)
}
