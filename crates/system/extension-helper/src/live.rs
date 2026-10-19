//! Live system updates: the staged image's changed components, applied now.
//!
//! ```text
//! tilo-extension live:system    apply what needs no desktop restart
//! tilo-extension live:session   apply everything a desktop restart can,
//!                               then restart the desktops in place
//! tilo-extension live:status    print the state the desktop reads
//! ```
//!
//! The update service stages the newest image at least daily
//! (`bootc-fetch-apply-updates.timer`). bootc has then verified it (the
//! containers policy's Cosign gate and, when armed, the owner's endorsement)
//! and laid it down as a deployment, which normally applies at the next boot.
//! This applies it now instead, component by component, with the same
//! machinery a developer's `tilo-vm swap` uses: one system extension over
//! `/usr` and the restarts each component's scope asks for
//! (`/usr/share/tilo/components.toml`).
//!
//! 1. The staged and the booted deployments are opened read-only: on the
//!    sealed composefs layout each is mounted from its composefs image with
//!    the fs-verity digest bootc reports, so what is read is exactly the
//!    verified image; on the ostree layout each is its deployment checkout.
//! 2. Their `/usr` trees are compared, and every difference is classified by
//!    the staged image's component manifest: a system component (`vm` scope:
//!    system services, the greeter), a session component (the compositor,
//!    the launcher, the user services), the image's apps and runtime, the
//!    base OS, or what only a boot changes (kernel, modules, firmware,
//!    bootloader, initramfs).
//! 3. `live:system` builds a layer of everything but the session components
//!    (and the apps, when the interface versions the image serves changed,
//!    since those apps speak the new ones), merges it through
//!    [`crate::activate`], and try-restarts the changed system components'
//!    units, in the system manager and in every logged-in user's. The greeter
//!    is never restarted under a login; it runs the new build next time it
//!    starts. Apps and the runtime need no restart: they are new at their
//!    next start.
//! 4. `live:session` builds the whole layer, merges it, restarts the changed
//!    session components' user units and restarts every Tilo desktop in place
//!    (`tilo-session` restarts the compositor on SIGUSR1; the login stays and
//!    its windows reopen). The desktop asks for it only when the person is
//!    idle or the screen is locked, and at most as often as it asks for
//!    restarts (`shell::system_update`).
//!    A newer image staged over one already applied this boot is compared
//!    with what is live too ([`Plan::settle`]): a component whose files are
//!    already live as the newer image has them does not restart again, and
//!    what the desktop already runs stays in the system phase's layer.
//! 5. A unit that fails after its restart, or a desktop that does not come
//!    back, rolls the layer back to the one before, restarts the same parts
//!    again, and records the update as failed: it then applies with the next
//!    restart of the machine, which the desktop asks for.
//!
//! What only a boot can change never enters a layer; the state says a restart
//! is needed, and the desktop asks for one. `/usr/etc` and the rpm database
//! stay as booted until then too: `/etc` is merged at deployment, and the
//! rpm database names the base the downloaded extensions were built for.
//!
//! The layer lives on disk (`/var/lib/tilo/live-update/layers`), linked into
//! the sysext search path from `/run/extensions`, so it lasts exactly until
//! the next boot, which boots the staged deployment that holds it all
//! anyway. Every boot deletes the layers (tmpfiles), and every apply rebuilds
//! its layer from the mounted, verified image; nothing written earlier is
//! trusted.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read as _};
use std::os::unix::fs::{MetadataExt as _, symlink};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::host::{Host, run};
use crate::machine::Layout;
use crate::{At, Error, Result};

/// The extension's name in the sysext search path.
pub const LIVE_NAME: &str = "tilo-live-update";
/// The image's interface declaration: what apps speak with the desktop.
const INTERFACES: &str = "/usr/share/tilo/system-interfaces.toml";
/// The component manifest the image carries.
const COMPONENTS: &str = "usr/share/tilo/components.toml";
/// The greeter: restarting it would end the login it serves.
const GREETER_UNIT: &str = "greetd.service";
/// The sandbox's view of the system, bound from `/usr` when it starts.
const SANDBOX_VIEW_UNIT: &str = "tilo-sandbox-view.service";
/// How long a restarted unit must stay up before the update counts.
const SETTLE: Duration = Duration::from_secs(5);
/// How long a restarted desktop must stay up before the update counts.
const DESKTOP_STABLE: Duration = Duration::from_secs(20);
/// How long an old compositor may take to exit after SIGTERM.
const DESKTOP_EXIT: Duration = Duration::from_secs(30);
/// The process that runs a login's compositor and restarts it on SIGUSR1.
const LOGIN_WRAPPER: &str = "tilo-session";
/// The compositor's process name.
const COMPOSITOR: &str = "shell";

/// Paths only a boot changes: the kernel and its modules, firmware the
/// kernel loads, the bootloader and the initramfs.
const BOOT_PREFIXES: [&str; 7] = [
    "/usr/lib/modules/",
    "/usr/lib/firmware/",
    "/usr/lib/bootupd/",
    "/usr/lib/systemd/boot/",
    "/usr/lib/efi/",
    "/usr/lib/dracut/",
    "/usr/lib/ostree-boot/",
];
/// Paths that stay as booted until the next boot without needing one:
/// `/etc` is merged at deployment, os-release names the base systemd matches
/// extensions against, and the rpm database the base the downloaded
/// extensions were built for.
const NEXT_BOOT_PREFIXES: [&str; 3] =
    ["/usr/etc/", "/usr/lib/sysimage/rpm/", "/usr/lib/os-release"];

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// Where the layer is linked into the sysext search path.
pub fn link(layout: &Layout) -> PathBuf {
    layout.at("/run/extensions").join(LIVE_NAME)
}

/// Where layers are built.
pub fn layers(layout: &Layout) -> PathBuf {
    layout.at("/var/lib/tilo/live-update/layers")
}

/// The state the desktop reads.
pub fn state_path(layout: &Layout) -> PathBuf {
    layout.at("/run/tilo/live-update/state.json")
}

