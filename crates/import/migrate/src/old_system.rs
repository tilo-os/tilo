//! A computer's old system, left on a disk: Windows, macOS or Linux that ran before Tilo did, on
//! a partition of this computer or a drive attached to it. Its homes are read from their folders
//! as they lie on the disk, with nothing of that system running, so what a person kept there can
//! still come over after Tilo is installed beside it, or after the drive was taken out of the old
//! computer.
//!
//! A system is found by its layout: Windows by `Windows/System32` and `Users`, macOS by `Users`
//! beside `Library`, Linux by `etc/os-release` beside `home`. A home is told apart the same way
//! ([`OldHome::at`]), so a home handed over by its path is read as what it is.

use std::path::{Path, PathBuf};

/// Which system a home was kept by: it decides where its things are inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Windows,
    MacOs,
    Linux,
}

impl Os {
    /// What a person calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::MacOs => "macOS",
            Self::Linux => "Linux",
        }
    }

    /// The familiar folders a home of this system keeps, by their names in it.
    #[must_use]
    pub const fn familiar_folders(self) -> &'static [&'static str] {
        match self {
            Self::Windows | Self::Linux => &[
                "Desktop",
                "Documents",
                "Downloads",
                "Music",
                "Pictures",
                "Videos",
            ],
            Self::MacOs => &[
                "Desktop",
                "Documents",
                "Downloads",
                "Movies",
                "Music",
                "Pictures",
            ],
        }
    }
}

/// One person's home on an old system's disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OldHome {
    pub os: Os,
    /// The home's folder.
    pub home: PathBuf,
    /// The name of the account it is the home of.
    pub person: String,
}

impl OldHome {
    /// The home `path` is, told by what it holds: `AppData` is a Windows home, `Library` beside
    /// `Desktop` a macOS one, any other folder a Linux home.
    ///
    /// # Errors
    /// `path` is not a folder.
    pub fn at(path: &Path) -> Result<Self, String> {
        if !path.is_dir() {
            return Err(format!("{} is not a folder", path.display()));
        }
        let os = if path.join("AppData").is_dir() || path.join("NTUSER.DAT").is_file() {
            Os::Windows
        } else if path.join("Library/Preferences").is_dir() {
            Os::MacOs
        } else {
            Os::Linux
        };
        let person = path
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        Ok(Self {
            os,
            home: path.to_path_buf(),
            person,
        })
    }

    /// How a person recognises it: `ada on Windows (/run/media/ada/Windows)`.
    #[must_use]
    pub fn label(&self) -> String {
        format!(
            "{} on {} ({})",
            self.person,
            self.os.label(),
            self.home.display()
        )
    }

    /// The folders this home keeps a person's things in, at the paths they lie at: its familiar
    /// folders, then every other visible folder of it.
    #[must_use]
    pub fn folders(&self) -> Vec<(PathBuf, bool)> {
        let mut folders: Vec<(PathBuf, bool)> = Vec::new();
        for name in self.os.familiar_folders() {
            let path = self.home.join(name);
            if path.is_dir() {
                folders.push((path, true));
            }
        }
        let Ok(entries) = std::fs::read_dir(&self.home) else {
            return folders;
        };
        let mut others: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                !name.starts_with('.') && !self.kept_by_the_system(&name)
            })
            .map(|entry| entry.path())
            .filter(|path| !folders.iter().any(|(known, _)| known == path))
            .collect();
        others.sort();
        folders.extend(others.into_iter().map(|path| (path, false)));
        folders
    }

    /// Whether the folder `name` of the home is the system's own rather than the person's.
    fn kept_by_the_system(&self, name: &str) -> bool {
        match self.os {
            Os::Windows => matches!(
                name,
                "AppData"
                    | "Application Data"
                    | "Cookies"
                    | "Local Settings"
                    | "NetHood"
                    | "PrintHood"
                    | "Recent"
                    | "SendTo"
                    | "Start Menu"
                    | "Templates"
                    | "My Documents"
                    | "IntelGraphicsProfiles"
                    | "MicrosoftEdgeBackups"
            ),
            Os::MacOs => matches!(name, "Library" | "Applications" | "Public"),
            Os::Linux => matches!(name, "snap"),
        }
    }
}

/// Which system the folder `root` is the top of, when it is the top of one.
#[must_use]
pub fn system_at(root: &Path) -> Option<Os> {
    if root.join("Windows/System32").is_dir() && root.join("Users").is_dir() {
        Some(Os::Windows)
    } else if root.join("Users").is_dir() && root.join("Library").is_dir() {
        Some(Os::MacOs)
    } else if root.join("etc/os-release").is_file() && root.join("home").is_dir() {
        Some(Os::Linux)
    } else {
        None
    }
}

