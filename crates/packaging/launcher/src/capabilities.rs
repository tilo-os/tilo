//! Workspace capabilities → real sandbox restrictions.
//!
//! The package manifest says what an app asks for *at install time*.
//! This module carries the other half: what the **user** granted it at
//! runtime, in the workspace it is being launched into. Both must be
//! true before a path, a device or the network is reachable from inside
//! the sandbox — the manifest is a ceiling the packager declared, the
//! capabilities are the user's decision, and the sandbox binds only
//! what survives both.
//!
//! ## The vocabulary is not ours
//!
//! A capability is a [`CapOverride`] from `collab-types` — the same
//! record the compositor stores on a workspace, shows in the
//! permissions UI and replicates to peers. There is deliberately no
//! launcher-local permission type: a second vocabulary is how the
//! recorded grant and the enforced grant drift apart.
//!
//! ## How a capability becomes a kernel restriction
//!
//! | Scope | Verb | Effect on the sandbox |
//! |---|---|---|
//! | [`Scope::FsPath`] | [`Verb::Read`] | `--ro-bind-try <p> <p>` + a Landlock read rule |
//! | [`Scope::FsPath`] | [`Verb::Write`] | `--bind-try <p> <p>` + a Landlock read/write rule |
//! | [`Scope::Device`] `"camera"` | any | the host's `/dev/video*` nodes are bound |
//! | [`Scope::Device`] `"microphone"` | any | the PipeWire socket + `/dev/snd` are bound |
//! | [`Scope::Device`] `"screencast"` | any | nothing — brokered by the portal, not a bind |
//! | [`Scope::NetHost`] | any | the network namespace is shared instead of isolated |
//!
//! Everything not in that table produces no bind at all. **Absence of a
//! capability is enforced by absence of the mount**: with no camera
//! capability there is no `/dev/video0` inside the namespace, so the
//! app cannot open one however it asks.
//!
//! ## Honest gap: per-host network filtering
//!
//! A [`Scope::NetHost`] capability naming a specific host is recorded
//! faithfully and then applied as *unrestricted* outbound network
//! access, because the only mechanism in play is the network namespace
//! and a namespace is all-or-nothing. There is no packet filter here.
//! [`CapabilityPlan::for_app`] logs a warning every time it applies one
//! so the gap is visible in the journal rather than only in a comment.
//!
//! ## Deny by default
//!
//! [`load_capability_file`] returns an empty list for a file that is
//! missing, unreadable or malformed. Every failure mode grants nothing:
//! the app runs with exactly what its manifest declared. Nothing here
//! can *widen* a manifest, only add to it what the user explicitly
//! granted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use collab_types::caps::{
    Actor, CapDecision, CapOverride, Effect, ResourceRef, Scope, Verb, decide_capability,
};