/// Present while a merge includes the layer: the next activation must
/// merge again even when nothing else is there, to take it off.
pub fn merged_marker(layout: &Layout) -> PathBuf {
    layout.at("/run/tilo/live-update/merged")
}

fn mounts(layout: &Layout) -> PathBuf {
    layout.at("/run/tilo/live-update/mnt")
}

/// Whether the layer is linked, from where this helper builds layers: the
/// one entry of the sysext search path it may merge besides its own parts.
pub fn linked(layout: &Layout) -> bool {
    let link = link(layout);
    fs::symlink_metadata(&link).is_ok_and(|meta| meta.file_type().is_symlink())
        && fs::read_link(&link).is_ok_and(|target| target.starts_with(layers(layout)))
}

// ---------------------------------------------------------------------------
// The command
// ---------------------------------------------------------------------------

/// Which part of an update a run applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    /// Everything that needs no desktop restart.
    System,
    /// Everything, with the desktops restarted in place.
    Session,
}

impl Phase {
    /// The phase a `live:<phase>` argument names.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "system" => Some(Self::System),
            "session" => Some(Self::Session),
            _ => None,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Session => "session",
        }
    }
}

/// `live:status`: the state, or that there is none.
pub fn status(layout: &Layout) -> i32 {
    let state = fs::read_to_string(state_path(layout)).unwrap_or_else(|_| "{}".to_owned());
    println!("{}", state.trim());
    0
}

/// Apply `phase` of the staged update. Returns the exit status: 0 applied
/// (or nothing to apply), 1 failed (and rolled back).
///
/// # Errors
/// A failure before anything changed: no layer was merged.
pub fn apply(host: &dyn Host, live: &dyn LiveHost, layout: &Layout, phase: Phase) -> Result<i32> {
    let (booted, staged) = deployments(&live.bootc_status()?)?;
    let Some(staged) = staged else {
        // Nothing staged: what is booted is current. A layer of an image that
        // is no longer staged would be stale; take it off.
        if linked(layout) {
            remove_link(layout)?;
            crate::activate(host, layout)?;
        }
        write_state(layout, &State::current())?;
        return Ok(0);
    };
    let previous = read_state(layout).filter(|state| state.image == staged.image());
    if previous.as_ref().is_some_and(|state| {
        state.failed
            || state.applied >= Some(phase)
            || (phase == Phase::Session
                && state.applied == Some(Phase::System)
                && !state.desktop_pending)
    }) {
        return Ok(0);
    }
    let system_applied = previous
        .as_ref()
        .is_some_and(|state| state.applied == Some(Phase::System));
    let booted = booted.ok_or_else(|| Error::pack("bootc reports no booted deployment"))?;
    let mounts = mounts(layout);
    let base_root = live.open(layout, &booted, &mounts.join("booted"))?;
    let new_root = match live.open(layout, &staged, &mounts.join("staged")) {
        Ok(root) => root,
        Err(error) => {
            live.close(&mounts.join("booted"));
            return Err(error);
        }
    };
    let outcome = apply_from(
        host,
        live,
        layout,
        Target {
            phase,
            staged: &staged,
            system_applied,
        },
        &base_root,
        &new_root,
    );
    live.close(&mounts.join("staged"));
    live.close(&mounts.join("booted"));
    outcome
}

/// What one run applies.
#[derive(Clone, Copy)]
struct Target<'a> {
    phase: Phase,
    staged: &'a Deployment,
    /// The system phase of the same image already ran.
    system_applied: bool,
}

/// [`apply`] with both deployments open.
fn apply_from(
    host: &dyn Host,
    live: &dyn LiveHost,
    layout: &Layout,
    target: Target<'_>,
    base_root: &Path,
    new_root: &Path,
) -> Result<i32> {
    let Target {
        phase,
        staged,
        system_applied,
    } = target;
    let manifest_path = new_root.join(COMPONENTS);
    let manifest = fs::read_to_string(&manifest_path).at(&manifest_path)?;
    let components = Components::parse(&manifest)?;
    let changes = diff(base_root, new_root).os()?;
    let mut plan = Plan::new(changes, &components);
    let before = fs::read_link(link(layout)).ok();
    if let Some(previous) = before.as_deref().filter(|previous| previous.is_dir()) {
        plan.settle(
            &components,
            &LiveTree {
                layer: Some(previous),
                base: base_root,
                live,
            },
            new_root,
        )?;
    }
    let name = layer_name(staged.image(), phase);
    let layer = layers(layout).join(&name);
    build_layer(host, live, &plan, phase, new_root, &layer, staged.image())?;
    swap_link(layout, &layer)?;
    let mut state = State {
        image: staged.image().to_owned(),
        applied: Some(phase),
        desktop_pending: phase == Phase::System && plan.desktop_pending(),
        reboot_required: plan.reboot,
        failed: false,
        deferred: Vec::new(),
        detail: String::new(),
    };
    let restarts = plan.restarts(phase, system_applied);
    let outcome = crate::activate(host, layout)
        .and_then(|code| {
            if code == 0 {
                Ok(())
            } else {
                Err(Error::pack(
                    "the system extensions could not be merged with the update",
                ))
            }
        })
        .and_then(|()| restart(live, &restarts, &mut state.deferred))
        .and_then(|()| {
            if phase == Phase::Session {
                restart_desktops(live)
            } else {
                Ok(())
            }
        });
    live.start_no_block(SANDBOX_VIEW_UNIT);
    match outcome {
        Ok(()) => {
            prune_layers(layout, &layer);
            write_state(layout, &state)?;
            Ok(0)
        }
        Err(error) => {
            eprintln!("tilo-extension: live update failed, rolling back: {error}");
            roll_back(host, live, layout, before.as_deref(), &restarts, phase);
            state.failed = true;
            state.applied = None;
            state.desktop_pending = false;
            state.reboot_required = true;
            state.detail = error.to_string();
            write_state(layout, &state)?;
            Ok(1)
        }
    }
}

