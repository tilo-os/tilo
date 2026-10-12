//! Independently pinned native sidecar installation, without a package-store registry.
use super::CaptureAdmission;
use std::{fmt::Write as _, io};
impl CaptureAdmission {
    /// Bind the fixed native exporter to independently configured publisher trust.
    /// # Errors
    /// Refuses changed signed evidence, payload, service identity or runtime.
    pub fn with_installation(
        mut self,
        installed: package_store::InstalledPackage,
        publisher: &str,
    ) -> io::Result<Self> {
        self.check()?;
        if installed.id != "so.tilo.imager.home-export" {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let directories = [
            installed.payload_root.clone(),
            installed
                .manifest_path
                .parent()
                .ok_or(io::ErrorKind::InvalidInput)?
                .into(),
            package_store::integrity::record_dir(&installed),
        ]
        .iter()
        .map(|path| super::inventory_scratch::ReadRoot::pin(path))
        .collect::<io::Result<Vec<_>>>()?;
        let manifest = crate::run::verified_launch_installation(
            &installed,
            &installed.manifest_path,
            &installed.payload_root,
            &installed.package_id_hex,
        )
        .map_err(io::Error::other)?;
        let record = package_store::integrity::read_record(
            &package_store::integrity::record_dir(&installed),
            &installed.id,
            &installed.package_id_hex,
        )
        .map_err(io::Error::other)?;
        let mut signed_publisher = String::with_capacity(64);
        for byte in record.root.publisher_identity {
            write!(&mut signed_publisher, "{byte:02x}").map_err(io::Error::other)?;
        }
        if signed_publisher != publisher
            || installed.publisher_key_hex != publisher
            || manifest.kind != "service"
            || manifest.runtime.as_deref() != Some(self.runtime_name.as_str())
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        for root in &directories {
            root.check()?;
            if let Some(scratch) = &self.inventory_scratch {
                root.refuse_scratch(scratch)?;
            }
        }
        self.installation_directories = directories;
        self.installation = Some(installed);
        self.check()?;
        Ok(self)
    }
    pub(in crate::owned_service) fn original_manifest(
        &self,
        manifest: &std::path::Path,
        payload: &std::path::Path,
        expected: &str,
    ) -> Option<Result<crate::manifest::ManifestView, crate::run::RunError>> {
        self.installation.as_ref().map(|installed| {
            crate::run::verified_launch_installation(installed, manifest, payload, expected)
        })
    }
}
