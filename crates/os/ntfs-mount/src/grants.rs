//! The remembered choices, one root-owned file per person.

use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::policy::Grant;

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    grants: Vec<Grant>,
}

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
        tilo_files::native::atomic::write_with_mode(&path, &bytes, 0o600)
    }
}
