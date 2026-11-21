//! Live system updates: the staged image's changed components, applied now.
//!
//! The update service stages and verifies the newest image at least daily, and
//! bootc lays it down as a deployment that normally applies at the next boot.
//! This applies it now instead, component by component, with the same machinery
//! a developer's `tilo-vm swap` uses: one system extension over `/usr` and the
//! restarts each component's scope asks for. `live:system` applies what needs no
//! desktop restart, `live:session` applies the rest and restarts the desktops in
//! place, `live:prune` removes the layers nothing mounts any more, and
//! `live:status` prints the state the desktop reads.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::host::{Host, run};
use crate::machine::Layout;
use crate::{At, Error, Result};

pub const LIVE_NAME: &str = "tilo-live-update";
const INTERFACES: &str = "/usr/share/tilo/system-interfaces.toml";
const COMPONENTS: &str = "usr/share/tilo/components.toml";
const GREETER_UNIT: &str = "greetd.service";
const SANDBOX_VIEW_UNIT: &str = "tilo-sandbox-view.service";
const SETTLE: Duration = Duration::from_secs(5);
/// How long a restarted desktop is given to become usable, in place, before the update is called
/// failed.
const DESKTOP_BACK: Duration = Duration::from_secs(60);
/// How long a desktop that published itself usable must stay so: one that comes back and then
/// crashes still rolls the update back.
const DESKTOP_STABLE: Duration = Duration::from_secs(3);
/// How often a starting desktop is looked at.
const DESKTOP_POLL: Duration = Duration::from_millis(250);
/// How long a desktop may take to stop before the attempt is given up.
const DESKTOP_EXIT: Duration = Duration::from_secs(30);
const LOGIN_WRAPPER: &str = "tilo-session";
const COMPOSITOR: &str = "tilo-session";

const BOOT_PREFIXES: [&str; 7] = [
    "/usr/lib/modules/",
    "/usr/lib/firmware/",
    "/usr/lib/bootupd/",
    "/usr/lib/systemd/boot/",
    "/usr/lib/efi/",
    "/usr/lib/dracut/",
    "/usr/lib/ostree-boot/",
];
const NEXT_BOOT_PREFIXES: [&str; 3] =
    ["/usr/etc/", "/usr/lib/sysimage/rpm/", "/usr/lib/os-release"];

/// The links that put our layers in the sysext search path.
///
/// Every layer is linked under its own name, so the superblock options of any mount that still
/// reads an older merge keep naming the layer it reads (see [`layer_in_use`]).
pub fn links(layout: &Layout) -> Vec<PathBuf> {
    let dir = layout.at("/run/extensions");
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut links: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| !name.starts_with('.'))
        })
        .map(|entry| entry.path())
        .filter(|path| is_link(layout, path))
        .collect();
    links.sort();
    links
}

/// Whether `path` is one of our layers' links.
pub fn is_link(layout: &Layout, path: &Path) -> bool {
    fs::read_link(path).is_ok_and(|target| target.starts_with(layers(layout)))
}

pub fn link(layout: &Layout) -> Option<PathBuf> {
    links(layout).into_iter().next()
}

pub fn layers(layout: &Layout) -> PathBuf {
    layout.at("/var/lib/tilo/live-update/layers")
}

pub fn state_path(layout: &Layout) -> PathBuf {
    layout.at("/run/tilo/live-update/state.json")
}

pub fn merged_marker(layout: &Layout) -> PathBuf {
    layout.at("/run/tilo/live-update/merged")
}

fn mounts(layout: &Layout) -> PathBuf {
    layout.at("/run/tilo/live-update/mnt")
}

pub fn linked(layout: &Layout) -> bool {
    !links(layout).is_empty()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    System,
    Session,
}

