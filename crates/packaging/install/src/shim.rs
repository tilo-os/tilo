//! The `[cli].commands` block becomes one PATH shim per command under
//! `~/.local/share/tilo-apps/bin/<cmd>`: a copy of the `tilo-cli-shim` binary and a
//! `<cmd>.tilo-shim` sidecar with the package id and command name.

use std::fs;
use std::path::{Path, PathBuf};

use cli_shim::{Sidecar, sidecar_path_for};

/// What `write_shims` writes.
#[derive(Debug, Clone)]
pub struct ShimInstall<'a> {
    /// Reverse-DNS package id.
    pub package_id: &'a str,
    /// PATH-visible command names (`manifest.cli.commands`).
    pub commands: &'a [String],
    /// The `tilo-cli-shim` binary to copy; [`locate_shim_source`] finds it.
    pub shim_source: &'a Path,
    /// The `bin/` directory under the user's tilo-apps root.
    pub bin_dir: &'a Path,
    /// Baked into each sidecar as `launcher`; `None` uses the system launcher path.
    pub launcher_override: Option<&'a Path>,
}

/// Install one shim per command. Returns the files written, so the caller can record them in the
/// registry entry and remove them later.
///
/// # Errors
///
/// When a command name is not filesystem-safe, or a directory, the shim or its sidecar cannot be
/// written.
pub fn write_shims(spec: &ShimInstall<'_>) -> Result<Vec<PathBuf>, ShimError> {
    fs::create_dir_all(spec.bin_dir).map_err(|e| ShimError::CreateDir {
        path: spec.bin_dir.to_path_buf(),
        source: e,
    })?;
    let mut written = Vec::with_capacity(spec.commands.len().saturating_mul(2));
    for cmd in spec.commands {
        validate_command_name(cmd)?;
        let shim_target = spec.bin_dir.join(cmd);
        let sidecar_target = sidecar_path_for(&shim_target);

        // Overwrite: an upgrade replaces the previous install's shims.
        let _ = fs::remove_file(&shim_target);
        let _ = fs::remove_file(&sidecar_target);

        fs::copy(spec.shim_source, &shim_target).map_err(|e| ShimError::CopyShim {
            src: spec.shim_source.to_path_buf(),
            dst: shim_target.clone(),
            source: e,
        })?;
        set_executable(&shim_target)?;

        let sidecar = Sidecar {
            package_id: spec.package_id.to_owned(),
            command: cmd.clone(),
            launcher: spec.launcher_override.map(Path::to_path_buf),
        };
        let sidecar_text =
            toml::to_string(&sidecar).map_err(|e| ShimError::EncodeSidecar(e.to_string()))?;
        fs::write(&sidecar_target, sidecar_text).map_err(|e| ShimError::WriteSidecar {
            path: sidecar_target.clone(),
            source: e,
        })?;

        written.push(shim_target);
        written.push(sidecar_target);
    }
    Ok(written)
}

/// Remove only shims whose sidecar still names this package and command: another package may have
/// replaced a shared command since installation. Missing or unreadable sidecars are left alone.
///
/// # Errors
///
/// [`ShimError::BadCommandName`] for an unsafe command in a stale registry entry, before any path
/// outside the shim directory is touched.
pub fn remove_shims(
    bin_dir: &Path,
    package_id: &str,
    commands: &[String],
) -> Result<(), ShimError> {
    for cmd in commands {
        validate_command_name(cmd)?;
        let shim = bin_dir.join(cmd);
        let sidecar_path = sidecar_path_for(&shim);
        let Ok(text) = fs::read_to_string(&sidecar_path) else {
            continue;
        };
        let Ok(sidecar) = toml::from_str::<Sidecar>(&text) else {
            continue;
        };
        if sidecar.package_id != package_id || sidecar.command != *cmd {
            continue;
        }
        let _ = fs::remove_file(sidecar_path);
        let _ = fs::remove_file(shim);
    }
    Ok(())
}