/// The people's homes of the system whose top is `root`, by the accounts that had them; the
/// system's own (`Public`, `Default`, `Shared`) are not a person's.
#[must_use]
pub fn homes_of(root: &Path, os: Os) -> Vec<OldHome> {
    let homes = match os {
        Os::Windows | Os::MacOs => root.join("Users"),
        Os::Linux => root.join("home"),
    };
    let Ok(entries) = std::fs::read_dir(&homes) else {
        return Vec::new();
    };
    let mut found: Vec<OldHome> = entries
        .filter_map(Result::ok)
        // A link (`All Users`, `Default User`) is the system's way to an account, not one.
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            !name.starts_with('.')
                && !matches!(
                    &*name,
                    "Public"
                        | "Default"
                        | "Default User"
                        | "All Users"
                        | "defaultuser0"
                        | "WDAGUtilityAccount"
                        | "Shared"
                        | "Guest"
                        | "lost+found"
                )
        })
        .map(|entry| OldHome {
            os,
            home: entry.path(),
            person: entry.file_name().to_string_lossy().into_owned(),
        })
        .collect();
    found.sort_by(|a, b| a.home.cmp(&b.home));
    found
}

/// Every old system's homes under `roots`: each root that is the top of a system, and each
/// folder right under a root that is (a drive's mount folder holding its volumes).
#[must_use]
pub fn find(roots: &[PathBuf]) -> Vec<OldHome> {
    let mut found = Vec::new();
    let mut look = |root: &Path| {
        if let Some(os) = system_at(root) {
            for home in homes_of(root, os) {
                if !found.contains(&home) {
                    found.push(home);
                }
            }
            return true;
        }
        false
    };
    for root in roots {
        if look(root) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        let mut below: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .collect();
        below.sort();
        for path in below {
            look(&path);
        }
    }
    found
}

/// The folders this computer mounts other disks' volumes under, for the person running this:
/// where the drives service and the desktop open a drive (`/run/media/<account>`), and the
/// traditional `/media` and `/mnt`.
#[must_use]
pub fn mounted() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    #[cfg(unix)]
    {
        let account = std::env::var("USER")
            .ok()
            .filter(|name| !name.is_empty() && !name.contains('/'));
        if let Some(account) = account {
            roots.push(PathBuf::from("/run/media").join(account));
        }
        roots.push(PathBuf::from("/media"));
        roots.push(PathBuf::from("/mnt"));
    }
    roots.retain(|root| root.is_dir());
    roots
}

/// The old systems' homes found on this computer's mounted disks.
#[must_use]
pub fn found_here() -> Vec<OldHome> {
    find(&mounted())
}

/// The old system on this computer, by what a person calls it (`Windows`): one whose homes lie on
/// a disk it has open, or a partition that holds one and is not open yet (Windows' NTFS data
/// partition, macOS's APFS or HFS+ volume), as the kernel and udev describe its block devices.
#[must_use]
pub fn on_this_computer() -> Option<&'static str> {
    if let Some(home) = found_here().first() {
        return Some(home.os.label());
    }
    partition_holding_one(Path::new("/sys/class/block"), Path::new("/run/udev/data")).map(Os::label)
}

/// GPT's type of Windows' data partitions.
const MICROSOFT_BASIC_DATA: &str = "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7";
/// The smallest NTFS data partition taken for a system's: smaller ones hold a recovery image or
/// a person's files, not Windows.
const SMALLEST_WINDOWS: u64 = 32 << 30;

/// The system a partition among `sys_block`'s block devices holds, read from what udev keeps of
/// each under `udev` (`b<major>:<minor>`).
fn partition_holding_one(sys_block: &Path, udev: &Path) -> Option<Os> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(sys_block)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join("partition").is_file())
        .collect();
    entries.sort();
    entries.into_iter().find_map(|partition| {
        let number = std::fs::read_to_string(partition.join("dev")).ok()?;
        let facts = std::fs::read_to_string(udev.join(format!("b{}", number.trim()))).ok()?;
        let fact = |name: &str| {
            facts.lines().find_map(|line| {
                line.strip_prefix("E:")?
                    .strip_prefix(name)?
                    .strip_prefix('=')
                    .map(str::to_ascii_lowercase)
            })
        };
        let bytes = std::fs::read_to_string(partition.join("size"))
            .ok()
            .and_then(|sectors| sectors.trim().parse::<u64>().ok())
            .map_or(0, |sectors| sectors.saturating_mul(512));
        match fact("ID_FS_TYPE").as_deref() {
            Some("ntfs")
                if fact("ID_PART_ENTRY_TYPE").as_deref() == Some(MICROSOFT_BASIC_DATA)
                    && bytes >= SMALLEST_WINDOWS =>
            {
                Some(Os::Windows)
            }
            Some("apfs" | "hfsplus") => Some(Os::MacOs),
            _ => None,
        }
    })
}