impl Phase {
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

pub fn status(layout: &Layout) -> i32 {
    let state = fs::read_to_string(state_path(layout)).unwrap_or_else(|_| "{}".to_owned());
    println!("{}", state.trim());
    0
}

pub fn apply(host: &dyn Host, live: &dyn LiveHost, layout: &Layout, phase: Phase) -> Result<i32> {
    let (booted, staged) = deployments(&live.bootc_status()?)?;
    let Some(staged) = staged else {
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
    let outcome = open_and_apply(host, live, layout, phase, booted, &staged, system_applied);
    if let Err(error) = &outcome {
        write_state(
            layout,
            &State {
                image: staged.image().to_owned(),
                reboot_required: true,
                failed: true,
                detail: error.to_string(),
                ..State::current()
            },
        )?;
    }
    outcome
}

fn open_and_apply(
    host: &dyn Host,
    live: &dyn LiveHost,
    layout: &Layout,
    phase: Phase,
    booted: Option<Deployment>,
    staged: &Deployment,
    system_applied: bool,
) -> Result<i32> {
    let booted = booted.ok_or_else(|| Error::pack("bootc reports no booted deployment"))?;
    let mounts = mounts(layout);
    let base_root = live.open(layout, &booted, &mounts.join("booted"))?;
    let new_root = match live.open(layout, staged, &mounts.join("staged")) {
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
            staged,
            system_applied,
        },
        &base_root,
        &new_root,
    );
    live.close(&mounts.join("staged"));
    live.close(&mounts.join("booted"));
    outcome
}

#[derive(Clone, Copy)]
struct Target<'a> {
    phase: Phase,
    staged: &'a Deployment,
    system_applied: bool,
}

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
    let before = link(layout).and_then(|link| fs::read_link(link).ok());
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
    let layer = build_layer(
        host,
        live,
        &plan,
        phase,
        new_root,
        &layers(layout).join(name),
        staged.image(),
    )?;
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
            prune_layers(live, layout, std::slice::from_ref(&layer));
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