/// The `tilo-cli-shim` binary: `TILO_CLI_SHIM_PATH`, then a sibling of this executable, then
/// `/usr/libexec/tilo-cli-shim`, else `PATH` at copy time.
pub fn locate_shim_source() -> PathBuf {
    if let Some(over) = std::env::var_os("TILO_CLI_SHIM_PATH") {
        return PathBuf::from(over);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        let sib = parent.join("tilo-cli-shim");
        if sib.exists() {
            return sib;
        }
    }
    let system = PathBuf::from("/usr/libexec/tilo-cli-shim");
    if system.exists() {
        return system;
    }
    PathBuf::from("tilo-cli-shim")
}

fn validate_command_name(name: &str) -> Result<(), ShimError> {
    if name.is_empty() {
        return Err(ShimError::BadCommandName("<empty>".into()));
    }
    // Keep the name inside `bin/` and visible: no separators, no leading dot. The manifest schema is
    // permissive here; the installer draws the line.
    if name.contains('/') || name.contains('\\') || name.starts_with('.') {
        return Err(ShimError::BadCommandName(name.to_string()));
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), ShimError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::metadata(path).map_err(|e| ShimError::CopyShim {
        src: path.to_path_buf(),
        dst: path.to_path_buf(),
        source: e,
    })?;
    let mut perms = meta.permissions();
    perms.set_mode(perms.mode() | 0o111);
    fs::set_permissions(path, perms).map_err(|e| ShimError::CopyShim {
        src: path.to_path_buf(),
        dst: path.to_path_buf(),
        source: e,
    })
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), ShimError> {
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum ShimError {
    #[error(
        "shim command name {0:?} is not filesystem-safe (empty, contains `/`, or starts with `.`)"
    )]
    BadCommandName(String),
    #[error("create dir {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("copy shim from {src} to {dst}: {source}")]
    CopyShim {
        src: PathBuf,
        dst: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("write sidecar {path}: {source}")]
    WriteSidecar {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("sidecar encode: {0}")]
    EncodeSidecar(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_shim(tmp: &Path) -> PathBuf {
        let p = tmp.join("tilo-cli-shim");
        fs::write(&p, b"#!/bin/sh\necho stub\n").unwrap();
        set_executable(&p).unwrap();
        p
    }

    #[test]
    fn removing_a_replaced_command_keeps_the_current_packages_shim() {
        let tmp = tempfile::tempdir().unwrap();
        let shim = dummy_shim(tmp.path());
        let bin_dir = tmp.path().join("bin");
        let commands = vec!["notes".to_string()];
        for package_id in ["so.tilo.old-notes", "so.tilo.new-notes"] {
            write_shims(&ShimInstall {
                package_id,
                commands: &commands,
                shim_source: &shim,
                bin_dir: &bin_dir,
                launcher_override: None,
            })
            .unwrap();
        }
        remove_shims(&bin_dir, "so.tilo.old-notes", &commands).unwrap();
        assert!(bin_dir.join("notes").is_file());
        let text = fs::read_to_string(bin_dir.join("notes.tilo-shim")).unwrap();
        let sidecar: Sidecar = toml::from_str(&text).unwrap();
        assert_eq!(sidecar.package_id, "so.tilo.new-notes");
        remove_shims(&bin_dir, "so.tilo.new-notes", &commands).unwrap();
        assert!(!bin_dir.join("notes").exists());
        assert!(!bin_dir.join("notes.tilo-shim").exists());
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn shims_are_written_idempotently_safely_and_removed_with_their_sidecars() {
        {
            let tmp = tempfile::tempdir().unwrap();
            let shim = dummy_shim(tmp.path());
            let bin_dir = tmp.path().join("bin");
            let commands = vec!["notes".to_string(), "notes-import".to_string()];
            let written = write_shims(&ShimInstall {
                package_id: "so.tilo.notes-cli",
                commands: &commands,
                shim_source: &shim,
                bin_dir: &bin_dir,
                launcher_override: None,
            })
            .expect("write_shims");
            assert_eq!(written.len(), 4);
            for cmd in &commands {
                let bin_path = bin_dir.join(cmd);
                let side_path = bin_dir.join(format!("{cmd}.tilo-shim"));
                assert!(bin_path.exists(), "shim {cmd} should exist");
                assert!(side_path.exists(), "sidecar {cmd}.tilo-shim should exist");
                let sidecar_text = fs::read_to_string(&side_path).unwrap();
                assert!(sidecar_text.contains("so.tilo.notes-cli"));
                assert!(sidecar_text.contains(cmd));
            }
        }
        {
            let tmp = tempfile::tempdir().unwrap();
            let shim = dummy_shim(tmp.path());
            let bin_dir = tmp.path().join("bin");
            let commands = vec!["notes".to_string()];
            let launcher = PathBuf::from("/tmp/dev/tilo-launcher");
            write_shims(&ShimInstall {
                package_id: "so.tilo.notes-cli",
                commands: &commands,
                shim_source: &shim,
                bin_dir: &bin_dir,
                launcher_override: Some(&launcher),
            })
            .unwrap();
            let text = fs::read_to_string(bin_dir.join("notes.tilo-shim")).unwrap();
            assert!(
                text.contains("launcher"),
                "sidecar must carry launcher override; got: {text}"
            );
            assert!(text.contains("/tmp/dev/tilo-launcher"));
        }
        {
            let tmp = tempfile::tempdir().unwrap();
            let shim = dummy_shim(tmp.path());
            let bin_dir = tmp.path().join("bin");
            let commands = vec!["notes".to_string()];
            write_shims(&ShimInstall {
                package_id: "so.tilo.notes-cli",
                commands: &commands,
                shim_source: &shim,
                bin_dir: &bin_dir,
                launcher_override: None,
            })
            .unwrap();
            assert!(bin_dir.join("notes").exists());
            remove_shims(&bin_dir, "so.tilo.notes-cli", &commands).unwrap();
            assert!(!bin_dir.join("notes").exists());
            assert!(!bin_dir.join("notes.tilo-shim").exists());
        }
        {
            let tmp = tempfile::tempdir().unwrap();
            let shim = dummy_shim(tmp.path());
            let bin_dir = tmp.path().join("bin");
            let bad_names = vec![
                "../evil".to_string(),
                "a/b".to_string(),
                ".hidden".to_string(),
                String::new(),
            ];
            for name in bad_names {
                let commands = vec![name.clone()];
                let e = write_shims(&ShimInstall {
                    package_id: "so.tilo.notes-cli",
                    commands: &commands,
                    shim_source: &shim,
                    bin_dir: &bin_dir,
                    launcher_override: None,
                })
                .expect_err(&format!("write_shims should reject {name:?}"));
                assert!(matches!(e, ShimError::BadCommandName(_)), "got {e:?}");
            }
        }
        {
            let tmp = tempfile::tempdir().unwrap();
            let shim = dummy_shim(tmp.path());
            let bin_dir = tmp.path().join("bin");
            let commands = vec!["notes".to_string()];
            write_shims(&ShimInstall {
                package_id: "so.tilo.notes-cli",
                commands: &commands,
                shim_source: &shim,
                bin_dir: &bin_dir,
                launcher_override: None,
            })
            .unwrap();
            // A second install with a different launcher override must end with the second one
            // baked in.
            let l2 = PathBuf::from("/tmp/other/tilo-launcher");
            write_shims(&ShimInstall {
                package_id: "so.tilo.notes-cli",
                commands: &commands,
                shim_source: &shim,
                bin_dir: &bin_dir,
                launcher_override: Some(&l2),
            })
            .unwrap();
            let text = fs::read_to_string(bin_dir.join("notes.tilo-shim")).unwrap();
            assert!(text.contains("/tmp/other/tilo-launcher"));
        }
    }
}