/// The resolved, host-side consequence of an app's capabilities.
///
/// Pure data: [`CapabilityPlan::for_app`] does the policy evaluation and
/// the device-node lookup, [`crate::sandbox::build_bwrap_argv`] turns
/// the result into bwrap flags, and `run.rs` turns the same result into
/// Landlock rules. Nothing downstream re-reads the capability list, so
/// the two enforcement layers cannot disagree about what was granted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilityPlan {
    /// Host paths to bind read-only, at the same path in the sandbox.
    pub ro_paths: Vec<PathBuf>,
    /// Host paths to bind read-write, at the same path in the sandbox.
    pub rw_paths: Vec<PathBuf>,
    /// Device classes that were granted (`"camera"`, `"microphone"`,
    /// `"screencast"`). Kept for logging and for tests; the paths they
    /// imply are already in [`Self::rw_paths`].
    pub devices: Vec<String>,
    /// Whether any network capability was granted. When false the
    /// sandbox keeps the unshared (empty) network namespace.
    pub allow_network: bool,
    /// The host patterns the network capabilities named. Advisory
    /// only — see the module docs' "honest gap".
    pub net_hosts: Vec<String>,
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
        }
    }

    /// Does this plan add anything to the manifest's sandbox?
    pub fn is_empty(&self) -> bool {
        self.ro_paths.is_empty()
            && self.rw_paths.is_empty()
            && self.devices.is_empty()
            && !self.allow_network
    }

    /// Resolve `caps` into the binds `app_id` has actually been granted,
    /// as of `now` (seconds since the Unix epoch; `None` skips expiry
    /// evaluation).
    ///
    /// The decision is `collab_types::decide_capability` — the same
    /// fail-closed function the compositor and the filesystem call — so
    /// a `Restrict` override or an expired grant removes a bind here
    /// exactly as it removes an allow there. An app holds no role, so it
    /// has no base authority: every bind below exists because some
    /// capability explicitly says so.
    pub fn for_app(app_id: &str, caps: &[CapOverride], now: Option<u64>) -> Self {
        Self::for_app_with(app_id, caps, now, host_device_paths)
    }

    /// [`Self::for_app`] with the device-node lookup injected.
    ///
    /// Which `/dev/video*` nodes exist is a property of the host, not of
    /// the policy, so tests substitute a fixed resolver to assert on an
    /// exact argv. The policy evaluation — who gets a camera at all — is
    /// the production path either way.
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
            // Only grants nominate a resource. A `Restrict` cannot
            // introduce a bind; it can only take one away, which it does
            // through `decide_capability` below.
            if cap.effect != Effect::Grant || !cap.subject.matches_actor(&actor) {
                continue;
            }
            match &cap.scope {
                Scope::FsPath(raw) => {
                    let resource = ResourceRef::FsPath(raw.clone());
                    let writable = allows(&actor, &resource, Verb::Write, caps, now);
                    let readable = writable || allows(&actor, &resource, Verb::Read, caps, now);
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
                    if !allows(&actor, &ResourceRef::Device(device.clone()), Verb::Read, caps, now)
                    {
                        continue;
                    }
                    if !devices.iter().any(|d| d == device) {
                        devices.push(device.clone());
                    }
                }
                Scope::NetHost(host) => {
                    if !allows(&actor, &ResourceRef::NetHost(host.clone()), Verb::Read, caps, now) {
                        continue;
                    }
                    if host != "*" {
                        tracing::warn!(
                            app = %app_id,
                            host = %host,
                            "network capability names a specific host, but the sandbox can only \
                             share or unshare the whole network namespace: this grant is applied \
                             as UNRESTRICTED outbound network access. Per-host filtering is not \
                             enforced."
                        );
                    }
                    if !net_hosts.iter().any(|h| h == host) {
                        net_hosts.push(host.clone());
                    }
                }
                // Workspace / App / Room / artifact / action scopes are
                // about the CRDT and the room, not about the kernel.
                // They are inert here — never permissive.
                _ => {}
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

fn allows(
    actor: &Actor,
    resource: &ResourceRef,
    verb: Verb,
    caps: &[CapOverride],
    now: Option<u64>,
) -> bool {
    decide_capability(actor, resource, verb, caps, now) == CapDecision::Allow
}

/// A path we are willing to bind: absolute, and not the root itself
/// (binding `/` over the sandbox root would replace the namespace the
/// sandbox just built).
fn is_bindable_path(raw: &str) -> bool {
    let path = Path::new(raw);
    path.is_absolute() && raw != "/" && !raw.trim().is_empty()
}

/// Host paths a device class needs, as they exist on *this* machine.
///
/// Best-effort by design: a host with no camera yields no paths and the
/// launch proceeds with the grant unsatisfied, rather than failing
/// because the user permitted hardware they don't have.
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
            let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
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
/// does not deserialise into `Vec<CapOverride>` all yield an empty list:
/// the app then gets exactly what its manifest declared and nothing
/// more. This function never returns an error for that reason — there is
/// no failure mode here that should widen the sandbox, and none that
/// should abort a launch either.
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

/// Seconds since the Unix epoch, for expiry evaluation.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::caps::{Expiry, Subject};

    /// Deterministic stand-in for the host's device nodes, so an argv
    /// assertion doesn't depend on whether the machine running the test
    /// happens to own a webcam.
    fn fake_device_paths(device: &str) -> Vec<PathBuf> {
        match device {
            "camera" => vec![PathBuf::from("/dev/video0")],
            "microphone" => vec![PathBuf::from("/dev/snd")],
            _ => Vec::new(),
        }
    }

    fn fs_grant(app: &str, path: &str, verb: Verb) -> CapOverride {
        CapOverride::grant(Subject::app(app), Scope::FsPath(path.to_string()), verb)
    }

    #[test]
    fn no_capabilities_plans_nothing() {
        let plan = CapabilityPlan::for_app_with("dev.qos.books", &[], Some(1000), fake_device_paths);
        assert_eq!(plan, CapabilityPlan::empty());
        assert!(plan.is_empty());
    }

    #[test]
    fn read_capability_plans_a_read_only_path_only() {
        let caps = vec![fs_grant("dev.qos.books", "/var/workspace/books", Verb::Read)];
        let plan =
            CapabilityPlan::for_app_with("dev.qos.books", &caps, Some(1000), fake_device_paths);
        assert_eq!(plan.ro_paths, vec![PathBuf::from("/var/workspace/books")]);
        assert!(
            plan.rw_paths.is_empty(),
            "a Read grant must never plan a writable bind: {plan:?}"
        );
    }

    #[test]
    fn write_capability_plans_a_read_write_path() {
        let caps = vec![fs_grant("dev.qos.books", "/var/workspace/books", Verb::Write)];
        let plan =
            CapabilityPlan::for_app_with("dev.qos.books", &caps, Some(1000), fake_device_paths);
        assert_eq!(plan.rw_paths, vec![PathBuf::from("/var/workspace/books")]);
        assert!(plan.ro_paths.is_empty());
    }

    #[test]
    fn another_apps_capability_is_not_planned() {
        let caps = vec![fs_grant("dev.qos.other", "/var/workspace/secrets", Verb::Write)];
        let plan =
            CapabilityPlan::for_app_with("dev.qos.books", &caps, Some(1000), fake_device_paths);
        assert_eq!(
            plan,
            CapabilityPlan::empty(),
            "a grant addressed to another app id must not reach this app: {plan:?}"
        );
    }

    #[test]
    fn expired_capability_is_not_planned() {
        let caps = vec![
            fs_grant("dev.qos.books", "/var/workspace/books", Verb::Write)
                .expiring(Expiry::AtUnix(500)),
        ];
        let plan =
            CapabilityPlan::for_app_with("dev.qos.books", &caps, Some(1000), fake_device_paths);
        assert_eq!(plan, CapabilityPlan::empty());
        // Same capability, evaluated before it expires, does plan a bind
        // — proving the emptiness above is the expiry and not a typo.
        let live =
            CapabilityPlan::for_app_with("dev.qos.books", &caps, Some(499), fake_device_paths);
        assert_eq!(live.rw_paths, vec![PathBuf::from("/var/workspace/books")]);
    }

    #[test]
    fn restriction_removes_a_granted_path() {
        let caps = vec![
            fs_grant("dev.qos.books", "/var/workspace/books", Verb::Write),
            CapOverride::new(
                Subject::app("dev.qos.books"),
                Scope::FsPath("/var/workspace/books".to_string()),
                Effect::Restrict,
                Verb::Read,
            ),
        ];
        let plan =
            CapabilityPlan::for_app_with("dev.qos.books", &caps, Some(1000), fake_device_paths);
        assert!(
            plan.rw_paths.is_empty(),
            "a Restrict capped at Read must remove the writable bind: {plan:?}"
        );
        assert_eq!(plan.ro_paths, vec![PathBuf::from("/var/workspace/books")]);
    }

    #[test]
    fn camera_capability_plans_device_nodes_and_absence_plans_none() {
        let caps = vec![CapOverride::grant(
            Subject::app("dev.qos.meet"),
            Scope::Device("camera".to_string()),
            Verb::Read,
        )];
        let plan = CapabilityPlan::for_app_with("dev.qos.meet", &caps, Some(1), fake_device_paths);
        assert_eq!(plan.devices, vec!["camera".to_string()]);
        assert_eq!(plan.rw_paths, vec![PathBuf::from("/dev/video0")]);

        let without = CapabilityPlan::for_app_with("dev.qos.meet", &[], Some(1), fake_device_paths);
        assert!(
            without.rw_paths.is_empty() && without.devices.is_empty(),
            "no camera capability means no device node at all: {without:?}"
        );
    }

    #[test]
    fn screencast_capability_binds_nothing() {
        let caps = vec![CapOverride::grant(
            Subject::app("dev.qos.meet"),
            Scope::Device("screencast".to_string()),
            Verb::Read,
        )];
        let plan = CapabilityPlan::for_app_with("dev.qos.meet", &caps, Some(1), fake_device_paths);
        assert_eq!(plan.devices, vec!["screencast".to_string()]);
        assert!(
            plan.rw_paths.is_empty() && plan.ro_paths.is_empty(),
            "screencast is portal-brokered, not a mount: {plan:?}"
        );
    }

    #[test]
    fn net_host_capability_opens_the_namespace_and_records_the_host() {
        let caps = vec![CapOverride::grant(
            Subject::app("dev.qos.mail"),
            Scope::NetHost("mail.example.com".to_string()),
            Verb::Write,
        )];
        let plan = CapabilityPlan::for_app_with("dev.qos.mail", &caps, Some(1), fake_device_paths);
        assert!(plan.allow_network);
        assert_eq!(plan.net_hosts, vec!["mail.example.com".to_string()]);

        let without = CapabilityPlan::for_app_with("dev.qos.mail", &[], Some(1), fake_device_paths);
        assert!(
            !without.allow_network,
            "without a network capability the namespace stays isolated"
        );
    }

    #[test]
    fn relative_and_root_paths_are_refused() {
        let caps = vec![
            fs_grant("dev.qos.books", "relative/path", Verb::Write),
            fs_grant("dev.qos.books", "/", Verb::Write),
        ];
        let plan =
            CapabilityPlan::for_app_with("dev.qos.books", &caps, Some(1), fake_device_paths);
        assert_eq!(
            plan,
            CapabilityPlan::empty(),
            "neither a relative path nor the root is a bindable grant: {plan:?}"
        );
    }

    #[test]
    fn missing_or_malformed_capability_file_grants_nothing() {
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

    #[test]
    fn well_formed_capability_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caps.json");
        let caps = vec![fs_grant("dev.qos.books", "/var/workspace/books", Verb::Read)];
        std::fs::write(&path, serde_json::to_vec(&caps).unwrap()).unwrap();
        assert_eq!(load_capability_file(&path), caps);
    }
}
