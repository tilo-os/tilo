//! What this device says of the actions a window shows, for the person at the window.
//!
//! A window decides no permission itself. It names the actions of the records it shows, as they
//! are ([`tilo_actions::shown::ShownAction`]); this device decides each with the person's
//! ordinary answer in the workspace holding the record (the window's, or another: a person's
//! profile record is kept in their own workspace), and answers again when those grants change.
//! The verdicts say how the window shows its controls; running an action is admitted on its own
//! when it is called, by the same dispatch.

use std::sync::Arc;
use std::time::Duration;

use tilo_actions::outcome::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, Pending, ReceiptStatus, pending,
};
use tilo_actions::shown::{
    ActionDecision, ActionVerdict, NOT_HERE, ShownAction, ShownAsk, UNAVAILABLE, UNREADABLE,
    decide, decisions_revision,
};
use tilo_broker_core::BrokerActor;
use tilo_broker_core::services::WorkspaceAuthority;
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::providers::{Providers, Reach};

/// The actions a window shows, as this device decides them.
pub(crate) struct Shown {
    workspaces: Workspaces,
    /// This device's person, as the window shows their controls: who each verdict is decided
    /// for when the caller names no person of its own.
    person: String,
}

impl Shown {
    /// The provider that decides what this device's windows show, for `person`.
    pub(crate) fn new(workspaces: Workspaces, person: String) -> Self {
        Self { workspaces, person }
    }

    /// Answer the shown actions on this device's dispatch from now on: a window's read, decided
    /// for the person at it.
    pub(crate) fn register(self: Arc<Self>, providers: &Providers) {
        providers.provide(
            tilo_actions::shown::PROVIDER,
            tilo_actions::shown::APP,
            Reach::ThisDevice,
            self,
        );
    }

    /// What this device says of `actions`, shown in a window open in `home`, for `caller` now.
    fn decisions(
        &self,
        caller: &Caller,
        home: Option<&str>,
        actions: &[ShownAction],
    ) -> Vec<ActionDecision> {
        decide_shown(&self.workspaces, &self.actor(caller), home, actions)
    }

    /// Who the verdicts are decided for: the person the caller names, else this device's person
    /// whose window shows them.
    fn actor(&self, caller: &Caller) -> BrokerActor {
        let person = caller
            .acting()
            .person_id()
            .or_else(|| caller.acting_for_person())
            .unwrap_or(&self.person);
        BrokerActor::person(person.to_owned()).capped(caller.is_foreign_device())
    }
}

/// What this device says of the actions `app` declares, at the app's root, and of anything of each
/// app it `uses`, for `person` at a window of it open in `workspace` now, with what the grants say
/// at each node below: handed with the window's launch, so its first frame shows them and acts on
/// them without asking.
#[cfg(target_os = "linux")]
pub(crate) fn declared(
    workspaces: &Workspaces,
    person: &str,
    workspace: &str,
    (app, uses): (&str, &[&str]),
) -> Vec<ActionDecision> {
    let resources = {
        let known = collab_types::app_surface::Surfaces::process().resources_of(app);
        if known.is_empty() {
            fleet_catalogue::surface(app)
                .and_then(|json| collab_types::surface::SurfaceFile::from_json(json).ok())
                .map(|file| file.resources)
                .unwrap_or_default()
        } else {
            known
        }
    };
    let mut actions = tilo_actions::shown::declared_by(app, &resources);
    for used in uses.iter().filter(|used| **used != app) {
        actions.extend(tilo_actions::shown::declared_by(used, &[]));
    }
    decide_shown(
        workspaces,
        &BrokerActor::person(person.to_owned()),
        Some(workspace),
        &actions,
    )
}

