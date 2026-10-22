//! Administering the person's identities and the devices trusted under
//! them, run by the host that keeps the identity store.
//!
//! The store — each identity record, the fallback root secrets, the active
//! selector and the device fleets under `~/.config/tilo/identity` — is the
//! host's. Settings used to edit it in its own process, which meant binding
//! it into the app's sandbox, root secret included. Every step now runs
//! here and answers in JSON without a secret: listing, activating, removing
//! and protecting an identity; listing the devices trusted under the room's
//! identity and root-signing a revocation; and removing a home server, whose
//! relay administration signs with the root key too.
//!
//! The same steps are served through two doors: the action broker, for
//! Settings acting as the person using it ([`BrokerDoor`]), and the palette,
//! the control socket and agents (`identity.*` shell actions). Who may take
//! a step is decided at the door, before it reaches this engine: only a
//! person who administers the workspace's identity, and every change but
//! delivering pending home-server removals needs explicit confirmation.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use tilo_actions::remote::{Pending, pending};
use tilo_actions::{ActionArgs, ActionParam, ParamType};

use crate::identity_registry::IdentityProfile;
use crate::shell::device_pairing::{
    Answer, Finished, HostCommand, Runner, Started, command_error, node_id, run_bounded,
};

pub(crate) use tilo_actions::remote::identity_admin::{
    ACTIVATE, DEVICES, HOME_PEER_REMOVE, HOME_PEER_RETRY, LIST, PROTECT, REMOVE, RESOURCE, REVOKE,
    STEPS,
};

const RELAY_ADMIN: &str = "/usr/libexec/tilo/tilo-relay-admin";
/// Removing a home server dials it to deliver its decommission; this bounds
/// a server that never answers.
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

/// What `step` may do and answers, the same through every door. Every step
/// is this device's own identity store. Changing the store is privileged and
/// asked first. Delivering the home-server removals still pending signs nothing
/// new — each was signed when it was confirmed — so it asks nothing.
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
    /// The person's identity directory: records, root secrets, the active
    /// selector and the device fleets.
    pub(crate) dir: PathBuf,
    /// This device's collaboration identity: its key and certificates.
    pub(crate) device: tilo_user_identity::IdentityPaths,
    /// The device root relay administration runs against.
    pub(crate) root: PathBuf,
}

impl Store {
    /// The store this session runs as.
    ///
    /// # Errors
    ///
    /// Why the person's identity directory cannot be found.
    pub(crate) fn from_env() -> Result<Self, String> {
        let paths = crate::shell::device_pairing::IdentityPaths::from_env()?;
        Ok(Self {
            dir: paths.dir,
            device: crate::firstboot::collaboration_identity_paths(),
            root: paths.root,
        })
    }

    fn fleets(&self) -> crate::device_fleet::DeviceFleetStore {
        crate::device_fleet::DeviceFleetStore::at(&self.dir, self.device.clone())
    }
}

/// The session's device fleets: the host-side identity store that keeps
/// the person's root and the devices trusted under it.
///
/// # Errors
///
/// Why the person's identity directory cannot be found.
pub(crate) fn fleet_store() -> Result<crate::device_fleet::DeviceFleetStore, String> {
    Ok(Store::from_env()?.fleets())
}

/// The host's identity steps.
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
    /// caller's workspace selected, in `store`.
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
                revoke(store, identity, args).map(Started::Answered)
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

    /// Run the relay administration `command` on a worker thread; the
    /// answer is what it reported.
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

/// The relay command that root-revokes and stops home server `device`
/// serving through `service`. The identity directory is the host's own.
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

/// What a finished relay command reported: its JSON, or an empty object
/// for one that reports nothing.
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

/// The devices trusted under `identity`, this one marked current.
pub(crate) fn devices(store: &Store, identity: &IdentityProfile) -> Answer {
    let devices = tilo_identity_host::device_admin::trusted_devices(
        &store.fleets(),
        &store.device,
        &identity.id,
        identity.user_public_key,
    )?;
    serde_json::to_value(devices).map_err(|error| error.to_string())
}

/// Root-sign the permanent revocation of the device `args` names under
/// `identity`, for the reason it gives (retired when none). The root key is
/// opened here, on the host, and nowhere else.
///
/// # Errors
///
/// Why the device was not revoked.
pub(crate) fn revoke(store: &Store, identity: &IdentityProfile, args: &ActionArgs) -> Answer {
    let device = tilo_identity_host::device_admin::revoke(
        &store.fleets(),
        &identity.id,
        args.text("device").unwrap_or_default(),
        args.text("reason").unwrap_or("retired"),
    )?;
    Ok(serde_json::json!({ "device": device, "revoked": true }))
}

