//! The person's changes to their package store ([`Change`]), as the service's action makes them
//! and as an OS image build makes them in-process (`tilo-install --offline-store`), where there is
//! no device, identity or service to ask. Both call [`apply`]: one implementation, two entries.
//! Writing a package is [`package_store::installer`]'s; this decides which writes a change makes
//! and says what they did.

use std::fs;
use std::path::Path;

use package_service_client::wire::{Change, ChangeReport, Replacement, SavedData, Trust, Uploaded};
use package_store::installer::{
    self, Error, data_home_root, decode_hex_32, load_revocation_list, load_trusted_attestors,
    revoked_keys_path, store_error, trusted_attestors_path,
};
use package_store::{Admission, Catalog, InstalledPackage, PackageStore, shim};
use tilo_app_package::trust::{AttestorScope, RevokedKey, TrustFile as _, TrustedAttestor};
use tilo_app_package::{BlobFetcher, PackageId};

mod offered;

/// The store a change writes, with the image's packages beside it, and the size cap collection
/// after the change applies.
pub struct Target<'a> {
    pub store: &'a PackageStore,
    pub catalog: &'a Catalog,
    pub max_store_bytes: u64,
}

impl Target<'_> {
    fn root(&self) -> &Path {
        self.store.layout().root()
    }
}

/// What a change says as it goes.
#[derive(Default)]
struct Said(ChangeReport);

impl Said {
    fn line(&mut self, line: impl Into<String>) {
        self.0.lines.push(line.into());
    }

    fn warn(&mut self, warning: impl Into<String>) {
        self.0.warnings.push(warning.into());
    }

    fn take(&mut self, report: installer::Report) {
        self.0.warnings.extend(report.warnings);
        self.0.lines.extend(report.lines);
    }
}

