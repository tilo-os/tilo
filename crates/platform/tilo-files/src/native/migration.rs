//! Forward-only migrations of a versioned on-disk format, with a snapshot
//! taken before each.
//!
//! User data is the one thing every update keeps
//! (docs/architecture/compatibility.md). A file a program writes in a
//! format of its own declares a [`Format`]: its current version, and one
//! [`Step`] per older version it still brings up, each taking the bytes of
//! one version to the next. Opening a file of an older version runs the
//! steps from there, after copying the file as it was to a snapshot beside
//! it (`<file>.v<version>`), so going back to the build that wrote it loses
//! nothing. A file of a newer version is left as it is and refused: a build
//! never rewrites what a newer one wrote.
//!
//! Each step is tested against a file the previous version wrote, and only
//! that ([`assert_step_reads_previous`]); `tools/policy/check_format_migrations.py`
//! holds every format to it and keeps the chain from losing a step.

use std::path::{Path, PathBuf};

/// Brings the bytes of one version of a format to the next.
pub type Step = fn(Vec<u8>) -> Result<Vec<u8>, String>;

/// A versioned on-disk format and how its older versions come up to date.
#[derive(Clone, Copy, Debug)]
pub struct Format {
    /// What the format holds, for snapshots' and errors' sake.
    pub name: &'static str,
    /// The version this build writes and reads.
    pub version: u32,
    /// The oldest version this build brings up; `steps[i]` takes version
    /// `oldest + i` to `oldest + i + 1`, so there are `version - oldest`.
    pub oldest: u32,
    pub steps: &'static [Step],
}

/// Why a file could not be brought up to date. The file is unchanged.
#[derive(Debug)]
pub enum MigrationError {
    /// A newer build wrote it.
    Newer {
        name: &'static str,
        stored: u32,
        version: u32,
    },
    /// It is older than any version this build brings up.
    TooOld {
        name: &'static str,
        stored: u32,
        oldest: u32,
    },
    /// A step refused the bytes.
    Step {
        name: &'static str,
        from: u32,
        reason: String,
    },
    /// The snapshot or the migrated file could not be written.
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
    /// Whether the declaration is whole: one step per version between
    /// [`Self::oldest`] and [`Self::version`].
    #[must_use]
    pub fn is_whole(&self) -> bool {
        self.version
            .checked_sub(self.oldest)
            .and_then(|count| usize::try_from(count).ok())
            .is_some_and(|count| count == self.steps.len())
    }

    /// `bytes` of version `stored`, brought up to [`Self::version`] by the
    /// steps from `stored` on.
    ///
    /// # Errors
    ///
    /// [`MigrationError::Newer`], [`MigrationError::TooOld`] or the
    /// [`MigrationError::Step`] that refused.
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
        let first = usize::try_from(stored - self.oldest).unwrap_or(usize::MAX);
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

    /// The file at `path`, whose contents `bytes` are of version `stored`,
    /// brought up to [`Self::version`] on disk. A file already current is
    /// returned as it is and nothing is written. Otherwise the file as it was
    /// is copied to [`Self::snapshot_path`] first, then replaced, atomically,
    /// by the migrated bytes, which are returned.
    ///
    /// # Errors
    ///
    /// As [`Self::migrate`], or [`MigrationError::Io`]; the file is unchanged
    /// on every error.
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

/// The test every [`Format`] past its first version carries: the step from
/// the previous version, applied to a file that version wrote (`previous`),
/// gives bytes `reads` accepts as the current version.
///
/// # Panics
///
/// When the format is not whole, has no step from the previous version, or
/// the step or `reads` refuses.
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
        .migrate(previous.to_vec(), format.version - 1)
        .unwrap_or_else(|error| panic!("{error}"));
    reads(&migrated);
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // USER REQUIREMENT (2026-10-18): "it's fine if we break compat with old versions every few days, people just need to update"
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

        // A current file is read as it is, and nothing is written.
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
