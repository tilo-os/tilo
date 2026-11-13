//! Administering the person's identities and the devices trusted under
//! them, run by the session that keeps the identity store.
//!
//! The store — each identity record, the fallback root secrets, the active
//! selector and the device fleets under the person's identity directory — is
//! the session's. Settings used to edit it in its own process, which meant
//! binding it into the app's sandbox, root secret included. Every step now
//! runs here and answers in JSON without a secret: listing, activating,
//! removing and protecting an identity; listing the devices trusted under the
//! workspace's identity and root-signing a revocation; and removing a home
//! server, whose relay administration signs with the root key too.
//!
//! Who may take a step is decided at the door, before it reaches this engine:
//! only a person who administers the workspace's identity, and every change
//! but delivering pending home-server removals needs explicit confirmation.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use tilo_actions::remote::{Pending, pending};
use tilo_actions::{ActionArgs, ActionParam, ParamType};
use tilo_identity_host::device_admin::{command_error, node_id};
use tilo_identity_host::registry::IdentityProfile;

use crate::pairing::{Answer, Finished, HostCommand, Runner, Started, run_bounded};

pub(crate) use tilo_actions::remote::identity_admin::{
    ACTIVATE, DEVICES, HOME_PEER_REMOVE, HOME_PEER_RETRY, LIST, PROTECT, REMOVE, RESOURCE, REVOKE,
    STEPS,
};

const RELAY_ADMIN: &str = "/usr/libexec/tilo/tilo-relay-admin";
/// Removing a home server dials it to deliver its decommission; this bounds a server that never
/// answers.
const HOME_PEER_TIMEOUT: Duration = Duration::from_secs(60);

const IDENTITY: ActionParam = ActionParam::required(
    "id",
    ParamType::Text,
    "The identity's id, as the identity list answers it.",
);

pub(crate) const LIST_PARAMS: &[ActionParam] = &[];
pub(crate) const ACTIVATE_PARAMS: &[ActionParam] = &[
    IDENTITY,
    ActionParam::required(
        "confirm",
        ParamType::Boolean,
        "Explicit confirmation that apps opened from now on sign in as this identity.",
    ),
];
pub(crate) const REMOVE_PARAMS: &[ActionParam] = &[
    IDENTITY,
    ActionParam::required(
        "confirm",
        ParamType::Boolean,
        "Explicit confirmation that this identity and its key leave this device.",
    ),
];
pub(crate) const PROTECT_PARAMS: &[ActionParam] = &[
    IDENTITY,
    ActionParam::required(
        "confirm",
        ParamType::Boolean,
        "Explicit confirmation that this identity's key moves into protected storage.",
    ),
];
pub(crate) const DEVICES_PARAMS: &[ActionParam] = &[];
pub(crate) const REVOKE_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "device",
        ParamType::Text,
        "The enrolled device's 64-character public-key fingerprint.",
    ),
    ActionParam::optional(
        "reason",
        ParamType::Text,
        "lost, stolen, retired, or compromised (defaults to retired).",
    ),
    ActionParam::required(
        "confirm",
        ParamType::Boolean,
        "Explicit confirmation that this permanent administrative revocation should run.",
    ),
];
pub(crate) const HOME_PEER_REMOVE_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "device",
        ParamType::Text,
        "The home server's node id, 64 hex characters.",
    ),
    ActionParam::required(
        "service",
        ParamType::Text,
        "The home server's relay service node id, 64 hex characters.",
    ),
    ActionParam::required(
        "confirm",
        ParamType::Boolean,
        "Explicit confirmation that this home server stops serving your workspaces for good.",
    ),
];
pub(crate) const HOME_PEER_RETRY_PARAMS: &[ActionParam] = &[];

/// What `step` takes.
pub(crate) fn params(step: &str) -> &'static [ActionParam] {
    match step {
        ACTIVATE => ACTIVATE_PARAMS,
        REMOVE => REMOVE_PARAMS,
        PROTECT => PROTECT_PARAMS,
        DEVICES => DEVICES_PARAMS,
        REVOKE => REVOKE_PARAMS,
        HOME_PEER_REMOVE => HOME_PEER_REMOVE_PARAMS,
        HOME_PEER_RETRY => HOME_PEER_RETRY_PARAMS,
        _ => LIST_PARAMS,
    }
}