/// Restart `restarts`; a greeter under a login is deferred. Every restarted
/// unit must still be up after [`SETTLE`].
fn restart(live: &dyn LiveHost, restarts: &Restarts, deferred: &mut Vec<String>) -> Result<()> {
    live.daemon_reload()?;
    let logins = live.logins();
    let mut restarted = Vec::new();
    for unit in &restarts.units {
        if unit == GREETER_UNIT && !logins.is_empty() {
            deferred.push(unit.clone());
            continue;
        }
        live.try_restart(None, unit)?;
        restarted.push((None, unit.clone()));
    }
    let users: BTreeSet<&str> = logins.iter().map(|login| login.user.as_str()).collect();
    for user in users {
        live.reload_user(user)?;
        for unit in &restarts.user_units {
            live.try_restart(Some(user), unit)?;
            restarted.push((Some(user.to_owned()), unit.clone()));
        }
    }
    if restarted.is_empty() {
        return Ok(());
    }
    live.sleep(SETTLE);
    let failed: Vec<String> = restarted
        .iter()
        .filter(|(user, unit)| live.unit_failed(user.as_deref(), unit))
        .map(|(user, unit)| match user {
            Some(user) => format!("{unit} ({user})"),
            None => unit.clone(),
        })
        .collect();
    if failed.is_empty() {
        Ok(())
    } else {
        Err(Error::Pack(format!(
            "failed after the update: {}",
            failed.join(", ")
        )))
    }
}

/// Restart every Tilo desktop in place, and wait until each is back.
fn restart_desktops(live: &dyn LiveHost) -> Result<()> {
    let logins = live.logins();
    for login in &logins {
        live.restart_desktop(login)?;
    }
    for login in &logins {
        if !live.desktop_back(login, DESKTOP_STABLE) {
            return Err(Error::Pack(format!(
                "{}'s desktop did not come back after the update",
                login.user
            )));
        }
    }
    Ok(())
}

/// Put back the layer that was linked before, or none, merge that, and
/// restart the same parts again.
fn roll_back(
    host: &dyn Host,
    live: &dyn LiveHost,
    layout: &Layout,
    before: Option<&Path>,
    restarts: &Restarts,
    phase: Phase,
) {
    let relinked = match before {
        Some(previous) if previous.is_dir() => swap_link(layout, previous),
        _ => remove_link(layout),
    };
    if let Err(error) = relinked.and_then(|()| crate::activate(host, layout).map(drop)) {
        eprintln!("tilo-extension: live update rollback could not merge: {error}");
    }
    let mut deferred = Vec::new();
    if let Err(error) = restart(live, restarts, &mut deferred) {
        eprintln!("tilo-extension: live update rollback restart: {error}");
    }
    if phase == Phase::Session
        && let Err(error) = restart_desktops(live)
    {
        eprintln!("tilo-extension: live update rollback desktop: {error}");
    }
    live.start_no_block(SANDBOX_VIEW_UNIT);
}

// ---------------------------------------------------------------------------
// Deployments
// ---------------------------------------------------------------------------

/// A deployment's root, as bootc reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Deployment {
    /// The sealed layout: a composefs image named by its fs-verity digest.
    Composefs { image: String, verity: String },
    /// The ostree layout: a deployment checkout.
    Ostree {
        image: String,
        stateroot: String,
        checksum: String,
        serial: u64,
    },
}

impl Deployment {
    /// The image digest it was deployed from.
    pub fn image(&self) -> &str {
        match self {
            Self::Composefs { image, .. } | Self::Ostree { image, .. } => image,
        }
    }
}

/// The booted and the staged deployment in `bootc status --format json`.
///
/// # Errors
/// Output that is not bootc's status, or a deployment whose identity is
/// malformed.
pub fn deployments(json: &str) -> Result<(Option<Deployment>, Option<Deployment>)> {
    let status: Value = serde_json::from_str(json)
        .map_err(|error| Error::Pack(format!("malformed bootc status: {error}")))?;
    let one = |key: &str| -> Result<Option<Deployment>> {
        match status.get("status").and_then(|status| status.get(key)) {
            None | Some(Value::Null) => Ok(None),
            Some(entry) => deployment(entry).map(Some),
        }
    };
    Ok((one("booted")?, one("staged")?))
}

fn deployment(entry: &Value) -> Result<Deployment> {
    let image = entry
        .get("image")
        .and_then(|image| image.get("imageDigest"))
        .and_then(Value::as_str)
        .filter(|digest| is_digest(digest, "sha256:", 64))
        .ok_or_else(|| Error::pack("a bootc deployment has no image digest"))?
        .to_owned();
    if let Some(verity) = entry
        .get("composefs")
        .and_then(|composefs| composefs.get("verity"))
        .and_then(Value::as_str)
    {
        let verity = verity.strip_prefix("sha512:").unwrap_or(verity);
        if !is_hex(verity, 128) && !is_hex(verity, 64) {
            return Err(Error::pack(
                "a bootc deployment has a malformed composefs digest",
            ));
        }
        return Ok(Deployment::Composefs {
            image,
            verity: verity.to_owned(),
        });
    }
    let ostree = entry
        .get("ostree")
        .ok_or_else(|| Error::pack("a bootc deployment is neither composefs nor ostree"))?;
    let text = |key: &str| ostree.get(key).and_then(Value::as_str).unwrap_or_default();
    let checksum = text("checksum");
    let stateroot = text("stateroot");
    let serial = ostree
        .get("deploySerial")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let stateroot_ok = !stateroot.is_empty()
        && stateroot
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !stateroot.starts_with('.');
    if !is_hex(checksum, 64) || !stateroot_ok {
        return Err(Error::pack(
            "a bootc deployment has a malformed ostree identity",
        ));
    }
    Ok(Deployment::Ostree {
        image,
        stateroot: stateroot.to_owned(),
        checksum: checksum.to_owned(),
        serial,
    })
}

fn is_hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_digest(text: &str, prefix: &str, len: usize) -> bool {
    text.strip_prefix(prefix)
        .is_some_and(|hex| is_hex(hex, len))
}

// ---------------------------------------------------------------------------
// The component manifest
// ---------------------------------------------------------------------------