/// Make `change` to `target`. `file` is the file the change names, uploaded or (in an image build)
/// on disk.
///
/// # Errors
///
/// Why the change was refused or failed, as the person is told, after what it said before then
/// (an update of every app removes copies the image superseded before one fails to fetch).
pub fn apply(
    target: &Target<'_>,
    change: Change,
    file: Option<&Path>,
) -> Result<ChangeReport, String> {
    // A payload fetched from a registry runs on a runtime of its own, which no thread of the
    // service's runtime (its blocking pool included) may start: the change gets a thread of its
    // own, whoever calls.
    std::thread::scope(|scope| {
        scope
            .spawn(|| apply_here(target, change, file))
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

fn apply_here(
    target: &Target<'_>,
    change: Change,
    file: Option<&Path>,
) -> Result<ChangeReport, String> {
    let mut said = Said::default();
    let started = tilo_clock::unix_now();
    // What changes the installed set: collection runs after it succeeds.
    let collects = matches!(
        change,
        Change::Install { dry_run: false, .. }
            | Change::Update { dry_run: false, .. }
            | Change::Uninstall { dry_run: false, .. }
    );
    if let Err(error) = run(target, change, file, &mut said) {
        let Said(ChangeReport {
            mut lines,
            warnings,
        }) = said;
        lines.extend(warnings);
        lines.push(error.to_string());
        return Err(lines.join("\n"));
    }
    if collects {
        collect_after(target, started, &mut said);
    }
    Ok(said.0)
}

fn uploaded<'a>(file: Option<&'a Path>, named: &Uploaded) -> Result<&'a Path, Error> {
    file.ok_or_else(|| {
        Error::Policy(format!(
            "{} was not uploaded before the change that names it",
            named.source
        ))
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per kind of change, each handing it to the function that makes it"
)]
fn run(
    target: &Target<'_>,
    change: Change,
    file: Option<&Path>,
    said: &mut Said,
) -> Result<(), Error> {
    match change {
        Change::Install {
            file: named,
            trust,
            pinned,
            replacement,
            ignore_missing_runtime,
            dry_run,
            payload_from,
        } => install(
            target,
            uploaded(file, &named)?,
            named.source,
            &installer::Request {
                trust: installer_trust(&trust),
                pinned,
                replacement: match replacement {
                    Replacement::Refuse => installer::Replacement::Refuse,
                    Replacement::Replace => installer::Replacement::Replace,
                    Replacement::PreserveHeld => installer::Replacement::PreserveHeld,
                },
                ignore_missing_runtime,
                dry_run,
            },
            payload_from,
            said,
        ),
        Change::Update {
            app,
            file: named,
            version,
            allow_downgrade,
            dry_run,
            trust,
            ignore_missing_runtime,
            payload_from,
        } => {
            let from = match &named {
                Some(named) => Some((uploaded(file, named)?, named.source.as_str())),
                None => None,
            };
            update(
                target,
                app.as_deref(),
                &UpdateRequest {
                    from,
                    version_req: version.as_deref(),
                    allow_downgrade,
                    dry_run,
                    trust: &trust,
                    ignore_missing_runtime,
                    payload_from: payload_from.map(app_registry::HttpBlobs::new),
                },
                said,
            )
        }
        Change::Uninstall {
            app,
            saved_data,
            dry_run,
        } => uninstall(target, &app, saved_data, dry_run, true, said),
        Change::SetPinned { app, pinned } => {
            target.store.set_pinned(&app, pinned).map_err(store_error)?;
            said.line(format!(
                "{}: {app}",
                if pinned { "pinned" } else { "unpinned" }
            ));
            Ok(())
        }
        Change::Opened { app } => target.store.mark_used(&app).map_err(store_error),
        Change::Collect {
            dry_run,
            grace_secs,
            max_store_bytes,
        } => collect(
            target,
            package_store::gc::Request {
                max_store_bytes,
                grace_secs,
                now: tilo_clock::unix_now(),
                dry_run,
                keep: Vec::new(),
            },
            said,
        ),
        Change::AddImage {
            file: named,
            publisher,
            payload_from,
        } => add_image(
            target,
            uploaded(file, &named)?,
            &named.source,
            publisher.as_deref(),
            payload_from,
            said,
        ),
        Change::RemoveImage { id } => {
            let id = PackageId::from_bytes(id);
            let removed = target.store.remove_image(id).map_err(store_error)?;
            said.warn(format!(
                "removed {id}; evicted {} blob(s)",
                removed.evicted_blobs
            ));
            Ok(())
        }
        Change::FetchImage { id, registry } => {
            let store =
                image_store(target.root())?.with_fetcher(app_registry::HttpBlobs::new(registry));
            let tree = package_store::materialized_tree(&store, &PackageId::from_bytes(id))
                .map_err(|error| Error::Store(error.to_string()))?;
            said.line(tree.display().to_string());
            Ok(())
        }
        Change::InstallRuntime {
            version,
            from,
            arch,
            identity,
            force,
        } => {
            let identity = match &identity {
                Some(named) => Some(uploaded(file, named)?),
                None => None,
            };
            install_runtime(
                &version,
                from.as_deref().map(Path::new),
                arch.as_deref(),
                identity,
                force,
                said,
            )
        }
        Change::RemoveRuntime { version } => remove_runtime(&version, said),
        Change::PinRuntime { version, pinned } => pin_runtime(&version, pinned, said),
        Change::AddAttestor { id, pubkey, scope } => add_attestor(&id, &pubkey, &scope, said),
        Change::RemoveAttestor { id } => remove_attestor(&id, said),
        Change::Revoke { pubkey, reason } => revoke(&pubkey, &reason, said),
    }
}

/// Install the package at `path` (`source` names where it came from), saying how to run it.
fn install(
    target: &Target<'_>,
    path: &Path,
    source: String,
    request: &installer::Request,
    payload_from: Option<String>,
    said: &mut Said,
) -> Result<(), Error> {
    let payload = payload_from.map(app_registry::HttpBlobs::new);
    let report = installer::install(
        target.store,
        target.catalog,
        &installer::Candidate::staged(path, source),
        request,
        payload.as_ref().map(|source| source as &dyn BlobFetcher),
    )?;
    let installed = report.outcome == installer::Outcome::Installed;
    let selected = report.selected.clone();
    said.take(report);
    if installed && let Some(selected) = selected {
        register_search_providers(said);
        said.line(format!("run with:  tilo-install run {}", selected.id));
    }
    Ok(())
}

/// The installer's trust from the wire's.
#[must_use]
pub fn installer_trust(trust: &Trust) -> installer::Trust {
    installer::Trust {
        require_package_root: trust.require_package_root.clone(),
        require_publisher: trust.require_publisher.clone(),
        require_attestor: trust.require_attestor.clone(),
        allow_self_signed: trust.allow_self_signed,
    }
}

/// Let GNOME Shell and `KRunner` search the resources the runtime beside this program admits. Best
/// effort: an app installs and runs without it.
#[cfg(all(unix, not(target_os = "macos")))]
fn register_search_providers(said: &mut Said) {
    let Some(runtime) = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("tilo-runtime")))
        .filter(|runtime| runtime.is_file())
    else {
        return;
    };
    let written = data_home_root()
        .map_err(|error| error.to_string())
        .and_then(|data| {
            let config = tilo_paths::config_dir().ok_or("no configuration directory")?;
            tilo_os_search::linux_files::write(&data, &config, &runtime)
                .map_err(|error| error.to_string())
        });
    match written {
        Ok(()) => said
            .line("  search   : KRunner finds its resources now, GNOME Shell after the next login"),
        Err(error) => said.warn(format!("host search not registered: {error}")),
    }
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn register_search_providers(_said: &mut Said) {}

struct UpdateRequest<'a> {
    /// The local file to update from, and the name it was given as.
    from: Option<(&'a Path, &'a str)>,
    version_req: Option<&'a str>,
    allow_downgrade: bool,
    dry_run: bool,
    trust: &'a Trust,
    ignore_missing_runtime: bool,
    /// Where payload blobs a local file does not carry come from.
    payload_from: Option<app_registry::HttpBlobs>,
}