/// Refuse unless `person` administers `room`'s identity: they may grant in
/// the workspace, and it has an identity selected, which is answered.
///
/// # Errors
///
/// Why `person` may not administer it.
pub(crate) fn authorize(
    tree: &crate::shell::state::WeakWorkspaceTreeHandle,
    room: &str,
    person: &str,
) -> Result<IdentityProfile, String> {
    let tree = tree
        .upgrade()
        .ok_or("the workspace authority is unavailable")?;
    let may_administer = tree
        .read()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room().to_string() == room)
        .ok_or("the workspace is no longer hosted here")?
        .may_govern_at(
            person,
            tilo_action_policy::roles::governance::GRANT,
            tilo_clock::unix_now(),
        );
    if !may_administer {
        return Err(format!(
            "{person} may not administer devices for this workspace's identity"
        ));
    }
    crate::identity_registry::selected_identity_profile(room)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "no identity selected".into())
}

/// The application whose Users and Devices controls administer identities
/// through the broker's door.
const SETTINGS_APP_ID: &str = "so.tilo.settings";

/// The broker's door to identity administration: how Settings lists and
/// changes the person's identities and devices, as the person using it,
/// without the identity store entering its sandbox.
pub(crate) struct BrokerDoor {
    pub(crate) tree: crate::shell::state::WeakWorkspaceTreeHandle,
    pub(crate) admin: &'static IdentityAdmin,
}

/// `step`'s fluent label and subtitle keys.
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

/// `step`, described for the broker's catalogue.
fn descriptor(step: &str) -> tilo_actions::ActionDescriptor {
    use crate::shell::i18n::t;
    let (label, description) = copy_keys(step);
    tilo_actions::ActionDescriptor {
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(RESOURCE, step),
        resource_kind: RESOURCE.into(),
        resource_label: t("search-action-identity-resource"),
        action_label: t(label),
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

/// `argument`, in its slot form, bound to `params`.
fn bind_slot(params: &[ActionParam], argument: Option<&str>) -> Result<ActionArgs, String> {
    let supplied = match argument {
        None | Some("") => serde_json::Map::new(),
        Some(slot) => {
            let json = slot
                .strip_prefix(tilo_actions::params::ARGUMENTS_JSON_PREFIX)
                .ok_or("identity steps take JSON arguments")?;
            serde_json::from_str(json).map_err(|error| error.to_string())?
        }
    };
    ActionArgs::bind_json(params, &supplied).map_err(|error| error.to_string())
}

fn outcome(answer: Answer) -> tilo_action_broker::services::HostStepOutcome {
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

/// This session's door to identity administration over `tree`.
pub(crate) fn broker_door(
    tree: &crate::shell::state::WorkspaceTreeHandle,
) -> Arc<dyn tilo_action_broker::services::HostStepServices> {
    Arc::new(BrokerDoor {
        tree: tree.downgrade(),
        admin: IdentityAdmin::session(),
    })
}

impl BrokerDoor {
    /// After a revocation, withdraw the filesystem grants exported for the
    /// revoked device at once.
    fn export_fs_capabilities(&self) {
        if let Some(tree) = self.tree.upgrade() {
            tree.read().export_fs_capabilities();
        }
    }
}

impl tilo_action_broker::services::HostStepServices for BrokerDoor {
    fn provider(&self) -> &'static str {
        tilo_actions::remote::identity_admin::PROVIDER
    }

    fn app_id(&self) -> &str {
        SETTINGS_APP_ID
    }

    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        STEPS.into_iter().map(descriptor).collect()
    }

    fn start(
        &self,
        request: &tilo_action_broker::services::HostStepRequest<'_>,
    ) -> Result<Pending<tilo_action_broker::services::HostStepOutcome>, String> {
        // The identity store is that person's own: an agent or an app acting
        // for nobody never administers it through here.
        let person = request
            .actor
            .direct_person()
            .ok_or("only a person administers their identities")?;
        let step = STEPS
            .into_iter()
            .find(|step| request.address.resource == RESOURCE && request.address.action == *step)
            .ok_or("unknown identity step")?;
        let args = bind_slot(params(step), request.argument)?;
        let identity = authorize(&self.tree, request.room, person)?;
        let store = Store::from_env()?;
        Ok(match self.admin.start(step, &args, &identity, &store)? {
            Started::Answered(value) => {
                if step == REVOKE {
                    self.export_fs_capabilities();
                }
                Pending::ready(outcome(Ok(value)))
            }
            Started::Later(answer) => answer.map(outcome),
        })
    }
}

#[cfg(test)]
#[path = "identity_admin_tests.rs"]
mod tests;