/// What the plan needs of one `[[component]]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    pub name: String,
    pub scope: String,
    pub units: Vec<String>,
    pub user_units: Vec<String>,
    /// Absolute paths; one ending in `/` is a directory tree.
    pub files: Vec<String>,
}

/// The image's component manifest (`os/components.toml`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Components(pub Vec<Component>);

impl Components {
    /// Read the manifest.
    ///
    /// # Errors
    /// Text that is not the manifest.
    pub fn parse(text: &str) -> Result<Self> {
        let table: toml::Table = text
            .parse()
            .map_err(|error| Error::Pack(format!("malformed component manifest: {error}")))?;
        let strings = |value: Option<&toml::Value>| -> Vec<String> {
            value
                .and_then(toml::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(toml::Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut components = Vec::new();
        for entry in table
            .get("component")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(name) = entry.get("name").and_then(toml::Value::as_str) else {
                continue;
            };
            let files = entry
                .get("files")
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|file| file.get("to").and_then(toml::Value::as_str))
                .map(str::to_owned)
                .collect();
            components.push(Component {
                name: name.to_owned(),
                scope: entry
                    .get("scope")
                    .and_then(toml::Value::as_str)
                    .unwrap_or("session")
                    .to_owned(),
                units: strings(entry.get("units")),
                user_units: strings(entry.get("user_units")),
                files,
            });
        }
        if components.is_empty() {
            return Err(Error::pack("the component manifest names no component"));
        }
        Ok(Self(components))
    }

    /// The component that installs `path`.
    pub fn owner(&self, path: &str) -> Option<&Component> {
        self.0.iter().find(|component| {
            component.files.iter().any(|to| {
                if to.ends_with('/') {
                    path.starts_with(to.as_str()) || path == to.trim_end_matches('/')
                } else {
                    path == to
                }
            })
        })
    }
}

// ---------------------------------------------------------------------------
// The plan
// ---------------------------------------------------------------------------

/// What a changed path is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Only a boot changes it: a restart of the machine is needed.
    Boot,
    /// Stays as booted until the next boot, which needs no asking.
    NextBoot,
    /// A system component's: its units restart.
    System,
    /// The base OS: new for what starts after the merge.
    Base,
    /// The image's apps and runtime: new at their next start.
    Apps,
    /// A session component's, or the interface declaration: the desktop
    /// restarts.
    Session,
}

/// One difference between the booted and the staged `/usr`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Absolute, e.g. `/usr/bin/shell`.
    pub path: String,
    pub kind: ChangeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// New or different in the staged image.
    Written,
    /// Gone from the staged image.
    Removed,
}

/// Every change, classified, and what each phase restarts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts the classification found about one update, not states of one machine"
)]
pub struct Plan {
    pub changes: Vec<(Change, Class)>,
    /// A restart of the machine is needed for part of the update.
    pub reboot: bool,
    /// The interface versions the image serves changed.
    pub interfaces_changed: bool,
    /// Changed components, by name, with their restarts.
    pub system: BTreeMap<String, Restarts>,
    pub session: BTreeMap<String, Restarts>,
    pub apps_changed: bool,
    /// Changes an earlier live update of this boot already applied: their
    /// paths are live as the staged image has them ([`Plan::settle`]).
    pub held: BTreeSet<String>,
    /// A session component's path an earlier live update changed and this
    /// one puts back as booted: the desktop restarts for it.
    pub session_undone: bool,
}

/// Units a change restarts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Restarts {
    pub units: Vec<String>,
    pub user_units: Vec<String>,
}

impl Restarts {
    fn add(&mut self, other: &Self) {
        for unit in &other.units {
            if !self.units.contains(unit) {
                self.units.push(unit.clone());
            }
        }
        for unit in &other.user_units {
            if !self.user_units.contains(unit) {
                self.user_units.push(unit.clone());
            }
        }
    }
}

/// How `path` is applied, given the component that installs it.
pub fn classify(path: &str, owner: Option<&Component>) -> Class {
    if BOOT_PREFIXES.iter().any(|prefix| path.starts_with(prefix)) {
        return Class::Boot;
    }
    if NEXT_BOOT_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        return Class::NextBoot;
    }
    if path == INTERFACES {
        return Class::Session;
    }
    match owner {
        Some(component) if component.scope == "vm" => Class::System,
        Some(component) if component.name == "apps+runtime" => Class::Apps,
        Some(_) => Class::Session,
        None => Class::Base,
    }
}

impl Plan {
    /// Classify `changes` by `components`.
    pub fn new(changes: Vec<Change>, components: &Components) -> Self {
        let mut plan = Self::default();
        for change in changes {
            let owner = components.owner(&change.path);
            let class = classify(&change.path, owner);
            match class {
                Class::Boot => plan.reboot = true,
                Class::Apps => plan.apps_changed = true,
                Class::NextBoot | Class::Base => {}
                Class::System | Class::Session => {
                    if let Some(component) = owner {
                        let restarts = Restarts {
                            units: component.units.clone(),
                            user_units: component.user_units.clone(),
                        };
                        let bucket = if class == Class::System {
                            &mut plan.system
                        } else {
                            &mut plan.session
                        };
                        bucket.insert(component.name.clone(), restarts);
                    }
                }
            }
            if change.path == INTERFACES {
                plan.interfaces_changed = true;
            }
            plan.changes.push((change, class));
        }
        plan
    }