fn find_installed(target: &Target<'_>, app: &str) -> Result<InstalledPackage, Error> {
    target
        .catalog
        .layers(app)
        .map_err(|e| Error::RegistryRead(e.to_string()))?
        .running()
        .cloned()
        .ok_or_else(|| Error::NotInstalled(app.into()))
}

/// Fetch a newer version and swap it in, keeping the person's per-workspace data.
fn update(
    target: &Target<'_>,
    app: Option<&str>,
    request: &UpdateRequest<'_>,
    said: &mut Said,
) -> Result<(), Error> {
    let targets: Vec<InstalledPackage> = if let Some(id) = app {
        vec![find_installed(target, id)?]
    } else {
        if request.from.is_some() {
            return Err(Error::Policy(
                "--from names one .tapp, so it needs one package id".into(),
            ));
        }
        if !request.dry_run {
            drop_superseded(target, said)?;
        }
        target
            .catalog
            .list()
            .map_err(|e| Error::RegistryRead(e.to_string()))?
    };
    if targets.is_empty() {
        said.line("(no packages installed)");
        return Ok(());
    }
    let mut updated: u32 = 0;
    let mut current: u32 = 0;
    for entry in &targets {
        match update_one(target, entry, request, said)? {
            installer::Outcome::Planned => {}
            installer::Outcome::AlreadyCurrent => current = current.saturating_add(1),
            _ => updated = updated.saturating_add(1),
        }
    }
    if targets.len() > 1 {
        said.line(format!("{updated} updated, {current} already current"));
    }
    Ok(())
}

/// Remove the person's copies of apps the image now ships a newer version of: nothing runs them any
/// more.
fn drop_superseded(target: &Target<'_>, said: &mut Said) -> Result<(), Error> {
    let superseded = target
        .catalog
        .superseded()
        .map_err(|e| Error::RegistryRead(e.to_string()))?;
    for entry in superseded {
        target.store.remove(&entry.id, true).map_err(store_error)?;
        if !entry.cli_commands.is_empty() {
            let _ =
                shim::remove_shims(&target.store.layout().bin(), &entry.id, &entry.cli_commands);
        }
        said.line(format!(
            "{} {}: the system image ships a newer version; removed this copy",
            entry.id, entry.version
        ));
    }
    Ok(())
}

