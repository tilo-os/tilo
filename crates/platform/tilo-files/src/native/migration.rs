//! Forward-only migrations of a versioned on-disk format, with a snapshot taken before
//! each, so going back to an older build loses nothing. A file a program writes
//! declares a [`Format`]: its current version, and one [`Step`] per older version it
//! still brings up. Opening an older file runs the steps from there, after copying it
//! as it was to `<file>.v<version>`; a newer one is left alone and refused, since a
//! build never rewrites what a newer one wrote. A build's version is pinned by the
//! bytes it wrote ([`assert_golden_bytes`]): a persisted type's layout is its field
//! order, so changing one bumps the version and adds the step that reads what devices
//! have.

use std::path::{Path, PathBuf};

pub type Step = fn(Vec<u8>) -> Result<Vec<u8>, String>;

/// A versioned on-disk format and how its older versions come up to date.
#[derive(Clone, Copy, Debug)]
pub struct Format {
    /// What the format holds, for snapshots and errors.
    pub name: &'static str,
    /// The version this build writes and reads.
    pub version: u32,
    /// The oldest version this build brings up: `steps[i]` takes version `oldest + i` to `oldest + i +
    /// 1`.
    pub oldest: u32,
    pub steps: &'static [Step],
}

/// Why a file could not be brought up to date; the file is unchanged.
#[derive(Debug)]
pub enum MigrationError {
    Newer {
        name: &'static str,
        stored: u32,
        version: u32,
    },
    TooOld {
        name: &'static str,
        stored: u32,
        oldest: u32,
    },
    Step {
        name: &'static str,
        from: u32,
        reason: String,
    },
    Io {
        name: &'static str,
        source: std::io::Error,
    },
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Newer {
                name,
                stored,
                version,
            } => write!(
                f,
                "{name} version {stored} was written by a newer build than this one ({version})"
            ),
            Self::TooOld {
                name,
                stored,
                oldest,
            } => write!(
                f,
                "{name} version {stored} is older than the oldest this build brings up ({oldest})"
            ),
            Self::Step { name, from, reason } => {
                write!(f, "{name}: bringing version {from} up: {reason}")
            }
            Self::Io { name, source } => write!(f, "{name}: {source}"),
        }
    }
}

impl std::error::Error for MigrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl Format {
    /// One step per version between [`Self::oldest`] and [`Self::version`].
    #[must_use]
    pub fn is_whole(&self) -> bool {
        self.version
            .checked_sub(self.oldest)
            .and_then(|count| usize::try_from(count).ok())
            .is_some_and(|count| count == self.steps.len())
    }

    /// `bytes` of version `stored`, brought up to [`Self::version`] by the steps from `stored` on.
    pub fn migrate(&self, mut bytes: Vec<u8>, stored: u32) -> Result<Vec<u8>, MigrationError> {
        if stored > self.version {
            return Err(MigrationError::Newer {
                name: self.name,
                stored,
                version: self.version,
            });
        }
        if stored < self.oldest || !self.is_whole() {
            return Err(MigrationError::TooOld {
                name: self.name,
                stored,
                oldest: self.oldest,
            });
        }
        let first = usize::try_from(stored.saturating_sub(self.oldest)).unwrap_or(usize::MAX);
        for (offset, step) in self.steps.iter().enumerate().skip(first) {
            let from = self
                .oldest
                .saturating_add(u32::try_from(offset).unwrap_or(u32::MAX));
            bytes = step(bytes).map_err(|reason| MigrationError::Step {
                name: self.name,
                from,
                reason,
            })?;
        }
        Ok(bytes)
    }

    /// Where the file at `path` is kept as it was at version `stored`.
    #[must_use]
    pub fn snapshot_path(path: &Path, stored: u32) -> PathBuf {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".v{stored}"));
        path.with_file_name(name)
    }

    /// The file at `path`, whose contents `bytes` are of version `stored`, brought up to
    /// [`Self::version`] on disk; a file already current is left alone.
    pub fn bring_up(
        &self,
        path: &Path,
        bytes: Vec<u8>,
        stored: u32,
    ) -> Result<Vec<u8>, MigrationError> {
        if stored == self.version {
            return Ok(bytes);
        }
        let migrated = self.migrate(bytes.clone(), stored)?;
        let io = |source| MigrationError::Io {
            name: self.name,
            source,
        };
        super::atomic::write(&Self::snapshot_path(path, stored), &bytes).map_err(io)?;
        super::atomic::write(path, &migrated).map_err(io)?;
        Ok(migrated)
    }
}