    /// Account for the live update of an earlier image this boot already
    /// applied (`before`: its layer over the booted tree): a component
    /// whose every change it already holds does not restart again, a change
    /// it holds goes into either phase's layer (so the system phase does not
    /// take back what the desktop already runs), and a component whose path
    /// it changed but the staged image has as booted restarts.
    ///
    /// # Errors
    /// A tree that cannot be read.
    pub fn settle(
        &mut self,
        components: &Components,
        before: &LiveTree<'_>,
        new_root: &Path,
    ) -> Result<()> {
        for (change, _) in &self.changes {
            let relative = change.path.trim_start_matches('/');
            let staged = match change.kind {
                ChangeKind::Written => Some(new_root.join(relative)),
                ChangeKind::Removed => None,
            };
            if same_live(before.entry(relative).as_deref(), staged.as_deref()).os()? {
                self.held.insert(change.path.clone());
            }
        }
        for bucket in [&mut self.system, &mut self.session] {
            bucket.retain(|name, _| {
                self.changes.iter().any(|(change, _)| {
                    !self.held.contains(&change.path)
                        && components
                            .owner(&change.path)
                            .is_some_and(|owner| owner.name == *name)
                })
            });
        }
        let changed: BTreeSet<&str> = self
            .changes
            .iter()
            .map(|(change, _)| change.path.as_str())
            .collect();
        let Some(layer) = before.layer else {
            return Ok(());
        };
        for path in layer_paths(layer).os()? {
            if changed.contains(path.as_str()) {
                continue;
            }
            let relative = path.trim_start_matches('/');
            let staged = new_root.join(relative);
            let staged = fs::symlink_metadata(&staged).is_ok().then_some(staged);
            if same_live(before.entry(relative).as_deref(), staged.as_deref()).os()? {
                continue;
            }
            let owner = components.owner(&path);
            let class = classify(&path, owner);
            if class == Class::Session {
                self.session_undone = true;
            }
            if let Some(component) =
                owner.filter(|_| matches!(class, Class::System | Class::Session))
            {
                let bucket = if class == Class::System {
                    &mut self.system
                } else {
                    &mut self.session
                };
                bucket.insert(
                    component.name.clone(),
                    Restarts {
                        units: component.units.clone(),
                        user_units: component.user_units.clone(),
                    },
                );
            }
        }
        Ok(())
    }

    /// Whether `change`, of `class`, goes into `phase`'s layer: what the
    /// phase takes, and what an earlier live update already made live.
    pub fn takes(&self, change: &Change, class: Class, phase: Phase) -> bool {
        self.in_layer(class, phase)
            || (!matches!(class, Class::Boot | Class::NextBoot) && self.held.contains(&change.path))
    }

    /// Whether a change of `class` goes into `phase`'s layer.
    pub fn in_layer(&self, class: Class, phase: Phase) -> bool {
        match class {
            Class::Boot | Class::NextBoot => false,
            Class::System | Class::Base => true,
            // Apps built for new interface versions wait for the desktop
            // that speaks them.
            Class::Apps => phase == Phase::Session || !self.interfaces_changed,
            Class::Session => phase == Phase::Session,
        }
    }

    /// Whether part of the update waits for a desktop restart.
    pub fn desktop_pending(&self) -> bool {
        self.session_undone
            || self.changes.iter().any(|(change, class)| {
                !self.takes(change, *class, Phase::System)
                    && self.takes(change, *class, Phase::Session)
            })
    }

    /// What `phase` restarts besides the desktop; `system_applied` when
    /// the system phase already restarted the system components.
    pub fn restarts(&self, phase: Phase, system_applied: bool) -> Restarts {
        let mut restarts = Restarts::default();
        if !system_applied {
            for component in self.system.values() {
                restarts.add(component);
            }
        }
        if phase == Phase::Session {
            for component in self.session.values() {
                restarts.add(component);
            }
        }
        restarts
    }
}

// ---------------------------------------------------------------------------
// Comparing the trees
// ---------------------------------------------------------------------------

/// Every difference between `base/usr` and `new/usr`, in path order. A
/// directory gone from `new` is one removal; its contents are not listed.
///
/// # Errors
/// A tree that cannot be read.
pub fn diff(base: &Path, new: &Path) -> io::Result<Vec<Change>> {
    let mut changes = Vec::new();
    walk_new(&base.join("usr"), &new.join("usr"), "/usr", &mut changes)?;
    walk_removed(&base.join("usr"), &new.join("usr"), "/usr", &mut changes)?;
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(changes)
}

fn entries(dir: &Path) -> io::Result<Vec<(String, fs::Metadata)>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        out.push((name, fs::symlink_metadata(entry.path())?));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn walk_new(base: &Path, new: &Path, at: &str, changes: &mut Vec<Change>) -> io::Result<()> {
    for (name, meta) in entries(new)? {
        let path = format!("{at}/{name}");
        let (base_path, new_path) = (base.join(&name), new.join(&name));
        let before = fs::symlink_metadata(&base_path).ok();
        if meta.is_dir() {
            // A directory that is new (or replaces a file, which an upper
            // directory shadows) is listed whole: nothing below it exists in
            // `base`.
            walk_new(&base_path, &new_path, &path, changes)?;
            continue;
        }
        if !(meta.is_file() || meta.file_type().is_symlink()) {
            continue;
        }
        let same = match &before {
            Some(old) => same_entry(&base_path, old, &new_path, &meta)?,
            None => false,
        };
        if !same {
            changes.push(Change {
                path,
                kind: ChangeKind::Written,
            });
        }
    }
    Ok(())
}

fn walk_removed(base: &Path, new: &Path, at: &str, changes: &mut Vec<Change>) -> io::Result<()> {
    for (name, meta) in entries(base)? {
        let path = format!("{at}/{name}");
        let (base_path, new_path) = (base.join(&name), new.join(&name));
        match fs::symlink_metadata(&new_path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => changes.push(Change {
                path,
                kind: ChangeKind::Removed,
            }),
            Err(error) => return Err(error),
            Ok(now) => {
                if meta.is_dir() && now.is_dir() {
                    walk_removed(&base_path, &new_path, &path, changes)?;
                }
            }
        }
    }
    Ok(())
}

/// Whether two files or links are the same: kind, mode, owner, size and
/// bytes (or link target).
fn same_entry(
    old_path: &Path,
    old: &fs::Metadata,
    new_path: &Path,
    new: &fs::Metadata,
) -> io::Result<bool> {
    if old.file_type().is_symlink() || new.file_type().is_symlink() {
        return Ok(old.file_type().is_symlink()
            && new.file_type().is_symlink()
            && fs::read_link(old_path)? == fs::read_link(new_path)?);
    }
    if old.mode() != new.mode()
        || old.uid() != new.uid()
        || old.gid() != new.gid()
        || old.len() != new.len()
    {
        return Ok(false);
    }
    same_bytes(old_path, new_path)
}

