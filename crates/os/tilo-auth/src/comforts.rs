//! The device's sign-in comforts in device storage, which the login screens
//! start with and the `tilo-comforts` token mirrors for the boot.

use anyhow::{Context as _, Result};
use settings_store::sign_in_comforts::SignInComforts;
use std::path::PathBuf;

pub const PATH: &str = "/var/lib/tilo/auth/comforts.json";

pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The comforts kept, clamped; `None` before any were chosen.
    pub fn load(&self) -> Result<Option<SignInComforts>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => Ok(Some(SignInComforts::from_value(
                &serde_json::from_slice(&bytes)
                    .with_context(|| format!("parsing {}", self.path.display()))?,
            ))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", self.path.display())),
        }
    }

    pub fn save(&self, comforts: &SignInComforts) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(comforts).context("encoding the sign-in comforts")?;
        tilo_files::native::atomic::write_private(&self.path, &bytes)
            .with_context(|| format!("writing {}", self.path.display()))
    }
}
