//! Who a caller inside this device reads records outside their app as, and the decisions every
//! read of a resource makes beside the record's own (`docs/designs/app-language/agents.md`,
//! "Reading records outside their app").
//!
//! The record itself is read by the device's one reader ([`crate::workspaces::RecordReads`]),
//! decided by the workspace the record is in. A caller inside this device that the workspace did
//! not admit as a device reads there as [`AppBrokerState::reading_caller`] names it: the person
//! signed in here, an app or an agent acting for them, never one a person's device handed its
//! session to. Before anyone signs in, the device itself reads the records of the workspace
//! founded on it, as the person its own certificate ([`CertificateAuthority::own_device`]) names.
//!
//! [`CertificateAuthority::own_device`]: crate::apps::certificate_authority::CertificateAuthority::own_device

use std::sync::{Arc, Weak};

use tilo_action_policy::{ActionKind, Answer};
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeRequest, RESOURCE_READ_REFUSED, RESOURCE_READS_FOR_AGENTS,
    ResourceAddress, ResourceChange, ResourceChanges,
};

use super::AppBrokerState;

impl AppBrokerState {
    /// The certificate this device acts under itself, when it does.
    fn own_device(&self) -> Option<tilo_user_identity::DeviceCert> {
        self.certificate_authority.get()?.own_device()
    }

    /// Who `caller`, a caller inside this device, reads `workspace`'s records as: the caller as
    /// the read is decided in that workspace, for the person it names. Its execution must still
    /// run. With nobody signed in here, a caller that names nobody (an app on this device) reads
    /// as this device in the workspace it founded.
    ///
    /// The workspace a caller's connection was admitted to decides nothing about another one it
    /// names: the read is decided there, for the caller's actor as it is in that workspace.
    ///
    /// # Errors
    /// A caller that reads no records here: one that reads for nobody, or before sign-in, one
    /// that names nobody outside the workspace founded here.
    pub(crate) fn reading_caller(
        &self,
        caller: &Caller,
        workspace: &str,
    ) -> Result<Caller, ActionCallError> {
        super::action_provider::check_execution(caller)?;
        if self.services.local_person.is_none() && names_nobody(caller) {
            return self.device_reader(caller, workspace);
        }
        if person_reading(caller).is_none() {
            return Err(ActionCallError::Refused {
                reason: RESOURCE_READS_FOR_AGENTS.into(),
            });
        }
        Ok(match caller.workspace() {
            Some(admitted) if admitted != workspace => caller.clone().in_workspace(workspace),
            _ => caller.clone(),
        })
    }

    /// The device as the reader, with nobody signed in here: what runs on this device reads the
    /// records of the workspace founded on it, decided as the person its own certificate names,
    /// and through its app when an app asks. Another device, an agent or a bot reads nothing as
    /// this device, and no other workspace's record is read before sign-in.
    fn device_reader(&self, caller: &Caller, workspace: &str) -> Result<Caller, ActionCallError> {
        let refused = || ActionCallError::Refused {
            reason: RESOURCE_READ_REFUSED.into(),
        };
        let acting = caller.acting();
        if caller.is_foreign_device()
            || caller.is_session_delegated()
            || acting.is_agent()
            || acting.is_bot()
            || acting.is_automation()
        {
            return Err(ActionCallError::Refused {
                reason: RESOURCE_READS_FOR_AGENTS.into(),
            });
        }
        let (Some(authority), Some(certificate)) =
            (self.certificate_authority.get(), self.own_device())
        else {
            return Err(ActionCallError::Unavailable {
                reason: tilo_actions::remote::RESOURCE_READS_UNAVAILABLE.into(),
            });
        };
        let founded_here =
            uuid::Uuid::parse_str(workspace).is_ok_and(|founded| authority.governs(founded, None));
        if !founded_here {
            return Err(refused());
        }
        let device = super::person_id_for_root(certificate.user_public_key);
        // Nobody is signed in here: a caller naming a person names this device's own.
        let named = match acting.app() {
            Some(_) => caller.acting_for_person(),
            None => acting.person_id(),
        };
        if named.is_some_and(|person| !person.is_empty() && person != device) {
            return Err(refused());
        }
        let decided = match acting.app() {
            Some(app) => Caller::local_app(app.as_str()).acting_for(device),
            None => Caller::local_person(device),
        }
        .in_workspace(workspace);
        // The read lives as long as the caller's execution does.
        let decided = match caller.execution_lease() {
            Some(lease) => decided.in_execution(lease),
            None => decided,
        };
        Ok(decided)
    }
}