fn restart(live: &dyn LiveHost, restarts: &Restarts, deferred: &mut Vec<String>) -> Result<()> {
    live.daemon_reload()?;
    let logins = live.logins();
    let mut restarted = Vec::new();
    for unit in &restarts.units {
        if unit == GREETER_UNIT && !logins.is_empty() {
            deferred.push(unit.clone());
            continue;
        }
        let failed_before = live.unit_failed(None, unit);
        live.try_restart(None, unit)?;
        restarted.push((None, unit.clone(), failed_before));
    }
    // One service manager per account, however many desktops it has open.
    let accounts: BTreeMap<u32, &Login> = logins.iter().map(|login| (login.uid, login)).collect();
    for login in accounts.into_values() {
        live.reload_user(login)?;
        for unit in &restarts.user_units {
            let failed_before = live.unit_failed(Some(login), unit);
            live.try_restart(Some(login), unit)?;
            restarted.push((Some(login), unit.clone(), failed_before));
        }
    }
    if restarted.is_empty() {
        return Ok(());
    }
    live.sleep(SETTLE);
    let failed: Vec<String> = restarted
        .iter()
        // A unit that had failed before the update is not the update's doing: a restart leaves a
        // stopped unit alone, so its old failure is still there afterwards. Blamed for it, an
        // update rolled back over an unrelated job that had ended badly an hour earlier.
        .filter(|(login, unit, failed_before)| !failed_before && live.unit_failed(*login, unit))
        .map(|(login, unit, _)| match login {
            Some(login) => format!("{unit} ({})", login.user),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Deployment {
    Composefs {
        image: String,
        verity: String,
    },
    Ostree {
        image: String,
        stateroot: String,
        checksum: String,
        serial: u64,
    },
}

impl Deployment {
    pub fn image(&self) -> &str {
        match self {
            Self::Composefs { image, .. } | Self::Ostree { image, .. } => image,
        }
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    pub name: String,
    pub scope: String,
    pub units: Vec<String>,
    pub user_units: Vec<String>,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Components(pub Vec<Component>);

impl Components {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Boot,
    NextBoot,
    System,
    Base,
    Apps,
    Session,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Written,
    Removed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts the classification found about one update, not states of one machine"
)]
pub struct Plan {
    pub changes: Vec<(Change, Class)>,
    pub reboot: bool,
    pub interfaces_changed: bool,
    pub system: BTreeMap<String, Restarts>,
    pub session: BTreeMap<String, Restarts>,
    pub apps_changed: bool,
    pub held: BTreeSet<String>,
    pub session_undone: bool,
}

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

/// Whether `path` is a kernel's start-up image, `/usr/lib/modules/<kernel>/initramfs.img`.
fn is_initramfs(path: &str) -> bool {
    path.strip_prefix("/usr/lib/modules/")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(_, file)| file == "initramfs.img")
}

pub fn classify(path: &str, owner: Option<&Component>) -> Class {
    // The start-up image holds Tilo's own sign-in and recovery programs, so it differs in nearly
    // every image, also under an unchanged kernel. The machine that is up never reads it again:
    // the next start takes the new one, and nobody is asked to restart for it. Called a reason to
    // restart, it made every update one, and none could finish with the desktop alone.
    if is_initramfs(path) {
        return Class::NextBoot;
    }
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

    pub fn takes(&self, change: &Change, class: Class, phase: Phase) -> bool {
        self.in_layer(class, phase)
            || (!matches!(class, Class::Boot | Class::NextBoot) && self.held.contains(&change.path))
    }

    pub fn in_layer(&self, class: Class, phase: Phase) -> bool {
        match class {
            Class::Boot | Class::NextBoot => false,
            Class::System | Class::Base => true,
            Class::Apps => phase == Phase::Session || !self.interfaces_changed,
            Class::Session => phase == Phase::Session,
        }
    }

    pub fn desktop_pending(&self) -> bool {
        self.session_undone
            || self.changes.iter().any(|(change, class)| {
                !self.takes(change, *class, Phase::System)
                    && self.takes(change, *class, Phase::Session)
            })
    }

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

pub struct LiveTree<'a> {
    pub layer: Option<&'a Path>,
    pub base: &'a Path,
    pub live: &'a dyn LiveHost,
}

impl LiveTree<'_> {
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
    let release = format!(
        "/usr/lib/extension-release.d/extension-release.{}",
        extension_name(layer)
    );
    let mut out = Vec::new();
    let usr = layer.join("usr");
    if usr.is_dir() {
        walk(&usr, "/usr", &mut out)?;
    }
    out.retain(|path| *path != release);
    Ok(out)
}

fn layer_name(image: &str, phase: Phase) -> String {
    let digest = image.strip_prefix("sha256:").unwrap_or(image);
    let short: String = digest.chars().take(16).collect();
    format!("{short}-{}", phase.name())
}

/// The name a layer's link and its `extension-release` file share.
///
/// systemd-sysext names a directory extension after the entry it found and reads the
/// `extension-release` file named after that, so a layer carries a file of its own name.
fn extension_name(layer: &Path) -> String {
    let name = layer
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!("{LIVE_NAME}-{name}")
}

/// The first free name for a layer, so a layer that may still be mounted is never rewritten.
fn fresh_layer(requested: &Path) -> Result<PathBuf> {
    let parent = requested
        .parent()
        .ok_or_else(|| Error::pack("a layer has no parent directory"))?;
    let name = requested
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| Error::pack("a layer has no usable name"))?;
    for count in 0..4096 {
        let candidate = if count == 0 {
            requested.to_path_buf()
        } else {
            parent.join(format!("{name}-{count}"))
        };
        if fs::symlink_metadata(&candidate).is_err() {
            return Ok(candidate);
        }
    }
    Err(Error::pack("too many layers of one image are in use"))
}

pub fn build_layer(
    host: &dyn Host,
    live: &dyn LiveHost,
    plan: &Plan,
    phase: Phase,
    new_root: &Path,
    layer: &Path,
    image: &str,
) -> Result<PathBuf> {
    let layer = fresh_layer(layer)?;
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
        let release = release_dir.join(format!("extension-release.{}", extension_name(&layer)));
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
    fs::rename(&staging, &layer).at(&layer)?;
    Ok(layer)
}

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

fn swap_link(layout: &Layout, layer: &Path) -> Result<()> {
    let dir = layout.at("/run/extensions");
    fs::create_dir_all(&dir).at(&dir)?;
    // The new link first, so a failure removing the old one leaves the merge the machine has.
    let name = extension_name(layer);
    let next = dir.join(format!(".{name}.next"));
    let _ = fs::remove_file(&next);
    symlink(layer, &next).at(&next)?;
    for previous in links(layout) {
        fs::remove_file(&previous).at(&previous)?;
    }
    fs::rename(&next, dir.join(&name)).at(&dir.join(&name))
}

fn remove_link(layout: &Layout) -> Result<()> {
    for link in links(layout) {
        match fs::remove_file(&link) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                return Err(Error::os(&error, Some(&link)));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Remove every layer not in `keep` that no mount reads any more.
///
/// A layer is immutable once merged. A sandbox started before the update keeps the merged `/usr`
/// it started with in its own mount namespace, and that mount still reads the layer as its lower
/// directory: deleting it would make the files the app has not opened yet vanish. The mounts, not
/// the link, decide what is still alive ([`LiveHost::layer_in_use`]).
fn prune_layers(live: &dyn LiveHost, layout: &Layout, keep: &[PathBuf]) {
    let Ok(entries) = fs::read_dir(layers(layout)) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !keep.contains(&path) && !live.layer_in_use(&path) {
            let _ = remove_tree(&path);
        }
    }
}

fn linked_layers(layout: &Layout) -> Vec<PathBuf> {
    links(layout)
        .into_iter()
        .filter_map(|link| fs::read_link(link).ok())
        .collect()
}

/// Remove every layer that is neither linked nor mounted.
///
/// The sandbox view's retirement runs this when the last sandbox holding a generation lets go,
/// which is the moment a layer's last reader may have gone. A launch that bound the system profile
/// per path holds no generation; the layer it read goes with the next sweep after it ended. An
/// applying update holds the live lock, so a build in progress is never reaped.
pub fn prune(live: &dyn LiveHost, layout: &Layout) -> i32 {
    prune_layers(live, layout, &linked_layers(layout));
    0
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub image: String,
    pub applied: Option<Phase>,
    pub desktop_pending: bool,
    pub reboot_required: bool,
    pub failed: bool,
    pub deferred: Vec<String>,
    pub detail: String,
}

impl State {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Login {
    pub wrapper: u32,
    pub user: String,
    /// The account the wrapper runs as: its runtime directory holds the desktop's records.
    pub uid: u32,
    /// The account's own group, which its service manager is reached as.
    pub gid: u32,
}

#[expect(
    clippy::missing_errors_doc,
    reason = "every method reports a failed side effect the same way"
)]
pub trait LiveHost {
    fn bootc_status(&self) -> Result<String>;
    fn open(&self, layout: &Layout, deployment: &Deployment, at: &Path) -> Result<PathBuf>;
    fn close(&self, at: &Path);
    fn whiteout(&self, path: &Path) -> Result<()>;
    fn is_whiteout(&self, path: &Path) -> bool;
    /// Whether a mount in any mount namespace still reads `layer` as a lower directory.
    fn layer_in_use(&self, layer: &Path) -> bool;
    fn daemon_reload(&self) -> Result<()>;
    fn reload_user(&self, login: &Login) -> Result<()>;
    fn try_restart(&self, login: Option<&Login>, unit: &str) -> Result<()>;
    fn unit_failed(&self, login: Option<&Login>, unit: &str) -> bool;
    fn start_no_block(&self, unit: &str);
    fn logins(&self) -> Vec<Login>;
    fn restart_desktop(&self, login: &Login) -> Result<()>;
    /// Whether `login`'s wrapper is still the process it was.
    fn login_alive(&self, login: &Login) -> bool;
    /// The compositor `login`'s wrapper runs now, if any.
    fn compositor(&self, login: &Login) -> Option<u32>;
    /// Whether the compositor `pid` published that its desktop is usable.
    fn desktop_ready(&self, login: &Login, pid: u32) -> bool;
    /// Whether `login`'s wrapper says the desktop it was asked to restart never became usable,
    /// and keeps the login until the restart is asked again.
    fn restart_failed(&self, login: &Login) -> bool;
    /// The clock the wait below keeps, which a test host advances instead of sleeping.
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);

    /// Whether `login`'s desktop is back and has stayed usable for `stable`: the wrapper and the
    /// compositor it started run, that compositor published its ready record, and nothing
    /// changed since. A desktop that is alive but never ready is not back; one that becomes ready
    /// and then goes is not either. Given up after [`DESKTOP_BACK`].
    fn desktop_back(&self, login: &Login, stable: Duration) -> bool {
        let started = self.now();
        let mut ready: Option<(u32, Instant)> = None;
        while self.now().saturating_duration_since(started) < DESKTOP_BACK {
            // The wrapper keeps the login and waits for the next request: asking again, with
            // the previous files back, is the only thing that helps, and sooner is better.
            if !self.login_alive(login) || self.restart_failed(login) {
                return false;
            }
            match (self.compositor(login), ready) {
                (Some(pid), Some((ready_pid, since))) if pid == ready_pid => {
                    if self.now().saturating_duration_since(since) >= stable {
                        return true;
                    }
                }
                (Some(pid), _) => {
                    ready = self.desktop_ready(login, pid).then(|| (pid, self.now()));
                }
                (None, _) => ready = None,
            }
            self.sleep(DESKTOP_POLL);
        }
        false
    }
}

