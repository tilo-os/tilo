//! Workspace capabilities → sandbox restrictions.
//!
//! The package manifest says what an app asks for *at install time*.
//! This module carries the other half: what the **user** granted it at
//! runtime. Both must be true before a path, device or network is
//! reachable — the manifest is a ceiling, the capabilities are the
//! user's decision, and the sandbox binds only what survives both.
//!
//! ## The vocabulary is not ours
//!
//! A capability is a [`CapOverride`] from `collab-types` — the same
//! record the compositor stores and shows in the permissions UI.
//! No launcher-local permission type is used, or the recorded and
//! enforced grants could drift apart.
//!
//! ## How a capability becomes a kernel restriction
//!
//! | Scope | Verb | Effect on the sandbox |
//! |---|---|---|
//! | [`Scope::FsPath`] | [`Access::Read`] | `--ro-bind-try <p> <p>` + a Landlock read rule |
//! | [`Scope::FsPath`] | [`Access::Write`] | `--bind-try <p> <p>` + a Landlock read/write rule |
//! | [`Scope::Device`] `"camera"` | any | the host's `/dev/video*` nodes are bound |
//! | [`Scope::Device`] `"microphone"` | any | the `PipeWire` socket + `/dev/snd` are bound |
//! | [`Scope::Device`] `"screencast"` | any | nothing — brokered by the portal, not a bind |
//! | [`Scope::NetHost`] `"*"` | any | the network namespace is shared instead of isolated |
//! | [`Scope::NetHost`] a host | any | the namespace stays isolated and egress is brokered through [`crate::netgate`] |
//!
//! Everything else produces no bind. **Absence of a capability is
//! enforced by absence of the mount.**
//!
//! ## Per-host network access
//!
//! A [`Scope::NetHost`] capability naming a host used to be applied as
//! *unrestricted*, since the network namespace is all-or-nothing. It
//! now reaches a mechanism: the namespace stays empty and the app's
//! only route out is a socket the launcher holds, matching the
//! allowlist against the **name** before it's resolved — see
//! [`crate::netgate`].
//!
//! ## Read-only participation
//!
//! [`Participation`] answers "may this app originate changes at all?"
//! — read as a capability, not a mode an app opts into. No new
//! vocabulary needed: a workspace-scoped `Restrict` at `Access::Read`
//! already reads "see everything, change nothing" through the same
//! [`decide_capability`] every other decision goes through.
//!
//! A read-only participant gets no writable path. The launcher still gives
//! it the public identity projection for attribution; no app receives root
//! signing material. `TILO_PARTICIPATION=read-only` tells the app which shape
//! it is in. At launch, an unscoped app inside a workspace is resolved to
//! read-only so missing policy fails closed.
//!
//! ## Deny by default
//!
//! [`load_capability_file`] returns an empty list for a missing,
//! unreadable or malformed file — every failure mode grants nothing.
//! Nothing here can *widen* a manifest, only add what the user granted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use collab_types::caps::{
    Access, Actor, CapDecision, CapOverride, Effect, ResourceRef, Scope, decide_capability,
};

/// How an app may take part in the space it is launched into. Resolved
/// from the capability list by [`CapabilityPlan::for_app`], not
/// declared by the app and not a UI setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Participation {
    /// No workspace-scoped capability names this app either way, so nothing
    /// is inferred — the default, deliberately: guessing here would decide
    /// who may author in a room, not this module's call.
    #[default]
    Unscoped,
    /// May see the space; may not change it. The identity is withheld, so
    /// the app cannot originate an authored change at all.
    ReadOnly,
    /// May change the space.
    Author,
}

impl Participation {
    /// Should the sandbox carry the user identity? The one question the
    /// answer feeds — a read-only participant holds no signing key, which
    /// is what makes "cannot author" structural rather than enforced after
    /// the fact.
    pub fn carries_identity(self) -> bool {
        !matches!(self, Participation::ReadOnly)
    }

    /// The value of `TILO_PARTICIPATION` inside the sandbox, if the mode is
    /// one worth telling the app about.
    pub fn env_value(self) -> Option<&'static str> {
        match self {
            Participation::ReadOnly => Some("read-only"),
            Participation::Author => Some("author"),
            Participation::Unscoped => None,
        }
    }
}