fn update_one(
    target: &Target<'_>,
    entry: &InstalledPackage,
    request: &UpdateRequest<'_>,
    said: &mut Said,
) -> Result<installer::Outcome, Error> {
    // The candidate is a local file or the configured registries' offer; either way the installer
    // verifies it before anything is stored. A registry serves thin packages: their payload comes
    // from the node that served them, and only the blobs the store lacks are fetched.
    let (staged, candidate, trust, provider) = if let Some((path, source)) = request.from {
        (
            None,
            installer::Candidate::staged(path, source),
            installer_trust(request.trust),
            None,
        )
    } else {
        let offered = offered::fetch_from_registries(entry, request.version_req, request.trust)?;
        let candidate = installer::Candidate::staged(offered.staged.path(), offered.source);
        (
            Some(offered.staged),
            candidate,
            installer_trust(&offered.trust),
            offered.payload,
        )
    };
    let payload: Option<&dyn BlobFetcher> = match (&provider, &request.payload_from) {
        (Some(provider), _) => Some(provider),
        (None, Some(server)) => Some(server),
        (None, None) => None,
    };
    let report = installer::update(
        target.store,
        target.catalog,
        &entry.id,
        &candidate,
        &installer::UpdateRequest {
            trust,
            origin: if request.from.is_some() {
                installer::Origin::LocalFile
            } else {
                installer::Origin::Registry
            },
            allow_downgrade: request.allow_downgrade,
            ignore_missing_runtime: request.ignore_missing_runtime,
            dry_run: request.dry_run,
        },
        payload,
    );
    drop(staged);
    let report = report?;
    let outcome = report.outcome;
    said.take(report);
    Ok(outcome)
}

/// Uninstall the person's copy of `app`, deciding about its saved data separately: the payload is
/// the publisher's, reproducible by reinstalling; the per-workspace data is the person's and does not
/// come back, so data is never deleted unless the person said so. A pinned package goes only with
/// `force`.
pub fn uninstall(
    target: &Target<'_>,
    app: &str,
    saved_data: SavedData,
    dry_run: bool,
    force: bool,
    said: &mut impl SaysChanges,
) -> Result<(), Error> {
    // Only the person's copy can go; removing it puts the image's back.
    let layers = target
        .catalog
        .layers(app)
        .map_err(|e| Error::RegistryRead(e.to_string()))?;
    let Some(entry) = layers.user.clone() else {
        return Err(match layers.image {
            Some(_) => Error::ImageApp(app.into()),
            None => Error::NotInstalled(app.into()),
        });
    };
    let data_home = data_home_root()?;
    let workspaces = package_store::state::find(&data_home, app);
    let bin = target.store.layout().bin();

    if dry_run {
        said.say(format!("plan: uninstall {} {}", entry.id, entry.version));
        if let Some(image) = &layers.image {
            said.say(format!(
                "  then runs : {} from the system image",
                image.version
            ));
        }
        said.say(format!(
            "  payload   : remove {}",
            entry.payload_root.display()
        ));
        said.say("  registry  : drop the entry");
        if entry.cli_commands.is_empty() {
            said.say("  shims     : none");
        } else {
            said.say(format!(
                "  shims     : remove {} from {}",
                entry.cli_commands.join(", "),
                bin.display()
            ));
        }
        said.say(format!(
            "  content   : evict the blobs no other package uses from {}",
            target.store.layout().content().display()
        ));
        for line in package_store::state::summary(&workspaces) {
            said.say(line);
        }
        said.say(format!(
            "  state     : {}",
            match saved_data {
                SavedData::Delete => "DELETE (--purge)",
                SavedData::Keep => "keep (--keep-state)",
                SavedData::Ask => "you would be asked",
            }
        ));
        said.say("(nothing was removed)");
        return Ok(());
    }

    let removed = target.store.remove(app, force).map_err(store_error)?;
    if !entry.cli_commands.is_empty() {
        shim::remove_shims(&bin, &entry.id, &entry.cli_commands)
            .map_err(|e| Error::Shim(e.to_string()))?;
        said.say(format!(
            "  removed {} PATH shim(s) from {}",
            entry.cli_commands.len(),
            bin.display()
        ));
    }
    said.say(format!("uninstalled: {app} {}", entry.version));
    if let Some(image) = &layers.image {
        said.say(format!(
            "  now runs : {} {} from the system image",
            image.id, image.version
        ));
    }
    said.say(format!(
        "  content  : evicted {} blob(s) no other package uses",
        removed.evicted_blobs
    ));
    if saved_data == SavedData::Delete {
        let (removed, failures) = package_store::state::remove(&workspaces);
        said.say(format!("  state    : deleted in {removed} workspace(s)"));
        for (path, e) in failures {
            said.complain(format!("could not remove {}: {e}", path.display()));
        }
    } else if !workspaces.is_empty() {
        said.say(format!(
            "  state    : kept — {} workspace(s), {} under {}",
            workspaces.len(),
            package_store::state::human_bytes(package_store::state::total_bytes(&workspaces)),
            data_home
                .join(package_store::state::WORKSPACES_DIR)
                .display()
        ));
        said.say("             reinstalling picks it back up; `--purge` deletes it.");
    }
    Ok(())
}