/// What a call made from a resource its caller may not read is refused with.
pub const MADE_FROM_UNREADABLE: &str =
    "This was asked about something you may not read, so it did not run";
/// What a call made from a resource it names without its app is refused with: nobody can say
/// who may read it.
pub const MADE_FROM_UNNAMED: &str =
    "This was asked about something it does not say the app of, so it did not run";

impl AppBrokerState {
    /// Whether `caller` may make `request` from the resource it names as made from
    /// ([`InvokeRequest::made_from`]): only a caller who may read that resource, decided at its
    /// node as every read of it is ([`Self::may_read_resource`]). A call made from nothing has
    /// nothing to decide here.
    ///
    /// # Errors
    ///
    /// [`ActionCallError::Refused`] with [`MADE_FROM_UNREADABLE`] when the workspace does not
    /// let the caller read it, with [`MADE_FROM_UNNAMED`] when its kind names no app
    /// (`tl.<app>.<Type>`), and [`ActionCallError::NotPermitted`] when the caller may not act
    /// in a workspace at all.
    pub(super) fn may_make_from(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<(), ActionCallError> {
        let Some(from) = request.made_from() else {
            return Ok(());
        };
        let Some(app) = from.app() else {
            return Err(ActionCallError::Refused {
                reason: MADE_FROM_UNNAMED.into(),
            });
        };
        let workspace = caller.workspace().ok_or(ActionCallError::NotPermitted)?;
        self.may_read_resource(caller, workspace, (app, &from.kind, &from.resource))
            .map_err(|refused| match refused {
                ActionCallError::Refused { .. } => ActionCallError::Refused {
                    reason: MADE_FROM_UNREADABLE.into(),
                },
                other => other,
            })
    }
}

impl AppBrokerState {
    /// Whether `caller` may read the resource `resource` (of kind `kind`) the app `app` publishes
    /// in `workspace` now, a resource a host publishes itself rather than a record (the device's
    /// styles, the session's windows): decided at the resource's node with the caller's actor, an
    /// agent's bounded by its person's authority, as a record's read is
    /// ([`Self::may_read_document`]).
    ///
    /// # Errors
    ///
    /// [`ActionCallError::Refused`] when the workspace does not let the caller read it, and
    /// [`ActionCallError::NotPermitted`] when the caller may not act in the workspace at all.
    pub fn may_read_resource(
        &self,
        caller: &Caller,
        workspace: &str,
        (app, kind, resource): (&str, &str, &str),
    ) -> Result<(), ActionCallError> {
        super::action_provider::check_execution(caller)?;
        match self.decide_for(caller, workspace, |workspace, actor, now| {
            let node = tilo_actions::resource_node(&workspace.policy.node(), app, kind, resource);
            actor.answer_in(workspace, &node, ActionKind::Read, now) == Answer::Allow
        }) {
            Some(true) => Ok(()),
            Some(false) => Err(ActionCallError::Refused {
                reason: RESOURCE_READ_REFUSED.into(),
            }),
            None => Err(ActionCallError::NotPermitted),
        }
    }
}

/// What a caller is told about a change the workspace does not allow it.
pub const DOCUMENT_CHANGE_REFUSED: &str = "not allowed to make this change";

impl AppBrokerState {
    /// The record change api, when this host has an origin for its workspaces' records.
    pub fn resource_changes(self: &Arc<Self>) -> Option<Arc<dyn ResourceChanges>> {
        self.services.resource_changes.as_ref()?;
        Some(Arc::new(BrokerDocumentChanges {
            state: Arc::downgrade(self),
        }))
    }
}

struct BrokerDocumentChanges {
    state: Weak<AppBrokerState>,
}

impl ResourceChanges for BrokerDocumentChanges {
    fn apply(
        &self,
        caller: &Caller,
        change: &ResourceChange,
    ) -> Result<serde_json::Value, ActionCallError> {
        let unavailable = |reason: String| ActionCallError::Unavailable { reason };
        let state = self
            .state
            .upgrade()
            .ok_or_else(|| unavailable("the broker stopped".into()))?;
        let (Some((person, workspace)), Some(agent)) =
            (agent_person(caller), caller.acting().agent_id())
        else {
            return Err(ActionCallError::Refused {
                reason: RESOURCE_READS_FOR_AGENTS.into(),
            });
        };
        super::action_provider::check_execution(caller)?;
        let answer = state
            .decide_for(caller, workspace, |workspace, actor, now| {
                let node = tilo_actions::action_node(
                    &workspace.policy.node(),
                    &change.app,
                    "",
                    &change.resource,
                    None,
                    &change.action,
                );
                actor.answer_in(workspace, &node, ActionKind::Undoable, now)
            })
            .ok_or(ActionCallError::NotPermitted)?;
        match answer {
            Answer::Allow => {}
            Answer::Ask => return Err(ActionCallError::confirmation_required()),
            Answer::Deny => {
                return Err(ActionCallError::Refused {
                    reason: DOCUMENT_CHANGE_REFUSED.into(),
                });
            }
        }
        apply_document_change(
            state,
            workspace,
            change,
            (agent, person, caller.agent_name()),
        )
    }
}

/// Apply `change` in `workspace` at its record's origin as `agent` acting for
/// `person`, by the name its admission gave it, admitted already.
fn apply_document_change(
    state: Arc<AppBrokerState>,
    workspace: &str,
    change: &ResourceChange,
    (agent, person, name): (&str, &str, Option<&str>),
) -> Result<serde_json::Value, ActionCallError> {
    let unavailable = |reason: String| ActionCallError::Unavailable { reason };
    let origin = state
        .services
        .resource_changes
        .clone()
        .ok_or_else(|| unavailable("this host has no origin for its records".into()))?;
    drop(state);
    let invoker = tilo_actions::ActionInvoker::agent(agent)
        .acting_for(person)
        .named(name.map(str::to_owned));
    origin
        .apply(workspace, change, &invoker)
        .map_err(unavailable)
}

/// The person an agent acts for, when `caller` is one: an agent the host admitted as a member
/// acting for its person, confined to a workspace.
fn agent_person(caller: &Caller) -> Option<(&str, &str)> {
    // An agent a person's device handed its session to reads nothing here. An agent acting under
    // a key of its own, or a roster agent answering a person's turn, reads and changes what both
    // its grants and its person's allow, each read and change decided at the record's node like
    // every other caller's.
    if !caller.acting().is_agent() || caller.is_session_delegated() {
        return None;
    }
    Some((caller.acting_for_person()?, caller.workspace()?))
}

/// Whether `caller` names no person to read for: an app acting for nobody, or a person with no
/// id, as what runs on a device nobody signed in on calls.
fn names_nobody(caller: &Caller) -> bool {
    let acting = caller.acting();
    let nobody = |person: Option<&str>| person.is_none_or(str::is_empty);
    if acting.is_agent() || acting.is_bot() || acting.is_automation() {
        return false;
    }
    match acting.app() {
        Some(_) => nobody(caller.acting_for_person()),
        None => nobody(acting.person_id()),
    }
}

/// Whose device certificate reads for `caller`: an agent's person, the person themself reading
/// records (a view of them, a test), or an app on their device reading another app's records for
/// them, as the workspace grants it.
fn person_reading(caller: &Caller) -> Option<&str> {
    if caller.acting().app().is_some() {
        return caller.acting_for_person();
    }
    match caller.acting().person_id() {
        Some(person) if !caller.is_session_delegated() => Some(person),
        _ => agent_person(caller).map(|(person, _)| person),
    }
}

impl AppBrokerState {
    /// A store read of `address` for `caller` inside this device, the read decided already by
    /// the workspace the address names: it reads, and writes nothing.
    pub(crate) fn store_read_record(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
        read: serde_json::Value,
    ) -> Result<serde_json::Value, ActionCallError> {
        let invalid = |reason: String| ActionCallError::InvalidRequest { reason };
        let decided = self.reading_caller(caller, &address.workspace)?;
        let command: tilo_resource_store::wire::Command = serde_json::from_value(read)
            .map_err(|error| invalid(format!("not a store command: {error}")))?;
        // A reader reads: what it changes goes through the app's actions, decided there.
        if command.writes() {
            return Err(invalid("a read of resources writes nothing".into()));
        }
        let resources =
            self.services
                .resources
                .clone()
                .ok_or_else(|| ActionCallError::Unavailable {
                    reason: tilo_actions::remote::RESOURCE_READS_UNAVAILABLE.into(),
                })?;
        let context = crate::apps::services::ResourceRequestContext::authenticated(
            address.app.clone(),
            &address.workspace,
            decided,
        );
        // The store answers under the workspace's lock, which no reactor is needed to take.
        let reply = futures::executor::block_on(resources.command(&context, command))
            .map_err(|reason| ActionCallError::Unavailable { reason })?;
        serde_json::to_value(reply).map_err(|error| invalid(error.to_string()))
    }
}

#[cfg(test)]
#[path = "resource_read_tests.rs"]
mod tests;