/// What `authority` says of `actions`, shown in a window open in `home`, for `actor` now: each
/// the actor's ordinary answer in the workspace that keeps its record, and what a person reads when
/// that workspace is not kept here or cannot be read.
fn decide_shown(
    workspaces: &Workspaces,
    actor: &BrokerActor,
    home: Option<&str>,
    actions: &[ShownAction],
) -> Vec<ActionDecision> {
    let authority: &(dyn WorkspaceAuthority + 'static) = workspaces;
    let now = tilo_clock::try_unix_now();
    // Each action asked about, and what it is around it: the action as declared, and at each node
    // below that a grant names. A record made while the window is open takes the nearest.
    let mut around = Vec::new();
    for action in actions {
        let Some(workspace) = action.workspace.as_deref().or(home) else {
            continue;
        };
        match authority.decide_in_workspace(workspace, |there| actor.shown_around(there, action)) {
            Ok(more) => around.extend(more.into_iter().filter(|more| !actions.contains(more))),
            Err(error) => {
                tracing::warn!(%error, %workspace, "a shown action's workspace could not be read");
            }
        }
    }
    around.sort();
    around.dedup();
    let asked: Vec<ShownAction> = actions.iter().cloned().chain(around).collect();
    let answered = &asked;
    decide(answered, |action| {
        // An answer besides those asked stands for a record the window did not ask about.
        let action = &if actions.contains(action) {
            action.clone()
        } else {
            action.on_any_record()
        };
        let Some(now) = now else {
            tracing::warn!("shown actions cannot be decided without the policy clock");
            return ActionVerdict::Refused(UNREADABLE.into());
        };
        let Some(workspace) = action.workspace.as_deref().or(home) else {
            return ActionVerdict::Refused(NOT_HERE.into());
        };
        authority
            .decide_in_workspace(workspace, |held| actor.shown_verdict(held, action, now))
            .unwrap_or_else(|error| {
                tracing::warn!(%error, %workspace, "a shown action's workspace could not be read");
                ActionVerdict::Refused(UNREADABLE.into())
            })
    })
}

impl ActionProvider for Shown {
    /// A window reads the actions it shows; it offers none of its own.
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Err(ActionCallError::Unavailable {
            reason: UNAVAILABLE.into(),
        }))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        Pending::ready(Ok(InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Rejected {
                error: ActionCallError::UnknownAction,
            },
        }))
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }

    /// What this device says of `ask` for `caller`: now, or once the grants change or the wait
    /// passes when it names the decisions it shows and a wait.
    fn shown_actions(
        &self,
        caller: &Caller,
        ask: ShownAsk,
    ) -> Pending<Result<Vec<ActionDecision>, ActionCallError>> {
        let home = caller.workspace().map(str::to_owned);
        let now = self.decisions(caller, home.as_deref(), &ask.actions);
        let wait = ask
            .wait_ms
            .map(|wait_ms| {
                Duration::from_millis(u64::from(wait_ms)).min(tilo_actions::remote::MAX_REMOTE_WAIT)
            })
            .filter(|wait| !wait.is_zero())
            .filter(|_| !ask.actions.is_empty())
            .filter(|_| ask.after.as_deref() == Some(decisions_revision(&now).as_str()));
        let Some(wait) = wait else {
            return Pending::ready(Ok(now));
        };
        let (completer, answer) = pending();
        let (workspaces, person) = (self.workspaces.clone(), self.person.clone());
        let caller = caller.clone();
        let deciding = move || {
            tilo_identity::policy_change::wait_for_a_change(Some(wait));
            let actor = BrokerActor::person(
                caller
                    .acting()
                    .person_id()
                    .or_else(|| caller.acting_for_person())
                    .unwrap_or(&person)
                    .to_owned(),
            )
            .capped(caller.is_foreign_device());
            completer.complete(Ok(decide_shown(
                &workspaces,
                &actor,
                home.as_deref(),
                &ask.actions,
            )));
        };
        if let Err(error) = std::thread::Builder::new()
            .name("tilo-shown-wait".into())
            .spawn(deciding)
        {
            tracing::warn!(%error, "a shown action's wait could not start");
            return Pending::ready(Ok(now));
        }
        answer
    }
}