/// Where a change's words go: a report the terminal shows, or nowhere a person reads (a Store
/// removal, whose answer is only whether it worked).
pub trait SaysChanges {
    fn say(&mut self, line: impl Into<String>);
    fn complain(&mut self, warning: impl Into<String>);
}

impl SaysChanges for Said {
    fn say(&mut self, line: impl Into<String>) {
        self.line(line);
    }

    fn complain(&mut self, warning: impl Into<String>) {
        self.warn(warning);
    }
}

/// Words nobody reads: what a change says is logged instead.
pub struct Logged;

impl SaysChanges for Logged {
    fn say(&mut self, _line: impl Into<String>) {}

    fn complain(&mut self, warning: impl Into<String>) {
        tracing::warn!(warning = %warning.into(), "package store change");
    }
}

fn runtime_bundles() -> Result<runtime_bundle::gc::BundleRoot, Error> {
    runtime_bundle::gc::BundleRoot::from_env().map_err(|e| Error::RuntimeBundle(e.to_string()))
}

/// Collect after an install, update or uninstall that began at `started` (Unix seconds): reclaim
/// what it left unneeded, keeping every app it wrote, which is about to be opened. Never fails the
/// change it follows, and says what it did as a warning, so the change's own lines stay as scripts
/// read them.
fn collect_after(target: &Target<'_>, started: u64, said: &mut Said) {
    let written = match package_store::gc::written_since(target.store, started) {
        Ok(written) => written,
        Err(error) => {
            said.warn(format!("warning: nothing reclaimed: {error}"));
            return;
        }
    };
    match collect_after_change(target, written) {
        Ok(report) if report.freed_bytes() > 0 || !report.evicted.is_empty() => said.warn(format!(
            "reclaimed {} ({} runtime build(s), {} app(s) evicted)",
            human_bytes(report.freed_bytes()),
            report.runtimes.removed.len(),
            report.evicted.len()
        )),
        Ok(_) => {}
        Err(error) => said.warn(format!("warning: nothing reclaimed: {error}")),
    }
}

/// What runs after every install, update and uninstall, and at every start of the daemon (a new
/// image or a swapped userspace changes the system store without any install running): the store's
/// size cap, payload trees and staging nothing names, and runtime builds no installed app runs any
/// more. A tree a launch holds stays, and so do the apps `keep` names.
///
/// # Errors
///
/// The first step that failed; the steps before it took effect.
pub fn collect_after_change(
    target: &Target<'_>,
    keep: Vec<String>,
) -> Result<package_store::gc::Report, String> {
    let request = package_store::gc::Request::after_change(target.max_store_bytes).keeping(keep);
    match runtime_bundles() {
        Ok(runtimes) => {
            package_store::gc::collect(target.store, target.catalog, &runtimes, request)
        }
        Err(error) => {
            tracing::warn!(%error, "no runtime bundle root; applying the size cap only");
            let keep: Vec<&str> = request.keep.iter().map(String::as_str).collect();
            target
                .store
                .sweep_keeping(target.max_store_bytes, &keep)
                .map(|_| package_store::gc::Report::default())
                .map_err(|error| error.to_string())
        }
    }
}

fn human_bytes(bytes: u64) -> String {
    package_store::state::human_bytes(bytes)
}

