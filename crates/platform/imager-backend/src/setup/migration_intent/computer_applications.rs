//! Two separately captured grants. Neither serialized metadata nor a narrowed
//! clone of Files consent can manufacture the selected application reader.
use super::*;
use crate::setup::migration_handoff::HomePolicy;
impl MigrationIntent {
    pub fn capture_computer_applications(
        source: &Path,
        directory: &Path,
        categories: [bool; 3],
        selected: Vec<InstalledRef>,
    ) -> io::Result<Self> {
        Self::capture_computer_applications_selected(source, directory, categories, selected, &[])
    }
    /// Capture shared Files and selected applications with nested exclusions.
    ///
    /// # Errors
    /// Refuses missing consent, unsafe grants and malformed exclusions.
    pub fn capture_computer_applications_selected(
        source: &Path,
        directory: &Path,
        categories: [bool; 3],
        selected: Vec<InstalledRef>,
        exclusions: &[NativePath],
    ) -> io::Result<Self> {
        if !categories[0] || !categories[1] || selected.is_empty() {
            return Err(error(
                "Computer composite requires shared Files and selected applications",
            ));
        }
        let mut main = Self::capture_source_selection(
            source,
            directory,
            [true, false, categories[2]],
            Vec::new(),
            HomePolicy::AccessReadWrite,
            exclusions,
        )?;
        main.source_pin.validate()?;
        let child = main
            .directory_pin
            .create_child(std::ffi::OsStr::new("applications"))?;
        child.validate_destination()?;
        migration_durability::sync_directory(&main.directory_pin.file)?;
        let path = directory.join("applications");
        let apps = Self::capture_with_policy(
            source,
            &path,
            [false, true, false],
            selected,
            HomePolicy::Copy,
        )?;
        main.computer_applications = Some(Arc::new(apps));
        main.validate_computer_applications()?;
        main.grant.computer_applications = Some(NativePath::encode(&path));
        let bytes = serde_json::to_vec(&main.grant)?;
        if bytes.len() as u64 > MAX {
            return Err(error("Computer composite intent is too large"));
        }
        let mut temporary =
            OwnedTemporary::create(&main.directory_pin, std::ffi::OsStr::new(FILE))?;
        temporary.file.write_all(&bytes)?;
        migration_durability::sync_file(&temporary.file)?;
        temporary.validate()?;
        cap_primitives::fs::rename(
            &main.directory_pin.file,
            Path::new(&temporary.name),
            &main.directory_pin.file,
            Path::new(FILE),
        )?;
        migration_durability::sync_directory(&main.directory_pin.file)?;
        main.validate_computer_applications()?;
        Ok(main)
    }
    pub(super) fn load_computer_applications(mut self) -> io::Result<Self> {
        if let Some(path) = &self.grant.computer_applications {
            let path = path.decode().map_err(io::Error::other)?;
            if path != self.directory.join("applications") {
                return Err(error(
                    "Computer application grant is outside its private intent",
                ));
            }
            let apps = Self::load_single(&path)?;
            // Exactly one child, never recursive metadata discovery.
            if apps.grant.computer_applications.is_some() {
                return Err(error("Nested Computer application grant refused"));
            }
            self.computer_applications = Some(Arc::new(apps));
            self.validate_computer_applications()?;
        }
        Ok(self)
    }
    pub fn computer_applications(&self) -> io::Result<Option<&MigrationIntent>> {
        self.validate_computer_applications()?;
        Ok(self.computer_applications.as_deref())
    }
    pub(super) fn validate_computer_applications(&self) -> io::Result<()> {
        let Some(apps) = &self.computer_applications else {
            if self.grant.computer_applications.is_some() {
                return Err(error("Computer applications custody is missing"));
            }
            return Ok(());
        };
        self.directory_pin.validate_destination()?;
        self.source_pin.validate()?;
        apps.directory_pin.validate_destination()?;
        apps.source_pin.validate()?;
        let main = &self.grant.plan;
        let copy = &apps.grant.plan;
        if !main.categories.home
            || main.categories.flatpak
            || main.home_policy != HomePolicy::AccessReadWrite
            || copy.categories.home
            || !copy.categories.flatpak
            || copy.categories.accounts
            || copy.home_policy != HomePolicy::Copy
            || main.home != copy.home
            || self.grant.source != apps.grant.source
            || !crate::setup::nofollow::same_directory_identity(
                &self.source_pin.identity,
                &apps.source_pin.identity,
            )
            || main.operation_id == copy.operation_id
            || apps.computer_applications.is_some()
            || apps.grant.computer_applications.is_some()
        {
            return Err(error("Computer Files and application grants differ"));
        }
        apps.renew_application_budget()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn serialized_child_cannot_escape_private_grant_or_discover_recursive_authority() {
        let (_root, mut main, _) = super::super::tests::fixture();
        let other = tempfile::tempdir().unwrap();
        main.grant.computer_applications = Some(NativePath::encode(other.path()));
        let error = main.load_computer_applications().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(std::fs::read_dir(other.path()).unwrap().next().is_none());
    }
    #[test]
    fn metadata_without_real_secondary_custody_never_authorizes_applications() {
        let (_root, mut main, _) = super::super::tests::fixture();
        main.grant.computer_applications =
            Some(NativePath::encode(&main.directory.join("applications")));
        assert!(main.computer_applications().is_err());
        assert!(main.for_media().is_err());
        assert_eq!(
            std::fs::read(main.grant.source.decode().unwrap().join("original")).unwrap(),
            b"preserve"
        );
    }
}