/// The step from the previous version, applied to a file that version wrote (`previous`), gives bytes
/// `reads` accepts as the current version.
#[allow(
    clippy::panic,
    reason = "an assertion other crates' tests call; failing is its whole answer"
)]
pub fn assert_step_reads_previous(format: &Format, previous: &[u8], reads: impl Fn(&[u8])) {
    assert!(
        format.is_whole(),
        "{} declares a step per version",
        format.name
    );
    assert!(
        format.version > format.oldest,
        "{} has no previous version to bring up",
        format.name
    );
    let migrated = format
        .migrate(previous.to_vec(), format.version.saturating_sub(1))
        .unwrap_or_else(|error| panic!("{error}"));
    reads(&migrated);
}

/// The test every kept layout carries beside it: `golden` is what a build wrote, `read` takes it with
/// this build's types, and `write` must give those bytes back exactly. A field added, removed,
/// reordered or retyped breaks it, which is when the format needs a new version and a [`Step`] for the
/// bytes devices already have. Nothing else notices: postcard reads a shorter or longer field as long
/// as the types line up, and names no field when it fails.
pub fn assert_golden_bytes(
    name: &str,
    golden: &[u8],
    read: impl FnOnce(&[u8]),
    write: impl FnOnce() -> Vec<u8>,
) {
    read(golden);
    let now = write();
    let parts_at = now
        .iter()
        .zip(golden)
        .position(|(now, kept)| now != kept)
        .unwrap_or_else(|| now.len().min(golden.len()));
    assert!(
        now == golden,
        "{}: this build writes {} bytes where the bytes kept beside it are {}; they part at byte \
         {} ({} against {}). A persisted type's layout changed, and what devices hold no longer \
         reads that way: give the format a new version and the step that reads the old one, or \
         keep the field where it was.",
        name,
        now.len(),
        golden.len(),
        parts_at,
        now.get(parts_at)
            .map_or_else(|| "its end".into(), |byte| format!("{byte:#04x}")),
        golden
            .get(parts_at)
            .map_or_else(|| "its end".into(), |byte| format!("{byte:#04x}")),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(
        clippy::unnecessary_wraps,
        reason = "a migration step's signature: steps may refuse"
    )]
    fn add_tail(mut bytes: Vec<u8>) -> Result<Vec<u8>, String> {
        bytes.push(b'+');
        Ok(bytes)
    }

    fn refuse(_: Vec<u8>) -> Result<Vec<u8>, String> {
        Err("cannot read this".into())
    }

    const NOTES: Format = Format {
        name: "notes",
        version: 3,
        oldest: 1,
        steps: &[add_tail, add_tail],
    };

    #[test]
    fn an_older_file_comes_up_to_date_and_the_old_one_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        std::fs::write(&path, b"v1").unwrap();

        let migrated = NOTES.bring_up(&path, b"v1".to_vec(), 1).unwrap();
        assert_eq!(migrated, b"v1++");
        assert_eq!(std::fs::read(&path).unwrap(), b"v1++");
        let snapshot = Format::snapshot_path(&path, 1);
        assert_eq!(snapshot, dir.path().join("notes.json.v1"));
        assert_eq!(
            std::fs::read(snapshot).unwrap(),
            b"v1",
            "rollback loses nothing"
        );

        assert_eq!(NOTES.bring_up(&path, b"v3".to_vec(), 3).unwrap(), b"v3");
        assert_eq!(std::fs::read(&path).unwrap(), b"v1++");
    }

    #[test]
    fn a_newer_file_or_a_refused_step_leaves_the_file_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        std::fs::write(&path, b"v4").unwrap();
        assert!(matches!(
            NOTES.bring_up(&path, b"v4".to_vec(), 4),
            Err(MigrationError::Newer { stored: 4, .. })
        ));
        assert!(matches!(
            NOTES.bring_up(&path, b"v0".to_vec(), 0),
            Err(MigrationError::TooOld { stored: 0, .. })
        ));
        let refusing = Format {
            steps: &[add_tail, refuse],
            ..NOTES
        };
        assert!(matches!(
            refusing.bring_up(&path, b"v1".to_vec(), 1),
            Err(MigrationError::Step { from: 2, .. })
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"v4");
        assert!(!Format::snapshot_path(&path, 1).exists());
    }

    #[test]
    fn golden_bytes_are_what_this_build_reads_and_writes() {
        assert_golden_bytes(
            "notes",
            b"v3",
            |bytes| assert_eq!(bytes, b"v3"),
            || b"v3".to_vec(),
        );
        let failed = std::panic::catch_unwind(|| {
            assert_golden_bytes("notes", b"v3", |_| {}, || b"v4".to_vec());
        });
        assert!(failed.is_err(), "a layout that changed does not pass");
    }

    #[test]
    fn a_format_has_a_step_per_version() {
        assert!(NOTES.is_whole());
        let missing = Format {
            steps: &[add_tail],
            ..NOTES
        };
        assert!(!missing.is_whole());
        assert!(matches!(
            missing.migrate(b"v1".to_vec(), 1),
            Err(MigrationError::TooOld { .. })
        ));
        assert_step_reads_previous(&NOTES, b"v2", |bytes| assert_eq!(bytes, b"v2+"));
    }
}
