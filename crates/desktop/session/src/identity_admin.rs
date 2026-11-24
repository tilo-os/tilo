//! Administering the person's identities, run by the session that keeps the
//! identity store.
//!
//! The store — each identity record, the fallback root secrets, the active
//! selector and the device fleets under the person's identity directory — is
//! the session's. Settings used to edit it in its own process, which meant
//! binding it into the app's sandbox, root secret included. Every step now
//! runs here and answers in JSON without a secret: listing, activating,
//! removing and protecting an identity. The person's devices are the
//! `devices` actions ([`crate::device_tickets`]).
//!
//! Who may take a step is decided at the API, before it reaches this engine:
//! only a person who administers the workspace's identity, and every change
//! needs explicit confirmation.

use std::path::PathBuf;
use std::sync::Arc;

use tilo_actions::remote::admitted::top_level;
use tilo_actions::remote::{Admitted, Caller, InvokeRequest, Outcome, Pending};
use tilo_actions::{ActionArgs, ActionParam, ParamType};
use tilo_broker_core::BrokerActor;
use tilo_identity::store::IdentityProfile;
use tilo_workspace_service::workspaces::providers::{Providers, Reach};

/// What a step answers: its JSON, or the sentence saying why it did not happen.
pub(crate) type Answer = Result<serde_json::Value, String>;

pub(crate) use tilo_actions::remote::identity_admin::{
    ACTIVATE, LIST, PROTECT, REMOVE, RESOURCE, STEPS,
};

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
/// What `step` takes.
pub(crate) fn params(step: &str) -> &'static [ActionParam] {
    match step {
        ACTIVATE => ACTIVATE_PARAMS,
        REMOVE => REMOVE_PARAMS,
        PROTECT => PROTECT_PARAMS,
        _ => LIST_PARAMS,
    }
}