fn same_bytes(a: &Path, b: &Path) -> io::Result<bool> {
    let (mut a, mut b) = (fs::File::open(a)?, fs::File::open(b)?);
    let (mut left, mut right) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let read = a.read(&mut left)?;
        if read == 0 {
            return Ok(b.read(&mut right)? == 0);
        }
        let Some(want) = right.get_mut(..read) else {
            return Ok(false);
        };
        if b.read_exact(want).is_err() {
            return Ok(false);
        }
        if left.get(..read) != right.get(..read) {
            return Ok(false);
        }
    }
}

/// The live `/usr`: a layer merged over the booted tree.
pub struct LiveTree<'a> {
    pub layer: Option<&'a Path>,
    pub base: &'a Path,
    pub live: &'a dyn LiveHost,
}

impl LiveTree<'_> {
    /// What is live at `relative` (e.g. `usr/bin/fs`): the layer's entry,
    /// nothing under a whiteout, or else the booted tree's.
    pub fn entry(&self, relative: &str) -> Option<PathBuf> {
        if let Some(layer) = self.layer {
            let parts: Vec<&str> = relative.split('/').collect();
            let mut at = layer.to_path_buf();
            let last = parts.len().saturating_sub(1);
            for (index, part) in parts.iter().enumerate() {
                at.push(part);
                let Ok(meta) = fs::symlink_metadata(&at) else {
                    break;
                };
                if self.live.is_whiteout(&at) {
                    return None;
                }
                if index == last {
                    return Some(at);
                }
                if !meta.is_dir() {
                    break;
                }
            }
        }
        let base = self.base.join(relative);
        fs::symlink_metadata(&base).is_ok().then_some(base)
    }
}

/// Whether two live entries (`None`: absent) are the same.
fn same_live(a: Option<&Path>, b: Option<&Path>) -> io::Result<bool> {
    match (a, b) {
        (None, None) => Ok(true),
        (Some(a), Some(b)) => {
            let (old, new) = (fs::symlink_metadata(a)?, fs::symlink_metadata(b)?);
            if old.is_dir() || new.is_dir() {
                return Ok(old.is_dir() && new.is_dir());
            }
            same_entry(a, &old, b, &new)
        }
        _ => Ok(false),
    }
}

/// Every file, link and whiteout `layer` holds, as absolute paths
/// (`/usr/...`), but its extension-release.
fn layer_paths(layer: &Path) -> io::Result<Vec<String>> {
    fn walk(dir: &Path, at: &str, out: &mut Vec<String>) -> io::Result<()> {
        for (name, meta) in entries(dir)? {
            let path = format!("{at}/{name}");
            if meta.is_dir() {
                walk(&dir.join(&name), &path, out)?;
            } else {
                out.push(path);
            }
        }
        Ok(())
    }
    let release = format!("/usr/lib/extension-release.d/extension-release.{LIVE_NAME}");
    let mut out = Vec::new();
    let usr = layer.join("usr");
    if usr.is_dir() {
        walk(&usr, "/usr", &mut out)?;
    }
    out.retain(|path| *path != release);
    Ok(out)
}

// ---------------------------------------------------------------------------
// The layer
// ---------------------------------------------------------------------------

fn layer_name(image: &str, phase: Phase) -> String {
    let digest = image.strip_prefix("sha256:").unwrap_or(image);
    let short: String = digest.chars().take(16).collect();
    format!("{short}-{}", phase.name())
}

/// Build `phase`'s layer at `layer` from `new_root`: every change the phase
/// takes, copied with its mode, owner and extended attributes, a whiteout
/// for every removal, an extension-release naming it, and the `SELinux`
/// labels its files get under `/`.
///
/// # Errors
/// A file that cannot be copied; nothing is left at `layer` then.
pub fn build_layer(
    host: &dyn Host,
    live: &dyn LiveHost,
    plan: &Plan,
    phase: Phase,
    new_root: &Path,
    layer: &Path,
    image: &str,
) -> Result<()> {
    let parent = layer
        .parent()
        .ok_or_else(|| Error::pack("a layer has no parent directory"))?;
    fs::create_dir_all(parent).at(parent)?;
    let staging = parent.join(format!(
        ".staging-{}",
        layer.file_name().and_then(OsStr::to_str).unwrap_or("layer")
    ));
    remove_tree(&staging)?;
    let built = (|| {
        for (change, class) in &plan.changes {
            if !plan.takes(change, *class, phase) {
                continue;
            }
            let relative = change.path.trim_start_matches('/');
            let target = staging.join(relative);
            if let Some(dir) = target.parent() {
                fs::create_dir_all(dir).at(dir)?;
            }
            match change.kind {
                ChangeKind::Written => copy_entry(&new_root.join(relative), &target)?,
                ChangeKind::Removed => live.whiteout(&target)?,
            }
        }
        let release_dir = staging.join("usr/lib/extension-release.d");
        fs::create_dir_all(&release_dir).at(&release_dir)?;
        let digest = image.strip_prefix("sha256:").unwrap_or(image);
        let release = release_dir.join(format!("extension-release.{LIVE_NAME}"));
        fs::write(
            &release,
            format!("ID=_any\nSYSEXT_SCOPE=system\nTILO_LIVE_UPDATE={digest}\n"),
        )
        .at(&release)?;
        host.relabel(&staging)
    })();
    if let Err(error) = built {
        let _ = remove_tree(&staging);
        return Err(error);
    }
    remove_tree(layer)?;
    fs::rename(&staging, layer).at(layer)
}

/// Copy one file or link with everything `cp --archive` keeps.
fn copy_entry(from: &Path, to: &Path) -> Result<()> {
    let _ = fs::remove_file(to);
    run(
        &[
            OsStr::new("cp"),
            OsStr::new("--archive"),
            OsStr::new("--no-target-directory"),
            from.as_os_str(),
            to.as_os_str(),
        ],
        true,
    )
    .map(drop)
}

fn remove_tree(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::os(&error, Some(path))),
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path).at(path),
        Ok(_) => fs::remove_file(path).at(path),
    }
}