/// `systemctl ARGS` for the system's service manager, or for the one of a logged-in account.
///
/// An account's manager is asked as that account, on its own bus. `systemctl --machine=NAME@
/// --user` asks the system's manager to start a bridge instead and hands it this process's
/// output pipe over the system bus; the bus may not write to a system service's pipe (`SELinux`),
/// drops the connection, and the update that asked rolled back.
fn systemctl(login: Option<&Login>, args: &[&str]) -> Vec<String> {
    let mut command = Vec::new();
    if let Some(login) = login {
        command.extend([
            "setpriv".to_owned(),
            format!("--reuid={}", login.uid),
            format!("--regid={}", login.gid),
            "--init-groups".to_owned(),
            "env".to_owned(),
            format!("XDG_RUNTIME_DIR=/run/user/{}", login.uid),
        ]);
    }
    command.push("systemctl".to_owned());
    if login.is_some() {
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

fn is_compositor(pid: u32) -> bool {
    comm(pid).as_deref() == Some(COMPOSITOR)
        && fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .and_then(|exe| {
                exe.file_name().map(|name| {
                    let name = name.to_string_lossy();
                    name.strip_suffix(" (deleted)").unwrap_or(&name) == COMPOSITOR
                })
            })
            .unwrap_or(false)
}

fn parent(pid: u32) -> Option<u32> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:"))
        .and_then(|parent| parent.trim().parse().ok())
}

fn children_of(wrapper: u32) -> Vec<u32> {
    let Ok(processes) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    processes
        .flatten()
        .filter_map(|process| {
            process
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
        })
        .filter(|pid| parent(*pid) == Some(wrapper))
        .collect()
}

fn compositor_of(wrapper: u32) -> Option<u32> {
    children_of(wrapper)
        .into_iter()
        .find(|pid| is_compositor(*pid) && !zombie(*pid))
}

fn user_name(uid: u32) -> Option<String> {
    let output = run_strings(&["id".to_owned(), "-nu".to_owned(), uid.to_string()], false).ok()?;
    let name = output.stdout.trim().to_owned();
    (output.success && !name.is_empty()).then_some(name)
}

fn composefs_mount(objects: &Path, image: &Path, at: &Path) -> Vec<OsString> {
    let mut options = OsString::from("ro,verity,basedir=");
    options.push(objects);
    vec![
        "mount.composefs".into(),
        "-o".into(),
        options,
        image.into(),
        at.into(),
    ]
}

fn verity_matches(algorithm: u16, digest: &[u8], expected: &str) -> bool {
    let wanted = match expected.len() {
        64 => FS_VERITY_HASH_ALG_SHA256,
        128 => FS_VERITY_HASH_ALG_SHA512,
        _ => return false,
    };
    algorithm == wanted && hex::encode(digest).eq_ignore_ascii_case(expected)
}

const FS_VERITY_HASH_ALG_SHA256: u16 = 1;
const FS_VERITY_HASH_ALG_SHA512: u16 = 2;

fn measure_verity(file: &fs::File) -> io::Result<(u16, Vec<u8>)> {
    const FS_IOC_MEASURE_VERITY: libc::Ioctl = 0xC004_6686;
    #[repr(C)]
    struct Measure {
        digest_algorithm: u16,
        digest_size: u16,
        digest: [u8; 64],
    }
    let mut arg = Measure {
        digest_algorithm: 0,
        digest_size: 64,
        digest: [0; 64],
    };
    let result = unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            FS_IOC_MEASURE_VERITY,
            std::ptr::from_mut(&mut arg),
        )
    };
    if result != 0 {
        let error = io::Error::last_os_error();
        return Err(match error.raw_os_error() {
            Some(libc::ENODATA | libc::EOPNOTSUPP | libc::ENOTTY) => {
                io::Error::other(format!("the file has no fs-verity: {error}"))
            }
            _ => error,
        });
    }
    let digest = arg
        .digest
        .get(..usize::from(arg.digest_size))
        .ok_or_else(|| io::Error::other("the kernel reported an oversized fs-verity digest"))?;
    Ok((arg.digest_algorithm, digest.to_vec()))
}