fn collect(
    target: &Target<'_>,
    request: package_store::gc::Request,
    said: &mut Said,
) -> Result<(), Error> {
    let dry_run = request.dry_run;
    let max_store_bytes = request.max_store_bytes;
    let report =
        package_store::gc::collect(target.store, target.catalog, &runtime_bundles()?, request)
            .map_err(Error::Gc)?;
    let runtimes = &report.runtimes;
    if runtimes.busy {
        said.line("runtimes: an install is running; collected by the next change");
    }
    for id in &report.evicted {
        said.line(format!(
            "evicted  : {id} (store over {max_store_bytes} bytes)"
        ));
    }
    for path in &report.orphans.removed {
        said.line(format!(
            "removed  : {} (named by no package)",
            path.display()
        ));
    }
    if report.orphans.staging_bytes > 0 {
        said.line(format!(
            "removed  : {} of staging crashed installs left",
            human_bytes(report.orphans.staging_bytes)
        ));
    }
    for (version, bytes) in &runtimes.removed {
        let verb = if dry_run { "would go" } else { "removed " };
        said.line(format!(
            "{verb} : runtime {version} ({})",
            human_bytes(*bytes)
        ));
    }
    for (version, why) in &runtimes.kept {
        let why = match why {
            runtime_bundle::gc::Kept::Referenced => "an installed package runs on it",
            runtime_bundle::gc::Kept::Pinned => "pinned",
            runtime_bundle::gc::Kept::Leased => "installed within the grace period",
            runtime_bundle::gc::Kept::InUse => "a running app has it mounted",
            runtime_bundle::gc::Kept::Launching => "an app launched on it holds it",
        };
        said.line(format!("kept     : runtime {version} ({why})"));
    }
    let verb = if dry_run { "would free" } else { "freed" };
    said.line(format!("{verb}: {}", human_bytes(report.freed_bytes())));
    Ok(())
}

/// The images the person's store at `root` holds, with the system stores' beside them and the
/// publishers the device trusts for images.
///
/// # Errors
///
/// An unreadable image trust file.
pub fn image_store(root: &Path) -> Result<package_store::Store, Error> {
    let mut store = package_store::Store::new(PackageStore::at(root));
    let system = package_store::SystemStore::from_env();
    if let Some(system) = &system {
        store = store
            .with_system(package_store::Layout::new(&system.data_dir))
            .with_system(package_store::Layout::new(&system.payload_dir));
    }
    let trust = package_store::ImageTrust::from_env(system.as_ref()).map_err(store_error)?;
    Ok(store.with_trust(trust))
}

fn add_image(
    target: &Target<'_>,
    tapp: &Path,
    source: &str,
    publisher: Option<&str>,
    payload_from: Option<String>,
    said: &mut Said,
) -> Result<(), Error> {
    let store = image_store(target.root())?;
    let mut trust = store.trust().clone();
    if let Some(publisher) = publisher {
        trust = trust
            .with_publisher(decode_hex_32(publisher).map_err(Error::InvalidRequiredPublisher)?);
    }
    let payload = payload_from.map(app_registry::HttpBlobs::new);
    let file = std::io::BufReader::new(fs::File::open(tapp)?);
    let meta = store
        .user()
        .install_image(
            file,
            |meta| {
                let attested_by = trust.check(meta).map_err(|error| error.to_string())?;
                Ok(Admission {
                    pinned: false,
                    attested_by,
                    source: source.to_owned(),
                    held: false,
                })
            },
            payload.as_ref().map(|fetcher| fetcher as &dyn BlobFetcher),
        )
        .map_err(store_error)?;
    said.line(format!(
        "{} {} {} {}",
        meta.id,
        meta.manifest.package.kind,
        meta.manifest.package.id,
        meta.manifest.package.version
    ));
    Ok(())
}

fn install_runtime(
    version: &str,
    from: Option<&Path>,
    arch: Option<&str>,
    identity: Option<&Path>,
    force: bool,
    said: &mut Said,
) -> Result<(), Error> {
    let dest = runtime_bundle::default_bundle_path(version)
        .map_err(|e| Error::RuntimeBundle(e.to_string()))?;
    if dest.join(runtime_bundle::BUNDLE_MANIFEST_FILENAME).exists() && !force {
        return Err(Error::RuntimeBundleExists(version.to_string()));
    }
    let bundles = runtime_bundles()?;
    let _collection_held_off = bundles.install_lock()?;
    if let Some(replaced) = bundles.retire(version)? {
        bundles.purge(&replaced, &package_store::gc::in_use_now())?;
    }
    // Built on this device on purpose, from no package: kept until removed.
    bundles.pin(version)?;
    fs::create_dir_all(&dest)?;
    let manifest = runtime_bundle::build_bundle_with(
        &dest,
        version,
        &runtime_bundle::BuildOptions {
            from,
            arch,
            identity,
            extra_libs: &[],
            extra_bins: &[],
        },
    )
    .map_err(|e| Error::RuntimeBundle(e.to_string()))?;
    said.line(format!("installed runtime bundle: tilo-runtime/{version}"));
    said.line(format!("  path     : {}", dest.display()));
    said.line(format!("  arch     : {}", manifest.arch));
    said.line(format!("  files    : {}", manifest.files.len()));
    said.line(format!("  root-hash: blake3:{}", manifest.root_hash));
    if let Some(pk) = &manifest.publisher_hex {
        said.line(format!("  publisher: ed25519:{pk}"));
    }
    let mut report = installer::Report::default();
    installer::seal_runtime_bundle(&dest, &manifest, &mut report);
    said.take(report);
    Ok(())
}

