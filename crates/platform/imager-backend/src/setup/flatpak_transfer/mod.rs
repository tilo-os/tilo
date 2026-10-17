//! Installed Flatpak inventory and target-user restoration. Home migration owns
//! `.var/app` and the user installation (including overrides and OSTree objects).
//! System apps are reinstalled through Flatpak, never by copying deployment refs.
//! No imported application is launched and signature verification stays enabled.

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

mod data;
mod settings;
pub use settings::AdministratorSettings;
#[cfg(target_os = "linux")]
mod handoff;
#[cfg(target_os = "linux")]
pub mod offline_original;
#[cfg(target_os = "linux")]
pub mod portable;
#[cfg(target_os = "linux")]
pub use handoff::{HandoffInventory, ReadOnlySource, SourceScopedRunner};
mod native;
mod repository;
#[cfg(target_os = "linux")]
pub(super) use data::review_subtree;
pub use data::{DataSubtree, data_subtrees};
pub use native::{CancellableNativeRunner, NativeRunner};
pub use repository::extract_remote_definition;
#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Installation {
    User,
    System(String),
}

impl Installation {
    pub fn argument(&self) -> String {
        match self {
            Self::User => "--user".into(),
            Self::System(name) => format!("--installation={name}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledRef {
    pub reference: String,
    pub origin: String,
    pub installation: Installation,
    pub name: String,
}

impl InstalledRef {
    pub fn app_id(&self) -> Option<&str> {
        self.reference.strip_prefix("app/")?.split('/').next()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remote {
    pub installation: Installation,
    pub name: String,
    pub url: String,
    pub options: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inventory {
    pub refs: Vec<InstalledRef>,
    pub remotes: Vec<Remote>,
}

/// A CLI boundary, injectable for fixtures. Arguments are individual values;
/// callers must never interpolate them into a shell. `home` identifies the
/// account whose *user* installation is queried or modified.
pub trait Runner {
    /// Review collection bindings and offline suitability without source writes.
    /// # Errors
    /// Refuses unavailable or changed metadata and unsupported extra-data.
    #[cfg(target_os = "linux")]
    fn review_offline_subset(
        &self,
        _grant: &portable::SourceExportGrant<'_>,
    ) -> Result<Option<portable::ReviewedSubset>, String> {
        Ok(None)
    }
    fn run(&self, home: &Path, arguments: &[String]) -> Result<String, String>;
    /// Independently check object integrity and ref bindings in the target repository.
    ///
    /// # Errors
    /// Refuses missing verification support or corrupt repository objects.
    fn verify_repository(&self, _home: &Path) -> Result<(), String> {
        Err("This runner cannot independently verify repository objects".into())
    }
}

fn token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        && !value.starts_with(['-', '.'])
}

fn reference(value: &str) -> bool {
    let parts: Vec<_> = value.split('/').collect();
    parts.len() == 4
        && matches!(parts[0], "app" | "runtime")
        && parts[1..].iter().all(|part| token(part))
}

fn installation(value: &str) -> Result<Installation, String> {
    // Native Flatpak prints named installation IDs as `system (ID)`; this is
    // independent of the configurable human-facing DisplayName.
    if let Some(name) = value
        .strip_prefix("system (")
        .and_then(|name| name.strip_suffix(')'))
    {
        if token(name) && !matches!(name, "user" | "system" | "default") {
            return Ok(Installation::System(name.into()));
        }
        return Err("Invalid Flatpak installation identifier".into());
    }
    match value {
        "user" => Ok(Installation::User),
        "system" | "default" => Ok(Installation::System("default".into())),
        name if token(name) => Ok(Installation::System(name.into())),
        _ => Err("Invalid Flatpak installation identifier".into()),
    }
}

/// Reads installed apps, runtimes (including extensions), and every associated
/// user/default/named system remote. A truncated or malformed inventory fails.
pub fn discover(runner: &impl Runner, home: &Path) -> Result<Inventory, String> {
    let mut result = Inventory::default();
    let mut seen = BTreeSet::new();
    for (kind, flag) in [("app", "--app"), ("runtime", "--runtime")] {
        let raw = runner.run(
            home,
            &[
                "list".into(),
                "--all".into(),
                flag.into(),
                "--columns=ref:f,origin:f,installation:f,name:f".into(),
            ],
        )?;
        for line in raw.lines().filter(|line| !line.is_empty()) {
            let columns: Vec<_> = line.split('\t').collect();
            let full_ref = format!("{kind}/{}", columns.first().copied().unwrap_or_default());
            if columns.len() != 4
                || !reference(&full_ref)
                || !token(columns[1])
                || columns[3].chars().any(char::is_control)
            {
                return Err("Invalid Flatpak inventory row".into());
            }
            let installation = installation(columns[2])?;
            if !seen.insert((installation.clone(), full_ref.clone())) {
                return Err("Duplicate Flatpak inventory ref".into());
            }
            result.refs.push(InstalledRef {
                reference: full_ref,
                origin: columns[1].into(),
                installation,
                name: columns[3].into(),
            });
        }
    }
    let scopes: BTreeSet<_> = result
        .refs
        .iter()
        .map(|item| item.installation.clone())
        .chain([Installation::User])
        .collect();
    for scope in scopes {
        let raw = runner.run(
            home,
            &[
                "remotes".into(),
                scope.argument(),
                "--show-disabled".into(),
                "--columns=name:f,url:f,options:f".into(),
            ],
        )?;
        for line in raw.lines().filter(|line| !line.is_empty()) {
            let columns: Vec<_> = line.split('\t').collect();
            if !(2..=3).contains(&columns.len())
                || !token(columns[0])
                || columns[1].chars().any(char::is_control)
            {
                return Err("Invalid Flatpak remote row".into());
            }
            result.remotes.push(Remote {
                installation: scope.clone(),
                name: columns[0].into(),
                url: columns[1].into(),
                options: columns.get(2).copied().unwrap_or_default().into(),
            });
        }
    }
    Ok(result)
}

/// A reviewed repository definition including its signing key. Remote names
/// are target names, preventing collisions between source installations. Only
/// the explicitly supplied `.flatpakrepo` files may establish new trust.
#[derive(Clone, Debug)]
pub struct ApprovedRemote {
    pub source_installation: Installation,
    pub source_name: String,
    pub target_name: String,
    pub definition: PathBuf,
}

/// Resolve each installation root using a deployed ref rather than guessing
/// custom system installation paths. No filesystem content is copied here.
pub fn installation_roots(
    runner: &impl Runner,
    home: &Path,
    inventory: &Inventory,
) -> Result<BTreeMap<Installation, PathBuf>, String> {
    let mut roots = BTreeMap::new();
    for item in &inventory.refs {
        if roots.contains_key(&item.installation) {
            continue;
        }
        if !reference(&item.reference) {
            return Err("Invalid Flatpak ref".into());
        }
        let location = runner.run(
            home,
            &[
                "info".into(),
                item.installation.argument(),
                "--show-location".into(),
                item.reference.clone(),
            ],
        )?;
        let location = Path::new(location.trim_end_matches('\n'));
        let mut parent = location;
        let mut suffix = Vec::new();
        for _ in 0..5 {
            suffix.push(
                parent
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or("Invalid Flatpak deployment location")?,
            );
            parent = parent
                .parent()
                .ok_or("Invalid Flatpak deployment location")?;
        }
        suffix.reverse();
        if !location.is_absolute()
            || suffix[..4].join("/") != item.reference
            || suffix[4].len() != 64
            || !suffix[4].bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("Flatpak deployment location does not match its ref".into());
        }
        roots.insert(item.installation.clone(), parent.into());
    }
    Ok(roots)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RestoreReport {
    pub restored: Vec<String>,
    pub failed: Vec<(String, String)>,
}

/// Reinstalls selected system refs in the new user's installation. Runtimes
/// precede apps; Flatpak also resolves dependencies and verifies signatures.
/// Online restores need the original repositories. Sideload repositories can
/// reduce downloads but do not promise offline availability of remote indexes.
/// User refs are owned by the full-home transfer and must not be passed here.
pub fn restore_system_refs(
    runner: &impl Runner,
    target_home: &Path,
    selected: &[InstalledRef],
    approved: &[ApprovedRemote],
    sideload: Option<&Path>,
) -> Result<RestoreReport, String> {
    restore_refs(runner, target_home, selected, approved, sideload, false)
}
pub fn repair_system_refs(
    runner: &impl Runner,
    target_home: &Path,
    selected: &[InstalledRef],
    approved: &[ApprovedRemote],
) -> Result<RestoreReport, String> {
    restore_refs(runner, target_home, selected, approved, None, true)
}
fn restore_refs(
    runner: &impl Runner,
    target_home: &Path,
    selected: &[InstalledRef],
    approved: &[ApprovedRemote],
    sideload: Option<&Path>,
    reinstall: bool,
) -> Result<RestoreReport, String> {
    let mut mapping = BTreeMap::new();
    let mut target_names = BTreeSet::new();
    for remote in approved {
        if !token(&remote.source_name)
            || !token(&remote.target_name)
            || !remote.definition.is_absolute()
            || remote
                .definition
                .extension()
                .is_none_or(|ext| ext != "flatpakrepo")
            || !target_names.insert(remote.target_name.clone())
            || mapping
                .insert(
                    (
                        remote.source_installation.clone(),
                        remote.source_name.clone(),
                    ),
                    remote,
                )
                .is_some()
        {
            return Err("Invalid or ambiguous approved Flatpak remote".into());
        }
    }
    let mut targets = BTreeSet::new();
    for item in selected {
        if item.installation == Installation::User
            || !reference(&item.reference)
            || !token(&item.origin)
        {
            return Err("Invalid system Flatpak selection".into());
        }
        if !targets.insert(item.reference.clone()) {
            return Err("Choose one source for each Flatpak application or runtime".into());
        }
    }
    if sideload.is_some_and(|path| !path.is_absolute()) {
        return Err("Flatpak sideload repository must be absolute".into());
    }
    let mut report = RestoreReport::default();
    let mut ready = BTreeSet::new();
    let mut refs: Vec<_> = selected.iter().collect();
    refs.sort_by_key(|item| (item.app_id().is_some(), item.reference.clone()));
    for item in refs {
        let key = (item.installation.clone(), item.origin.clone());
        let Some(remote) = mapping.get(&key) else {
            report
                .failed
                .push((item.reference.clone(), "Repository needs approval".into()));
            continue;
        };
        if ready.insert(key.clone()) {
            let args = vec![
                "remote-add".into(),
                "--user".into(),
                "--if-not-exists".into(),
                "--from".into(),
                remote.target_name.clone(),
                remote.definition.to_string_lossy().into_owned(),
            ];
            if let Err(error) = runner.run(target_home, &args) {
                ready.remove(&key);
                report.failed.push((item.reference.clone(), error));
                continue;
            }
        }
        let mut args = vec![
            "install".into(),
            "--user".into(),
            "--noninteractive".into(),
            "--assumeyes".into(),
        ];
        if reinstall {
            args.push("--reinstall".into());
        }
        if let Some(repo) = sideload {
            args.push(format!("--sideload-repo={}", repo.display()));
        }
        args.extend([remote.target_name.clone(), item.reference.clone()]);
        match runner.run(target_home, &args) {
            Ok(_) => report.restored.push(item.reference.clone()),
            Err(error) => report.failed.push((item.reference.clone(), error)),
        }
    }
    Ok(report)
}

/// Exports installed signed objects with Flatpak's native USB exporter. It
/// includes application dependencies. The output must be a fresh private
/// directory prepared by the caller; originals are only read.
pub fn export_sideload(
    runner: &impl Runner,
    home: &Path,
    scope: &Installation,
    refs: &[String],
    destination: &Path,
) -> Result<(), String> {
    if matches!(scope, Installation::System(name) if !token(name))
        || refs.is_empty()
        || refs.iter().any(|item| !reference(item))
        || !destination.is_absolute()
    {
        return Err("Invalid Flatpak export selection".into());
    }
    let mut args = vec![
        "create-usb".into(),
        scope.argument(),
        destination.to_string_lossy().into_owned(),
    ];
    args.extend(refs.iter().cloned());
    runner.run(home, &args).map(|_| ())
}

/// Device-local read-only review detail; this does not alter transfer schemas.
/// Unknown/unreadable source settings remain None, never an empty success claim.
pub fn administrator_settings(
    home: &Path,
    inventory: &Inventory,
) -> Option<Vec<AdministratorSettings>> {
    #[cfg(target_os = "linux")]
    {
        review_administrator_settings(&NativeRunner, home, inventory)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (home, inventory);
        None
    }
}

#[cfg(target_os = "linux")]
pub fn review_administrator_settings(
    runner: &impl Runner,
    home: &Path,
    inventory: &Inventory,
) -> Option<Vec<AdministratorSettings>> {
    settings::review(runner, home, &inventory.refs).ok()
}
