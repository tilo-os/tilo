//! Workspace capabilities → sandbox restrictions.
//!
//! The manifest says what an app asks for at install time; this module carries what the
//! user granted it at runtime. Both must be true before a path, device or network is
//! reachable, and the absence of a capability is enforced by the absence of the mount.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use collab_types::caps::{
    Access, Actor, CapDecision, CapOverride, Effect, LaunchGrants, ResourceRef, Scope,
    decide_capability,
};

/// How an app may take part in the space it is launched into.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Participation {
    /// No workspace-scoped capability names this app either way, so nothing is inferred — the default,
    /// deliberately: guessing here would decide who may author in a workspace, not this module's call.
    #[default]
    Unscoped,
    /// May see the space; may not change it.
    ReadOnly,
    /// May change the space.
    Author,
}

impl Participation {
    /// Should the sandbox carry the user identity?
    pub fn carries_identity(self) -> bool {
        !matches!(self, Participation::ReadOnly)
    }

    /// The value of `TILO_PARTICIPATION` inside the sandbox, if the mode is one worth telling the app
    /// about.
    pub fn env_value(self) -> Option<&'static str> {
        match self {
            Participation::ReadOnly => Some("read-only"),
            Participation::Author => Some("author"),
            Participation::Unscoped => None,
        }
    }
}

/// The resolved, host-side consequence of an app's capabilities.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilityPlan {
    /// Host paths to bind read-only, at the same path in the sandbox.
    pub ro_paths: Vec<PathBuf>,
    /// Host paths to bind read-write, at the same path in the sandbox.
    pub rw_paths: Vec<PathBuf>,
    /// Device classes that were granted (`"camera"`, `"microphone"`, `"screencast"`).
    pub devices: Vec<String>,
    /// Whether any network capability was granted at all — says nothing about *which* hosts.
    pub allow_network: bool,
    /// The host patterns the network capabilities named.
    pub net_hosts: Vec<String>,
    /// Whether the app may originate changes in the space.
    pub participation: Participation,
    /// Private stores a granted path would otherwise expose, at the path the grant shows them in the
    /// sandbox.
    pub masked_dirs: Vec<PathBuf>,
    /// Directories inside a writable grant on the way to a masked store, outermost first.
    pub pinned_dirs: Vec<PathBuf>,
}

impl CapabilityPlan {
    /// The plan for an app that was granted nothing.
    pub const fn empty() -> Self {
        Self {
            ro_paths: Vec::new(),
            rw_paths: Vec::new(),
            devices: Vec::new(),
            allow_network: false,
            net_hosts: Vec::new(),
            participation: Participation::Unscoped,
            masked_dirs: Vec::new(),
            pinned_dirs: Vec::new(),
        }
    }

    /// Does this plan add anything to the manifest's sandbox?
    pub fn is_empty(&self) -> bool {
        self.ro_paths.is_empty()
            && self.rw_paths.is_empty()
            && self.devices.is_empty()
            && !self.allow_network
    }

    /// Resolve `caps` into the binds `app_id` has actually been granted, as of `now` (seconds since the
    /// Unix epoch; `None` skips expiry evaluation).
    pub fn for_app(app_id: &str, caps: &[CapOverride], now: Option<u64>) -> Self {
        Self::for_app_with(app_id, caps, now, host_device_paths)
            .confine_private_stores(&private_store_roots())
    }