fn remove_runtime(version: &str, said: &mut Said) -> Result<(), Error> {
    let bundles = runtime_bundles()?;
    let _collection_held_off = bundles.install_lock()?;
    bundles.unpin(version)?;
    // Out of the version's directory in one rename first: a removal stopped halfway would otherwise
    // leave a bundle a launch picks.
    let trashed = bundles
        .retire(version)?
        .ok_or_else(|| Error::RuntimeBundleNotInstalled(version.to_string()))?;
    if bundles.purge(&trashed, &package_store::gc::in_use_now())? {
        said.line(format!("removed runtime bundle: tilo-runtime/{version}"));
    } else {
        said.line(format!(
            "removed runtime bundle: tilo-runtime/{version}; a running app still has it \
             mounted, so its files go once that app closes (`tilo-install gc`)"
        ));
    }
    Ok(())
}

fn pin_runtime(version: &str, pinned: bool, said: &mut Said) -> Result<(), Error> {
    let bundles = runtime_bundles()?;
    if !bundles
        .bundle(version)
        .join(runtime_bundle::BUNDLE_MANIFEST_FILENAME)
        .exists()
    {
        return Err(Error::RuntimeBundleNotInstalled(version.to_string()));
    }
    if pinned {
        bundles.pin(version)?;
        said.line(format!("pinned runtime bundle: tilo-runtime/{version}"));
    } else {
        bundles.unpin(version)?;
        said.line(format!("unpinned runtime bundle: tilo-runtime/{version}"));
    }
    Ok(())
}

fn add_attestor(id: &str, pubkey_hex: &str, scope: &str, said: &mut Said) -> Result<(), Error> {
    // Validate scope and pubkey before touching the file.
    let scope_parsed = AttestorScope::parse(scope).map_err(|e| Error::Trust(e.to_string()))?;
    let _ = decode_hex_32(pubkey_hex).map_err(Error::Trust)?;
    let mut attestors = load_trusted_attestors()?;
    attestors.attestor.retain(|a| a.id != id);
    attestors.attestor.push(TrustedAttestor {
        id: id.into(),
        pubkey_hex: pubkey_hex.into(),
        scope: scope_parsed,
    });
    let path = trusted_attestors_path()?;
    attestors
        .save(&path)
        .map_err(|e| Error::Trust(e.to_string()))?;
    said.line(format!("trust: added {id} (scope: {scope})"));
    said.line(format!("  file: {}", path.display()));
    Ok(())
}

fn remove_attestor(id: &str, said: &mut Said) -> Result<(), Error> {
    let mut attestors = load_trusted_attestors()?;
    let before = attestors.attestor.len();
    attestors.attestor.retain(|a| a.id != id);
    if attestors.attestor.len() == before {
        return Err(Error::Trust(format!("no trusted attestor with id {id:?}")));
    }
    let path = trusted_attestors_path()?;
    attestors
        .save(&path)
        .map_err(|e| Error::Trust(e.to_string()))?;
    said.line(format!("trust: removed {id}"));
    Ok(())
}

fn revoke(pubkey_hex: &str, reason: &str, said: &mut Said) -> Result<(), Error> {
    let _ = decode_hex_32(pubkey_hex).map_err(Error::Trust)?;
    let mut list = load_revocation_list()?;
    list.revoked.retain(|k| k.pubkey_hex != pubkey_hex);
    list.revoked.push(RevokedKey {
        pubkey_hex: pubkey_hex.into(),
        reason: reason.into(),
    });
    let path = revoked_keys_path()?;
    list.save(&path).map_err(|e| Error::Trust(e.to_string()))?;
    said.line(format!("revoked: ed25519:{pubkey_hex}"));
    if !reason.is_empty() {
        said.line(format!("  reason: {reason}"));
    }
    said.line(format!("  file: {}", path.display()));
    Ok(())
}
