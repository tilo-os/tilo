//! Offline, signed system applications carried separately from the source home.
//! Mutable user app data remains in the Home package; restoration never launches apps.
use super::{Installation, InstalledRef, Remote, Runner};
use crate::setup::{
    migration::{PinnedDirectory, changed},
    migration_review::SourceInventory,
};
use serde::{Deserialize, Serialize};
#[cfg(feature = "capture-worker")]
use sha2::Digest;
use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};
pub(super) mod dependencies;
pub(super) mod export;
mod extra_data;
pub use export::{ReviewedSubset, SourceExportGrant, SubsetExportGrant};
mod home_phase;
mod native;
#[cfg(test)]
pub mod native_tests;
mod restore;
mod scoped;
mod selected_state;
mod stage;
mod state_grant;
pub use selected_state::AppState;
pub use state_grant::ApplicationStateGrant;
#[cfg(any(test, feature = "test-support"))]
pub mod tests;
pub use native::{CancellableOfflineRunner, NativeOfflineRunner};
pub use restore::ApplicationImport;
pub use stage::{Expectation, StageGrant, StagedApplications};

/// Reuse the export planner's installed dependency closure for capacity review.
/// These descriptive refs grant neither export nor source access.
pub fn review_selected_dependencies(
    runner: &impl Runner,
    home: &Path,
    inventory: &super::Inventory,
    selected: &[InstalledRef],
) -> Result<Vec<InstalledRef>, String> {
    if selected
        .iter()
        .any(|item| item.app_id().is_none() || !inventory.refs.contains(item))
    {
        return Err("Selected application source changed before dependency review".into());
    }
    let scopes: BTreeSet<_> = selected
        .iter()
        .map(|item| item.installation.clone())
        .collect();
    let mut required = Vec::new();
    for scope in scopes {
        let closure = dependencies::review(runner, home, &scope, &inventory.refs, selected)?;
        for reference in closure {
            let matches: Vec<_> = inventory
                .refs
                .iter()
                .filter(|item| item.installation == scope && item.reference == reference)
                .collect();
            let [item] = matches.as_slice() else {
                return Err("Required deployment has no unique reviewed installation".into());
            };
            required.push((**item).clone());
            if required.len() > 100_000 {
                return Err("Application dependency closure exceeds bounds".into());
            }
        }
    }
    Ok(required)
}