/// What `step` may do and answers, the same through every door. Every step is this device's own
/// identity store. Changing the store is privileged and asked first. Delivering the home-server
/// removals still pending signs nothing new — each was signed when it was confirmed — so it asks
/// nothing.
pub(crate) fn contract(step: &str) -> tilo_actions::ActionContract {
    use tilo_actions::{
        ActionContract, ActionKind, ConfirmationPolicy, ExecutionMode, ResultSpec, RetryPolicy,
        RiskLevel,
    };
    let json = |schema: serde_json::Value| ResultSpec::Json { schema };
    match step {
        LIST => ActionContract {
            result: json(serde_json::json!({ "type": "array", "items": { "type": "object" } })),
            ..ActionContract::view()
        },
        DEVICES => ActionContract {
            result: json(serde_json::json!({
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["label", "key_hex", "fingerprint", "current", "revoked", "can_host"]
                }
            })),
            ..ActionContract::view()
        },
        ACTIVATE | PROTECT => ActionContract {
            kind: ActionKind::Outside,
            risk: RiskLevel::Privileged,
            confirmation: ConfirmationPolicy::Confirm,
            retry: RetryPolicy::Idempotent,
            result: json(serde_json::json!({ "type": "object" })),
            ..ActionContract::default()
        },
        HOME_PEER_REMOVE => ActionContract {
            kind: ActionKind::Outside,
            risk: RiskLevel::Privileged,
            confirmation: ConfirmationPolicy::Confirm,
            retry: RetryPolicy::AtMostOnce,
            execution: ExecutionMode::Background { cancellable: false },
            result: json(serde_json::json!({ "type": "object" })),
            ..ActionContract::default()
        },
        HOME_PEER_RETRY => ActionContract {
            kind: ActionKind::Outside,
            risk: RiskLevel::Low,
            retry: RetryPolicy::Idempotent,
            execution: ExecutionMode::Background { cancellable: false },
            result: json(serde_json::json!({ "type": "object" })),
            ..ActionContract::default()
        },
        _ => ActionContract {
            kind: ActionKind::Outside,
            risk: RiskLevel::Privileged,
            confirmation: ConfirmationPolicy::Confirm,
            retry: RetryPolicy::AtMostOnce,
            result: json(serde_json::json!({ "type": "object" })),
            ..ActionContract::default()
        },
    }
}

/// Where the identity store and this device's identity live.
#[derive(Clone, Debug)]
pub(crate) struct Store {
    /// The person's identity directory: records, root secrets, the active selector and the device
    /// fleets.
    pub(crate) dir: PathBuf,
    /// This device's collaboration identity: its key and certificates.
    pub(crate) device: tilo_user_identity::IdentityPaths,
    /// The device root the privileged helpers run against.
    pub(crate) root: PathBuf,
}

impl Store {
    /// The store this session runs as.
    ///
    /// # Errors
    ///
    /// Why the person's identity directory cannot be found.
    pub(crate) fn from_env() -> Result<Self, String> {
        let paths = tilo_identity_host::device_admin::PersonPaths::from_env()?;
        Ok(Self {
            dir: paths.dir,
            device: tilo_identity_host::provisioning::collaboration_identity_paths(),
            root: paths.root,
        })
    }

    fn fleets(&self) -> tilo_identity_host::device_fleet::DeviceFleetStore {
        tilo_identity_host::device_fleet::DeviceFleetStore::at(&self.dir, self.device.clone())
    }
}

/// The session's device fleets: the identity store that keeps the person's
/// root and the devices trusted under it.
///
/// # Errors
///
/// Why the person's identity directory cannot be found.
pub(crate) fn fleet_store() -> Result<tilo_identity_host::device_fleet::DeviceFleetStore, String> {
    Ok(Store::from_env()?.fleets())
}

/// What the files `person`'s fleet is read from looked like, as a value the
/// session's pass over the desk compares with the one it last saw: a pairing
/// writes a certificate into the fleet on a thread of its own, and the
/// person's record keeps the keys with their list, so a fleet that moved
/// says the list again.
pub(crate) fn fleet_stamps(
    person: &[u8; 32],
) -> Option<tilo_identity_host::device_fleet::FleetStamps> {
    fleet_store().ok()?.stamps(&hex::encode(person))
}

/// The session's identity steps.
pub(crate) struct IdentityAdmin {
    runner: Runner,
}

static SESSION: LazyLock<IdentityAdmin> =
    LazyLock::new(|| IdentityAdmin::with_runner(Arc::new(run_bounded)));