/// What `step` may do and answers, the same through every caller. Every step is this device's own
/// identity store. Changing the store is privileged and asked first.
pub(crate) fn contract(step: &str) -> tilo_actions::ActionContract {
    use tilo_actions::{
        ActionContract, ActionKind, ConfirmationPolicy, ResultSpec, RetryPolicy, RiskLevel,
    };
    let json = |schema: serde_json::Value| ResultSpec::Json { schema };
    match step {
        LIST => ActionContract {
            result: json(serde_json::json!({ "type": "array", "items": { "type": "object" } })),
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
    pub(crate) device: tilo_device_identity::IdentityPaths,
}

impl Store {
    /// The store this session runs as.
    ///
    /// # Errors
    ///
    /// Why the person's identity directory cannot be found.
    pub(crate) fn from_env() -> Result<Self, String> {
        Ok(Self {
            dir: tilo_identity::device_admin::PersonPaths::from_env()?.dir,
            device: tilo_identity::provisioning::collaboration_identity_paths(),
        })
    }

    fn fleets(&self) -> tilo_identity::device_fleet::DeviceFleetStore {
        tilo_identity::device_fleet::DeviceFleetStore::at(&self.dir, self.device.clone())
    }
}

/// The session's device fleets: the identity store that keeps the person's
/// root and the devices trusted under it.
///
/// # Errors
///
/// Why the person's identity directory cannot be found.
pub(crate) fn fleet_store() -> Result<tilo_identity::device_fleet::DeviceFleetStore, String> {
    Ok(Store::from_env()?.fleets())
}

/// Take `step` with `args`, which the API bound to [`params`] after it
/// decided the caller administers the identity the caller's workspace
/// selected, in `store`.
///
/// # Errors
///
/// Why the step did not happen.
pub(crate) fn start(step: &str, args: &ActionArgs, store: &Store) -> Answer {
    match step {
        LIST => list(store),
        ACTIVATE => {
            confirmed(args, "use this identity")?;
            activate(store, identity_id(args)?)
        }
        REMOVE => {
            confirmed(args, "remove this identity")?;
            remove(store, identity_id(args)?)
        }
        PROTECT => {
            confirmed(args, "protect this identity's key")?;
            protect(store, identity_id(args)?)
        }
        _ => Err(format!("`{step}` is not an identity step")),
    }
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
        tilo_identity::store::list_identities(&store.dir).map_err(|error| error.to_string())?;
    serde_json::to_value(identities).map_err(|error| error.to_string())
}

fn activate(store: &Store, id: &str) -> Answer {
    tilo_identity::store::set_active_identity(&store.dir, id).map_err(|error| error.to_string())?;
    Ok(serde_json::json!({ "id": id, "active": true }))
}

fn remove(store: &Store, id: &str) -> Answer {
    tilo_identity::store::remove_identity(&store.dir, id).map_err(|error| error.to_string())?;
    Ok(serde_json::json!({ "id": id, "removed": true }))
}

fn protect(store: &Store, id: &str) -> Answer {
    let protection =
        tilo_identity::store::secure_identity(&store.dir, id).map_err(|error| error.to_string())?;
    Ok(serde_json::json!({ "id": id, "protection": protection }))
}

/// Refuse unless `person` administers `workspace`'s identity: they may grant
/// in the workspace, and it has an identity selected, which is answered.
///
/// # Errors
///
/// Why `person` may not administer it.
pub(crate) fn authorize(
    session: &crate::app_wire::LayoutAuthority,
    workspace: &str,
    person: &str,
) -> Result<IdentityProfile, String> {
    use tilo_broker_core::services::WorkspaceAuthority as _;
    let now = tilo_clock::unix_now();
    let mut hosted = false;
    let mut administers = false;
    session.with_tree(&mut |tree| {
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
    tilo_identity::store::identity_for_workspace(workspace)
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
        _ => (
            "search-action-identity-protect",
            "search-action-identity-protect-subtitle",
        ),
    }
}

/// `step` , described for the broker's catalogue.
fn descriptor(step: &str) -> tilo_actions::ActionDescriptor {
    use tilo_session_core::desktop::i18n::t;
    let (label, description) = copy_keys(step);
    tilo_actions::ActionDescriptor {
        did: None,
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
        kind: if step == LIST {
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

pub(crate) fn outcome(answer: Answer) -> Outcome {
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

/// The broker's API to identity administration: how Settings lists and
/// changes the person's identities, as the person using it, without the
/// identity store entering its sandbox.
pub(crate) struct BrokerApi {
    pub(crate) layout: Arc<crate::app_wire::LayoutAuthority>,
}

impl BrokerApi {
    /// Answer the person's identity steps from now on, admitted by the grants of Settings' app,
    /// for the person at this device alone: the identity store stays here.
    pub(crate) fn register(self: Arc<Self>, providers: &Providers) {
        providers.provide(
            tilo_actions::remote::identity_admin::PROVIDER,
            settings_sections::APP_ID,
            Reach::ThisDevice,
            self,
        );
    }
}

impl tilo_actions::remote::Admits for BrokerApi {
    const PROVIDER: &'static str = tilo_actions::remote::identity_admin::PROVIDER;
    const APP: &'static str = settings_sections::APP_ID;

    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        top_level(STEPS.into_iter().map(descriptor).collect())
    }

    /// Carry out an invocation the device's dispatch admitted for `caller`.
    fn start(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        _admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        // The identity store is that person's own: an agent or an app acting for nobody never
        // administers it through here.
        let actor = BrokerActor::for_caller(caller);
        let person = actor
            .direct_person()
            .ok_or("only a person administers their identities")?;
        let step = STEPS
            .into_iter()
            .find(|step| {
                request.target.address.resource == RESOURCE
                    && request.target.address.action == *step
            })
            .ok_or("unknown identity step")?;
        let argument = request.argument.to_slot();
        let args = bind_slot(params(step), argument.as_deref())?;
        let workspace = caller
            .workspace()
            .ok_or("the identities were asked in no workspace of this device")?;
        authorize(&self.layout, workspace, person)?;
        let store = Store::from_env()?;
        Ok(Pending::ready(outcome(Ok(start(step, &args, &store)?))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let device = tilo_device_identity::IdentityPaths::under(root.path().join("system"));
        tilo_identity::provisioning::ensure_collaboration_identity(
            &dir,
            &device,
            tilo_identity::provisioning::IdentityStorage::Files,
        )
        .expect("provision");
        let registry = tilo_identity::store::IdentityRegistryStore::at(&dir)
            .load()
            .expect("registry");
        let id = registry.default_identity.clone().expect("default identity");
        let identity = registry.identities.get(&id).expect("profile").clone();
        Provisioned {
            store: Store { dir, device },
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

    /// What a step answered.
    fn answered(started: Answer) -> serde_json::Value {
        started.expect("the step runs")
    }

    #[test]
    fn identity_steps_list_change_and_protect_the_store_without_answering_a_secret() {
        // The temporary root is bound, not dropped with `..` : it holds the store.
        let Provisioned {
            _root,
            store,
            identity,
        } = provisioned();

        let listed = answered(start(LIST, &ActionArgs::none(), &store));
        let entries = listed.as_array().expect("a list");
        assert_eq!(entries.len(), 1, "{listed}");
        let entry = &entries[0];
        assert_eq!(entry["active"], true);
        assert_eq!(entry["public_key"], hex::encode(identity.user_public_key));
        let summary: tilo_identity_client::IdentitySummary =
            serde_json::from_value(entry.clone()).expect("Settings reads the answer back");
        let text = listed.to_string();
        assert!(
            !text.contains("secret") && !text.contains("resource_path"),
            "no secret or record path leaves the host: {text}"
        );

        let id = serde_json::Value::String(summary.id.clone());
        let yes = serde_json::Value::Bool(true);
        assert!(
            start(
                ACTIVATE,
                &args(
                    ACTIVATE,
                    &[
                        ("id", id.clone()),
                        ("confirm", serde_json::Value::Bool(false))
                    ]
                ),
                &store,
            )
            .is_err(),
            "using an identity needs the person's confirmation"
        );
        assert_eq!(
            answered(start(
                ACTIVATE,
                &args(ACTIVATE, &[("id", id.clone()), ("confirm", yes.clone())]),
                &store,
            )),
            serde_json::json!({ "id": summary.id, "active": true })
        );
        let protected = answered(start(
            PROTECT,
            &args(PROTECT, &[("id", id.clone()), ("confirm", yes)]),
            &store,
        ));
        assert_eq!(protected["id"], summary.id.as_str());

        // Removing needs the person's confirmation, and the store keeps its last identity whatever
        // the caller confirmed.
        let unconfirmed = start(
            REMOVE,
            &args(
                REMOVE,
                &[
                    ("id", id.clone()),
                    ("confirm", serde_json::Value::Bool(false)),
                ],
            ),
            &store,
        );
        assert!(
            matches!(&unconfirmed, Err(reason) if reason.contains("confirmation")),
            "{:?}",
            unconfirmed.err()
        );
        assert!(
            start(
                REMOVE,
                &args(
                    REMOVE,
                    &[("id", id), ("confirm", serde_json::Value::Bool(true))]
                ),
                &store,
            )
            .is_err(),
            "the active, final identity stays"
        );
        assert_eq!(
            tilo_identity::store::list_identities(&store.dir)
                .expect("listed")
                .len(),
            1
        );
    }

    /// A session hosting `workspace` , with nothing else on it.
    fn shell_with(workspace: tilo_desktop_model::Workspace) -> crate::app_wire::LayoutAuthority {
        let active = workspace.id();
        crate::app_wire::LayoutAuthority::new(
            tilo_desktop_model::Layout {
                workspaces: vec![workspace],
                active,
                ..tilo_desktop_model::Layout::default()
            },
            crate::engine::revocations_of(None),
            crate::app_wire::Names::default(),
        )
    }

    #[test]
    fn only_a_person_who_governs_the_hosted_workspace_administers_its_devices() {
        let mut workspace = tilo_desktop_model::Workspace::new(
            tilo_desktop_model::WorkspaceId::from_u128(1),
            "Session",
        );
        workspace.set_owner("alice");
        let hosted = workspace.id().to_string();
        let session = shell_with(workspace);

        let refused =
            authorize(&session, &hosted, "bob").expect_err("a member who does not govern");
        assert!(refused.contains("may not administer"), "{refused}");
        let gone = authorize(&session, "no such workspace", "alice").expect_err("not hosted here");
        assert!(gone.contains("no longer hosted"), "{gone}");
    }
}