/// The OSTree boundary is distinct from Flatpak and always receives a held,
/// re-attested repository grant. Commands contain no shell interpolation.
pub trait OfflineRunner: Runner {
    /// Export an independently reviewed subset without changing its source.
    /// # Errors
    /// Refuses source changes, missing native review or invalid destination grants.
    fn export_offline_subset(&self, _grant: &SubsetExportGrant<'_>) -> Result<bool, String> {
        Ok(false)
    }
    /// # Errors
    /// Refuses unavailable OSTree, invalid repositories or failed verification.
    fn ostree(&self, repository: &Path, arguments: &[String]) -> Result<String, String>;
    /// Verify retained raw extra-data against exact signed commit metadata.
    /// # Errors
    /// Refuses missing, malformed or changed original download blobs.
    fn verify_extra_data(&self, _repository: &Path, _commit: &str) -> Result<u64, String> {
        Ok(0)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deployment {
    pub installed: InstalledRef,
    pub commit: String,
    pub repository: String,
    pub remote: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trust {
    pub remote: String,
    pub definition: String,
    pub sha256: [u8; 32],
    pub source: Remote,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationsManifest {
    pub schema: u32,
    pub selected: Vec<InstalledRef>,
    pub deployments: Vec<Deployment>,
    pub trust: Vec<Trust>,
    pub user_state: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub app_state: Vec<AppState>,
}
impl ApplicationsManifest {
    /// # Errors
    /// Refuses malformed refs, ambiguous installations or undeclared trust.
    pub fn validate(&self) -> io::Result<()> {
        if !matches!(self.schema, 1 | 2)
            || (self.selected.is_empty() && !self.user_state)
            || self.deployments.len() > 100_000
            || self.trust.len() > 4096
        {
            return Err(changed("Invalid portable application bounds"));
        }
        if (self.schema == 1 && !self.app_state.is_empty()) || (self.schema == 2 && self.user_state)
        {
            return Err(changed(
                "Application state schema differs from declared intent",
            ));
        }
        if self.schema == 2 {
            AppState::validate_selected(&self.app_state, &self.selected)?;
        }
        let mut refs = BTreeSet::new();
        let mut remotes = BTreeSet::new();
        for trust in &self.trust {
            if !super::token(&trust.remote)
                || !leaf(&trust.definition)
                || !remotes.insert(&trust.remote)
            {
                return Err(changed("Invalid portable application signing authority"));
            }
        }
        for item in &self.deployments {
            if !super::reference(&item.installed.reference)
                || !commit(&item.commit)
                || (self.schema == 1
                    && !matches!(item.installed.installation, Installation::System(_)))
                || !leaf(&item.repository)
                || !refs.insert(&item.installed.reference)
                || !remotes.contains(&item.remote)
                || !self.trust.iter().any(|trust| {
                    trust.remote == item.remote
                        && trust.source.installation == item.installed.installation
                        && trust.source.name == item.installed.origin
                })
            {
                return Err(changed(
                    "Invalid or ambiguous portable application deployment",
                ));
            }
        }
        let mut selected = BTreeSet::new();
        for item in &self.selected {
            if !selected.insert(&item.reference)
                || !self
                    .deployments
                    .iter()
                    .any(|deployment| &deployment.installed == item)
            {
                return Err(changed(
                    "Selected applications differ from the offline package",
                ));
            }
        }
        if self.deployments.iter().any(|item| {
            item.installed.app_id().is_some() && !self.selected.contains(&item.installed)
        }) {
            return Err(changed(
                "Offline package contains an unselected application",
            ));
        }
        Ok(())
    }
}
fn commit(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn leaf(value: &str) -> bool {
    !value.is_empty()
        && value.len() < 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
        && !value.starts_with('.')
}
struct OriginalInstallation {
    scope: Installation,
    path: PathBuf,
    pin: Arc<PinnedDirectory>,
    inventory: SourceInventory,
    deployments: Vec<(InstalledRef, String)>,
    remotes: Vec<(Remote, Vec<u8>)>,
    required: BTreeSet<String>,
    native_export: Option<ReviewedSubset>,
}
impl OriginalInstallation {
    fn validate_pending_authority(
        &self,
        inventory: &super::HandoffInventory,
        operation: &str,
    ) -> io::Result<()> {
        self.pin.validate()?;
        inventory
            .validate_operation(operation)
            .map_err(io::Error::other)?;
        inventory
            .validate_pending_source(&self.scope, &self.path)
            .map_err(io::Error::other)?;
        for selected in inventory
            .selected()
            .iter()
            .filter(|item| item.installation == self.scope)
        {
            let (remote, definition) = self
                .remotes
                .iter()
                .find(|(remote, _)| {
                    remote.name == selected.origin && remote.installation == self.scope
                })
                .ok_or_else(|| {
                    io::Error::other("Selected pending app has no retained signing authority")
                })?;
            if super::extract_remote_definition(remote, &self.path).map_err(io::Error::other)?
                != *definition
            {
                return Err(io::Error::other(
                    "Pending selected app signing authority changed",
                ));
            }
        }
        self.pin.validate()
    }
}
/// Explicit source consent retains original installations and signing metadata.
/// No root path is accepted from an unauthenticated portable manifest.
#[cfg(any(
    feature = "capture-worker",
    all(test, feature = "native-capture-qualification")
))]
pub struct StorageRequirement {
    pub staging_bytes: u64,
    pub sealed_bytes: u64,
    #[cfg(feature = "capture-worker")]
    pub limits: crate::setup::portable_transfer::Limits,
}
pub struct PortableFlatpakSource {
    home: PathBuf,
    home_pin: Arc<PinnedDirectory>,
    selected: Vec<InstalledRef>,
    originals: Vec<OriginalInstallation>,
    staging_estimate: u64,
    selected_state: Option<selected_state::SelectedState>,
}
impl PortableFlatpakSource {
    /// Conservative full-original-repo bound, not a claim of minimal subset size.
    #[cfg(any(
        feature = "capture-worker",
        all(test, feature = "native-capture-qualification")
    ))]
    pub fn storage_requirement(&self, include_user_state: bool) -> io::Result<StorageRequirement> {
        self.validate()?;
        if include_user_state && self.selected_state.is_none() {
            return Err(changed(
                "Original whole-home worker must not duplicate user state",
            ));
        }
        if include_user_state != self.selected_state.is_some() {
            return Err(changed(
                "Application data consent differs from reviewed source",
            ));
        }
        let mut entries = 32usize
            .checked_add(
                self.selected_state
                    .as_ref()
                    .map_or(Ok(0), |state| state.entry_bound())?,
            )
            .ok_or_else(|| changed("App storage entry bound overflows"))?;
        let mut definitions = 0u64;
        for original in &self.originals {
            entries = entries
                .checked_add(original.inventory.entry_count())
                .and_then(|value| value.checked_add(original.deployments.len().checked_mul(16)?))
                .and_then(|value| value.checked_add(original.remotes.len().checked_mul(4)?))
                .ok_or_else(|| changed("App storage entry bound overflows"))?;
            for (_, definition) in &original.remotes {
                definitions = definitions
                    .checked_add(
                        u64::try_from(definition.len())
                            .map_err(io::Error::other)?
                            .checked_mul(3)
                            .ok_or_else(|| changed("Signing metadata bound overflows"))?,
                    )
                    .ok_or_else(|| changed("Signing metadata bound overflows"))?;
            }
        }
        if entries > 1_000_000 {
            return Err(changed("App staging exceeds supported entry bound"));
        }
        let metadata_bytes = 512 * 1024 * 1024usize;
        let plaintext_bytes = self
            .staging_estimate
            .checked_add(
                self.selected_state
                    .as_ref()
                    .map_or(0, |state| state.bytes()),
            )
            .and_then(|value| value.checked_add(definitions))
            .and_then(|value| value.checked_add(u64::try_from(entries).ok()?.checked_mul(8192)?))
            .and_then(|value| value.checked_add(metadata_bytes as u64))
            .ok_or_else(|| changed("App storage bound overflows"))?;
        let limits = crate::setup::portable_transfer::Limits {
            plaintext_bytes,
            entries,
            metadata_bytes,
        };
        let sealed_bytes = limits
            .sealed_bound()
            .ok_or_else(|| changed("Sealed application bound overflows"))?;
        self.validate()?;
        // Stage + independent authenticated extraction + untrusted signature
        // verification may coexist. Never assume local-pull hardlinks.
        let staging_bytes = plaintext_bytes
            .checked_mul(3)
            .ok_or_else(|| changed("Independent application verification storage overflows"))?;
        Ok(StorageRequirement {
            staging_bytes,
            sealed_bytes,
            #[cfg(feature = "capture-worker")]
            limits,
        })
    }
    /// Authentication and exact protected source selection proof, no target install.
    #[cfg(feature = "capture-worker")]
    pub fn verify_sealed(
        &self,
        expected: &Expectation,
        package: &Path,
        password: &[u8],
        quarantine: &Path,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<()> {
        crate::setup::migration::check_cancel(cancel)?;
        self.validate()?;
        let mut imported =
            ApplicationImport::review_expected(expected, package, password, quarantine, cancel)?;
        self.verify_original_manifest(&imported, cancel)?;
        let scratch = quarantine
            .parent()
            .ok_or_else(|| changed("Application verification requires private parent"))?;
        for original in &self.originals {
            let deployment = imported
                .manifest
                .deployments
                .iter()
                .find(|item| item.installed.installation == original.scope)
                .ok_or_else(|| changed("Original installation omitted"))?;
            if imported.manifest.deployments.iter().any(|item| {
                item.installed.installation == original.scope
                    && item.repository != deployment.repository
            }) {
                return Err(changed(
                    "Original installation was split across carrier repositories",
                ));
            }
            let reviewed = original
                .native_export
                .as_ref()
                .ok_or_else(|| changed("Original native export missing"))?;
            export::verify_carrier(
                original,
                &imported.root.join(&deployment.repository),
                scratch,
                reviewed,
                Some(cancel),
            )
            .map_err(io::Error::other)?;
        }
        imported
            .review
            .verify_existing_tree(&imported.root, cancel)?;
        self.validate()?;
        crate::setup::migration::check_cancel(cancel)
    }

    #[cfg(feature = "capture-worker")]
    fn verify_original_manifest(
        &self,
        imported: &ApplicationImport,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<()> {
        if imported.manifest.user_state
            || imported.manifest.selected != self.selected
            || imported.manifest.trust.iter().any(|trust| {
                !imported
                    .manifest
                    .deployments
                    .iter()
                    .any(|deployment| deployment.remote == trust.remote)
            })
        {
            return Err(changed(
                "Original selected application scope differs from carrier",
            ));
        }
        let expected_deployments: Vec<_> = self
            .originals
            .iter()
            .flat_map(|original| original.deployments.iter())
            .collect();
        if expected_deployments.len() != imported.manifest.deployments.len() {
            return Err(changed("Original dependency closure differs from carrier"));
        }
        for item in &imported.manifest.deployments {
            crate::setup::migration::check_cancel(cancel)?;
            let original = self
                .originals
                .iter()
                .find(|original| original.scope == item.installed.installation)
                .ok_or_else(|| changed("Unselected original installation in carrier"))?;
            if !original
                .deployments
                .iter()
                .any(|(selected, commit)| selected == &item.installed && commit == &item.commit)
            {
                return Err(changed("Original exact commit differs from carrier"));
            }
            let trust = imported
                .manifest
                .trust
                .iter()
                .find(|trust| trust.remote == item.remote)
                .ok_or_else(|| changed("Missing carrier signing authority"))?;
            let (remote, definition) = original
                .remotes
                .iter()
                .find(|(remote, _)| remote.name == item.installed.origin)
                .ok_or_else(|| changed("Original approved remote missing"))?;
            if &trust.source != remote
                || <[u8; 32]>::from(sha2::Sha256::digest(definition)) != trust.sha256
            {
                return Err(changed(
                    "Carrier signing authority differs from protected source",
                ));
            }
            extra_data::verify(
                &imported.root.join(&item.repository).join(".ostree/repo"),
                &item.commit,
                Some(cancel),
            )?;
        }
        Ok(())
    }
    /// Original authority for pending export only, never final content proof.
    pub fn validate_pending_offline_scope(
        &self,
        plan: &crate::setup::migration_handoff::Plan,
    ) -> io::Result<()> {
        self.home_pin.validate()?;
        if !plan.categories.home
            || !plan.categories.flatpak
            || plan.home_policy != crate::setup::migration_handoff::HomePolicy::Copy
        {
            return Err(io::Error::other(
                "Application scope is not pending whole-home consent",
            ));
        }
        let inventory = plan
            .flatpak_inventory
            .as_ref()
            .ok_or_else(|| io::Error::other("Pending apps lack exact reviewed inventory"))?;
        let inventory = super::HandoffInventory::parse(&serde_json::to_vec(inventory)?)
            .map_err(io::Error::other)?;
        inventory
            .validate_operation(&plan.operation_id)
            .map_err(io::Error::other)?;
        if inventory.selected().len() != self.selected.len()
            || self
                .selected
                .iter()
                .any(|item| !inventory.selected().contains(item))
        {
            return Err(io::Error::other(
                "Pending app selection differs from reviewed inventory",
            ));
        }
        let current = crate::setup::migration_handoff::capture(
            &self.home,
            &plan.operation_id,
            plan.categories,
        )?;
        if current.home != plan.home {
            return Err(io::Error::other(
                "Application scope differs from original home",
            ));
        }
        for original in &self.originals {
            original.validate_pending_authority(&inventory, &plan.operation_id)?;
        }
        self.home_pin.validate()
    }
    /// # Errors
    /// Refuses user refs, source changes, unsigned remotes and ambiguous app origins.
    pub fn review(
        runner: &impl Runner,
        home: &Path,
        selected: Vec<InstalledRef>,
    ) -> Result<Arc<Self>, String> {
        Self::review_selection(runner, home, selected, false)
    }
    /// Review exactly selected user or system refs and their app data after explicit
    /// app inclusion consent. Whole-Home callers must use the legacy no-state review.
    /// # Errors
    /// Refuses missing selections, changed source data or signing authority.
    pub fn review_selected_state(
        runner: &impl Runner,
        home: &Path,
        selected: Vec<InstalledRef>,
    ) -> Result<Arc<Self>, String> {
        Self::review_selection(runner, home, selected, true)
    }
    fn review_selection(
        runner: &impl Runner,
        home: &Path,
        selected: Vec<InstalledRef>,
        include_state: bool,
    ) -> Result<Arc<Self>, String> {
        let home_pin = PinnedDirectory::open_absolute(home).map_err(|e| e.to_string())?;
        let inventory = super::discover(runner, home)?;
        let mut selected_refs = BTreeSet::new();
        for item in &selected {
            if !inventory.refs.contains(item)
                || !super::reference(&item.reference)
                || !selected_refs.insert(&item.reference)
            {
                return Err("Selected portable apps are missing or ambiguous".into());
            }
        }
        if !include_state
            && selected
                .iter()
                .any(|item| item.installation == Installation::User)
        {
            return Err("Portable system apps require an explicit system selection".into());
        }
        let roots = super::installation_roots(runner, home, &inventory)?;
        let scopes: BTreeSet<_> = selected
            .iter()
            .map(|item| item.installation.clone())
            .collect();
        let mut originals = Vec::new();
        let mut staging_estimate = 16 * 1024 * 1024u64;
        for scope in scopes {
            let path = roots
                .get(&scope)
                .ok_or("Original installation is unavailable")?
                .clone();
            let (mut original, size) =
                capture_installation(runner, home, &inventory, scope, &path, &selected)?;
            original.native_export = runner.review_offline_subset(&SourceExportGrant {
                original: &original,
            })?;
            let extra = original
                .native_export
                .as_ref()
                .map_or(0, ReviewedSubset::extra_storage);
            staging_estimate = staging_estimate
                .checked_add(extra)
                .and_then(|value| value.checked_add(size))
                .ok_or("Staging size overflows")?;
            originals.push(original);
        }
        let selected_state = if include_state {
            Some(
                selected_state::SelectedState::capture(home, &selected)
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        let review = Arc::new(Self {
            home: home.into(),
            home_pin,
            selected,
            originals,
            staging_estimate,
            selected_state,
        });
        review.validate().map_err(|e| e.to_string())?;
        Ok(review)
    }
    /// Closed reviewed dependencies come from the protected original intent;
    /// boot never rediscovers a different runtime, extension or latest commit.
    #[cfg(any(feature = "capture-worker", test))]
    pub(in crate::setup::flatpak_transfer) fn review_closed_original(
        runner: &super::SourceScopedRunner<NativeOfflineRunner>,
        inventory: &super::HandoffInventory,
        home: &Path,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Arc<Self>, String> {
        Self::review_closed_material(runner, inventory, home, false, cancel)
    }
    #[cfg(any(feature = "capture-worker", test))]
    pub(in crate::setup::flatpak_transfer) fn review_closed_original_selected(
        runner: &super::SourceScopedRunner<NativeOfflineRunner>,
        inventory: &super::HandoffInventory,
        home: &Path,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Arc<Self>, String> {
        Self::review_closed_material(runner, inventory, home, true, cancel)
    }
    #[cfg(any(feature = "capture-worker", test))]
    fn review_closed_material(
        runner: &super::SourceScopedRunner<NativeOfflineRunner>,
        inventory: &super::HandoffInventory,
        home: &Path,
        include_state: bool,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Arc<Self>, String> {
        crate::setup::migration::check_cancel(cancel).map_err(|error| error.to_string())?;
        let _verified = runner.original_files(inventory)?;
        let home_pin = PinnedDirectory::open_absolute(home).map_err(|error| error.to_string())?;
        let selected: Vec<_> = inventory
            .selected()
            .iter()
            .filter(|item| item.app_id().is_some())
            .cloned()
            .collect();
        let scopes: BTreeSet<_> = inventory
            .selected()
            .iter()
            .map(|item| item.installation.clone())
            .collect();
        let mut originals = Vec::new();
        let mut staging_estimate = 16 * 1024 * 1024u64;
        for scope in scopes {
            crate::setup::migration::check_cancel(cancel).map_err(|error| error.to_string())?;
            let path = runner.closed_root(&scope)?.to_owned();
            let pin = PinnedDirectory::open_absolute(&path).map_err(|error| error.to_string())?;
            let captured =
                SourceInventory::capture_home(&pin).map_err(|error| error.to_string())?;
            let mut deployments = Vec::new();
            let mut required = BTreeSet::new();
            for item in inventory
                .selected()
                .iter()
                .filter(|item| item.installation == scope)
            {
                required.insert(item.reference.clone());
                deployments.push((item.clone(), runner.closed_commit(item)?.to_owned()));
            }
            let mut remotes = Vec::new();
            for remote in runner
                .closed_inventory()
                .remotes
                .iter()
                .filter(|remote| remote.installation == scope)
            {
                remotes.push((
                    remote.clone(),
                    super::extract_remote_definition(remote, &path)?,
                ));
            }
            let mut original = OriginalInstallation {
                scope,
                path,
                pin,
                inventory: captured,
                deployments,
                remotes,
                required,
                native_export: None,
            };
            original.native_export = Some(export::review(
                &SourceExportGrant {
                    original: &original,
                },
                Some(cancel),
            )?);
            if original.native_export.is_none() {
                return Err("Original offline subset exporter is unavailable".into());
            }
            let bytes =
                crate::setup::portable_transfer::stored_bytes(&original.pin, &original.inventory)
                    .map_err(|error| error.to_string())?;
            let extra = original
                .native_export
                .as_ref()
                .map_or(0, ReviewedSubset::extra_storage);
            staging_estimate = staging_estimate
                .checked_add(bytes)
                .and_then(|size| size.checked_add(extra))
                .ok_or("Original app staging reservation overflows")?;
            originals.push(original);
        }
        let selected_state = if include_state {
            let state = selected_state::SelectedState::capture(home, &selected)
                .map_err(|error| error.to_string())?;
            Some(state)
        } else {
            None
        };
        let review = Arc::new(Self {
            home: home.into(),
            home_pin,
            selected,
            originals,
            staging_estimate,
            selected_state,
        });
        review.validate().map_err(|error| error.to_string())?;
        Ok(review)
    }
    /// # Errors
    /// Refuses any changed original installation or deployment/signing metadata.
    pub fn validate(&self) -> io::Result<()> {
        self.home_pin.validate()?;
        if let Some(state) = &self.selected_state {
            state.validate()?;
        }
        for original in &self.originals {
            original.pin.validate()?;
            original.inventory.validate(&original.pin)?;
            for (remote, expected) in &original.remotes {
                if super::extract_remote_definition(remote, &original.path)
                    .map_err(io::Error::other)?
                    != *expected
                {
                    return Err(changed("Original application signing authority changed"));
                }
            }
        }
        Ok(())
    }
}

fn capture_installation(
    runner: &impl Runner,
    home: &Path,
    inventory: &super::Inventory,
    scope: Installation,
    path: &Path,
    selected: &[InstalledRef],
) -> Result<(OriginalInstallation, u64), String> {
    let mut staging_estimate = 0u64;
    let pin = PinnedDirectory::open_absolute(path).map_err(|e| e.to_string())?;
    let source = SourceInventory::capture_home(&pin).map_err(|e| e.to_string())?;
    let required = dependencies::review(runner, home, &scope, &inventory.refs, selected)?;
    let mut deployments = Vec::new();
    for item in inventory
        .refs
        .iter()
        .filter(|item| item.installation == scope)
    {
        let value = runner.run(
            home,
            &[
                "info".into(),
                scope.argument(),
                "--show-commit".into(),
                item.reference.clone(),
            ],
        )?;
        if !commit(value.trim()) {
            return Err("Invalid original deployment commit".into());
        }
        if item.app_id().is_none() || selected.contains(item) {
            let size = runner.run(
                home,
                &[
                    "info".into(),
                    scope.argument(),
                    "--show-size".into(),
                    item.reference.clone(),
                ],
            )?;
            let size = size
                .trim()
                .parse::<u64>()
                .map_err(|_| "Invalid source deployment size")?;
            staging_estimate = staging_estimate
                .checked_add(size.checked_mul(2).ok_or("Staging size overflows")?)
                .ok_or("Staging size overflows")?;
        }
        deployments.push((item.clone(), value.trim().into()));
    }
    let mut remotes = Vec::new();
    for remote in inventory
        .remotes
        .iter()
        .filter(|remote| remote.installation == scope)
    {
        // Unrelated unsigned remotes are not selected authority; refuse only if
        // the actual exported dependency closure needs such a remote.
        if let Ok(bytes) = super::extract_remote_definition(remote, path) {
            remotes.push((remote.clone(), bytes));
        }
    }
    Ok((
        OriginalInstallation {
            scope,
            path: path.into(),
            pin,
            inventory: source,
            deployments,
            remotes,
            required,
            native_export: None,
        },
        staging_estimate,
    ))
}
