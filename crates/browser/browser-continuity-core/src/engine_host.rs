//! Native browser helper launch boundary.
//!
//! Every helper, including dedicated service profiles, excludes the reviewed
//! migration projection before exec. The UI owns the protocol, not OS read
//! authority; session cleanup stays with its existing lease-holding owner.
use std::path::{Path, PathBuf};
use std::process::Command;

/// Construct a helper command without the app's allocator or loader overrides.
///
/// # Errors
/// Refuses malformed projections and unavailable mandatory read confinement.
pub fn command(host: &Path) -> std::io::Result<Command> {
    let mut command = Command::new(host);
    command.env_remove("LD_LIBRARY_PATH");
    command.env_remove("LD_PRELOAD");
    exclude_migration_from_engine(&mut command, Path::new(crate::PROJECTION_DIRECTORY))?;
    Ok(command)
}

/// Attach the read exclusion used by the helper command factory.
///
/// A missing projection grants nothing and needs no policy. A present invalid
/// projection fails closed. The caller still owns spawning/reaping the child.
#[cfg(target_os = "linux")]
pub fn exclude_migration_from_engine(
    command: &mut Command,
    projection: &Path,
) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    match std::fs::symlink_metadata(projection) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(std::io::Error::other(
                "Invalid browser migration projection",
            ));
        }
    }
    let policy = sandbox_core::read_exclusion::PreparedReadExclusion::prepare(projection)?;
    // SAFETY: all policy construction happens above. The callback performs only
    // integer-only kernel calls; no allocations, locks, paths or shared state.
    unsafe {
        command.pre_exec(move || policy.install_before_exec());
    }
    Ok(())
}

/// Other hosts never receive the Linux launcher's migration projection.
///
/// # Errors
/// Refuses an unexpected projection rather than launching without confinement.
#[cfg(not(target_os = "linux"))]
pub fn exclude_migration_from_engine(
    _command: &mut Command,
    projection: &Path,
) -> std::io::Result<()> {
    match std::fs::symlink_metadata(projection) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(std::io::Error::other(
            "Browser migration confinement is unavailable",
        )),
    }
}

/// The engine shipped in a macOS app bundle, if this host uses that layout.
pub fn bundled_host(executable: Option<&Path>) -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let contents = executable?.parent()?.parent()?;
    Some(
        contents
            .join("Frameworks")
            .join("Browser Engine.app/Contents/MacOS/tilo-chromium-host"),
    )
}