impl IdentityAdmin {
    /// This session's identity administration, which every door shares.
    pub(crate) fn session() -> &'static Self {
        &SESSION
    }

    /// Identity administration whose relay commands `runner` runs.
    pub(crate) fn with_runner(runner: Runner) -> Self {
        Self { runner }
    }

    /// Start `step` with `args`, which the door bound to [`params`] after
    /// it decided the caller administers `identity`, the identity the
    /// caller's workspace selected, in `store`. `announce` tells the people's
    /// hosts of a revocation this device's root signs, which is how the key
    /// is refused beyond this device.
    ///
    /// # Errors
    ///
    /// Why the step did not happen.
    pub(crate) fn start(
        &self,
        step: &str,
        args: &ActionArgs,
        identity: &IdentityProfile,
        store: &Store,
        announce: &dyn Fn(&tilo_user_identity::RevocationEntry),
    ) -> Result<Started, String> {
        match step {
            LIST => list(store).map(Started::Answered),
            ACTIVATE => {
                confirmed(args, "use this identity")?;
                activate(store, identity_id(args)?).map(Started::Answered)
            }
            REMOVE => {
                confirmed(args, "remove this identity")?;
                remove(store, identity_id(args)?).map(Started::Answered)
            }
            PROTECT => {
                confirmed(args, "protect this identity's key")?;
                protect(store, identity_id(args)?).map(Started::Answered)
            }
            DEVICES => devices(store, identity).map(Started::Answered),
            REVOKE => {
                confirmed(args, "revoke this device")?;
                let (device, entry) = revoke(store, identity, args)?;
                announce(&entry);
                Ok(Started::Answered(
                    serde_json::json!({ "device": device, "revoked": true }),
                ))
            }
            HOME_PEER_REMOVE => {
                confirmed(args, "remove this home server")?;
                let device = node_id(args.text("device").unwrap_or_default(), "device")?;
                let service = node_id(args.text("service").unwrap_or_default(), "service")?;
                self.relay_admin(home_peer_remove_command(store, &device, &service))
                    .map(Started::Later)
            }
            HOME_PEER_RETRY => self
                .relay_admin(home_peer_retry_command(store))
                .map(Started::Later),
            _ => Err(format!("`{step}` is not an identity step")),
        }
    }

    /// Run the relay administration `command` on a worker thread; the answer is what it reported.
    fn relay_admin(&self, command: HostCommand) -> Result<Pending<Answer>, String> {
        let (completer, answer) = pending();
        let runner = Arc::clone(&self.runner);
        std::thread::Builder::new()
            .name("tilo-identity-admin".into())
            .spawn(move || {
                let ran = runner(&command, &AtomicBool::new(false));
                completer.complete(ran.and_then(|finished| relay_answer(&finished)));
            })
            .map(drop)
            .map_err(|error| format!("could not start the home server change: {error}"))?;
        Ok(answer)
    }
}

/// The relay command that root-revokes and stops home server `device` serving through `service` .
/// The identity directory is the session's own.
pub(crate) fn home_peer_remove_command(store: &Store, device: &str, service: &str) -> HostCommand {
    let mut args = relay_args("home-peer-remove", store);
    args.extend(["--device", device, "--service", service].map(OsString::from));
    HostCommand {
        program: PathBuf::from(RELAY_ADMIN),
        args,
        timeout: HOME_PEER_TIMEOUT,
    }
}

/// The relay command that delivers the home-server removals still pending.
pub(crate) fn home_peer_retry_command(store: &Store) -> HostCommand {
    HostCommand {
        program: PathBuf::from(RELAY_ADMIN),
        args: relay_args("home-peer-retry-removals", store),
        timeout: HOME_PEER_TIMEOUT,
    }
}

fn relay_args(command: &str, store: &Store) -> Vec<OsString> {
    vec![
        command.into(),
        "--identity-root".into(),
        store.root.clone().into_os_string(),
        "--identity-dir".into(),
        store.dir.clone().into_os_string(),
    ]
}

