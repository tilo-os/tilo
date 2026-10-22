//! The person's settings, kept by the session.
//!
//! Settings' sections (appearance, input, power, …) are records: each is
//! published with its field edits and verbs, and each change is checked by
//! the section's model and recorded in the settings document with what made
//! it and who. The session keeps them, so the palette, agents and scripts
//! change them through those same actions whether or not Settings is open,
//! and the Settings window is one view of them. Whether a change may run is
//! read from the published action, for the value it carries: one that
//! weakens the person's security asks them first, and an agent's needs
//! their own answer.

use std::sync::{Arc, Mutex};

use tilo_actions::{ActionAddress, ActionDescriptor, ActionInvoker, ConfirmationPolicy, RiskLevel};
use tilo_app::framework::SettingsRecords;

use crate::shell::search::action_api::ActionError;

/// The app whose sections they are, and whose controls change them.
type SettingsApp = settings_core::resource::SettingsShell;

fn records() -> SettingsRecords {
    SettingsRecords::of::<SettingsApp>()
}

/// The action `action` of the section `kind` (`settings.power`).
pub(crate) fn address(kind: &str, action: &str) -> ActionAddress {
    ActionAddress::new(format!("settings:{kind}"), action)
}

/// The arguments of a field edit setting `field` to `value`.
pub(crate) fn set_field(
    field: &str,
    value: &serde_json::Value,
) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        tilo_actions::contract::FIELD_EDIT_PARAM.to_owned(),
        serde_json::json!({ "path": [{ "field": field }], "op": "set", "value": value }),
    );
    arguments
}

/// Run `address` with `arguments` as the person at this seat, on behalf of
/// the action `invoked_by` (the palette's `appearance.theme.dark`), which
/// the change records beside the section action it runs. A change whose
/// published action asks first runs only once they `confirmed` it.
///
/// # Errors
/// [`ActionError::ConfirmationRequired`] for a change they have yet to
/// confirm; otherwise why the section refused it.
pub(crate) fn change(
    address: &ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
    confirmed: bool,
    invoked_by: &settings_store::InvokingAction,
) -> Result<(), ActionError> {
    let records = records();
    let resources = records.resources();
    let spec = tilo_actions::door::resolve(&resources, address).ok_or_else(|| {
        ActionError::Refused(format!(
            "Settings publishes no {} on {}",
            address.action, address.resource
        ))
    })?;
    if spec.confirmation_for_json(arguments) != ConfirmationPolicy::None && !confirmed {
        return Err(ActionError::ConfirmationRequired);
    }
    let person = crate::shell::state::local_user_id();
    records
        .invoke(
            address,
            arguments,
            &ActionInvoker::actor(person),
            Some(invoked_by),
        )
        .map_err(ActionError::Refused)
}

/// This session's door to the person's settings, for the broker.
pub(crate) fn broker_door() -> Arc<dyn tilo_action_broker::services::SettingsRecordServices> {
    Arc::new(BrokerDoor::default())
}

/// What the broker offers: every section's actions, described once per
/// version of the settings and of the language they are worded in.
#[derive(Default)]
struct BrokerDoor {
    described: Mutex<Option<(Described, Vec<ActionDescriptor>)>>,
}

type Described = (ui::storage::SettingsRevision, u64);

fn described_at() -> Described {
    (
        ui::storage::SettingsDocument::Session.revision(),
        ui::i18n::I18nManager::revision(),
    )
}

/// The typed arguments an argument slot carries.
fn arguments_of(argument: Option<&str>) -> Option<serde_json::Map<String, serde_json::Value>> {
    let json = argument?.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)?;
    serde_json::from_str(json).ok()
}

impl tilo_action_broker::services::SettingsRecordServices for BrokerDoor {
    fn app_id(&self) -> &str {
        <SettingsApp as tilo_app::framework::ResourceApp>::ID
    }

    fn descriptors(&self) -> Vec<ActionDescriptor> {
        let at = described_at();
        let mut described = self
            .described
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((when, descriptors)) = described.as_ref()
            && *when == at
        {
            return descriptors.clone();
        }
        let descriptors = tilo_actions::door::survey(&records().resources());
        *described = Some((at, descriptors.clone()));
        descriptors
    }

