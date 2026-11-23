//! Room on the host filesystem a VM's files will be written to.
//!
//! `df` answers for the directory that exists, which is not always the path the caller
//! names (a directory under a tmpfs or another mount), so a refusal says which filesystem
//! it measured as well as how much room it has.

use std::path::Path;

use super::human_bytes;

pub(super) fn existing_ancestor(path: &Path) -> &Path {
    path.ancestors()
        .find(|p| p.exists())
        .unwrap_or_else(|| Path::new("/"))
}

/// Room on the filesystem a file written at `path` would land on, as `df` reports it: its
/// mount and device name too, so a refusal says which filesystem it measured instead of
/// leaving the reader to guess (a directory under a tmpfs or another mount is not the
/// filesystem its parent path suggests).
pub(super) struct HostSpace {
    pub(super) free: u64,
    total: u64,
    source: String,
    mount: String,
}

pub(super) fn host_space(path: &Path) -> Option<HostSpace> {
    #[cfg(windows)]
    {
        let output = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[IO.DriveInfo]::new([IO.Path]::GetPathRoot($env:TILO_VM_DISK_DIR)).AvailableFreeSpace",
            ])
            .env("TILO_VM_DISK_DIR", existing_ancestor(path))
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let free = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .ok()?;
        let root = existing_ancestor(path).display().to_string();
        Some(HostSpace {
            free,
            total: 0,
            source: root.clone(),
            mount: root,
        })
    }
    #[cfg(unix)]
    {
        let output = std::process::Command::new("df")
            .arg("-Pk")
            .arg(existing_ancestor(path))
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        parse_df(&text)
    }
}

/// `df -Pk`'s second line: the filesystem, its 1K blocks, then the mount point.
#[cfg(unix)]
fn parse_df(text: &str) -> Option<HostSpace> {
    let line = text.lines().nth(1)?;
    let mut fields = line.split_whitespace();
    let source = fields.next()?.to_owned();
    let total: u64 = fields.next()?.parse().ok()?;
    let _used = fields.next()?;
    let avail: u64 = fields.next()?.parse().ok()?;
    let _capacity = fields.next()?;
    let mount = fields.collect::<Vec<_>>().join(" ");
    Some(HostSpace {
        free: avail.saturating_mul(1024),
        total: total.saturating_mul(1024),
        source,
        mount,
    })
}

/// The path measured and the filesystem it is on: `~/.cache/tilo-vm/disks (/dev/nvme0n1p3
/// mounted at /var/home)`.
pub(super) fn measured(dir: &Path, space: &HostSpace) -> String {
    let at = existing_ancestor(dir);
    if space.mount.is_empty() || space.mount == space.source {
        format!("{} ({})", at.display(), space.source)
    } else {
        format!(
            "{} ({} mounted at {})",
            at.display(),
            space.source,
            space.mount
        )
    }
}

/// The free space with the size of the filesystem beside it, so a small one is recognizable.
pub(super) fn room(space: &HostSpace) -> String {
    if space.total == 0 {
        human_bytes(space.free)
    } else {
        format!(
            "{} of {}",
            human_bytes(space.free),
            human_bytes(space.total)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A refusal that names only the path lets a reader blame "the disk" when the path was
    /// on a small filesystem of its own; the filesystem's size and device name say which
    /// one was measured.
    #[cfg(unix)]
    #[test]
    fn a_space_refusal_names_the_filesystem_it_measured() {
        let space = parse_df(
            "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
             tmpfs 16316416 6144000 9216784 40% /run/user/1000 extra words\n",
        )
        .expect("a df line");
        assert_eq!(space.free, 9_216_784 * 1024);
        assert_eq!(space.total, 16_316_416 * 1024);
        assert_eq!(space.source, "tmpfs");
        assert_eq!(space.mount, "/run/user/1000 extra words");
        assert_eq!(room(&space), "8.7 GB of 15.5 GB");
        let place = measured(Path::new("/"), &space);
        assert_eq!(place, "/ (tmpfs mounted at /run/user/1000 extra words)");
        assert_eq!(
            measured(
                Path::new("/"),
                &HostSpace {
                    mount: "tmpfs".into(),
                    source: "tmpfs".into(),
                    ..space
                }
            ),
            "/ (tmpfs)"
        );
        assert!(parse_df("Filesystem 1024-blocks\n").is_none());
    }
}
