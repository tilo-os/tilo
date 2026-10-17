//! The remembered choices, one root-owned file per person.
//!
//! They live with the service rather than in the person's settings because a
//! grant stands in for consent: nothing the person's own processes can write
//! may create one. Only the shell's prompt does, through the service.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::policy::Grant;

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    grants: Vec<Grant>,
}

/// Where every person's grants are kept.
#[derive(Clone, Debug)]
pub struct GrantStore {
    dir: PathBuf,
}

impl GrantStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self, uid: u32) -> PathBuf {
        self.dir.join(format!("{uid}.json"))
    }

    /// The grants `uid` has remembered.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the file exists and cannot be read or parsed.
    pub fn load(&self, uid: u32) -> io::Result<Vec<Grant>> {
        match fs::read(self.path(uid)) {
            Ok(bytes) => serde_json::from_slice::<File>(&bytes)
                .map(|file| file.grants)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    /// Replace `uid`'s grants, atomically.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the file cannot be written.
    pub fn save(&self, uid: u32, grants: &[Grant]) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let path = self.path(uid);
        if grants.is_empty() {
            return match fs::remove_file(&path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            };
        }
        let bytes = serde_json::to_vec_pretty(&File {
            grants: grants.to_vec(),
        })
        .map_err(io::Error::other)?;
        let staged = self.dir.join(format!(".{uid}.json.new"));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&staged)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&staged, &path)?;
        sync_dir(&self.dir)
    }
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use crate::policy::Binding;
    use crate::protocol::Mode;

    #[test]
    fn grants_round_trip_per_person_and_vanish_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = GrantStore::new(dir.path().join("grants"));
        assert!(store.load(1000).unwrap().is_empty());
        let grant = Grant {
            id: "g".into(),
            label: "Windows".into(),
            mode: Mode::Open,
            binding: Binding::External {
                identity: Identity {
                    volume: Some(1),
                    ..Identity::default()
                },
            },
        };
        store.save(1000, std::slice::from_ref(&grant)).unwrap();
        assert_eq!(store.load(1000).unwrap(), vec![grant]);
        assert!(
            store.load(1001).unwrap().is_empty(),
            "another person's are separate"
        );
        store.save(1000, &[]).unwrap();
        assert!(!dir.path().join("grants/1000.json").exists());
    }
}