    fn invocation(
        &self,
        address: &ActionAddress,
        argument: Option<&str>,
    ) -> Option<ActionDescriptor> {
        let resources = records().resources();
        let spec = tilo_actions::door::resolve(&resources, address)?;
        let need = spec.need_for(arguments_of(argument).as_ref(), argument);
        let mut descriptor = tilo_actions::door::survey(&resources)
            .into_iter()
            .find(|descriptor| &descriptor.address == address)?;
        descriptor.contract.kind = need.kind;
        if need.privileged {
            descriptor.contract.risk = RiskLevel::Privileged;
        }
        Some(descriptor)
    }

    fn apply(
        &self,
        request: &tilo_action_broker::services::SettingsRecordRequest<'_>,
    ) -> Result<(), String> {
        let arguments = arguments_of(request.argument).ok_or("the change carries no arguments")?;
        records().invoke(
            request.address,
            &arguments,
            &ActionInvoker::actor(request.actor.id()),
            None,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tilo_action_broker::test_support::{Grants as _, KindSet, Subject};
    use tilo_actions::remote::{
        ActionHostError, Caller, Confirmation, InvokeArgument, InvokeRequest, RemoteActionTarget,
        settings_records::PROVIDER,
    };

    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};

    fn lock_after(minutes: u32, confirmation: Option<Confirmation>) -> InvokeRequest {
        InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: PROVIDER.into(),
                address: super::address("settings.power", "field-edit"),
            },
            argument: InvokeArgument::Json(super::set_field(
                "lock_minutes",
                &serde_json::json!(minutes),
            )),
            context: None,
            confirmation,
            revision: None,
            wait_ms: None,
            appearance: None,
        }
    }

    /// With no window of Settings, an agent changes the person's settings
    /// through the session: it locks sooner on its own, but never makes the
    /// lock later or turns it off without the person's own answer.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the test keeps its broker alive through its assertions"
    )]
    #[test]
    #[serial_test::serial]
    fn with_settings_closed_an_agent_weakens_the_lock_only_with_the_persons_answer() {
        let _home = tilo_test_env::TestHome::new().unwrap();
        let paths = settings_store::SessionPaths::configured().unwrap();
        let session = Arc::new(settings_store::SessionSettings::open(paths).unwrap());
        let settings = crate::shell::test_support::install_settings(session.host_view().unwrap());
        let mut workspace = Workspace::new(WorkspaceId(7), "Settings");
        workspace.set_owner("owner");
        workspace.admit_agent("scribe", KindSet::READ);
        workspace.allow_app(
            Subject::agent("scribe"),
            <super::SettingsApp as tilo_app::framework::ResourceApp>::ID,
            KindSet::ALL,
        );
        let room = workspace.room().to_string();
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace);
        let broker =
            tilo_action_broker::collab_broker_capnp::CapnpBrokerState::test_broker_with_authority(
                Arc::new(crate::shell::broker_services::NativeWorkspaceAuthority(
                    tree.shared(),
                )),
                tilo_action_broker::services::HostServices {
                    settings_records: Some(super::broker_door()),
                    ..Default::default()
                },
            );
        let agent = Caller::local_agent("scribe").in_room(room);
        let invoke = |request: &InvokeRequest| {
            broker
                .state()
                .action_invoke(&agent, None, None, request)
                .wait_timeout(tilo_test_env::hang_bound())
                .ok()
                .flatten()
                .expect("admission answers at once")
        };

        let offered = broker
            .state()
            .action_query(
                &agent,
                &tilo_actions::remote::CatalogueQuery {
                    provider: Some(PROVIDER.into()),
                    max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
                    ..Default::default()
                },
            )
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("the catalogue answers")
            .expect("the agent may look");
        assert!(
            offered
                .rows
                .iter()
                .any(|row| row.action.as_ref().is_some_and(|action| {
                    action.address == super::address("settings.power", "field-edit")
                })),
            "the session offers the lock's field edit"
        );

        invoke(&lock_after(1, None)).expect("locking sooner asks nothing");
        assert_eq!(settings_store::power::LOCK_MINUTES.get(), 1);
        for minutes in [10, 0] {
            for confirmation in [None, Some(Confirmation::Confirmed)] {
                assert!(
                    matches!(
                        invoke(&lock_after(minutes, confirmation)),
                        Err(ActionHostError::ConfirmationRequired | ActionHostError::NotPermitted)
                    ),
                    "an agent's own confirmation is no answer of the person's"
                );
            }
        }
        assert_eq!(
            settings_store::power::LOCK_MINUTES.get(),
            1,
            "nothing weakened"
        );
        drop(settings);
    }
}