/// The resolved, host-side consequence of an app's capabilities.
///
/// Pure data: [`CapabilityPlan::for_app`] does the policy evaluation,
/// [`crate::sandbox::build_bwrap_argv`] turns the result into bwrap
/// flags, and `run.rs` turns it into Landlock rules — nothing
/// downstream re-reads the capability list, so the two enforcement
/// layers cannot disagree about what was granted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilityPlan {
    /// Host paths to bind read-only, at the same path in the sandbox.
    pub ro_paths: Vec<PathBuf>,
    /// Host paths to bind read-write, at the same path in the sandbox.
    pub rw_paths: Vec<PathBuf>,
    /// Device classes that were granted (`"camera"`, `"microphone"`,
    /// `"screencast"`). Kept for logging/tests; the paths they imply are
    /// already in [`Self::rw_paths`].
    pub devices: Vec<String>,
    /// Whether any network capability was granted at all — says nothing
    /// about *which* hosts. [`Self::net_hosts`] carries the difference,
    /// and `crate::sandbox::egress_decision` is what acts on it.
    pub allow_network: bool,
    /// The host patterns the network capabilities named. Enforced: a list
    /// that is not exactly `["*"]` leaves the namespace isolated and
    /// becomes the allowlist [`crate::netgate`] filters on.
    pub net_hosts: Vec<String>,
    /// Whether the app may originate changes in the space. Decides
    /// whether the sandbox carries a signing identity at all — see
    /// [`Participation`].
    pub participation: Participation,
}

impl CapabilityPlan {
    /// The plan for an app that was granted nothing. `const` so call
    /// sites can spell "no capabilities" without allocating.
    pub const fn empty() -> Self {
        Self {
            ro_paths: Vec::new(),
            rw_paths: Vec::new(),
            devices: Vec::new(),
            allow_network: false,
            net_hosts: Vec::new(),
            participation: Participation::Unscoped,
        }
    }

    /// Does this plan add anything to the manifest's sandbox?
    pub fn is_empty(&self) -> bool {
        self.ro_paths.is_empty()
            && self.rw_paths.is_empty()
            && self.devices.is_empty()
            && !self.allow_network
    }

    /// Resolve `caps` into the binds `app_id` has actually been granted, as
    /// of `now` (seconds since the Unix epoch; `None` skips expiry
    /// evaluation).
    ///
    /// The decision is `collab_types::decide_capability` — the same
    /// fail-closed function the compositor and the filesystem call — so a
    /// `Restrict` override or an expired grant removes a bind here exactly
    /// as it removes an allow there. An app holds no base authority: every
    /// bind below exists because some capability explicitly says so.
    pub fn for_app(app_id: &str, caps: &[CapOverride], now: Option<u64>) -> Self {
        Self::for_app_with(app_id, caps, now, host_device_paths)
    }

    /// [`Self::for_app`] with the device-node lookup injected.
    ///
    /// Which `/dev/video*` nodes exist is a property of the host, not the
    /// policy, so tests substitute a fixed resolver to assert on an exact
    /// argv — the policy evaluation itself is the production path either way.
    pub fn for_app_with(
        app_id: &str,
        caps: &[CapOverride],
        now: Option<u64>,
        device_paths: fn(&str) -> Vec<PathBuf>,
    ) -> Self {
        let actor = Actor::app(app_id);
        // path → writable. A path granted both Read and Write ends up
        // read-write once, never bound twice.
        let mut fs: BTreeMap<String, bool> = BTreeMap::new();
        let mut devices: Vec<String> = Vec::new();
        let mut net_hosts: Vec<String> = Vec::new();

        for cap in caps {
            // Only grants nominate a resource. A `Restrict` cannot introduce a
            // bind, only take one away, through `decide_capability` below.
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
                    // `Read` is the floor of the verb ladder, so a grant
                    // phrased in any verb answers it.
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
                    // A stored grant that is not a host in the one grammar
                    // (an IP literal, written before addresses were refused)
                    // covers nothing; say so rather than drop it silently.
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
                // Workspace / App / Room / artifact / action scopes are about the CRDT
                // and the room, not the kernel — inert here, never permissive.
                _ => {}
            }
        }

        let mut plan = CapabilityPlan {
            allow_network: !net_hosts.is_empty(),
            net_hosts,
            participation: participation_for(&actor, caps, now),
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

/// Read the app's participation in the space out of the capability list.
///
/// One question asked twice of [`decide_capability`], against
/// `ResourceRef::Workspace`: may it write, and may it read? "Read but
/// not write" is read-only participation.
///
/// Deliberately not a fourth enforcement point to keep in step with the
/// other three — same decision function, same capability list, so a
/// grant the compositor shows as read-only cannot be an authoring
/// grant here.
fn participation_for(actor: &Actor, caps: &[CapOverride], now: Option<u64>) -> Participation {
    let resource = ResourceRef::Workspace;
    if allows(actor, &resource, Access::Write, caps, now) {
        return Participation::Author;
    }
    if allows(actor, &resource, Access::Read, caps, now) {
        return Participation::ReadOnly;
    }
    Participation::Unscoped
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

/// A path we are willing to bind: absolute, and not the root itself
/// (binding `/` over the sandbox root would replace the namespace the
/// sandbox just built).
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
            // A parent component can turn `/allowed/..` into `/`; accepting
            // it would widen both the bwrap bind and the Landlock PathFd rule.
            Component::ParentDir | Component::CurDir | Component::Prefix(_) => return false,
        }
    }
    normal_components > 0
}

