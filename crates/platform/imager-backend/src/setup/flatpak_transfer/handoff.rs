//! Metadata-only reviewed Flatpak intent. Original public trust is re-read from
//! explicitly resolved, read-only installation grants after reboot.
use super::*;
use crate::setup::migration::PinnedDirectory;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::{io, path::Path, sync::Arc};
#[cfg(test)]
mod tests;

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_INSTALLATIONS: usize = 64;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffInventory {
    schema: u32,
    inventory: Inventory,
    selected: Vec<InstalledRef>,
    installations: Vec<InstallationSource>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallationSource {
    installation: Installation,
    source_plan: Vec<u8>,
    repositories: Vec<RepositoryTrust>,
    deployments: Vec<Deployment>,
    config_sha256: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepositoryTrust {
    remote: Remote,
    definition_sha256: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Deployment {
    reference: String,
    commit: String,
    deploy_sha256: [u8; 32],
}

impl HandoffInventory {
    /// Re-attest original deployment and signing metadata before staging, then
    /// bind every stable root to the actual installation operation.
    pub fn bind_staging(
        &self,
        home: &Path,
        original: &str,
        operation: &str,
        installation: &crate::setup::staging::plan::Plan,
    ) -> Result<Self, String> {
        let fresh = Self::capture_material(
            &NativeRunner,
            home,
            self.selected.clone(),
            self.schema == 1,
            |root| {
                crate::setup::migration_handoff::capture(
                    root,
                    original,
                    crate::setup::migration_handoff::Categories {
                        home: false,
                        flatpak: true,
                        accounts: false,
                    },
                )
                .and_then(|plan| plan.bytes())
                .map_err(|e| e.to_string())
            },
        )?;
        self.rebind_attested(fresh, original, operation, installation)
    }
    fn rebind_attested(
        &self,
        fresh: Self,
        original: &str,
        operation: &str,
        installation: &crate::setup::staging::plan::Plan,
    ) -> Result<Self, String> {
        // Root locator categories describe the same source rather than selection.
        let mut retained = self.clone();
        for source in &mut retained.installations {
            let mut plan = crate::setup::migration_handoff::Plan::parse(&source.source_plan)
                .map_err(|e| e.to_string())?;
            plan.categories = crate::setup::migration_handoff::Categories {
                home: false,
                flatpak: true,
                accounts: false,
            };
            source.source_plan = plan.bytes().map_err(|e| e.to_string())?;
        }
        if fresh != retained {
            return Err(
                "The reviewed Flatpak deployments, repositories or source roots changed".into(),
            );
        }
        for source in &mut retained.installations {
            let mut plan = crate::setup::migration_handoff::Plan::parse(&source.source_plan)
                .map_err(|e| e.to_string())?;
            if plan.operation_id != original {
                return Err("Flatpak inventory operation differs from reviewed migration".into());
            }
            operation.clone_into(&mut plan.operation_id);
            plan.validate_for_install(installation)
                .map_err(|e| e.to_string())?;
            source.source_plan = plan.bytes().map_err(|e| e.to_string())?;
        }
        retained.validate_operation(operation)?;
        Ok(retained)
    }
    pub fn validate_operation(&self, operation: &str) -> Result<(), String> {
        self.validate()?;
        for (_, bytes) in self.sources() {
            let plan =
                crate::setup::migration_handoff::Plan::parse(bytes).map_err(|e| e.to_string())?;
            if plan.operation_id != operation || plan.flatpak_inventory.is_some() {
                return Err(
                    "Flatpak inventory has an unrelated or recursive source binding".into(),
                );
            }
        }
        Ok(())
    }
    /// `bind` returns the existing migration-handoff stable directory plan for
    /// each installation root. No host-native path is retained in the artifact.
    pub fn capture(
        runner: &impl Runner,
        home: &Path,
        selected: Vec<InstalledRef>,
        bind: impl FnMut(&Path) -> Result<Vec<u8>, String>,
    ) -> Result<Self, String> {
        Self::capture_material(runner, home, selected, true, bind)
    }
    pub fn capture_selected_state(
        runner: &impl Runner,
        home: &Path,
        selected: Vec<InstalledRef>,
        bind: impl FnMut(&Path) -> Result<Vec<u8>, String>,
    ) -> Result<Self, String> {
        Self::capture_material(runner, home, selected, false, bind)
    }
    fn capture_material(
        runner: &impl Runner,
        home: &Path,
        selected: Vec<InstalledRef>,
        full_home: bool,
        mut bind: impl FnMut(&Path) -> Result<Vec<u8>, String>,
    ) -> Result<Self, String> {
        let mut inventory = discover(runner, home)?;
        validate_selection(&inventory, &selected, full_home)?;
        let scopes: BTreeSet<_> = selected
            .iter()
            .map(|item| item.installation.clone())
            .collect();
        let mut required = BTreeSet::new();
        for scope in scopes {
            for reference in super::portable::dependencies::review(
                runner,
                home,
                &scope,
                &inventory.refs,
                &selected,
            )? {
                required.insert((scope.clone(), reference));
            }
        }
        let selected: Vec<_> = inventory
            .refs
            .iter()
            .filter(|item| required.contains(&(item.installation.clone(), item.reference.clone())))
            .cloned()
            .collect();
        validate_selection(&inventory, &selected, full_home)?;
        inventory.refs.retain(|item| {
            (full_home && item.installation == Installation::User) || selected.contains(item)
        });
        inventory.remotes.retain(|remote| {
            selected
                .iter()
                .any(|item| item.installation == remote.installation && item.origin == remote.name)
        });
        let roots = installation_roots(runner, home, &inventory)?;
        let scopes: BTreeSet<_> = inventory
            .refs
            .iter()
            .map(|item| item.installation.clone())
            .collect();
        let mut installations = Vec::new();
        for scope in scopes {
            let root = roots
                .get(&scope)
                .ok_or("Reviewed Flatpak source root is unavailable")?;
            let source_plan = bind(root)?;
            crate::setup::migration_handoff::Plan::parse(&source_plan)
                .map_err(|error| error.to_string())?;
            let mut repositories = Vec::new();
            let names: BTreeSet<_> = selected
                .iter()
                .filter(|item| item.installation == scope)
                .map(|item| item.origin.as_str())
                .collect();
            for name in names {
                let remote = inventory
                    .remotes
                    .iter()
                    .find(|remote| remote.installation == scope && remote.name == name)
                    .ok_or("Reviewed Flatpak remote is unavailable")?;
                validate_metadata_remote(remote)?;
                let definition = extract_remote_definition(remote, root)?;
                repositories.push(RepositoryTrust {
                    remote: remote.clone(),
                    definition_sha256: Sha256::digest(definition).into(),
                });
            }
            let mut deployments = Vec::new();
            for item in inventory
                .refs
                .iter()
                .filter(|item| item.installation == scope)
            {
                let location = runner.run(
                    home,
                    &[
                        "info".into(),
                        scope.argument(),
                        "--show-location".into(),
                        item.reference.clone(),
                    ],
                )?;
                let location = Path::new(location.trim_end_matches('\n'));
                let commit = location
                    .file_name()
                    .and_then(|name| name.to_str())
                    .filter(|name| {
                        name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
                    })
                    .ok_or("Invalid original Flatpak deployment commit")?;
                if root.join(&item.reference).join(commit) != location {
                    return Err("Original Flatpak deployment moved outside its installation".into());
                }
                let deploy_sha256 = deployment_digest(root, &item.reference, commit)?;
                deployments.push(Deployment {
                    reference: item.reference.clone(),
                    commit: commit.into(),
                    deploy_sha256,
                });
            }
            installations.push(InstallationSource {
                installation: scope,
                source_plan,
                repositories,
                deployments,
                config_sha256: configuration_digest(root)?,
            });
        }
        let result = Self {
            schema: if full_home { 1 } else { 2 },
            inventory,
            selected,
            installations,
        };
        result.validate()?;
        for source in &result.installations {
            source.verify_original(
                roots
                    .get(&source.installation)
                    .ok_or("Flatpak source root disappeared")?,
            )?;
        }
        result.bytes()?;
        Ok(result)
    }
    pub fn selected(&self) -> &[InstalledRef] {
        &self.selected
    }
    /// Pending-only source authority: no deployment/content epoch is certified.
    pub fn validate_pending_source(
        &self,
        installation: &Installation,
        root: &Path,
    ) -> Result<(), String> {
        self.validate()?;
        let source = self
            .installations
            .iter()
            .find(|source| &source.installation == installation)
            .ok_or("Pending source installation was not reviewed")?;
        let expected = crate::setup::migration_handoff::Plan::parse(&source.source_plan)
            .map_err(|error| error.to_string())?;
        let current = crate::setup::migration_handoff::capture(
            root,
            &expected.operation_id,
            expected.categories,
        )
        .map_err(|error| error.to_string())?;
        if current.home != expected.home {
            return Err("Pending installation source scope changed".into());
        }
        for repository in &source.repositories {
            let definition = extract_remote_definition(&repository.remote, root)?;
            let digest: [u8; 32] = Sha256::digest(definition).into();
            if digest != repository.definition_sha256 {
                return Err("Pending installation signing authority changed".into());
            }
        }
        Ok(())
    }
    /// Stable metadata plans that the caller must resolve independently through
    /// the existing read-only volume service; arbitrary host paths are not grants.
    /// Pure prospective geometry; publication still requires the staging
    /// helper's recorded and observed completed resize proof.
    pub fn for_completed_staging(
        &self,
        installation: &crate::setup::staging::plan::Plan,
    ) -> Result<Self, String> {
        self.validate()?;
        let mut result = self.clone();
        for source in &mut result.installations {
            source.source_plan = crate::setup::migration_handoff::Plan::parse(&source.source_plan)
                .and_then(|plan| plan.for_completed_staging(installation))
                .and_then(|plan| plan.bytes())
                .map_err(|error| error.to_string())?;
        }
        result.validate()?;
        Ok(result)
    }

    pub fn sources(&self) -> impl Iterator<Item = (&Installation, &[u8])> {
        self.installations
            .iter()
            .map(|source| (&source.installation, source.source_plan.as_slice()))
    }
    pub fn bytes(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        if bytes.len() > MAX_BYTES {
            return Err("Flatpak handoff metadata exceeds its 8 MiB bound".into());
        }
        Ok(bytes)
    }
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_BYTES {
            return Err("Flatpak handoff metadata exceeds its 8 MiB bound".into());
        }
        let result: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        result.validate()?;
        Ok(result)
    }
    fn validate(&self) -> Result<(), String> {
        if !matches!(self.schema, 1 | 2)
            || self.installations.len() > MAX_INSTALLATIONS
            || self.inventory.refs.len() > 100_000
            || self.inventory.remotes.len() > 4096
        {
            return Err("Invalid Flatpak handoff bounds or schema".into());
        }
        validate_selection(&self.inventory, &self.selected, self.schema == 1)?;
        let mut remote_ids = BTreeSet::new();
        for remote in &self.inventory.remotes {
            validate_metadata_remote(remote)?;
            if !remote_ids.insert((&remote.installation, &remote.name)) {
                return Err("Duplicate retained Flatpak repository".into());
            }
        }
        let mut scopes = BTreeSet::new();
        for source in &self.installations {
            if !scopes.insert(&source.installation) {
                return Err("Duplicate Flatpak source".into());
            }
            crate::setup::migration_handoff::Plan::parse(&source.source_plan)
                .map_err(|error| error.to_string())?;
            let mut repos = BTreeSet::new();
            for trust in &source.repositories {
                if trust.remote.installation != source.installation
                    || !repos.insert(&trust.remote.name)
                    || !self.inventory.remotes.contains(&trust.remote)
                {
                    return Err("Flatpak trust metadata does not match its installation".into());
                }
            }
            let mut deployments = BTreeSet::new();
            for deployment in &source.deployments {
                if !reference(&deployment.reference)
                    || deployment.commit.len() != 64
                    || !deployment
                        .commit
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
                    || !deployments.insert(&deployment.reference)
                    || !self.inventory.refs.iter().any(|item| {
                        item.installation == source.installation
                            && item.reference == deployment.reference
                    })
                {
                    return Err("Invalid retained Flatpak deployment binding".into());
                }
            }
            if self
                .inventory
                .refs
                .iter()
                .filter(|item| item.installation == source.installation)
                .any(|item| !deployments.contains(&item.reference))
            {
                return Err("Retained Flatpak reference has no original deployment binding".into());
            }
            for item in self
                .selected
                .iter()
                .filter(|item| item.installation == source.installation)
            {
                if !repos.contains(&item.origin) {
                    return Err("Selected Flatpak source repository has no reviewed trust".into());
                }
            }
        }
        if self
            .inventory
            .refs
            .iter()
            .any(|item| !scopes.contains(&item.installation))
        {
            return Err("Selected Flatpak installation has no stable source binding".into());
        }
        Ok(())
    }
    /// Resolve retained installation roots through the canonical stable-volume,
    /// read-only migration service, retaining every mounted source lease.
    pub fn resolve_installed<R: Runner>(
        &self,
        home: &Path,
        target: R,
    ) -> Result<SourceScopedRunner<R>, String> {
        self.resolve(
            home,
            target,
            crate::setup::migration_handoff::resolve_flatpak_source,
        )
    }
    /// Resolve each stored plan through the device's read-only mount service.
    /// Retained leases survive cloning this runner into a transfer worker.
    pub fn resolve<R: Runner>(
        &self,
        home: &Path,
        target: R,
        mut resolve: impl FnMut(&[u8]) -> Result<ReadOnlySource, String>,
    ) -> Result<SourceScopedRunner<R>, String> {
        self.validate()?;
        let mut roots = BTreeMap::new();
        let mut leases = Vec::new();
        let mut deployments = BTreeMap::new();
        for source in &self.installations {
            let grant = resolve(&source.source_plan)?;
            grant.pin.validate().map_err(|error| error.to_string())?;
            kernel_read_only(&grant.pin).map_err(|error| error.to_string())?;
            source.verify_original(&grant.root)?;
            for deployment in &source.deployments {
                deployments.insert(
                    (source.installation.clone(), deployment.reference.clone()),
                    deployment.commit.clone(),
                );
            }
            roots.insert(source.installation.clone(), grant.root.clone());
            leases.push(grant);
        }
        let selected_commits = self
            .selected
            .iter()
            .map(|item| {
                deployments
                    .get(&(item.installation.clone(), item.reference.clone()))
                    .cloned()
                    .map(|commit| (item.reference.clone(), commit))
                    .ok_or("Selected deployment commit is unavailable".to_owned())
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        Ok(SourceScopedRunner {
            source_home: home.to_owned(),
            inventory: self.inventory.clone(),
            roots,
            deployments,
            selected_commits,
            leases,
            target,
        })
    }
}
fn validate_selection(
    inventory: &Inventory,
    selected: &[InstalledRef],
    full_home: bool,
) -> Result<(), String> {
    let mut inventory_refs = BTreeSet::new();
    for item in &inventory.refs {
        if !valid_scope(&item.installation)
            || !reference(&item.reference)
            || !token(&item.origin)
            || item.name.len() > 4096
            || item.name.chars().any(char::is_control)
            || !inventory_refs.insert((item.installation.clone(), item.reference.clone()))
        {
            return Err("Invalid retained Flatpak inventory".into());
        }
    }
    let mut refs = BTreeSet::new();
    for item in selected {
        if (full_home && item.installation == Installation::User)
            || !inventory.refs.contains(item)
            || !refs.insert(&item.reference)
            || (full_home
                && inventory.refs.iter().any(|old| {
                    old.installation == Installation::User && old.reference == item.reference
                }))
        {
            return Err("Flatpak selection is absent, duplicated or conflicts with the copied user installation".into());
        }
    }
    Ok(())
}
fn validate_metadata_remote(remote: &Remote) -> Result<(), String> {
    let url = reqwest::Url::parse(&remote.url).map_err(|_| "Invalid retained repository URL")?;
    if !valid_scope(&remote.installation)
        || !token(&remote.name)
        || remote.url.len() > 4096
        || remote.options.len() > 4096
        || remote.url.chars().any(char::is_control)
        || remote.options.chars().any(char::is_control)
        || url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Repository metadata needs an explicit new connection; credentials cannot enter a handoff artifact".into());
    }
    Ok(())
}
fn kernel_read_only(root: &PinnedDirectory) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut status = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: live descriptor and full writable statvfs storage.
    if unsafe { libc::fstatvfs(root.file.as_raw_fd(), status.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful syscall initialized the structure.
    if unsafe { status.assume_init() }.f_flag & libc::ST_RDONLY == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Flatpak source grant is not kernel read-only",
        ));
    }
    Ok(())
}
/// Only a successful existing stable source-plan resolver may create this grant.
/// The resolver owns GPT/UUID/inode/subvolume verification and retains its mount lease.
pub struct ReadOnlySource {
    root: PathBuf,
    pin: Arc<PinnedDirectory>,
    lease: SourceLease,
}
enum SourceLease {
    #[cfg(any(feature = "capture-worker", test))]
    Borrowed(Arc<dyn Send + Sync>),
    Resolved(Arc<crate::setup::migration_handoff::ResolvedSource>),
}
impl ReadOnlySource {
    #[cfg(any(feature = "capture-worker", test))]
    pub fn from_resolved(
        root: &Path,
        pin: Arc<PinnedDirectory>,
        lease: Arc<dyn Send + Sync>,
    ) -> Result<Self, String> {
        pin.validate().map_err(|error| error.to_string())?;
        kernel_read_only(&pin).map_err(|error| error.to_string())?;
        Ok(Self {
            root: root.to_owned(),
            pin,
            lease: SourceLease::Borrowed(lease),
        })
    }
    pub fn from_owned_resolved(
        root: &Path,
        pin: Arc<PinnedDirectory>,
        lease: Arc<crate::setup::migration_handoff::ResolvedSource>,
    ) -> Result<Self, String> {
        pin.validate().map_err(|error| error.to_string())?;
        kernel_read_only(&pin).map_err(|error| error.to_string())?;
        Ok(Self {
            root: root.to_owned(),
            pin,
            lease: SourceLease::Resolved(lease),
        })
    }
}
/// Queries of the reviewed mounted source are metadata-only. Target operations
/// are real native Flatpak calls; no global installation on Tilo is substituted.
pub struct SourceScopedRunner<R: Runner> {
    source_home: PathBuf,
    inventory: Inventory,
    roots: BTreeMap<Installation, PathBuf>,
    deployments: BTreeMap<(Installation, String), String>,
    selected_commits: BTreeMap<String, String>,
    leases: Vec<ReadOnlySource>,
    target: R,
}
impl<R: Runner> SourceScopedRunner<R> {
    /// Consuming root-controller teardown only. Close every projected directory
    /// before handing native resolver custody to the combined retirement batch.
    pub(in crate::setup::flatpak_transfer) fn into_original_sources(
        self,
    ) -> io::Result<Vec<Arc<crate::setup::migration_handoff::ResolvedSource>>> {
        let Self {
            leases,
            source_home,
            inventory,
            roots,
            deployments,
            selected_commits,
            target,
        } = self;
        drop((
            source_home,
            inventory,
            roots,
            deployments,
            selected_commits,
            target,
        ));
        let mut sources = Vec::new();
        #[cfg(any(feature = "capture-worker", test))]
        let mut borrowed = false;
        for source in leases {
            let ReadOnlySource { root, pin, lease } = source;
            drop((root, pin));
            match lease {
                SourceLease::Resolved(source) => sources.push(source),
                #[cfg(any(feature = "capture-worker", test))]
                SourceLease::Borrowed(lease) => {
                    drop(lease);
                    borrowed = true;
                }
            }
        }
        #[cfg(any(feature = "capture-worker", test))]
        if borrowed {
            // Refuse and retain known native custody instead of silently
            // unmounting a subset through ordinary Drop.
            std::mem::forget(sources);
            return Err(io::Error::other(
                "Original application teardown lacks typed native resolver custody",
            ));
        }
        Ok(sources)
    }

    pub(in crate::setup::flatpak_transfer) fn original_lease_files(
        &self,
        inventory: &HandoffInventory,
    ) -> Result<Vec<(Installation, std::fs::File)>, String> {
        let mut files = Vec::new();
        for source in &inventory.installations {
            let root = self
                .roots
                .get(&source.installation)
                .ok_or("Original projection root missing")?;
            let lease = self
                .leases
                .iter()
                .find(|lease| &lease.root == root)
                .ok_or("Original projection lease missing")?;
            lease.pin.validate().map_err(|error| error.to_string())?;
            kernel_read_only(&lease.pin).map_err(|error| error.to_string())?;
            let file = crate::setup::migration_durability::readable_directory(&lease.pin.file)
                .map_err(|error| error.to_string())?;
            files.push((source.installation.clone(), file));
        }
        Ok(files)
    }
    pub(in crate::setup::flatpak_transfer) fn original_files(
        &self,
        inventory: &HandoffInventory,
    ) -> Result<Vec<(Installation, std::fs::File)>, String> {
        inventory.validate()?;
        let mut files = Vec::new();
        for source in &inventory.installations {
            let root = self
                .roots
                .get(&source.installation)
                .ok_or("Original projection root missing")?;
            let lease = self
                .leases
                .iter()
                .find(|lease| &lease.root == root)
                .ok_or("Original projection lease missing")?;
            lease.pin.validate().map_err(|error| error.to_string())?;
            kernel_read_only(&lease.pin).map_err(|error| error.to_string())?;
            source.verify_original(root)?;
            let file = crate::setup::migration_durability::readable_directory(&lease.pin.file)
                .map_err(|error| error.to_string())?;
            files.push((source.installation.clone(), file));
        }
        Ok(files)
    }
    #[cfg(any(feature = "capture-worker", test))]
    pub(in crate::setup::flatpak_transfer) fn closed_inventory(&self) -> &Inventory {
        &self.inventory
    }
    #[cfg(any(feature = "capture-worker", test))]
    pub(in crate::setup::flatpak_transfer) fn closed_root(
        &self,
        scope: &Installation,
    ) -> Result<&Path, String> {
        self.roots
            .get(scope)
            .map(PathBuf::as_path)
            .ok_or("Original root missing".into())
    }
    #[cfg(any(feature = "capture-worker", test))]
    pub(in crate::setup::flatpak_transfer) fn closed_commit(
        &self,
        item: &InstalledRef,
    ) -> Result<&str, String> {
        self.deployments
            .get(&(item.installation.clone(), item.reference.clone()))
            .map(String::as_str)
            .ok_or("Original exact deployment missing".into())
    }
    fn target_run(&self, home: &Path, arguments: &[String]) -> Result<String, String> {
        if arguments.first().is_none_or(|verb| verb != "install") {
            return self.target.run(home, arguments);
        }
        for lease in &self.leases {
            lease.pin.validate().map_err(|error| error.to_string())?;
            kernel_read_only(&lease.pin).map_err(|error| error.to_string())?;
        }
        let reference = arguments.last().ok_or("Missing selected Flatpak ref")?;
        let commit = self
            .selected_commits
            .get(reference)
            .ok_or("Target install is outside the reviewed Flatpak selection")?;
        if arguments
            .iter()
            .any(|arg| arg == "--commit" || arg.starts_with("--commit="))
        {
            return Err("Target install cannot override the reviewed deployment commit".into());
        }
        let mut exact = arguments.to_vec();
        let position = exact
            .len()
            .checked_sub(2)
            .ok_or("Missing reviewed origin")?;
        exact.insert(position, "--no-deps".into());
        exact.insert(position, "--no-related".into());
        exact.insert(position, format!("--commit={commit}"));
        let result = self.target.run(home, &exact)?;
        let actual = self.target.run(
            home,
            &[
                "info".into(),
                "--user".into(),
                "--show-commit".into(),
                reference.clone(),
            ],
        )?;
        if actual.trim() != commit {
            return Err("Installed Flatpak commit differs from the reviewed deployment".into());
        }
        let origin = arguments.get(position).ok_or("Missing reviewed origin")?;
        let actual_origin = self.target.run(
            home,
            &[
                "info".into(),
                "--user".into(),
                "--show-origin".into(),
                reference.clone(),
            ],
        )?;
        if actual_origin.trim() != origin {
            return Err("Installed Flatpak origin differs from the approved repository".into());
        }
        Ok(result)
    }
}
impl<R: Runner> Runner for SourceScopedRunner<R> {
    fn review_offline_subset(
        &self,
        grant: &super::portable::SourceExportGrant<'_>,
    ) -> Result<Option<super::portable::ReviewedSubset>, String> {
        for lease in &self.leases {
            lease.pin.validate().map_err(|error| error.to_string())?;
            kernel_read_only(&lease.pin).map_err(|error| error.to_string())?;
        }
        self.target.review_offline_subset(grant)
    }
    fn verify_repository(&self, home: &Path) -> Result<(), String> {
        if home == self.source_home {
            return Err(
                "Verification cannot mutate or substitute the original installation".into(),
            );
        }
        self.target.verify_repository(home)
    }
    fn run(&self, home: &Path, args: &[String]) -> Result<String, String> {
        if home != self.source_home {
            return self.target_run(home, args);
        }
        for lease in &self.leases {
            lease.pin.validate().map_err(|error| error.to_string())?;
            kernel_read_only(&lease.pin).map_err(|error| error.to_string())?;
        }
        if args.len() == 4
            && args[0] == "list"
            && args[1] == "--all"
            && matches!(args[2].as_str(), "--app" | "--runtime")
            && args[3] == "--columns=ref:f,origin:f,installation:f,name:f"
        {
            let kind = if args[2] == "--app" {
                "app/"
            } else {
                "runtime/"
            };
            return Ok(self
                .inventory
                .refs
                .iter()
                .filter_map(|item| {
                    item.reference.strip_prefix(kind).map(|reference| {
                        format!(
                            "{reference}\t{}\t{}\t{}\n",
                            item.origin,
                            installation_name(&item.installation),
                            item.name
                        )
                    })
                })
                .collect());
        }
        if args.len() == 4
            && args[0] == "remotes"
            && args[2] == "--show-disabled"
            && args[3] == "--columns=name:f,url:f,options:f"
        {
            let scope = argument_installation(&args[1])?;
            let mut output = String::new();
            for remote in self
                .inventory
                .remotes
                .iter()
                .filter(|remote| remote.installation == scope)
            {
                let _ = writeln!(
                    output,
                    "{}\t{}\t{}",
                    remote.name, remote.url, remote.options
                );
            }
            return Ok(output);
        }
        if args.len() == 4
            && args[0] == "info"
            && args[2] == "--show-location"
            && reference(&args[3])
        {
            let scope = argument_installation(&args[1])?;
            if !self
                .inventory
                .refs
                .iter()
                .any(|item| item.installation == scope && item.reference == args[3])
            {
                return Err("Unreviewed Flatpak ref query".into());
            }
            let root = self
                .roots
                .get(&scope)
                .ok_or("Original Flatpak installation has no read-only source grant")?;
            let commit = self
                .deployments
                .get(&(scope.clone(), args[3].clone()))
                .ok_or("Original deployment binding is unavailable")?;
            return Ok(root
                .join(&args[3])
                .join(commit)
                .to_string_lossy()
                .into_owned());
        }
        Err("This operation is not an authorized query of the original Flatpak source".into())
    }
}
fn installation_name(scope: &Installation) -> &str {
    match scope {
        Installation::User => "user",
        Installation::System(name) => name,
    }
}
fn argument_installation(argument: &str) -> Result<Installation, String> {
    if argument == "--user" {
        Ok(Installation::User)
    } else if let Some(name) = argument.strip_prefix("--installation=") {
        if token(name) {
            Ok(Installation::System(name.into()))
        } else {
            Err("Invalid source installation argument".into())
        }
    } else {
        Err("Invalid source installation argument".into())
    }
}

fn deployment_digest(root: &Path, reference: &str, commit: &str) -> Result<[u8; 32], String> {
    use cap_fs_ext::OpenOptionsFollowExt;
    use cap_primitives::fs::{FollowSymlinks, OpenOptions};
    use std::io::Read;
    let root =
        PinnedDirectory::open_absolute(&root.join(reference)).map_err(|error| error.to_string())?;
    let active = cap_primitives::fs::read_link_contents(&root.file, Path::new("active"))
        .map_err(|error| error.to_string())?;
    if active != Path::new(commit) {
        return Err("Original Flatpak deployment is no longer active".into());
    }
    let metadata = crate::setup::nofollow::metadata_nofollow(&root.file, Path::new(commit))
        .map_err(|error| error.to_string())?;
    let deployment = root
        .open_child(std::ffi::OsStr::new(commit), &metadata)
        .map_err(|error| error.to_string())?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = cap_primitives::fs::open(&deployment.file, Path::new("deploy"), &options)
        .map_err(|error| error.to_string())?;
    let before = file.metadata().map_err(|error| error.to_string())?;
    if !before.is_file() || before.len() == 0 || before.len() > 1024 * 1024 {
        return Err("Original Flatpak deployment metadata is invalid".into());
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let after = file.metadata().map_err(|error| error.to_string())?;
    use std::os::unix::fs::MetadataExt;
    if bytes.len() as u64 != before.len()
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err("Original Flatpak deployment changed during review".into());
    }
    deployment.validate().map_err(|error| error.to_string())?;
    Ok(Sha256::digest(bytes).into())
}

fn valid_scope(scope: &Installation) -> bool {
    match scope {
        Installation::User => true,
        Installation::System(name) => token(name),
    }
}
impl InstallationSource {
    fn verify_original(&self, root: &Path) -> Result<(), String> {
        if configuration_digest(root)? != self.config_sha256 {
            return Err("Original Flatpak repository configuration changed after review".into());
        }
        for trust in &self.repositories {
            let definition = extract_remote_definition(&trust.remote, root)?;
            if <[u8; 32]>::from(Sha256::digest(definition)) != trust.definition_sha256 {
                return Err("Original Flatpak repository trust changed after review".into());
            }
        }
        for deployment in &self.deployments {
            if deployment_digest(root, &deployment.reference, &deployment.commit)?
                != deployment.deploy_sha256
            {
                return Err("Original Flatpak deployment changed after review".into());
            }
        }
        Ok(())
    }
}

fn configuration_digest(root: &Path) -> Result<[u8; 32], String> {
    use cap_fs_ext::OpenOptionsFollowExt;
    use cap_primitives::fs::{FollowSymlinks, OpenOptions};
    use std::{io::Read, os::unix::fs::MetadataExt};
    let repo =
        PinnedDirectory::open_absolute(&root.join("repo")).map_err(|error| error.to_string())?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = cap_primitives::fs::open(&repo.file, Path::new("config"), &options)
        .map_err(|error| error.to_string())?;
    let before = file.metadata().map_err(|error| error.to_string())?;
    if !before.is_file() || before.len() == 0 || before.len() > 1024 * 1024 {
        return Err("Invalid original Flatpak repository configuration".into());
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let after = file.metadata().map_err(|error| error.to_string())?;
    if bytes.len() as u64 != before.len()
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err("Original Flatpak repository configuration changed during review".into());
    }
    repo.validate().map_err(|error| error.to_string())?;
    Ok(Sha256::digest(bytes).into())
}
