//! The host helper's dynamic loader closure, privately mounted in the guest.
//! Environment images supply tools, not the libraries of the trusted helper.
//! Resolve through the helper's own ELF interpreter without running its entry
//! point; expose individual files under `/run/ush`, never a host library tree.

use std::collections::BTreeMap;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
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
            .output()?;
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
        use std::os::unix::fs::PermissionsExt;
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
            || metadata.uid() != 0
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
}