/// What a finished relay command reported: its JSON, or an empty object for one that reports
/// nothing.
fn relay_answer(finished: &Finished) -> Answer {
    if !finished.success {
        return Err(command_error(
            &finished.stderr,
            "the home server was not reachable",
        ));
    }
    if finished.stdout.iter().all(u8::is_ascii_whitespace) {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_slice(&finished.stdout)
        .map_err(|_| "the home server change returned an invalid answer".to_string())
}

fn confirmed(args: &ActionArgs, what: &str) -> Result<(), String> {
    if args.boolean("confirm") == Some(true) {
        Ok(())
    } else {
        Err(format!("explicit confirmation is required to {what}"))
    }
}

fn identity_id(args: &ActionArgs) -> Result<&str, String> {
    let id = args.text("id").unwrap_or_default().trim();
    if id.is_empty() {
        Err("an identity id is required".into())
    } else {
        Ok(id)
    }
}

/// The person's identities, without secrets.
pub(crate) fn list(store: &Store) -> Answer {
    let identities =
        runtime_identity::list_identities(&store.dir).map_err(|error| error.to_string())?;
    serde_json::to_value(identities).map_err(|error| error.to_string())
}

fn activate(store: &Store, id: &str) -> Answer {
    runtime_identity::set_active_identity(&store.dir, id).map_err(|error| error.to_string())?;
    Ok(serde_json::json!({ "id": id, "active": true }))
}

fn remove(store: &Store, id: &str) -> Answer {
    runtime_identity::remove_identity(&store.dir, id).map_err(|error| error.to_string())?;
    Ok(serde_json::json!({ "id": id, "removed": true }))
}

fn protect(store: &Store, id: &str) -> Answer {
    let protection =
        runtime_identity::secure_identity(&store.dir, id).map_err(|error| error.to_string())?;
    Ok(serde_json::json!({ "id": id, "protection": protection }))
}

/// The keys `person`'s root certified that this device's identity store
/// keeps: the fleet of that root, when this device holds it; none otherwise.
/// A fleet is filed under its root's hex, as the identity is.
pub(crate) fn root_certificates(person: &[u8; 32]) -> Vec<tilo_user_identity::DeviceCert> {
    fleet_store()
        .ok()
        .and_then(|fleets| fleets.load_cached(&hex::encode(person)).ok())
        .map(|fleet| fleet.certificates.clone())
        .unwrap_or_default()
}

/// The devices this device's fleet trusts under `identity`, this one marked
/// current.
fn fleet_devices(
    store: &Store,
    identity: &IdentityProfile,
) -> Result<Vec<tilo_actions::remote::identity_admin::FleetDevice>, String> {
    tilo_identity_host::device_admin::trusted_devices(
        &store.fleets(),
        &store.device,
        &identity.id,
        identity.user_public_key,
    )
}

/// The devices trusted under `identity` , this one marked current.
pub(crate) fn devices(store: &Store, identity: &IdentityProfile) -> Answer {
    serde_json::to_value(fleet_devices(store, identity)?).map_err(|error| error.to_string())
}

/// The devices trusted under `identity`, and the keys `record` adds: the
/// person's record on their hosts lists the keys their root certified on
/// every device of theirs, so a device paired on another of their devices,
/// or a browser certified in its own tab, is listed here too.
pub(crate) fn devices_with_record(
    store: &Store,
    identity: &IdentityProfile,
    record: tilo_identity_host::device_admin::Record,
) -> Answer {
    let listed =
        tilo_identity_host::device_admin::with_record(fleet_devices(store, identity)?, record);
    serde_json::to_value(listed).map_err(|error| error.to_string())
}

/// Root-sign the permanent revocation of the device `args` names under
/// `identity`, for the reason it gives (retired when none). The root key is
/// opened here, in the session, and nowhere else. Answers the entry the root
/// signed, which is announced to the hosts the person keeps their record on.
///
/// # Errors
///
/// Why the device was not revoked.
pub(crate) fn revoke(
    store: &Store,
    identity: &IdentityProfile,
    args: &ActionArgs,
) -> Result<(String, tilo_user_identity::RevocationEntry), String> {
    let entry = tilo_identity_host::device_admin::revoke(
        &store.fleets(),
        &identity.id,
        args.text("device").unwrap_or_default(),
        args.text("reason").unwrap_or("retired"),
    )?;
    Ok((hex::encode(entry.revoked_device_public_key), entry))
}

/// Refuse unless `person` administers `workspace`'s identity: they may grant
/// in the workspace, and it has an identity selected, which is answered.
///
/// # Errors
///
/// Why `person` may not administer it.
pub(crate) fn authorize(
    desk: &crate::plane::DeskAuthority,
    workspace: &str,
    person: &str,
) -> Result<IdentityProfile, String> {
    use tilo_broker_core::services::WorkspaceAuthority as _;
    let now = tilo_clock::unix_now();
    let mut hosted = false;
    let mut administers = false;
    desk.with_tree(&mut |tree| {
        for held in &tree.workspaces {
            if held.id().to_string() == workspace {
                hosted = true;
                administers =
                    held.may_govern_at(person, tilo_action_policy::roles::governance::GRANT, now);
            }
        }
    })?;
    if !hosted {
        return Err("the workspace is no longer hosted here".into());
    }
    if !administers {
        return Err(format!(
            "{person} may not administer devices for this workspace's identity"
        ));
    }
    tilo_identity_host::registry::identity_for_workspace(workspace)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "no identity selected".into())
}