    /// [`Self::for_app`]'s policy evaluation with the device-node lookup injected, before
    /// [`Self::confine_private_stores`].
    pub fn for_app_with(
        app_id: &str,
        caps: &[CapOverride],
        now: Option<u64>,
        device_paths: fn(&str) -> Vec<PathBuf>,
    ) -> Self {
        let actor = Actor::app(app_id);
        let mut fs: BTreeMap<String, bool> = BTreeMap::new();
        let mut devices: Vec<String> = Vec::new();
        let mut net_hosts: Vec<String> = Vec::new();

        for cap in caps {
            // Only grants nominate a resource.
            if cap.effect != Effect::Grant || !cap.subject.matches_actor(&actor) {
                continue;
            }
            match &cap.scope {
                Scope::FsPath(raw) => {
                    let resource = ResourceRef::FsPath(raw.clone());
                    let writable = allows(&actor, &resource, Access::Write, caps, now);
                    let readable = writable || allows(&actor, &resource, Access::Read, caps, now);
                    if !readable {
                        continue;
                    }
                    if !is_bindable_path(raw) {
                        tracing::warn!(
                            path = %raw,
                            "ignoring filesystem capability: not an absolute path below the root"
                        );
                        continue;
                    }
                    let slot = fs.entry(raw.clone()).or_insert(false);
                    *slot |= writable;
                }
                Scope::Device(device) => {
                    // `Read` is the floor of the verb ladder, so a grant phrased in any verb answers
                    // it.
                    if !allows(
                        &actor,
                        &ResourceRef::Device(device.clone()),
                        Access::Read,
                        caps,
                        now,
                    ) {
                        continue;
                    }
                    if !devices.iter().any(|d| d == device) {
                        devices.push(device.clone());
                    }
                }
                Scope::NetHost(host) => {
                    // A stored grant that is not a host in the one grammar (an IP literal, written
                    // before addresses were refused) covers nothing; say so rather than drop it
                    // silently.
                    if let Err(reason) = collab_types::caps::NetHostPattern::parse(host) {
                        tracing::warn!(%reason, "ignoring a network grant that is not a host");
                        continue;
                    }
                    if !allows(
                        &actor,
                        &ResourceRef::NetHost(host.clone()),
                        Access::Read,
                        caps,
                        now,
                    ) {
                        continue;
                    }
                    if !net_hosts.iter().any(|h| h == host) {
                        net_hosts.push(host.clone());
                    }
                }
                // A collaborative-file record is reached through the workspace's documents, not the
                // kernel — inert here, never permissive.
                Scope::FsRecord(_) => {}
            }
        }

        let mut plan = CapabilityPlan {
            allow_network: !net_hosts.is_empty(),
            net_hosts,
            ..CapabilityPlan::empty()
        };
        for (path, writable) in fs {
            if writable {
                plan.rw_paths.push(PathBuf::from(path));
            } else {
                plan.ro_paths.push(PathBuf::from(path));
            }
        }
        for device in &devices {
            for path in device_paths(device) {
                if !plan.rw_paths.contains(&path) {
                    plan.rw_paths.push(path);
                }
            }
        }
        plan.devices = devices;
        plan
    }
}

/// The variable naming a file-backed secret store, as `tilo_secrets` reads it
/// (`tilo_secrets::SECRETS_DIR_ENV`).
const SECRETS_DIR_ENV: &str = "TILO_SECRETS_DIR";

/// Symlinks followed while resolving one private store's path before the resolution gives up — the
/// kernel's own limit.
const MAX_SYMLINK_HOPS: usize = 40;

/// The directories holding Tilo's own key material and trust roots, as this process's environment names
/// them.
pub fn private_store_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(config) = tilo_paths::xdg::config_home() {
        roots.push(config.join("tilo"));
    }
    if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        roots.push(PathBuf::from(home).join(".config").join("tilo"));
    }
    if let Some(dir) = std::env::var_os(SECRETS_DIR_ENV).filter(|dir| !dir.is_empty()) {
        roots.push(PathBuf::from(dir));
    }
    roots
}

/// A path resolved one component at a time, recording where each symlink on the way lives.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Route {
    /// The path with every symlink followed.
    real: PathBuf,
    /// The resolved directory holding each symlink the resolution followed.
    link_dirs: Vec<PathBuf>,
}