/// Link `layer` into the sysext search path, replacing what was linked in
/// one rename.
fn swap_link(layout: &Layout, layer: &Path) -> Result<()> {
    let link = link(layout);
    let dir = link
        .parent()
        .ok_or_else(|| Error::pack("the sysext link has no parent directory"))?;
    fs::create_dir_all(dir).at(dir)?;
    let next = dir.join(format!(".{LIVE_NAME}.next"));
    let _ = fs::remove_file(&next);
    symlink(layer, &next).at(&next)?;
    fs::rename(&next, &link).at(&link)
}

fn remove_link(layout: &Layout) -> Result<()> {
    let link = link(layout);
    match fs::remove_file(&link) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            Err(Error::os(&error, Some(&link)))
        }
        _ => Ok(()),
    }
}

/// Delete every layer but `keep`.
fn prune_layers(layout: &Layout, keep: &Path) {
    let Ok(entries) = fs::read_dir(layers(layout)) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path != keep {
            let _ = remove_tree(&path);
        }
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// What the desktop reads (`shell::system_update`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// The staged image this describes; empty when nothing is staged.
    pub image: String,
    /// The last phase applied; `None` before any, or after a failure.
    pub applied: Option<Phase>,
    /// Part of the update waits for a desktop restart.
    pub desktop_pending: bool,
    /// Part of the update waits for a restart of the machine.
    pub reboot_required: bool,
    /// The update could not be applied live; it applies with the next boot.
    pub failed: bool,
    /// Units that run their new build the next time they start.
    pub deferred: Vec<String>,
    pub detail: String,
}

impl State {
    /// Nothing staged: the booted image is current.
    fn current() -> Self {
        Self {
            image: String::new(),
            applied: None,
            desktop_pending: false,
            reboot_required: false,
            failed: false,
            deferred: Vec::new(),
            detail: String::new(),
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "schema": 1,
            "image": self.image,
            "applied": self.applied.map(Phase::name),
            "desktop_pending": self.desktop_pending,
            "reboot_required": self.reboot_required,
            "failed": self.failed,
            "deferred": self.deferred,
            "detail": self.detail,
        })
    }

    fn from_json(value: &Value) -> Option<Self> {
        if value.get("schema").and_then(Value::as_u64) != Some(1) {
            return None;
        }
        let flag = |key: &str| value.get(key).and_then(Value::as_bool).unwrap_or(false);
        Some(Self {
            image: value.get("image")?.as_str()?.to_owned(),
            applied: value
                .get("applied")
                .and_then(Value::as_str)
                .and_then(Phase::parse),
            desktop_pending: flag("desktop_pending"),
            reboot_required: flag("reboot_required"),
            failed: flag("failed"),
            deferred: value
                .get("deferred")
                .and_then(Value::as_array)
                .map(|units| {
                    units
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            detail: value
                .get("detail")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        })
    }
}

fn read_state(layout: &Layout) -> Option<State> {
    let text = fs::read_to_string(state_path(layout)).ok()?;
    State::from_json(&serde_json::from_str(&text).ok()?)
}

fn write_state(layout: &Layout, state: &State) -> Result<()> {
    crate::machine::write_json(&state_path(layout), &state.to_json(), 0o644)
}

// ---------------------------------------------------------------------------
// The machine
// ---------------------------------------------------------------------------

/// A Tilo desktop login: its `tilo-session` wrapper and whose it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Login {
    pub wrapper: u32,
    pub user: String,
}

/// What a live update asks of the machine beyond [`Host`]. [`crate::System`]
/// is the production behaviour; tests stand in for it.
#[expect(
    clippy::missing_errors_doc,
    reason = "every method reports a failed side effect the same way"
)]
pub trait LiveHost {
    /// `bootc status --format json`.
    fn bootc_status(&self) -> Result<String>;
    /// Open `deployment` read-only; returns the root its `usr/` is under.
    fn open(&self, layout: &Layout, deployment: &Deployment, at: &Path) -> Result<PathBuf>;
    /// Close what [`Self::open`] mounted at `at`.
    fn close(&self, at: &Path);
    /// A whiteout at `path`: the file below it is gone.
    fn whiteout(&self, path: &Path) -> Result<()>;
    /// Whether `path` in a layer is a whiteout [`Self::whiteout`] made.
    fn is_whiteout(&self, path: &Path) -> bool;
    /// Reload the system manager and create new system accounts and paths.
    fn daemon_reload(&self) -> Result<()>;
    /// Reload `user`'s manager.
    fn reload_user(&self, user: &str) -> Result<()>;
    /// Restart `unit` if it runs, in `user`'s manager or the system's.
    fn try_restart(&self, user: Option<&str>, unit: &str) -> Result<()>;
    /// Whether `unit` failed.
    fn unit_failed(&self, user: Option<&str>, unit: &str) -> bool;
    /// Start `unit` without waiting for it.
    fn start_no_block(&self, unit: &str);
    /// Every Tilo desktop login.
    fn logins(&self) -> Vec<Login>;
    /// Ask `login`'s wrapper to restart its compositor, and wait until the
    /// old one has exited.
    fn restart_desktop(&self, login: &Login) -> Result<()>;
    /// Whether `login`'s new compositor runs and stays up for `stable`.
    fn desktop_back(&self, login: &Login, stable: Duration) -> bool;
    fn sleep(&self, duration: Duration);
}

fn systemctl(user: Option<&str>, args: &[&str]) -> Vec<String> {
    let mut command = vec!["systemctl".to_owned()];
    if let Some(user) = user {
        command.push(format!("--machine={user}@"));
        command.push("--user".to_owned());
    }
    command.extend(args.iter().map(|arg| (*arg).to_owned()));
    command
}

fn run_strings(argv: &[String], check: bool) -> Result<crate::host::Output> {
    let argv: Vec<&OsStr> = argv.iter().map(OsStr::new).collect();
    run(&argv, check)
}

fn comm(pid: u32) -> Option<String> {
    fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|comm| comm.trim().to_owned())
}

fn zombie(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("State:"))
                .map(|state| state.trim().starts_with('Z'))
        })
        .unwrap_or(true)
}

