//! `tilo-install-handoff-copy SOURCE DESTINATION`: copies the setup handoff's
//! contents into the installed system without importing the private staging
//! directory's metadata.
//!
//! Directories that already exist keep their ownership, mode and security
//! label; a new directory is 0755 so its contents stay reachable. Every copied
//! file is 0600: `tilo-install` then applies the explicit network profile,
//! identity-key and public-key policies. A symlink on either side, or anything
//! that is neither a regular file nor a directory, refuses the copy.

use std::fs::{self, DirBuilder, File, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;

use rustix::fs::{Mode, OFlags};

use crate::args::{Arg, Command};
use crate::{Error, Result};

/// The command line.
pub const COMMAND: Command = Command {
    prog: "tilo-install-handoff-copy",
    usage: "usage: tilo-install-handoff-copy [-h] source destination\n",
    help: "
Copy handoff contents without importing private staging-directory metadata.

positional arguments:
  source
  destination

options:
  -h, --help   show this help message and exit
",
    args: &[Arg::Positional("source"), Arg::Positional("destination")],
};

fn is_real_dir(path: &Path) -> bool {
    !path.is_symlink() && path.is_dir()
}

/// Copies the tree under `source` into `destination`.
///
/// # Errors
/// A symlink, a special file or a type mismatch on either side, refused before
/// that entry is written; or an I/O failure.
pub fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    if !is_real_dir(source) {
        return Err(Error::new("handoff source must be a real directory"));
    }
    if !is_real_dir(destination) {
        return Err(Error::new("installed destination must be a real directory"));
    }
    let mut names: Vec<_> = fs::read_dir(source)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<io::Result<_>>()?;
    names.sort();
    for name in names {
        let entry = source.join(&name);
        let target = destination.join(&name);
        if entry.is_symlink() || target.is_symlink() {
            return Err(Error::new("handoff paths must not contain symlinks"));
        }
        if entry.is_dir() {
            if !target.exists() {
                DirBuilder::new().mode(0o755).create(&target)?;
                fs::set_permissions(&target, Permissions::from_mode(0o755))?;
            }
            copy_tree(&entry, &target)?;
        } else if entry.is_file() {
            if target.exists() && !target.is_file() {
                return Err(Error::new("handoff destination is not a regular file"));
            }
            copy_file(&entry, &target)?;
        } else {
            return Err(Error::new(
                "handoff source is not a regular file or directory",
            ));
        }
    }
    Ok(())
}

/// A new file starts private and never follows a planted link.
fn copy_file(entry: &Path, target: &Path) -> Result<()> {
    let descriptor = rustix::fs::open(
        target,
        OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?;
    let mut output = File::from(descriptor);
    output.set_permissions(Permissions::from_mode(0o600))?;
    let mut stream = File::open(entry)?;
    io::copy(&mut stream, &mut output)?;
    Ok(())
}

/// The command: `copy_tree(source, destination)`.
///
/// # Errors
/// As [`copy_tree`].
pub fn run(values: &crate::args::Values) -> Result<()> {
    copy_tree(
        Path::new(values.required("source")?),
        Path::new(values.required("destination")?),
    )
}