impl Route {
    /// `None` for a path that is not absolute or loops through symlinks.
    fn resolve(path: &Path) -> Option<Self> {
        if !path.is_absolute() {
            return None;
        }
        let mut pending = std::collections::VecDeque::new();
        queue_components(&mut pending, path, false);
        let mut real = PathBuf::from("/");
        let mut link_dirs: Vec<PathBuf> = Vec::new();
        let mut hops = 0usize;
        let mut missing = false;
        while let Some(part) = pending.pop_front() {
            if part == ".." {
                real.pop();
                continue;
            }
            let next = real.join(&part);
            let meta = if missing {
                None
            } else {
                std::fs::symlink_metadata(&next).ok()
            };
            match meta {
                Some(meta) if meta.file_type().is_symlink() => {
                    hops = hops.saturating_add(1);
                    if hops > MAX_SYMLINK_HOPS {
                        return None;
                    }
                    let target = std::fs::read_link(&next).ok()?;
                    if !link_dirs.contains(&real) {
                        link_dirs.push(real.clone());
                    }
                    if target.is_absolute() {
                        real = PathBuf::from("/");
                    }
                    queue_components(&mut pending, &target, true);
                }
                Some(_) => real = next,
                None => {
                    missing = true;
                    real = next;
                }
            }
        }
        Some(Self { real, link_dirs })
    }
}

/// Queue `path`'s normal and parent components, at the front (a symlink's target, resolved before
/// whatever followed the link) or at the back.
fn queue_components(
    pending: &mut std::collections::VecDeque<std::ffi::OsString>,
    path: &Path,
    front: bool,
) {
    use std::path::Component;

    let parts: Vec<std::ffi::OsString> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_os_string()),
            Component::ParentDir => Some("..".into()),
            Component::RootDir | Component::CurDir | Component::Prefix(_) => None,
        })
        .collect();
    if front {
        for part in parts.into_iter().rev() {
            pending.push_front(part);
        }
    } else {
        pending.extend(parts);
    }
}

/// One private store: the path Tilo names it by and its resolved route.
#[derive(Debug, Clone)]
struct PrivateRoot {
    lexical: PathBuf,
    route: Route,
}

impl PrivateRoot {
    /// Is `path` at or inside this store, by either spelling?
    fn contains(&self, path: &Path) -> bool {
        path.starts_with(&self.lexical) || path.starts_with(&self.route.real)
    }
}

/// The private stores worth guarding: absolute, resolvable, and not inside another one (the outer
/// store's mask already hides it, and a nested mask could not be mounted inside the outer read-only
/// one).
fn outermost_roots(roots: &[PathBuf]) -> Vec<PrivateRoot> {
    let mut resolved: Vec<PrivateRoot> = Vec::new();
    for raw in roots {
        if !raw.is_absolute()
            || raw
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        {
            continue;
        }
        // `components` drops a stray `.` and doubled `/`.
        let lexical: PathBuf = raw.components().collect();
        let Some(route) = Route::resolve(&lexical) else {
            tracing::warn!(path = %raw.display(), "private store path does not resolve");
            continue;
        };
        if !resolved.iter().any(|other| other.route.real == route.real) {
            resolved.push(PrivateRoot { lexical, route });
        }
    }
    let all = resolved.clone();
    resolved.retain(|root| {
        !all.iter().any(|other| {
            other.route.real != root.route.real && root.route.real.starts_with(&other.route.real)
        })
    });
    resolved
}

/// Create `dir` and any missing parent owner-only, unless it exists.
fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;

    match std::fs::metadata(dir) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(std::io::Error::other("exists and is not a directory")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir),
        Err(error) => Err(error),
    }
}

/// Does a grant resolving to `real` reach `route` — contain where it lands, or a symlink on the way
/// there?
fn reaches(route: &Route, real: &Path) -> bool {
    route.real.starts_with(real) || route.link_dirs.iter().any(|dir| dir.starts_with(real))
}

impl CapabilityPlan {
    /// Keep the private stores in `roots` out of this plan's filesystem grants; the module docs say
    /// how.
    #[must_use]
    pub fn confine_private_stores(mut self, roots: &[PathBuf]) -> Self {
        if self.ro_paths.is_empty() && self.rw_paths.is_empty() {
            return self;
        }
        for root in outermost_roots(roots) {
            self.confine_one(&root);
        }
        self.pinned_dirs.sort_by(|a, b| {
            a.components()
                .count()
                .cmp(&b.components().count())
                .then(a.cmp(b))
        });
        self
    }

