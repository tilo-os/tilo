//! A source capability minted only after authentication of an application archive.
use super::ApplicationImport;
use crate::setup::{
    migration::{PinnedDirectory, changed},
    migration_review::SourceInventory,
};
use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};
pub struct ApplicationStateGrant {
    home: PathBuf,
    source: PathBuf,
    home_pin: Arc<PinnedDirectory>,
    source_pin: Arc<PinnedDirectory>,
    inventory: SourceInventory,
}
impl ApplicationStateGrant {
    pub fn permits(&self, source: &Path, home: &Path) -> Result<bool, String> {
        self.home_pin
            .validate_destination()
            .map_err(|e| e.to_string())?;
        self.source_pin
            .validate_destination()
            .map_err(|e| e.to_string())?;
        self.inventory
            .validate(&self.source_pin)
            .map_err(|e| e.to_string())?;
        Ok(source == self.source && home == self.home)
    }
}
impl ApplicationImport {
    pub(super) fn state_grant(
        &mut self,
        home: &Path,
        workspace: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<ApplicationStateGrant> {
        if workspace
            != home
                .join(".local/state/tilo/portable-applications")
                .join(&self.expectation.operation)
            || self.root != workspace.join("imported/home")
        {
            return Err(changed(
                "Application source is not the authenticated protected operation",
            ));
        }
        self.review.verify_existing_tree(&self.root, cancel)?;
        let home_pin = PinnedDirectory::open_absolute(home)?;
        let source = self.root.join("user-state");
        let source_pin = PinnedDirectory::open_absolute(&source)?;
        for pin in [&home_pin, &source_pin] {
            pin.validate_destination()?;
            crate::setup::destination_security::check_private_file(&pin.file)?;
        }
        let inventory = SourceInventory::capture_home(&source_pin)?;
        Ok(ApplicationStateGrant {
            home: home.into(),
            source,
            home_pin,
            source_pin,
            inventory,
        })
    }
}