/// Host paths a device class needs, as they exist on *this* machine.
///
/// Best-effort by design: a host with no camera yields no paths and the
/// launch proceeds with the grant unsatisfied, rather than failing.
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
            let runtime_dir = tilo_paths::xdg::runtime_dir().unwrap_or_else(|| {
                // SAFETY: getuid always succeeds.
                PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }))
            });
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
        // Screencast is brokered by the portal (a PipeWire node handed
        // over an existing connection), not by a mount. Nothing to bind.
        _ => Vec::new(),
    }
}

/// Read a capability file written by the compositor.
///
/// **Deny by default.** A missing file, an unreadable one, or one that
/// doesn't deserialise into `Vec<CapOverride>` all yield an empty list
/// — the app gets exactly what its manifest declared, nothing more.
/// Never returns an error for that reason: no failure mode here should
/// widen the sandbox, and none should abort a launch either.
pub fn load_capability_file(path: &Path) -> Vec<CapOverride> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "capability file unreadable; launching with manifest permissions only"
            );
            return Vec::new();
        }
    };
    match serde_json::from_slice::<Vec<CapOverride>>(&bytes) {
        Ok(caps) => caps,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "capability file malformed; launching with manifest permissions only"
            );
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::caps::{Expiry, Subject};

    /// Deterministic stand-in for the host's device nodes, so an argv
    /// assertion doesn't depend on whether the test machine owns a webcam.
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
        // Phase `no_capabilities_plans_nothing`.
        {
            let plan =
                CapabilityPlan::for_app_with("so.tilo.books", &[], Some(1000), fake_device_paths);
            assert_eq!(plan, CapabilityPlan::empty());
            assert!(plan.is_empty());
        }
        // Phase `read_capability_plans_a_read_only_path_only`.
        {
            let caps = vec![
                CapOverride::grant(
                    Subject::app("so.tilo.books"),
                    Scope::Workspace,
                    Access::Write,
                ),
                fs_grant("so.tilo.books", "/var/workspace/books", Access::Read),
            ];
            let plan =
                CapabilityPlan::for_app_with("so.tilo.books", &caps, Some(1000), fake_device_paths);
            assert_eq!(plan.ro_paths, vec![PathBuf::from("/var/workspace/books")]);
            assert!(
                plan.rw_paths.is_empty(),
                "a Read grant must never plan a writable bind: {plan:?}"
            );
        }
        // Phase `write_capability_plans_a_read_write_path`.
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
        // Phase `another_apps_capability_is_not_planned`.
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
        // Phase `expired_capability_is_not_planned`.
        {
            let caps = vec![
                fs_grant("so.tilo.books", "/var/workspace/books", Access::Write)
                    .expiring(Expiry::AtUnix(500)),
            ];
            let plan =
                CapabilityPlan::for_app_with("so.tilo.books", &caps, Some(1000), fake_device_paths);
            assert_eq!(plan, CapabilityPlan::empty());
            // Same capability, evaluated before it expires, does plan a bind
            // — proving the emptiness above is the expiry and not a typo.
            let live =
                CapabilityPlan::for_app_with("so.tilo.books", &caps, Some(499), fake_device_paths);
            assert_eq!(live.rw_paths, vec![PathBuf::from("/var/workspace/books")]);
        }
        // Phase `restriction_removes_a_granted_path`.
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
        // Phase `camera_capability_plans_device_nodes_and_absence_plans_none`.
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
        // Phase `screencast_capability_binds_nothing`.
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
        // Phase `net_host_capability_opens_the_namespace_and_records_the_host`.
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
        // Phase `domain_grants_survive_the_plan_and_address_grants_do_not`.
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
        // Phase `relative_root_and_lexical_escape_paths_are_refused`.
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
        // Phase `missing_or_malformed_capability_file_grants_nothing`.
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
        // Phase `well_formed_capability_file_round_trips`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("caps.json");
            let caps = vec![fs_grant(
                "so.tilo.books",
                "/var/workspace/books",
                Access::Read,
            )];
            std::fs::write(&path, serde_json::to_vec(&caps).unwrap()).unwrap();
            assert_eq!(load_capability_file(&path), caps);
        }
    }

    /// A workspace-scoped `Restrict` at `Access::Read`: "see everything
    /// here, change nothing". The existing vocabulary, not a new one.
    fn read_only_participation(app: &str) -> Vec<CapOverride> {
        vec![
            CapOverride::grant(Subject::app(app), Scope::Workspace, Access::Read),
            CapOverride::new(
                Subject::app(app),
                Scope::Workspace,
                Effect::Restrict,
                Access::Read,
            ),
        ]
    }

    #[test]
    fn participation_follows_the_live_workspace_grant() {
        // Phase `a_read_only_grant_resolves_to_read_only_participation`.
        {
            let caps = read_only_participation("so.tilo.viewer");
            let plan = CapabilityPlan::for_app_with(
                "so.tilo.viewer",
                &caps,
                Some(1_000),
                fake_device_paths,
            );
            assert_eq!(plan.participation, Participation::ReadOnly);
            assert!(
                !plan.participation.carries_identity(),
                "a read-only participant must hold no signing key — that is what makes it unable \
                 to originate a change rather than merely refused one"
            );
            assert_eq!(plan.participation.env_value(), Some("read-only"));
        }
        // Phase `a_write_grant_resolves_to_authorship`.
        {
            let caps = vec![CapOverride::grant(
                Subject::app("so.tilo.editor"),
                Scope::Workspace,
                Access::Write,
            )];
            let plan = CapabilityPlan::for_app_with(
                "so.tilo.editor",
                &caps,
                Some(1_000),
                fake_device_paths,
            );
            assert_eq!(plan.participation, Participation::Author);
            assert!(plan.participation.carries_identity());
        }
        // Phase `no_workspace_capability_leaves_participation_unscoped`.
        // No workspace-scoped capability says nothing about participation, so
        // nothing is inferred — this module doesn't decide who may author.
        {
            let caps = vec![fs_grant(
                "so.tilo.books",
                "/var/workspace/books",
                Access::Write,
            )];
            let plan = CapabilityPlan::for_app_with(
                "so.tilo.books",
                &caps,
                Some(1_000),
                fake_device_paths,
            );
            assert_eq!(plan.participation, Participation::Unscoped);
            assert!(plan.participation.carries_identity());
            assert_eq!(plan.participation.env_value(), None);

            let none =
                CapabilityPlan::for_app_with("so.tilo.books", &[], Some(1_000), fake_device_paths);
            assert_eq!(none.participation, Participation::Unscoped);
        }
        // Phase `read_only_participation_leaves_no_writable_bind`.
        // The same restriction that caps participation caps the filesystem,
        // since `Scope::Workspace` matches every resource — one capability,
        // not two rules that could disagree.
        {
            let mut caps = read_only_participation("so.tilo.viewer");
            caps.push(fs_grant(
                "so.tilo.viewer",
                "/var/workspace/shared",
                Access::Write,
            ));
            let plan = CapabilityPlan::for_app_with(
                "so.tilo.viewer",
                &caps,
                Some(1_000),
                fake_device_paths,
            );
            assert_eq!(plan.participation, Participation::ReadOnly);
            assert!(
                plan.rw_paths.is_empty(),
                "a read-only participant must have no writable path: {plan:?}"
            );
            assert_eq!(
                plan.ro_paths,
                vec![PathBuf::from("/var/workspace/shared")],
                "it still sees the shared tree — read-only participation is participation"
            );
        }
        // Phase `another_apps_read_only_grant_does_not_apply`.
        // Participation is another app's business only if it names that app.
        {
            let caps = read_only_participation("so.tilo.viewer");
            let plan = CapabilityPlan::for_app_with(
                "so.tilo.editor",
                &caps,
                Some(1_000),
                fake_device_paths,
            );
            assert_eq!(plan.participation, Participation::Unscoped);
        }
        // Phase `an_expired_participation_grant_says_nothing`.
        // An expired read-only grant expires into "nothing said", not
        // authorship — identity returns only because nothing scopes it.
        {
            let caps: Vec<CapOverride> = read_only_participation("so.tilo.viewer")
                .into_iter()
                .map(|c| c.expiring(Expiry::AtUnix(500)))
                .collect();
            let live =
                CapabilityPlan::for_app_with("so.tilo.viewer", &caps, Some(499), fake_device_paths);
            assert_eq!(live.participation, Participation::ReadOnly);
            let lapsed =
                CapabilityPlan::for_app_with("so.tilo.viewer", &caps, Some(501), fake_device_paths);
            assert_eq!(lapsed.participation, Participation::Unscoped);
        }
    }
}