    fn confine_one(&mut self, root: &PrivateRoot) {
        // At or inside the store, by its name or by where the grant resolves: no bind at all.
        let inside = |granted: &PathBuf| {
            root.contains(granted)
                || std::fs::canonicalize(granted).is_ok_and(|real| root.contains(&real))
        };
        for granted in self.all_grants().filter(|granted| inside(granted)) {
            tracing::warn!(
                path = %granted.display(),
                "ignoring filesystem capability: it names Tilo's private configuration"
            );
        }
        self.ro_paths.retain(|granted| !inside(granted));
        self.rw_paths.retain(|granted| !inside(granted));

        let reaching = |route: &Route, granted: &PathBuf| {
            std::fs::canonicalize(granted).is_ok_and(|real| reaches(route, &real))
        };
        if !self
            .all_grants()
            .any(|granted| reaching(&root.route, granted))
        {
            return;
        }
        let prepared = ensure_private_dir(&root.lexical).and_then(|()| {
            Route::resolve(&root.lexical)
                .ok_or_else(|| std::io::Error::other("symlink loop on the way"))
        });
        let route = match prepared {
            Ok(route) => route,
            Err(error) => {
                tracing::warn!(
                    store = %root.lexical.display(),
                    %error,
                    "private store cannot be prepared; refusing the grants that reach it"
                );
                self.ro_paths
                    .retain(|granted| !reaching(&root.route, granted));
                self.rw_paths
                    .retain(|granted| !reaching(&root.route, granted));
                return;
            }
        };

        // A symlink on the store's route inside a writable grant could be repointed: the grant keeps
        // its reach and loses its write.
        let unpinnable: Vec<PathBuf> = self
            .rw_paths
            .iter()
            .filter(|granted| {
                std::fs::canonicalize(granted)
                    .is_ok_and(|real| route.link_dirs.iter().any(|dir| dir.starts_with(&real)))
            })
            .cloned()
            .collect();
        for granted in unpinnable {
            tracing::warn!(
                path = %granted.display(),
                "binding a writable grant read-only: a symlink inside it leads to Tilo's private \
                 configuration"
            );
            self.rw_paths.retain(|g| g != &granted);
            if !self.ro_paths.contains(&granted) {
                self.ro_paths.push(granted);
            }
        }

        let grants: Vec<PathBuf> = self.all_grants().cloned().collect();
        for granted in &grants {
            let Ok(real) = std::fs::canonicalize(granted) else {
                continue;
            };
            let Ok(below) = route.real.strip_prefix(&real) else {
                continue;
            };
            let mask = granted.join(below);
            if !self.masked_dirs.contains(&mask) {
                self.masked_dirs.push(mask);
            }
            // Pin each directory between the grant and the store whose visible mount is writable; a
            // grant path is a mount already.
            let mut dir = granted.clone();
            let mut parts = below.components().peekable();
            while let Some(part) = parts.next() {
                if parts.peek().is_none() {
                    break;
                }
                dir.push(part);
                if !grants.contains(&dir)
                    && self.innermost_grant_is_writable(&dir)
                    && !self.pinned_dirs.contains(&dir)
                {
                    self.pinned_dirs.push(dir.clone());
                }
            }
        }
    }

    fn all_grants(&self) -> impl Iterator<Item = &PathBuf> {
        self.ro_paths.iter().chain(&self.rw_paths)
    }

    /// Is the innermost grant containing `path` — the mount the sandbox shows there — writable?
    fn innermost_grant_is_writable(&self, path: &Path) -> bool {
        let depth = |granted: &PathBuf| granted.components().count();
        let innermost = |paths: &[PathBuf]| {
            paths
                .iter()
                .filter(|granted| path.starts_with(granted))
                .map(depth)
                .max()
        };
        match (innermost(&self.rw_paths), innermost(&self.ro_paths)) {
            (Some(rw), Some(ro)) => rw > ro,
            (Some(_), None) => true,
            (None, _) => false,
        }
    }
}

fn allows(
    actor: &Actor,
    resource: &ResourceRef,
    verb: Access,
    caps: &[CapOverride],
    now: Option<u64>,
) -> bool {
    decide_capability(actor, resource, verb, caps, now) == CapDecision::Allow
}