fn compositor_of(wrapper: u32) -> Option<u32> {
    fs::read_to_string(format!("/proc/{wrapper}/task/{wrapper}/children"))
        .ok()?
        .split_whitespace()
        .filter_map(|pid| pid.parse::<u32>().ok())
        .find(|pid| comm(*pid).as_deref() == Some(COMPOSITOR) && !zombie(*pid))
}

fn user_name(uid: u32) -> Option<String> {
    let output = run_strings(&["id".to_owned(), "-nu".to_owned(), uid.to_string()], false).ok()?;
    let name = output.stdout.trim().to_owned();
    (output.success && !name.is_empty()).then_some(name)
}

impl LiveHost for crate::System {
    fn bootc_status(&self) -> Result<String> {
        run_strings(
            &[
                "bootc".to_owned(),
                "status".to_owned(),
                "--format".to_owned(),
                "json".to_owned(),
            ],
            true,
        )
        .map(|output| output.stdout)
    }

    fn open(&self, layout: &Layout, deployment: &Deployment, at: &Path) -> Result<PathBuf> {
        match deployment {
            Deployment::Ostree {
                stateroot,
                checksum,
                serial,
                ..
            } => {
                let root = layout
                    .at("/sysroot/ostree/deploy")
                    .join(stateroot)
                    .join("deploy")
                    .join(format!("{checksum}.{serial}"));
                if root.join("usr").is_dir() {
                    Ok(root)
                } else {
                    Err(Error::Pack(format!(
                        "the deployment {} is not on this disk",
                        root.display()
                    )))
                }
            }
            Deployment::Composefs { verity, .. } => {
                self.close(at);
                fs::create_dir_all(at).at(at)?;
                let repository = layout.at("/sysroot/composefs");
                let image = repository.join("images").join(verity);
                let options = format!(
                    "ro,basedir={},digest={verity}",
                    repository.join("objects").display()
                );
                // mount.composefs checks the image's fs-verity digest against
                // the one bootc reported before it mounts anything.
                run(
                    &[
                        OsStr::new("mount"),
                        OsStr::new("-t"),
                        OsStr::new("composefs"),
                        OsStr::new("-o"),
                        OsStr::new(&options),
                        image.as_os_str(),
                        at.as_os_str(),
                    ],
                    true,
                )?;
                Ok(at.to_path_buf())
            }
        }
    }

    fn close(&self, at: &Path) {
        if at.is_dir() {
            let _ = run(&[OsStr::new("umount"), at.as_os_str()], false);
        }
    }

    fn whiteout(&self, path: &Path) -> Result<()> {
        run(
            &[
                OsStr::new("mknod"),
                path.as_os_str(),
                OsStr::new("c"),
                OsStr::new("0"),
                OsStr::new("0"),
            ],
            true,
        )
        .map(drop)
    }

    fn is_whiteout(&self, path: &Path) -> bool {
        use std::os::unix::fs::FileTypeExt as _;
        fs::symlink_metadata(path)
            .is_ok_and(|meta| meta.file_type().is_char_device() && meta.rdev() == 0)
    }

    fn daemon_reload(&self) -> Result<()> {
        run_strings(&systemctl(None, &["daemon-reload"]), true)?;
        run_strings(&["systemd-sysusers".to_owned()], true)?;
        run_strings(
            &["systemd-tmpfiles".to_owned(), "--create".to_owned()],
            false,
        )
        .map(drop)
    }

    fn reload_user(&self, user: &str) -> Result<()> {
        run_strings(&systemctl(Some(user), &["daemon-reload"]), true).map(drop)
    }

    fn try_restart(&self, user: Option<&str>, unit: &str) -> Result<()> {
        run_strings(&systemctl(user, &["try-restart", unit]), true).map(drop)
    }

    fn unit_failed(&self, user: Option<&str>, unit: &str) -> bool {
        run_strings(&systemctl(user, &["is-failed", "--quiet", unit]), false)
            .is_ok_and(|output| output.success)
    }

    fn start_no_block(&self, unit: &str) {
        let _ = run_strings(&systemctl(None, &["start", "--no-block", unit]), false);
    }

    fn logins(&self) -> Vec<Login> {
        let Ok(entries) = fs::read_dir("/proc") else {
            return Vec::new();
        };
        let mut logins = Vec::new();
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|pid| pid.parse::<u32>().ok())
            else {
                continue;
            };
            if comm(pid).as_deref() != Some(LOGIN_WRAPPER) {
                continue;
            }
            let Some(user) = entry.metadata().ok().and_then(|meta| user_name(meta.uid())) else {
                continue;
            };
            logins.push(Login { wrapper: pid, user });
        }
        logins
    }

    fn restart_desktop(&self, login: &Login) -> Result<()> {
        let old = compositor_of(login.wrapper);
        run_strings(
            &[
                "kill".to_owned(),
                "-USR1".to_owned(),
                login.wrapper.to_string(),
            ],
            true,
        )?;
        let Some(old) = old else {
            return Ok(());
        };
        let started = Instant::now();
        while Path::new(&format!("/proc/{old}")).exists() && !zombie(old) {
            if started.elapsed() >= DESKTOP_EXIT {
                return Err(Error::Pack(format!(
                    "{}'s compositor did not exit within {} s",
                    login.user,
                    DESKTOP_EXIT.as_secs()
                )));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }

    fn desktop_back(&self, login: &Login, stable: Duration) -> bool {
        let started = Instant::now();
        let mut up_since: Option<(u32, Instant)> = None;
        while started.elapsed() < stable.saturating_mul(3) {
            if comm(login.wrapper).as_deref() != Some(LOGIN_WRAPPER) {
                // The login ended: the compositor did not start.
                return false;
            }
            match (compositor_of(login.wrapper), up_since) {
                (Some(pid), Some((since_pid, since))) if pid == since_pid => {
                    if since.elapsed() >= stable {
                        return true;
                    }
                }
                (Some(pid), _) => up_since = Some((pid, Instant::now())),
                (None, _) => up_since = None,
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        false
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

#[cfg(test)]
mod tests;
