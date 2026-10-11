//! Retain the signed declarations of the exact package this host launches.
use super::Apps;
use tilo_action_broker::services::ApplicationCreationEvidence;
use tilo_installed_apps::InstalledApp;

pub(super) struct OriginalLaunch {
    pub(super) package_hash: String,
    pub(super) creation: Option<ApplicationCreationEvidence>,
}

impl Apps {
    pub(super) fn original_launch(&self, app: &InstalledApp) -> Result<OriginalLaunch, String> {
        let refuse = |why| {
            format!(
                "refusing to launch {}: original signed package: {why}",
                app.package_id
            )
        };
        let installed = self
            .catalog
            .as_ref()
            .ok_or_else(|| refuse("no app store".to_owned()))?
            .list()
            .map_err(|error| refuse(error.to_string()))?
            .into_iter()
            .find(|package| {
                let matches_identity = package.id == app.package_id;
                matches_identity
                    && package.manifest_path == app.manifest_path
                    && package.payload_root == app.payload_root
            })
            .ok_or_else(|| refuse("installation changed".to_owned()))?;
        let record = package_store::integrity::read_record(
            &package_store::integrity::record_dir(&installed),
            &installed.id,
            &installed.package_id_hex,
        )
        .map_err(|error| refuse(error.to_string()))?;
        if std::fs::read(&app.manifest_path).map_err(|error| refuse(error.to_string()))?
            != record.manifest_bytes
        {
            return Err(refuse("manifest changed".to_owned()));
        }
        let source = std::str::from_utf8(&record.manifest_bytes)
            .map_err(|error| refuse(error.to_string()))?;
        let manifest = tilo_app_package::manifest::Manifest::from_toml_str(source)
            .map_err(|error| refuse(error.to_string()))?;
        if manifest.package.id != app.package_id {
            return Err(refuse("package identity changed".to_owned()));
        }
        let package_hash = installed.package_id_hex;
        let creation =
            (!manifest.resources.kinds.is_empty()).then(|| ApplicationCreationEvidence {
                package_hash: package_hash.clone(),
                declared_kinds: manifest.resources.kinds,
            });
        if creation.as_ref().is_some_and(|evidence| !evidence.valid()) {
            return Err(refuse("invalid resource declarations".to_owned()));
        }
        Ok(OriginalLaunch {
            package_hash,
            creation,
        })
    }
}