#[cfg(any(test, feature = "fixture"))]
pub mod fixture;

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(root: &Path, paths: &[&str]) {
        for path in paths {
            std::fs::create_dir_all(root.join(path)).unwrap();
        }
    }

    #[test]
    fn each_system_is_told_by_its_layout_and_only_peoples_homes_are_found() {
        let media = tempfile::tempdir().unwrap();
        let windows = media.path().join("Windows-SSD");
        dirs(
            &windows,
            &[
                "Windows/System32",
                "Users/ada/AppData/Local",
                "Users/ada/Documents",
                "Users/Public/Documents",
                "Users/Default/AppData",
            ],
        );
        let mac = media.path().join("Macintosh HD - Data");
        dirs(
            &mac,
            &["Library", "Users/bea/Library/Preferences", "Users/Shared"],
        );
        let linux = media.path().join("fedora");
        dirs(&linux, &["etc", "home/cy/Documents"]);
        std::fs::write(linux.join("etc/os-release"), "NAME=Fedora\n").unwrap();
        dirs(media.path(), &["USB-STICK/photos"]);

        let found = find(&[media.path().to_path_buf()]);
        let named: Vec<(Os, &str)> = found
            .iter()
            .map(|home| (home.os, home.person.as_str()))
            .collect();
        assert_eq!(
            named,
            [(Os::MacOs, "bea"), (Os::Windows, "ada"), (Os::Linux, "cy")]
        );
        assert_eq!(
            OldHome::at(&windows.join("Users/ada")).unwrap().os,
            Os::Windows
        );
        assert_eq!(OldHome::at(&mac.join("Users/bea")).unwrap().os, Os::MacOs);
        assert_eq!(OldHome::at(&linux.join("home/cy")).unwrap().os, Os::Linux);
        assert!(OldHome::at(&media.path().join("nothing")).is_err());
    }

    #[test]
    fn windows_is_found_on_a_partition_not_open_yet_by_its_data_partition() {
        let machine = tempfile::tempdir().unwrap();
        let (sys, udev) = (machine.path().join("sys"), machine.path().join("udev"));
        std::fs::create_dir_all(&udev).unwrap();
        let partition = |name: &str, dev: &str, gib: u64, facts: &str| {
            let at = sys.join(name);
            std::fs::create_dir_all(&at).unwrap();
            std::fs::write(at.join("partition"), "1\n").unwrap();
            std::fs::write(at.join("dev"), format!("{dev}\n")).unwrap();
            std::fs::write(at.join("size"), format!("{}\n", gib * 2 * 1024 * 1024)).unwrap();
            std::fs::write(udev.join(format!("b{dev}")), facts).unwrap();
        };
        let data = format!("E:ID_FS_TYPE=ntfs\nE:ID_PART_ENTRY_TYPE={MICROSOFT_BASIC_DATA}\n");
        // A small NTFS data partition (a USB stick's, a recovery image) is no system.
        partition("sda1", "8:1", 8, &data);
        partition("nvme0n1p1", "259:1", 1, "E:ID_FS_TYPE=vfat\n");
        assert_eq!(partition_holding_one(&sys, &udev), None);
        partition("nvme0n1p3", "259:3", 200, &data);
        assert_eq!(partition_holding_one(&sys, &udev), Some(Os::Windows));
    }

    #[test]
    fn a_windows_home_offers_the_persons_folders_and_not_the_systems() {
        let root = tempfile::tempdir().unwrap();
        dirs(
            root.path(),
            &[
                "AppData/Roaming",
                "Documents",
                "Pictures",
                "Saved Games",
                "Start Menu",
                ".vscode",
            ],
        );
        let home = OldHome::at(root.path()).unwrap();
        let folders: Vec<(String, bool)> = home
            .folders()
            .into_iter()
            .map(|(path, familiar)| {
                (
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                    familiar,
                )
            })
            .collect();
        assert_eq!(
            folders,
            [
                ("Documents".to_owned(), true),
                ("Pictures".to_owned(), true),
                ("Saved Games".to_owned(), false),
            ]
        );
    }
}