/// A path we are willing to bind: absolute, and not the root itself (binding `/` over the sandbox root
/// would replace the namespace the sandbox just built).
fn is_bindable_path(raw: &str) -> bool {
    use std::path::Component;

    let path = Path::new(raw);
    if !path.is_absolute() || raw.trim().is_empty() {
        return false;
    }
    if raw
        .as_bytes()
        .split(|byte| *byte == b'/')
        .any(|segment| segment == b"." || segment == b"..")
    {
        return false;
    }
    let mut normal_components = 0usize;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(_) => normal_components = normal_components.saturating_add(1),
            // A parent component can turn `/allowed/..` into `/`; accepting it would widen both the
            // bwrap bind and the Landlock PathFd rule.
            Component::ParentDir | Component::CurDir | Component::Prefix(_) => return false,
        }
    }
    normal_components > 0
}

/// Host paths a device class needs, as they exist on *this* machine.
pub fn host_device_paths(device: &str) -> Vec<PathBuf> {
    match device {
        "camera" => {
            let mut out = Vec::new();
            if let Ok(entries) = std::fs::read_dir("/dev") {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("video"))
                    {
                        out.push(path);
                    }
                }
            }
            out.sort();
            out
        }
        "microphone" => {
            let mut out = Vec::new();
            let runtime_dir = tilo_paths::xdg::runtime_dir_or_default();
            let pipewire = runtime_dir.join("pipewire-0");
            if pipewire.exists() {
                out.push(pipewire);
            }
            let snd = PathBuf::from("/dev/snd");
            if snd.exists() {
                out.push(snd);
            }
            out
        }
        // Screencast is brokered by the portal (a PipeWire node handed over an existing connection),
        // not by a mount.
        _ => Vec::new(),
    }
}