/// `step` 's fluent label and subtitle keys.
pub(crate) fn copy_keys(step: &str) -> (&'static str, &'static str) {
    match step {
        LIST => (
            "search-action-identity-list",
            "search-action-identity-list-subtitle",
        ),
        ACTIVATE => (
            "search-action-identity-activate",
            "search-action-identity-activate-subtitle",
        ),
        REMOVE => (
            "search-action-identity-remove",
            "search-action-identity-remove-subtitle",
        ),
        PROTECT => (
            "search-action-identity-protect",
            "search-action-identity-protect-subtitle",
        ),
        DEVICES => (
            "search-action-identity-devices-list",
            "search-action-identity-devices-list-subtitle",
        ),
        REVOKE => (
            "search-action-identity-device-revoke",
            "search-action-identity-device-revoke-subtitle",
        ),
        HOME_PEER_REMOVE => (
            "search-action-identity-home-peer-remove",
            "search-action-identity-home-peer-remove-subtitle",
        ),
        _ => (
            "search-action-identity-home-peer-retry",
            "search-action-identity-home-peer-retry-subtitle",
        ),
    }
}

/// `step` , described for the broker's catalogue.
fn descriptor(step: &str) -> tilo_actions::ActionDescriptor {
    use tilo_desktop_surface::i18n::t;
    let (label, description) = copy_keys(step);
    tilo_actions::ActionDescriptor {
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(RESOURCE, step),
        resource_kind: RESOURCE.into(),
        resource_label: t("search-action-identity-resource"),
        action_label: t(label),
        keywords: Vec::new(),
        icon: None,
        description: Some(t(description)),
        locus: tilo_actions::ExecutionLocus::default(),
        kind: if matches!(step, LIST | DEVICES) {
            tilo_actions::OperationKind::View
        } else {
            tilo_actions::OperationKind::Effect
        },
        contract: contract(step),
        input: tilo_actions::InputSpec::None,
        params: params(step).to_vec(),
        view: None,
    }
}

/// `argument` , in its slot form, bound to `params` .
pub(crate) fn bind_slot(
    params: &[ActionParam],
    argument: Option<&str>,
) -> Result<ActionArgs, String> {
    let supplied = match argument {
        None | Some("") => serde_json::Map::new(),
        Some(slot) => {
            let json = slot
                .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                .ok_or("identity steps take JSON arguments")?;
            serde_json::from_str(json).map_err(|error| error.to_string())?
        }
    };
    ActionArgs::bind_json(params, &supplied).map_err(|error| error.to_string())
}

pub(crate) fn outcome(answer: Answer) -> tilo_broker_core::steps::HostStepOutcome {
    answer
        .map(|value| tilo_actions::ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        })
        .map_err(|message| {
            Box::new(tilo_actions::ActionFailure {
                code: "identity_admin_failed".into(),
                message,
                details: None,
                retryable: false,
            })
        })
}

/// The broker's door to identity administration: how Settings lists and
/// changes the person's identities and devices, as the person using it,
/// without the identity store entering its sandbox.
///
/// The devices are the person's record, not this device's fleet alone:
/// `hosting` is where their list is kept, and where the devices of it are
/// read back from, so a device paired on another of their devices, or a
/// browser certified in its own tab, is listed here too. `engine` is the
/// workspaces whose hosts answer it.
pub(crate) struct BrokerDoor {
    pub(crate) desk: Arc<crate::plane::DeskAuthority>,
    pub(crate) admin: &'static IdentityAdmin,
    pub(crate) hosting: Option<crate::hosted_workspaces::HostedWorkspaces>,
    pub(crate) engine: Option<Arc<crate::engine::Engine>>,
}

