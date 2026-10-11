//! Runtime publisher pins installed by authenticated OS/component updates.
//! These paths are never supplied by an app, environment or user data store.

use std::io::Read as _;
use std::path::Path;

const PINS: [&str; 3] = [
    "/usr/share/tilo/runtime-bundle-publisher.hex",
    "/usr/share/tilo/browser-runtime-publisher.hex",
    "/usr/share/tilo/registry-runtime-publisher.hex",
];

/// Combine the operator's existing trust with immutable installed pins.
///
/// # Errors
/// A present component pin is unreadable, mutable, malformed or insecure.
pub fn load_with_components(operator_list: &Path) -> std::io::Result<Option<Vec<String>>> {
    let mut keys = match std::fs::read_to_string(operator_list) {
        Ok(text) => Some(
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(str::to_ascii_lowercase)
                .collect::<Vec<_>>(),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    for path in PINS {
        if let Some(key) = read_pin(Path::new(path))? {
            keys.get_or_insert_with(Vec::new).push(key);
        }
    }
    if let Some(keys) = &mut keys {
        keys.sort();
        keys.dedup();
    }
    Ok(keys)
}

fn denied() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "installed runtime publisher pin is not immutable and root-owned",
    )
}

#[cfg(unix)]
fn read_pin(path: &Path) -> std::io::Result<Option<String>> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(denied());
    }
    let canonical = path.canonicalize()?;
    let expected = Path::new("/usr")
        .canonicalize()?
        .join(path.strip_prefix("/usr").map_err(|_| denied())?);
    if canonical != expected {
        return Err(denied());
    }
    let mut ancestor = canonical.parent();
    while let Some(path) = ancestor {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(denied());
        }
        ancestor = path.parent();
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&canonical)?;
    let opened = file.metadata()?;
    if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
        return Err(denied());
    }
    let mut filesystem = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: the owned file descriptor is live; fstatvfs initializes this struct.
    if unsafe { libc::fstatvfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful fstatvfs initialized the structure.
    if unsafe { filesystem.assume_init() }.f_flag & libc::ST_RDONLY == 0 {
        return Err(denied());
    }
    let mut bytes = Vec::new();
    file.take(258).read_to_end(&mut bytes)?;
    decode(&bytes, path == Path::new(PINS[1]))
}

#[cfg(not(unix))]
fn read_pin(path: &Path) -> std::io::Result<Option<String>> {
    if path.try_exists()? {
        Err(denied())
    } else {
        Ok(None)
    }
}

fn decode(bytes: &[u8], optional_engine: bool) -> std::io::Result<Option<String>> {
    // An explicitly empty optional-engine pin is the existing image contract.
    if bytes.is_empty() && optional_engine {
        return Ok(None);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| denied())?.trim();
    if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "installed runtime publisher pin is malformed",
        ));
    }
    Ok(Some(text.to_ascii_lowercase()))
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::process::Command;

    fn namespace(executable: &Path, operator: &Path) -> Command {
        let mut command = Command::new("timeout");
        command.args([
            "20",
            "bwrap",
            "--die-with-parent",
            "--unshare-all",
            "--tmpfs",
            "/",
            "--dir",
            "/usr",
            "--dir",
            "/usr/share",
            "--dir",
            "/usr/share/tilo",
            "--dir",
            "/foreign",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
        ]);
        for path in ["/usr/lib64", "/usr/lib"] {
            if Path::new(path).is_dir() {
                command.args(["--ro-bind", path, path]);
            }
        }
        command.args([
            "--symlink",
            "usr/lib64",
            "/lib64",
            "--symlink",
            "usr/lib",
            "/lib",
        ]);
        command.arg("--ro-bind").arg(executable).arg("/fixture");
        command.arg("--ro-bind").arg(operator).arg("/operator.txt");
        command
    }

    #[test]
    fn installed_component_pins_use_real_readonly_files_and_fail_closed() {
        const CASE: &str = "TILO_TEST_INSTALLED_PIN_CASE";
        if let Ok(case) = std::env::var(CASE) {
            let loaded = load_with_components(Path::new("/operator.txt"));
            match case.as_str() {
                "readonly" => {
                    let keys = loaded.unwrap().unwrap();
                    assert!(keys.contains(&"11".repeat(32)));
                    assert!(keys.contains(&"22".repeat(32)));
                }
                "mutable" | "malformed" | "symlink" | "escape" | "empty" | "writebits" => {
                    assert!(loaded.is_err());
                }
                other => panic!("unexpected fixture case: {other}"),
            }
            return;
        }
        let fixture = tempfile::tempdir().unwrap();
        let pin = fixture.path().join("pin");
        let malformed = fixture.path().join("malformed");
        let operator = fixture.path().join("operator");
        let empty = fixture.path().join("empty");
        let writable = fixture.path().join("writable");
        std::fs::write(&pin, "11".repeat(32)).unwrap();
        std::fs::write(&malformed, "not a publisher").unwrap();
        std::fs::write(&operator, "22".repeat(32)).unwrap();
        std::fs::write(&empty, "").unwrap();
        std::fs::write(&writable, "11".repeat(32)).unwrap();
        std::fs::set_permissions(&writable, std::fs::Permissions::from_mode(0o666)).unwrap();
        let executable = std::env::current_exe().unwrap();
        for case in [
            "readonly",
            "mutable",
            "malformed",
            "symlink",
            "escape",
            "empty",
            "writebits",
        ] {
            let mut command = namespace(&executable, &operator);
            if std::fs::metadata(&pin).unwrap().uid() != 0 {
                // An unprivileged runner's files are its own. Map it to root in
                // the namespace so the fixture looks as the root-owned image
                // files the pins are read from do.
                command.args(["--uid", "0", "--gid", "0"]);
            }
            match case {
                "symlink" => {
                    command.args(["--symlink", "/operator.txt", PINS[0]]);
                }
                "escape" => {
                    command.args(["--tmpfs", "/usr/share"]);
                    command.args(["--symlink", "/foreign", "/usr/share/tilo"]);
                    command
                        .arg("--ro-bind")
                        .arg(&pin)
                        .arg("/foreign/runtime-bundle-publisher.hex");
                }
                _ => {
                    command.arg(if case == "mutable" {
                        "--bind"
                    } else {
                        "--ro-bind"
                    });
                    command
                        .arg(match case {
                            "malformed" => &malformed,
                            "empty" => &empty,
                            "writebits" => &writable,
                            _ => &pin,
                        })
                        .arg(PINS[0]);
                }
            }
            command.args(["--setenv", CASE, case, "--", "/fixture", "--exact",
                "component_pins::tests::installed_component_pins_use_real_readonly_files_and_fail_closed", "--nocapture"]);
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "case {case}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