/// Whether any process's mount table reads `layer` as an overlay's lower directory.
///
/// systemd-sysext binds a directory extension at `/run/systemd/sysext/extensions/<name>` and lists
/// that name in the overlay's `lowerdir` option; a sandbox started before the update keeps a clone
/// of the overlay in its own mount namespace, and `/proc/<pid>/mountinfo` reports the superblock's
/// options in every namespace that holds one. A `/proc` that cannot be read answers "in use", so
/// nothing is ever deleted on a guess.
fn layer_mounted(proc_root: &Path, layer: &Path) -> bool {
    let name = extension_name(layer);
    let Ok(entries) = fs::read_dir(proc_root) else {
        return true;
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_str()
            .and_then(|pid| pid.parse::<u32>().ok())
            .is_none()
        {
            continue;
        }
        match fs::read_to_string(entry.path().join("mountinfo")) {
            Ok(info) => {
                if mountinfo_reads(&info, layer, &name) {
                    return true;
                }
            }
            // The process ended between the directory read and this read: its mounts went with it.
            Err(error) if matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ESRCH)) => {}
            // Any other reason a namespace cannot be read leaves its mounts unknown: keep the layer.
            Err(_) => return true,
        }
    }
    false
}

fn mountinfo_reads(info: &str, layer: &Path, name: &str) -> bool {
    // sysext names the extension after the link; a version that resolves it first would use the
    // layer's own directory name instead. Either way the mount's options say which layer they read.
    let directory = layer
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default();
    info.lines().any(|line| {
        let Some((_, rest)) = line.split_once(" - ") else {
            return false;
        };
        let mut fields = rest.split_whitespace();
        let (Some(fstype), Some(_source), Some(options)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return false;
        };
        fstype == "overlay"
            && options.split(',').any(|option| {
                let Some(lower) = option
                    .strip_prefix("lowerdir=")
                    .or_else(|| option.strip_prefix("lowerdir+="))
                else {
                    return false;
                };
                lower.split(':').any(|dir| {
                    Path::new(dir).starts_with(layer)
                        || dir.split('/').any(|part| part == name || part == directory)
                })
            })
    })
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
                let path = repository.join("images").join(verity);
                let image = fs::File::open(&path).at(&path)?;
                let measured = measure_verity(&image).at(&path)?;
                if !verity_matches(measured.0, &measured.1, verity) {
                    return Err(Error::Pack(format!(
                        "the composefs image {} is not the one bootc reports",
                        path.display()
                    )));
                }
                let held = PathBuf::from(format!(
                    "/proc/{}/fd/{}",
                    std::process::id(),
                    image.as_raw_fd()
                ));
                run(
                    &composefs_mount(&repository.join("objects"), &held, at)
                        .iter()
                        .map(OsString::as_os_str)
                        .collect::<Vec<_>>(),
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

    fn layer_in_use(&self, layer: &Path) -> bool {
        layer_mounted(Path::new("/proc"), layer)
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

    fn reload_user(&self, login: &Login) -> Result<()> {
        run_strings(&systemctl(Some(login), &["daemon-reload"]), true).map(drop)
    }

    fn try_restart(&self, login: Option<&Login>, unit: &str) -> Result<()> {
        run_strings(&systemctl(login, &["try-restart", unit]), true).map(drop)
    }

    fn unit_failed(&self, login: Option<&Login>, unit: &str) -> bool {
        run_strings(&systemctl(login, &["is-failed", "--quiet", unit]), false)
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
            if comm(pid).as_deref() != Some(LOGIN_WRAPPER) || is_compositor(pid) {
                continue;
            }
            let Some(metadata) = entry.metadata().ok() else {
                continue;
            };
            let uid = metadata.uid();
            let Some(user) = user_name(uid) else {
                continue;
            };
            logins.push(Login {
                wrapper: pid,
                user,
                uid,
                gid: metadata.gid(),
            });
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
            // No compositor runs: the wrapper waited for this request after a restart that
            // failed. It takes its report down as it starts the next one; reading the old
            // report as this start's would call the desktop gone before it was tried.
            let asked = Instant::now();
            while self.restart_failed(login) && asked.elapsed() < Duration::from_secs(3) {
                std::thread::sleep(Duration::from_millis(50));
            }
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

    fn login_alive(&self, login: &Login) -> bool {
        comm(login.wrapper).as_deref() == Some(LOGIN_WRAPPER)
    }

    fn compositor(&self, login: &Login) -> Option<u32> {
        compositor_of(login.wrapper)
    }

    fn desktop_ready(&self, login: &Login, pid: u32) -> bool {
        desktop_ready(login, pid)
    }

    fn restart_failed(&self, login: &Login) -> bool {
        let path = PathBuf::from(format!("/run/user/{}", login.uid))
            .join(tilo_paths::login::SESSION_DIR)
            .join(tilo_paths::login::DESKTOP_RESTART_FAILED_FILE);
        fs::symlink_metadata(&path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.uid() == login.uid)
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Whether `login`'s compositor `pid` published that its desktop is usable: the record it wrote
/// names exactly that process, is a regular file `login`'s user owns, and is not a redirect. A
/// record a previous run left names another process, so it never answers for this one.
fn desktop_ready(login: &Login, pid: u32) -> bool {
    let path = PathBuf::from(format!("/run/user/{}", login.uid))
        .join(tilo_paths::login::SESSION_DIR)
        .join(tilo_paths::login::DESKTOP_READY_FILE);
    let Ok(file) = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
    else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if !metadata.is_file() || metadata.uid() != login.uid || metadata.len() > 64 {
        return false;
    }
    let mut record = String::new();
    file.take(64)
        .read_to_string(&mut record)
        .is_ok_and(|_| tilo_process::identity_names(&record, pid))
}

#[cfg(test)]
mod tests;
