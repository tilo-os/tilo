//! Frontend preparation assertions execute in the separately linked UI fixture.
//! This side authenticates the resulting encrypted setup again before use.
use crate::setup::{
    flatpak_transfer::InstalledRef,
    migration_intent::MigrationIntent,
    prepared_setup::{Manifest, PreparedSetupReview},
    qualification_registration::AdmittedMain,
    staging,
};
use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

pub(super) struct SelectedGrant {
    package: PathBuf,
    manifest: Manifest,
    intent: MigrationIntent,
}
impl SelectedGrant {
    pub(super) fn capture(
        main: &Arc<AdmittedMain>,
        home: &Path,
        directory: &Path,
        selected: Vec<InstalledRef>,
        installation: &staging::plan::Plan,
    ) -> io::Result<Self> {
        main.with_original(|| {
            let response = crate::setup::qualification_frontend::call(serde_json::json!({
                "operation": "selected-grant", "home": home, "directory": directory,
                "selected": selected, "installation": installation,
                "registration_main": crate::setup::qualification_registration::current()?
                    .ok_or(io::ErrorKind::PermissionDenied)?,
            }));
            Ok(Self {
                package: serde_json::from_value(response["package"].clone())
                    .map_err(io::Error::other)?,
                manifest: serde_json::from_value(response["manifest"].clone())
                    .map_err(io::Error::other)?,
                intent: MigrationIntent::load(&directory.join("grant"))?,
            })
        })
    }
    pub(super) fn package(&self) -> io::Result<std::fs::File> {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&self.package)
    }
    pub(super) fn review(&self, quarantine: &Path) -> io::Result<PreparedSetupReview> {
        let review = PreparedSetupReview::open(
            &self.package,
            super::PASSWORD,
            quarantine,
            &AtomicBool::new(false),
        )?;
        let plan = self.intent.for_media()?;
        if review.manifest() != &self.manifest
            || review
                .manifest()
                .retained_home
                .as_ref()
                .is_none_or(|p| p.as_plan() != &plan)
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(review)
    }
}
