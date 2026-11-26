//! Who a caller inside this device reads records outside their app as, and the decisions every
//! read of a resource makes beside the record's own (`docs/designs/app-language/agents.md`,
//! "Reading records outside their app").
//!
//! The record itself is read by the device's one reader ([`tilo_workspace_service::workspaces::RecordReads`]),
//! decided by the workspace the record is in. A caller inside this device that the workspace did
//! not admit as a device reads there as [`AppBrokerState::reading_caller`] names it: the person
//! signed in here, an app or an agent acting for them, never one a person's device handed its
//! session to. Before anyone signs in, the device itself reads the records of a workspace whose
//! signed member list names its person owner or administrator, as the person its own certificate
//! ([`CertificateAuthority::own_device`]) names.
//!
//! [`CertificateAuthority::own_device`]: crate::certificate_authority::CertificateAuthority::own_device

use std::sync::Arc;

use tilo_action_policy::{ActionKind, Answer};
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeRequest, RESOURCE_READ_REFUSED, RESOURCE_READS_FOR_AGENTS,
};

use super::AppBrokerState;

impl AppBrokerState {
    /// The certificate this device acts under itself, when it does.
    fn own_device(&self) -> Option<tilo_user_identity::DeviceCert> {
        self.certificate_authority.get()?.own_device()
    }

    /// The certificate an app this device launched acts under in `workspace`: its own person's,
    /// as this host's certificate authority answers, or as the identity this device keeps
    /// selects it where the host has no authority.
    pub(super) fn app_certificate(
        &self,
        workspace: &str,
    ) -> Result<tilo_user_identity::DeviceCert, String> {
        match self.certificate_authority.get() {
            Some(authority) => authority.app_certificate(workspace),
            None => tilo_identity::trusted_device::current(workspace)
                .map(|(_, certificate)| certificate)
                .map_err(|error| error.to_string()),
        }
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
        // A placed device's caller is decided by its certificate where records are read
        // ([`tilo_workspace_service::workspaces::RecordReads`]), never as anyone here.
        if caller.placed_records().is_some() {
            return Err(ActionCallError::Refused {
                reason: RESOURCE_READ_REFUSED.into(),
            });
        }
        // A visitor reads as the visitor it is, reaching what the workspace opens to any key,
        // whether or not anyone is signed in here: it names nobody, and is never this device's
        // person.
        if caller.acting().visitor_key().is_some() {
            return Ok(match caller.workspace() {
                Some(admitted) if admitted != workspace => caller.clone().in_workspace(workspace),
                Some(_) => caller.clone(),
                None => caller.clone().in_workspace(workspace),
            });
        }
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
        let (Some(membership), Some(certificate)) = (self.membership.as_ref(), self.own_device())
        else {
            return Err(ActionCallError::Unavailable {
                reason: tilo_actions::remote::RESOURCE_READS_UNAVAILABLE.into(),
            });
        };
        let device = super::person_id_for_root(certificate.user_public_key);
        // The device reads as its own person only where the workspace's signed member list
        // names that person its owner or an administrator.
        let administers = membership
            .decide_in_workspace(workspace, |held| {
                held.is_some_and(|held| {
                    held.policy.is_owner(&device) || held.policy.is_administrator(&device)
                })
            })
            .map_err(|reason| ActionCallError::Unavailable { reason })?;
        if !administers {
            return Err(refused());
        }
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