impl tilo_broker_core::steps::HostStepServices for BrokerDoor {
    fn provider(&self) -> &'static str {
        tilo_actions::remote::identity_admin::PROVIDER
    }

    fn app_id(&self) -> &str {
        settings_sections::APP_ID
    }

    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        STEPS.into_iter().map(descriptor).collect()
    }

    fn start(
        &self,
        request: &tilo_broker_core::steps::HostStepRequest<'_>,
    ) -> Result<Pending<tilo_broker_core::steps::HostStepOutcome>, String> {
        // The identity store is that person's own: an agent or an app acting for nobody never
        // administers it through here.
        let person = request
            .actor
            .direct_person()
            .ok_or("only a person administers their identities")?;
        let step = STEPS
            .into_iter()
            .find(|step| request.address.resource == RESOURCE && request.address.action == *step)
            .ok_or("unknown identity step")?;
        let args = bind_slot(params(step), request.argument)?;
        let identity = authorize(&self.desk, request.workspace, person)?;
        let store = Store::from_env()?;
        // A device the person's root certified elsewhere is in their record on the host that
        // keeps it, and in no fleet of this one's: the list is read where it is kept, and the
        // host's answer joins what this device knows.
        if step == DEVICES
            && let (Some(hosting), Some(engine)) = (&self.hosting, &self.engine)
        {
            return Ok(hosting
                .record_devices(engine)
                .map(move |record| outcome(devices_with_record(&store, &identity, record))));
        }
        let announce = |entry: &tilo_user_identity::RevocationEntry| {
            if let (Some(hosting), Some(engine)) = (&self.hosting, &self.engine) {
                hosting.announce_revocation(engine, entry);
            }
        };
        Ok(
            match self
                .admin
                .start(step, &args, &identity, &store, &announce)?
            {
                Started::Answered(value) => Pending::ready(outcome(Ok(value))),
                Started::Later(answer) => answer.map(outcome),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::mpsc;
    use tilo_actions::remote::identity_admin::FleetDevice;

    /// A device holding no workspace on a host: nothing hears of a revocation
    /// here, and the store's own record is what a step's answer reads.
    fn quiet(_: &tilo_user_identity::RevocationEntry) {}

    /// An identity store provisioned the way first boot does, in files, with the identity its
    /// workspace selects.
    struct Provisioned {
        _root: tempfile::TempDir,
        store: Store,
        identity: IdentityProfile,
    }

    fn provisioned() -> Provisioned {
        let root = tempfile::tempdir().expect("temp root");
        let dir = root.path().join("config/tilo/identity");
        let device = tilo_user_identity::IdentityPaths::under(root.path().join("system"));
        tilo_identity_host::provisioning::ensure_collaboration_identity(
            &dir,
            &device,
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .expect("provision");
        let registry = tilo_identity_host::registry::IdentityRegistryStore::at(&dir)
            .load()
            .expect("registry");
        let id = registry.default_identity.clone().expect("default identity");
        let identity = registry.identities.get(&id).expect("profile").clone();
        Provisioned {
            store: Store {
                dir,
                device,
                root: root.path().join("system"),
            },
            _root: root,
            identity,
        }
    }

    fn args(step: &str, supplied: &[(&str, serde_json::Value)]) -> ActionArgs {
        let supplied = supplied
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect();
        ActionArgs::bind_json(params(step), &supplied).expect("arguments bind")
    }

    /// What a step answered at once.
    fn answered(started: Result<Started, String>) -> serde_json::Value {
        match started.expect("the step runs") {
            Started::Answered(value) => value,
            Started::Later(_) => panic!("the step answers at once"),
        }
    }

    /// The admin whose relay commands are recorded and answered with `stdout` .
    fn recording(stdout: &'static str) -> (IdentityAdmin, Arc<Mutex<Vec<HostCommand>>>) {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&commands);
        let admin = IdentityAdmin::with_runner(Arc::new(move |command, _| {
            recorded.lock().expect("commands").push(command.clone());
            Ok(Finished {
                success: true,
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
            })
        }));
        (admin, commands)
    }

    #[test]
    fn identity_steps_list_change_and_protect_the_store_without_answering_a_secret() {
        // The temporary root is bound, not dropped with `..` : it holds the store.
        let Provisioned {
            _root,
            store,
            identity,
        } = provisioned();
        let (admin, commands) = recording("");

        let listed = answered(admin.start(LIST, &ActionArgs::none(), &identity, &store, &quiet));
        let entries = listed.as_array().expect("a list");
        assert_eq!(entries.len(), 1, "{listed}");
        let entry = &entries[0];
        assert_eq!(entry["active"], true);
        assert_eq!(entry["public_key"], hex::encode(identity.user_public_key));
        let summary: runtime_identity::IdentitySummary =
            serde_json::from_value(entry.clone()).expect("Settings reads the answer back");
        let text = listed.to_string();
        assert!(
            !text.contains("secret") && !text.contains("record_path"),
            "no secret or record path leaves the host: {text}"
        );

        let id = serde_json::Value::String(summary.id.clone());
        let yes = serde_json::Value::Bool(true);
        assert!(
            admin
                .start(
                    ACTIVATE,
                    &args(
                        ACTIVATE,
                        &[
                            ("id", id.clone()),
                            ("confirm", serde_json::Value::Bool(false))
                        ]
                    ),
                    &identity,
                    &store,
                    &quiet,
                )
                .is_err(),
            "using an identity needs the person's confirmation"
        );
        assert_eq!(
            answered(admin.start(
                ACTIVATE,
                &args(ACTIVATE, &[("id", id.clone()), ("confirm", yes.clone())]),
                &identity,
                &store,
                &quiet
            )),
            serde_json::json!({ "id": summary.id, "active": true })
        );
        let protected = answered(admin.start(
            PROTECT,
            &args(PROTECT, &[("id", id.clone()), ("confirm", yes)]),
            &identity,
            &store,
            &quiet,
        ));
        assert_eq!(protected["id"], summary.id.as_str());

        // Removing needs the person's confirmation, and the store keeps its last identity whatever
        // the caller confirmed.
        let unconfirmed = admin.start(
            REMOVE,
            &args(
                REMOVE,
                &[
                    ("id", id.clone()),
                    ("confirm", serde_json::Value::Bool(false)),
                ],
            ),
            &identity,
            &store,
            &quiet,
        );
        assert!(
            matches!(&unconfirmed, Err(reason) if reason.contains("confirmation")),
            "{:?}",
            unconfirmed.err()
        );
        assert!(
            admin
                .start(
                    REMOVE,
                    &args(
                        REMOVE,
                        &[("id", id), ("confirm", serde_json::Value::Bool(true))]
                    ),
                    &identity,
                    &store,
                    &quiet,
                )
                .is_err(),
            "the active, final identity stays"
        );
        assert_eq!(
            runtime_identity::list_identities(&store.dir)
                .expect("listed")
                .len(),
            1
        );
        assert!(
            commands.lock().expect("commands").is_empty(),
            "no step that stays on this device starts a process"
        );
    }

    /// Enroll a phone under `identity` in `store` ; its key, in hex.
    fn enroll_phone(store: &Store, identity: &IdentityProfile) -> String {
        let fleets = store.fleets();
        let issued_at_ns = fleets.load(&identity.id).expect("fleet").certificates[0].issued_at_ns;
        let token = fleets
            .mint_enrollment(
                &identity.id,
                tilo_user_identity::DeviceCapabilities::member(),
                issued_at_ns,
                issued_at_ns.saturating_add(5_000),
            )
            .expect("token");
        let phone = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
        fleets
            .complete_enrollment(
                &identity.id,
                &token,
                tilo_identity_host::enrollment::DeviceRole::PersonalDevice,
                phone.verifying_key().to_bytes(),
                "Phone",
                issued_at_ns,
            )
            .expect("enrolled");
        hex::encode(phone.verifying_key().to_bytes())
    }

    #[test]
    fn devices_are_listed_and_revoked_by_the_host_root_key() {
        // The temporary root is bound, not dropped with `..` : it holds the store.
        let Provisioned {
            _root,
            store,
            identity,
        } = provisioned();
        let admin = IdentityAdmin::with_runner(Arc::new(|_, _| Err("no process runs".into())));

        let devices: Vec<FleetDevice> = serde_json::from_value(answered(admin.start(
            DEVICES,
            &ActionArgs::none(),
            &identity,
            &store,
            &quiet,
        )))
        .expect("devices");
        assert_eq!(devices.len(), 1, "{devices:?}");
        assert!(devices[0].current && !devices[0].revoked, "{devices:?}");

        // Enroll a phone, then revoke it through the step.
        let phone_hex = enroll_phone(&store, &identity);

        let device = serde_json::Value::String(phone_hex.clone());
        assert!(
            admin
                .start(
                    REVOKE,
                    &args(
                        REVOKE,
                        &[
                            ("device", device.clone()),
                            ("confirm", serde_json::Value::Bool(false))
                        ]
                    ),
                    &identity,
                    &store,
                    &quiet,
                )
                .is_err(),
            "revocation needs the person's confirmation"
        );
        assert!(
            admin
                .start(
                    REVOKE,
                    &args(
                        REVOKE,
                        &[
                            ("device", device.clone()),
                            ("reason", serde_json::Value::String("misplaced".into())),
                            ("confirm", serde_json::Value::Bool(true)),
                        ],
                    ),
                    &identity,
                    &store,
                    &quiet,
                )
                .is_err(),
            "an unknown reason is refused"
        );
        assert_eq!(
            answered(admin.start(
                REVOKE,
                &args(
                    REVOKE,
                    &[
                        ("device", device),
                        ("reason", serde_json::Value::String("lost".into())),
                        ("confirm", serde_json::Value::Bool(true)),
                    ],
                ),
                &identity,
                &store,
                &quiet,
            )),
            serde_json::json!({ "device": phone_hex, "revoked": true })
        );
        let devices: Vec<FleetDevice> = serde_json::from_value(answered(admin.start(
            DEVICES,
            &ActionArgs::none(),
            &identity,
            &store,
            &quiet,
        )))
        .expect("devices");
        let phone = devices
            .iter()
            .find(|device| device.key_hex == phone_hex)
            .expect("the phone is listed");
        assert!(phone.revoked && !phone.current, "{devices:?}");
        assert_eq!(phone.label, "Phone");
    }

    #[test]
    fn home_server_removal_runs_on_the_host_with_its_own_identity_directory() {
        let store = Store {
            dir: PathBuf::from("/home/person/.config/tilo/identity"),
            device: tilo_user_identity::IdentityPaths::under("/var/lib/tilo"),
            root: PathBuf::from("/var/lib/tilo"),
        };
        let identity = IdentityProfile {
            id: "00".repeat(32),
            display_name: String::new(),
            record_path: PathBuf::from("/nonexistent"),
            user_public_key: [0; 32],
        };
        let (admin, commands) = recording("{\"decommissioned\":true,\"files_ready\":false}\n");
        let device = "AA".repeat(32);
        let service = "bb".repeat(32);

        let unconfirmed = admin.start(
            HOME_PEER_REMOVE,
            &args(
                HOME_PEER_REMOVE,
                &[
                    ("device", serde_json::Value::String(device.clone())),
                    ("service", serde_json::Value::String(service.clone())),
                    ("confirm", serde_json::Value::Bool(false)),
                ],
            ),
            &identity,
            &store,
            &quiet,
        );
        assert!(unconfirmed.is_err());
        assert!(commands.lock().expect("commands").is_empty(), "nothing ran");

        let Started::Later(answer) = admin
            .start(
                HOME_PEER_REMOVE,
                &args(
                    HOME_PEER_REMOVE,
                    &[
                        ("device", serde_json::Value::String(device)),
                        ("service", serde_json::Value::String(service.clone())),
                        ("confirm", serde_json::Value::Bool(true)),
                    ],
                ),
                &identity,
                &store,
                &quiet,
            )
            .expect("starts")
        else {
            panic!("removal reaches the home server later");
        };
        let answer = answer
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("answered")
            .expect("removed");
        assert_eq!(answer["decommissioned"], true);

        let Started::Later(retried) = admin
            .start(
                HOME_PEER_RETRY,
                &ActionArgs::none(),
                &identity,
                &store,
                &quiet,
            )
            .expect("starts")
        else {
            panic!("retrying reaches the home servers later");
        };
        let (done, wait) = mpsc::channel();
        retried.then(move |answer| done.send(answer).expect("sent"));
        let retried: Option<Answer> = tilo_test_env::recv("the retry's answer", &wait);
        assert!(retried.expect("answered").is_ok());

        let recorded = commands.lock().expect("commands").clone();
        let expected = |command: &str, tail: &[&str]| HostCommand {
            program: PathBuf::from(RELAY_ADMIN),
            args: [
                command,
                "--identity-root",
                "/var/lib/tilo",
                "--identity-dir",
                "/home/person/.config/tilo/identity",
            ]
            .iter()
            .chain(tail)
            .map(OsString::from)
            .collect(),
            timeout: HOME_PEER_TIMEOUT,
        };
        assert_eq!(
            recorded,
            [
                expected(
                    "home-peer-remove",
                    &["--device", &"aa".repeat(32), "--service", &service]
                ),
                expected("home-peer-retry-removals", &[]),
            ]
        );
    }

    /// A desk hosting `workspace` , with nothing else on it.
    fn desk_with(workspace: tilo_desktop_model::Workspace) -> crate::plane::DeskAuthority {
        let active = workspace.slot;
        crate::plane::DeskAuthority::new(
            tilo_desktop_model::DesktopTree {
                workspaces: vec![workspace],
                active,
                ..tilo_desktop_model::DesktopTree::default()
            },
            crate::engine::revocations_of(None),
            None,
            crate::plane::Names::default(),
        )
    }

    #[test]
    fn only_a_person_who_governs_the_hosted_workspace_administers_its_devices() {
        let mut workspace =
            tilo_desktop_model::Workspace::new(tilo_desktop_model::WorkspaceSlot(1), "Desk");
        workspace.set_owner("alice");
        let hosted = workspace.id().to_string();
        let desk = desk_with(workspace);

        let refused = authorize(&desk, &hosted, "bob").expect_err("a member who does not govern");
        assert!(refused.contains("may not administer"), "{refused}");
        let gone = authorize(&desk, "no such workspace", "alice").expect_err("not hosted here");
        assert!(gone.contains("no longer hosted"), "{gone}");
    }
}
