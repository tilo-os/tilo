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
    load_with_component_root(operator_list, Path::new("/usr"))
}

/// Resolve the same pin policy in an independently prepared readonly system
/// view. The root argument does not relax any ownership, path or mount check.
/// It is used by the maintained updater before a generation can be activated.
///
/// # Errors
/// The same malformed, mutable or non-root-owned pin failures as
/// [`load_with_components`].
pub fn load_with_component_root(
    operator_list: &Path,
    usr_root: &Path,
) -> std::io::Result<Option<Vec<String>>> {
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
    for original in PINS {
        let suffix = Path::new(original)
            .strip_prefix("/usr")
            .map_err(|_| denied())?;
        let path = usr_root.join(suffix);
        if let Some(key) = read_pin(&path, usr_root, original == PINS[1])? {
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
fn read_pin(
    path: &Path,
    usr_root: &Path,
    optional_engine: bool,
) -> std::io::Result<Option<String>> {
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
    let expected = usr_root
        .canonicalize()?
        .join(path.strip_prefix(usr_root).map_err(|_| denied())?);
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
    decode(&bytes, optional_engine)
}

#[cfg(not(unix))]
fn read_pin(
    path: &Path,
    _usr_root: &Path,
    _optional_engine: bool,
) -> std::io::Result<Option<String>> {
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;
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

    fn alternate_view(command: &mut Command, case: &str, pin: &Path) {
        command.args([
            "--dir",
            "/alternate",
            "--dir",
            "/alternate/usr",
            "--dir",
            "/alternate/usr/share",
            "--dir",
            "/alternate/usr/share/tilo",
        ]);
        if case == "alternate-symlink" {
            command.args([
                "--symlink",
                "/operator.txt",
                "/alternate/usr/share/tilo/runtime-bundle-publisher.hex",
            ]);
        } else {
            command
                .arg(if case == "alternate-mutable" {
                    "--bind"
                } else {
                    "--ro-bind"
                })
                .arg(pin)
                .arg("/alternate/usr/share/tilo/runtime-bundle-publisher.hex");
        }
    }

    #[test]
    fn installed_component_pins_use_real_readonly_files_and_fail_closed() {
        const CASE: &str = "TILO_TEST_INSTALLED_PIN_CASE";
        if let Ok(case) = std::env::var(CASE) {
            let loaded = if case.starts_with("alternate") {
                load_with_component_root(Path::new("/operator.txt"), Path::new("/alternate/usr"))
            } else {
                load_with_components(Path::new("/operator.txt"))
            };
            match case.as_str() {
                "readonly" | "alternate" => {
                    let keys = loaded.unwrap().unwrap();
                    assert!(keys.contains(&"11".repeat(32)));
                    assert!(keys.contains(&"22".repeat(32)));
                }
                "mutable" | "malformed" | "symlink" | "escape" | "empty" | "writebits"
                | "alternate-mutable" | "alternate-symlink" => {
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
            "alternate",
            "alternate-mutable",
            "alternate-symlink",
        ] {
            let mut command = namespace(&executable, &operator);
            match case {
                "alternate" | "alternate-mutable" | "alternate-symlink" => {
                    alternate_view(&mut command, case, &pin);
                }
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