/// Read a capability file written by the compositor.
pub fn load_capability_file(path: &Path) -> LaunchGrants {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "capability file unreadable; launching with manifest permissions only"
            );
            return LaunchGrants::default();
        }
    };
    match serde_json::from_slice::<LaunchGrants>(&bytes) {
        Ok(grants) => grants,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "capability file malformed; launching with manifest permissions only"
            );
            LaunchGrants::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::caps::{Expiry, Subject};

    /// Deterministic stand-in for the host's device nodes, so an argv assertion doesn't depend on
    /// whether the test machine owns a webcam.
    fn fake_device_paths(device: &str) -> Vec<PathBuf> {
        match device {
            "camera" => vec![PathBuf::from("/dev/video0")],
            "microphone" => vec![PathBuf::from("/dev/snd")],
            _ => Vec::new(),
        }
    }

    fn fs_grant(app: &str, path: &str, verb: Access) -> CapOverride {
        CapOverride::grant(Subject::app(app), Scope::FsPath(path.to_string()), verb)
    }

    #[test]
    fn path_capabilities_plan_only_live_grants_for_this_app() {
        {
            let plan =
                CapabilityPlan::for_app_with("so.tilo.books", &[], Some(1000), fake_device_paths);
            assert_eq!(plan, CapabilityPlan::empty());
            assert!(plan.is_empty());
        }
        {
            let caps = vec![fs_grant(
                "so.tilo.books",
                "/var/workspace/books",
                Access::Read,
            )];
            let plan =
                CapabilityPlan::for_app_with("so.tilo.books", &caps, Some(1000), fake_device_paths);
            assert_eq!(plan.ro_paths, vec![PathBuf::from("/var/workspace/books")]);
            assert!(
                plan.rw_paths.is_empty(),
                "a Read grant must never plan a writable bind: {plan:?}"
            );
        }
        {
            let caps = vec![fs_grant(
                "so.tilo.books",
                "/var/workspace/books",
                Access::Write,
            )];
            let plan =
                CapabilityPlan::for_app_with("so.tilo.books", &caps, Some(1000), fake_device_paths);
            assert_eq!(plan.rw_paths, vec![PathBuf::from("/var/workspace/books")]);
            assert!(plan.ro_paths.is_empty());
        }
        {
            let caps = vec![fs_grant(
                "so.tilo.other",
                "/var/workspace/secrets",
                Access::Write,
            )];
            let plan =
                CapabilityPlan::for_app_with("so.tilo.books", &caps, Some(1000), fake_device_paths);
            assert_eq!(
                plan,
                CapabilityPlan::empty(),
                "a grant addressed to another app id must not reach this app: {plan:?}"
            );
        }
        {
            let caps = vec![
                fs_grant("so.tilo.books", "/var/workspace/books", Access::Write)
                    .expiring(Expiry::AtUnix(500)),
            ];
            let plan =
                CapabilityPlan::for_app_with("so.tilo.books", &caps, Some(1000), fake_device_paths);
            assert_eq!(plan, CapabilityPlan::empty());
            // Same capability, evaluated before it expires, does plan a bind — proving the emptiness
            // above is the expiry and not a typo.
            let live =
                CapabilityPlan::for_app_with("so.tilo.books", &caps, Some(499), fake_device_paths);
            assert_eq!(live.rw_paths, vec![PathBuf::from("/var/workspace/books")]);
        }
        {
            let caps = vec![
                fs_grant("so.tilo.books", "/var/workspace/books", Access::Write),
                CapOverride::new(
                    Subject::app("so.tilo.books"),
                    Scope::FsPath("/var/workspace/books".to_string()),
                    Effect::Restrict,
                    Access::Read,
                ),
            ];
            let plan =
                CapabilityPlan::for_app_with("so.tilo.books", &caps, Some(1000), fake_device_paths);
            assert!(
                plan.rw_paths.is_empty(),
                "a Restrict capped at Read must remove the writable bind: {plan:?}"
            );
            assert_eq!(plan.ro_paths, vec![PathBuf::from("/var/workspace/books")]);
        }
    }

    #[test]
    fn device_and_network_capabilities_plan_their_own_resources() {
        {
            let caps = vec![CapOverride::grant(
                Subject::app("so.tilo.meet"),
                Scope::Device("camera".to_string()),
                Access::Read,
            )];
            let plan =
                CapabilityPlan::for_app_with("so.tilo.meet", &caps, Some(1), fake_device_paths);
            assert_eq!(plan.devices, vec!["camera".to_string()]);
            assert_eq!(plan.rw_paths, vec![PathBuf::from("/dev/video0")]);

            let without =
                CapabilityPlan::for_app_with("so.tilo.meet", &[], Some(1), fake_device_paths);
            assert!(
                without.rw_paths.is_empty() && without.devices.is_empty(),
                "no camera capability means no device node at all: {without:?}"
            );
        }
        {
            let caps = vec![CapOverride::grant(
                Subject::app("so.tilo.meet"),
                Scope::Device("screencast".to_string()),
                Access::Read,
            )];
            let plan =
                CapabilityPlan::for_app_with("so.tilo.meet", &caps, Some(1), fake_device_paths);
            assert_eq!(plan.devices, vec!["screencast".to_string()]);
            assert!(
                plan.rw_paths.is_empty() && plan.ro_paths.is_empty(),
                "screencast is portal-brokered, not a mount: {plan:?}"
            );
        }
        {
            let caps = vec![CapOverride::grant(
                Subject::app("so.tilo.mail"),
                Scope::NetHost("mail.example.com".to_string()),
                Access::Write,
            )];
            let plan =
                CapabilityPlan::for_app_with("so.tilo.mail", &caps, Some(1), fake_device_paths);
            assert!(plan.allow_network);
            assert_eq!(plan.net_hosts, vec!["mail.example.com".to_string()]);

            let without =
                CapabilityPlan::for_app_with("so.tilo.mail", &[], Some(1), fake_device_paths);
            assert!(
                !without.allow_network,
                "without a network capability the namespace stays isolated"
            );
        }
        {
            let grant = |host: &str| {
                CapOverride::grant(
                    Subject::app("so.tilo.mail"),
                    Scope::NetHost(host.to_string()),
                    Access::Write,
                )
            };
            let caps = vec![
                grant(".fastmail.com"),
                grant("*.example.com"),
                grant("10.0.0.5"),
            ];
            let plan =
                CapabilityPlan::for_app_with("so.tilo.mail", &caps, Some(1), fake_device_paths);
            assert_eq!(
                plan.net_hosts,
                vec![".fastmail.com".to_string(), "*.example.com".to_string()],
                "a domain grant in either spelling stands; an address is not a host"
            );
        }
    }

    #[test]
    fn capability_files_fail_closed_and_round_trip() {
        {
            let caps = vec![
                fs_grant("so.tilo.books", "relative/path", Access::Write),
                fs_grant("so.tilo.books", "/", Access::Write),
                fs_grant("so.tilo.books", "//", Access::Write),
                fs_grant("so.tilo.books", "/tmp/..", Access::Write),
                fs_grant("so.tilo.books", "/var/./workspace", Access::Write),
            ];
            let plan =
                CapabilityPlan::for_app_with("so.tilo.books", &caps, Some(1), fake_device_paths);
            assert_eq!(
                plan,
                CapabilityPlan::empty(),
                "root aliases and traversal must never become bind/Landlock grants: {plan:?}"
            );
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let missing = dir.path().join("absent.json");
            assert!(load_capability_file(&missing).is_empty());

            let malformed = dir.path().join("bad.json");
            std::fs::write(&malformed, b"{ this is not json ").unwrap();
            assert!(load_capability_file(&malformed).is_empty());

            let wrong_shape = dir.path().join("wrong.json");
            std::fs::write(&wrong_shape, br#"{"subject":"nope"}"#).unwrap();
            assert!(load_capability_file(&wrong_shape).is_empty());
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("caps.json");
            let caps = vec![fs_grant(
                "so.tilo.books",
                "/var/workspace/books",
                Access::Read,
            )];
            let grants = LaunchGrants {
                authors: vec!["so.tilo.books".into()],
                sandbox: caps,
            };
            std::fs::write(&path, serde_json::to_vec(&grants).unwrap()).unwrap();
            assert_eq!(load_capability_file(&path), grants);
        }
    }

    /// A person's home with Tilo's store in it, the way `private_store_roots` names it, and plans
    /// confined against it.
    struct Fixture {
        _dir: tempfile::TempDir,
        home: PathBuf,
        store: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join("home");
            let store = home.join(".config/tilo");
            std::fs::create_dir_all(store.join("identity/secrets")).unwrap();
            std::fs::create_dir_all(home.join("Documents")).unwrap();
            Self {
                _dir: dir,
                home,
                store,
            }
        }

        fn path(&self, rel: &str) -> String {
            self.home.join(rel).display().to_string()
        }

        fn plan(&self, grants: &[(String, Access)]) -> CapabilityPlan {
            let caps: Vec<CapOverride> = grants
                .iter()
                .map(|(path, verb)| fs_grant("so.tilo.files", path, *verb))
                .collect();
            CapabilityPlan::for_app_with("so.tilo.files", &caps, Some(1), fake_device_paths)
                .confine_private_stores(std::slice::from_ref(&self.store))
        }
    }

    #[test]
    fn grants_covering_the_private_store_mask_it_and_pin_the_way_there() {
        let home = Fixture::new();
        let config = home.home.join(".config");
        {
            let plan = home.plan(&[(home.path(""), Access::Write)]);
            assert_eq!(plan.rw_paths, vec![home.home.clone()]);
            assert_eq!(plan.masked_dirs, vec![home.store.clone()]);
            assert_eq!(
                plan.pinned_dirs,
                vec![config.clone()],
                "~/.config must be a mount point, or the app could move the store aside"
            );
        }
        {
            let plan = home.plan(&[(home.path(""), Access::Read)]);
            assert_eq!(plan.ro_paths, vec![home.home.clone()]);
            assert_eq!(plan.masked_dirs, vec![home.store.clone()]);
            assert!(plan.pinned_dirs.is_empty(), "{plan:?}");
        }
        {
            let caps = vec![
                fs_grant("so.tilo.files", &home.path(""), Access::Write),
                fs_grant("so.tilo.files", &home.path(".config"), Access::Read),
                CapOverride::new(
                    Subject::app("so.tilo.files"),
                    Scope::FsPath(home.path(".config")),
                    Effect::Restrict,
                    Access::Read,
                ),
            ];
            let plan =
                CapabilityPlan::for_app_with("so.tilo.files", &caps, Some(1), fake_device_paths)
                    .confine_private_stores(std::slice::from_ref(&home.store));
            assert_eq!(plan.ro_paths, vec![config]);
            assert_eq!(plan.masked_dirs, vec![home.store.clone()]);
            assert!(
                plan.pinned_dirs.is_empty(),
                "~/.config is a grant mount already; a writable pin would widen it: {plan:?}"
            );
        }
        {
            let plan = home.plan(&[(home.path("Documents"), Access::Write)]);
            assert_eq!(plan.rw_paths, vec![home.home.join("Documents")]);
            assert!(plan.masked_dirs.is_empty() && plan.pinned_dirs.is_empty());
        }
        {
            for inside in [
                ".config/tilo",
                ".config/tilo/identity",
                ".config/tilo/x.toml",
            ] {
                let plan = home.plan(&[(home.path(inside), Access::Write)]);
                assert!(
                    plan.ro_paths.is_empty() && plan.rw_paths.is_empty(),
                    "a grant on {inside} must bind nothing: {plan:?}"
                );
            }
            std::os::unix::fs::symlink(home.store.join("identity"), home.home.join("keys"))
                .unwrap();
            let plan = home.plan(&[(home.path("keys"), Access::Read)]);
            assert!(
                plan.ro_paths.is_empty(),
                "a grant reaching the store through a symlink is refused: {plan:?}"
            );
        }
    }

    #[test]
    fn a_missing_or_symlinked_store_still_cannot_be_reached() {
        {
            use std::os::unix::fs::PermissionsExt as _;

            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join("home");
            std::fs::create_dir_all(&home).unwrap();
            let store = home.join(".config/tilo");
            let caps = vec![fs_grant(
                "so.tilo.files",
                &home.display().to_string(),
                Access::Write,
            )];
            let plan =
                CapabilityPlan::for_app_with("so.tilo.files", &caps, Some(1), fake_device_paths)
                    .confine_private_stores(std::slice::from_ref(&store));
            assert!(
                store.is_dir(),
                "a writable grant must not be able to create the store itself"
            );
            assert_eq!(
                std::fs::metadata(&store).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(plan.masked_dirs, vec![store]);
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let store = dir.path().join("home/.config/tilo");
            let caps = vec![fs_grant(
                "so.tilo.files",
                "/var/workspace/books",
                Access::Write,
            )];
            let plan =
                CapabilityPlan::for_app_with("so.tilo.files", &caps, Some(1), fake_device_paths)
                    .confine_private_stores(std::slice::from_ref(&store));
            assert!(!store.exists());
            assert!(plan.masked_dirs.is_empty());
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join("home");
            let real = home.join("dotfiles/tilo");
            std::fs::create_dir_all(&real).unwrap();
            std::fs::create_dir_all(home.join(".config")).unwrap();
            std::os::unix::fs::symlink(&real, home.join(".config/tilo")).unwrap();
            let caps = vec![fs_grant(
                "so.tilo.files",
                &home.display().to_string(),
                Access::Write,
            )];
            let plan =
                CapabilityPlan::for_app_with("so.tilo.files", &caps, Some(1), fake_device_paths)
                    .confine_private_stores(&[home.join(".config/tilo")]);
            assert!(plan.rw_paths.is_empty(), "{plan:?}");
            assert_eq!(plan.ro_paths, vec![home]);
            assert_eq!(plan.masked_dirs, vec![real]);
            assert!(plan.pinned_dirs.is_empty());
        }
    }

    /// A launch file names which apps author the workspace; an app reads its own entry and nothing of
    /// another app's.
    #[test]
    fn a_launch_file_tells_each_app_whether_it_authors() {
        let grants = LaunchGrants {
            authors: vec!["so.tilo.editor".into()],
            sandbox: vec![fs_grant(
                "so.tilo.viewer",
                "/var/workspace/shared",
                Access::Write,
            )],
        };
        assert!(grants.authors("so.tilo.editor"));
        assert!(!grants.authors("so.tilo.viewer"));
        let viewer = grants.for_app("so.tilo.viewer");
        assert!(viewer.authors.is_empty());
        assert_eq!(viewer.sandbox.len(), 1);
        assert!(grants.for_app("so.tilo.editor").sandbox.is_empty());
    }
}
