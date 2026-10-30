//! The shell as an app: its actions published through the toolkit's app
//! host on the session's broker, like any app's, and run here.
//!
//! The shell mounts one app of its own, [`APP_ID`], in the room of the
//! workspace in front, through the same in-process mount and broker
//! connection the apps it hosts use. The app's root is the toolkit's
//! [`HostedActions`]: it publishes one spec per [`Action`] on the `actions`
//! resource, so an action keeps its address `actions/<id>`, and the broker
//! admits each invocation with the caller's authority under the room's policy
//! before it reaches the app. Each admitted invocation is run here, on the
//! compositor thread, by the one handler every door reaches ([`begin`]), and
//! its answer goes back to the caller through the app.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_actions::{
    ActionFailure, ActionSpec, ActionState, EncodedAction, InvocationId, InvocationUpdate,
    OperationKind, ResourceSpec,
};
use tilo_app::framework::{HostedActions, HostedActionsLink, HostedInvocation};
use ui::application::{
    AppComponent, ApplicationDefinition, HostContext, LaunchTarget, RestoreInput,
};

use super::{Action, begin, declared_keywords};
use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::i18n::t;
use crate::shell::search::action_api::{ActionArgs, ActionError};
use crate::shell::search::{ActionReceipt, InvocationDisposition, PendingInvocation};

/// The shell's app id: the app its actions are published by, and the node
/// they sit under in a workspace's policy.
pub(crate) const APP_ID: &str = crate::shell::search::visibility::SHELL_APP_ID;

/// The operation every shell action encodes as: which action, with which
/// arguments.
pub(crate) const OPERATION: &str = "so.tilo.shell.action";

/// The resource the shell's actions are published on.
pub(crate) const RESOURCE: &str = "actions";

#[derive(serde::Serialize, serde::Deserialize)]
struct Invocation {
    id: String,
    #[serde(default)]
    arguments: serde_json::Map<String, serde_json::Value>,
}

/// What the published actions show of the shell's state now: each row's
/// badge, keys and parameters, and whether it has anything to act on.
pub(crate) struct Live<'a> {
    state: &'a ChromeState,
    shortcuts: settings_store::hardware::ShortcutConfig,
    display_waiting: bool,
    invite_offered: bool,
}

impl<'a> Live<'a> {
    pub(crate) fn read(state: &'a ChromeState) -> Self {
        Self {
            state,
            shortcuts: settings_store::hardware::ShortcutConfig::load(),
            display_waiting: super::display_change_waiting(),
            invite_offered: state.peer_sync_enabled
                && crate::shell::invite_policy::active_invite_route(state)
                    .is_some_and(crate::shell::invite_policy::InviteRoute::issues_here),
        }
    }

    /// Whether `action` is offered here at all: a session without peer
    /// sync has nothing to invite into, an invitation the issuer would
    /// refuse from here has no composer, and an agent needs a room and a
    /// working broker.
    fn offered(&self, action: &Action) -> bool {
        let id = action.id;
        if (id.starts_with("peer.") || super::INVITATION_ACTION_IDS.contains(&id))
            && !self.state.peer_sync_enabled
        {
            return false;
        }
        if matches!(id, "peer.invite" | "peer.invite.named") && !self.invite_offered {
            return false;
        }
        !(id.starts_with("agent.")
            && (!self.state.collab_available()
                || self.state.tree().desktop.active_workspace().is_none()))
    }

    fn availability(&self, action: &Action) -> tilo_actions::Availability {
        if !self.offered(action) {
            tilo_actions::Availability::Unavailable {
                code: "not_offered".into(),
                reason: t("shell-action-not-offered"),
            }
        } else if !super::action_available(action.id, self.state, self.display_waiting) {
            tilo_actions::Availability::Unavailable {
                code: "nothing_to_act_on".into(),
                reason: t("shell-action-nothing-to-act-on"),
            }
        } else {
            tilo_actions::Availability::Available
        }
    }
}

impl Action {
    /// What this action is published as: its address, words, parameters,
    /// contract and kind, as declared, and what its row shows of the
    /// shell's state now.
    pub(crate) fn spec(&'static self, live: &Live<'_>) -> ActionSpec {
        let id = self.id;
        let kind = match self.kind {
            OperationKind::View => OperationKind::View,
            // The handler changes the shell's state, not the app's document.
            _ => OperationKind::Effect,
        };
        let mut contract = self.contract();
        contract.availability = live.availability(self);
        let spec = ActionSpec::typed_operation(
            id,
            t(self.label),
            "",
            kind,
            OPERATION,
            super::live_params(self, live.state),
            move |args| EncodedAction {
                type_id: OPERATION.into(),
                payload: serde_json::to_vec(&Invocation {
                    id: id.to_owned(),
                    arguments: args.to_json(),
                })
                .unwrap_or_default(),
            },
        )
        .with_description(t(self.subtitle))
        .with_keywords(published_keywords(self))
        .with_contract(contract)
        .with_state(ActionState {
            badge: super::state_badge(id, live.state),
            toggles: self.toggles,
            shortcut: super::action_shortcut(&live.shortcuts, id),
        });
        match super::action_icon(id) {
            Some(icon) => spec.with_icon(icon.name()),
            None => spec,
        }
    }
}

/// The other words an action is found by: its declared keywords, and for
/// joining a workspace the community site's name ("tilo.coffee"), which is
/// the image's configuration rather than a word.
fn published_keywords(action: &Action) -> Vec<String> {
    let mut keywords = declared_keywords(action.keywords);
    if action.id == "peer.connect"
        && let Some(site) = crate::shell::network::configured_community_site()
        && let Some(host) = site.host_str()
    {
        keywords.push(host.to_owned());
    }
    keywords
}

/// Every shell action, as the one resource the shell publishes.
pub(crate) fn resource(state: &ChromeState) -> ResourceSpec {
    let live = Live::read(state);
    Action::ALL.iter().fold(
        ResourceSpec::new(RESOURCE, RESOURCE, t("search-kind-actions")),
        |resource, action| resource.with_action(action.spec(&live)),
    )
}

/// What the published specs are built from, cheap to read every frame: the
/// shell publishes anew only when it changes.
#[derive(Clone, PartialEq, Eq)]
struct Published {
    chrome: u64,
    revision: u64,
    external_tree_revision: u64,
    locale: String,
    settings: Option<settings_store::SettingsStamp>,
    peer_sync: bool,
    collab: bool,
    /// Whether things that come and go are there to act on.
    present: [bool; 8],
}

impl Published {
    fn read(state: &ChromeState) -> Self {
        Self {
            chrome: state.instance(),
            revision: state.revision,
            external_tree_revision: state.tree().desktop.external_revision,
            locale: crate::shell::i18n::current_locale(),
            settings: settings_store::stamp(),
            peer_sync: state.peer_sync_enabled,
            collab: state.collab_available(),
            present: [
                state.dnd(),
                state.axis_navigation_enabled(),
                state.overlays.projection,
                state.search_context.following,
                state.search_context.dictating,
                state.notification_log().history().is_empty(),
                state.can_undo_notification_history_clear(),
                crate::bluetooth::PairingPromptHandle::global()
                    .current()
                    .is_some(),
            ],
        }
    }
}

static DEFINITION: std::sync::LazyLock<ApplicationDefinition> =
    std::sync::LazyLock::new(|| ApplicationDefinition {
        app_id: APP_ID.into(),
        title: "Tilo".into(),
        icon: ui::widget::IconDescriptor::Named("tilo".into()),
        record_kind: ui::record::RecordKind::new("shell.actions"),
        required: vec![],
        optional: vec![],
        opens: &[],
        room_record: true,
        member_record: false,
        opens_before_record: false,
        create,
    });

fn create(
    mut context: HostContext,
    _target: LaunchTarget,
    _restore: RestoreInput,
) -> Result<AppComponent, ui::launch::LaunchError> {
    let link = context
        .take_lent()
        .into_iter()
        .find_map(|lent| lent.downcast::<HostedActionsLink>().ok())
        .ok_or_else(|| {
            ui::launch::LaunchError::Operation("the shell lent its app no actions".into())
        })?;
    Ok(AppComponent::new(
        context,
        Box::new(HostedActions::new(
            *link,
            ui::widget::AppKind::Settings,
            DEFINITION.title.to_string(),
            DEFINITION.icon.clone(),
        )),
        tilo_actions::ActionRegistry::new(),
    ))
}

thread_local! {
    static REFUSED_ROOM: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// An invocation that finishes later, with the ticket that tells when.
struct Waiting {
    invocation: InvocationId,
    ticket: PendingInvocation,
}

/// The shell's app, mounted in one room.
pub(crate) struct ShellActionsHost {
    room: String,
    link: HostedActionsLink,
    app: Box<dyn ui::widget::AppWidget>,
    woken: Arc<AtomicBool>,
    waiting: Vec<Waiting>,
    /// What the published specs were built from.
    published: Published,
}

impl ShellActionsHost {
    /// Mount the shell's app in `room` through the session's in-process apps.
    pub(crate) fn mount(
        apps: &crate::shell::in_process::SharedInProcessApps,
        room: &str,
        state: &ChromeState,
    ) -> Option<Self> {
        let link = HostedActionsLink::new(OPERATION);
        let published = Published::read(state);
        link.publish(vec![resource(state)]);
        let woken = Arc::new(AtomicBool::new(false));
        {
            let woken = Arc::clone(&woken);
            link.on_asked(move || {
                woken.store(true, Ordering::Release);
                crate::shell::wake_compositor();
            });
        }
        let mut apps = apps.borrow_mut();
        let lent = link.clone();
        let component = apps
            .mount(
                &DEFINITION,
                room,
                0,
                ui::record::RecordId::room_default(),
                |context| context.lend(lent),
            )
            .inspect_err(|error| tracing::warn!(%error, "the shell's actions could not be mounted"))
            .ok()?;
        let collaboration = apps.collaboration(&component, room);
        drop(apps);
        let wake = {
            let woken = Arc::clone(&woken);
            move |_: ui::WakeEvent| {
                woken.store(true, Ordering::Release);
                crate::shell::wake_compositor();
            }
        };
        let app = tilo_desktop::resolver::app_tile(component, collaboration, wake)
            .inspect_err(
                |error| tracing::warn!(%error, "the shell's actions could not be published"),
            )
            .ok()?;
        Some(Self {
            room: room.to_owned(),
            link,
            app,
            woken,
            waiting: Vec::new(),
            published,
        })
    }

    /// The shell's app in the room of the workspace in front, mounted there
    /// once the session lends the apps it mounts in its own process, and
    /// mounted anew when another workspace comes to the front. `None` without
    /// a workspace or in-process apps (a bare test host).
    pub(crate) fn keep<'a>(held: &'a mut Option<Self>, chrome: &Chrome) -> Option<&'a mut Self> {
        let (apps, room) = {
            let state = chrome.borrow();
            let apps = state.hosted_inputs.in_process().cloned();
            let room = state
                .tree()
                .desktop
                .active_workspace()
                .map(|workspace| workspace.room().to_string());
            (apps, room)
        };
        let (Some(apps), Some(room)) = (apps, room) else {
            return held.as_mut();
        };
        if held.as_ref().is_none_or(|host| host.room != room) {
            // A room the app could not be mounted in is not tried again on
            // every frame.
            let tried = REFUSED_ROOM.with(|refused| refused.borrow().as_deref() == Some(&room));
            if held.is_none() && tried {
                return None;
            }
            *held = Self::mount(&apps, &room, &chrome.borrow());
            if held.is_none() {
                REFUSED_ROOM.with(|refused| *refused.borrow_mut() = Some(room));
            }
        }
        held.as_mut()
    }

    /// Run the app one step: publish, admit, run what was admitted and
    /// answer what finished. Whether the chrome may have changed.
    pub(crate) fn pump(
        &mut self,
        chrome: &Chrome,
        doc: &ui::widget::AppModel,
        path: &ui::widget::ModelPath,
    ) -> bool {
        {
            // Labels are published in the person's language, and each row
            // shows the state it acts on as it is now.
            let state = chrome.borrow();
            let published = Published::read(&state);
            if published != self.published {
                self.link.publish(vec![resource(&state)]);
                self.published = published;
            }
        }
        let woken = self.woken.swap(false, Ordering::AcqRel);
        let mut changed = self.app.update(doc, path) | woken;
        for asked in self.link.take_asked() {
            changed = true;
            self.run(chrome, &asked);
        }
        let link = &self.link;
        self.waiting.retain(|waiting| match (waiting.ticket)() {
            Ok(None) => true,
            outcome => {
                link.answer(
                    waiting.invocation,
                    update(waiting.invocation, outcome.transpose()),
                );
                false
            }
        });
        if changed {
            // The app reports the answers it now has.
            self.app.update(doc, path);
        }
        changed
    }

    fn run(&mut self, chrome: &Chrome, asked: &HostedInvocation) {
        let invocation = asked.invocation;
        let started = serde_json::from_slice::<Invocation>(&asked.payload)
            .map_err(|error| ActionError::Refused(error.to_string()))
            .and_then(|request| {
                let action = Action::find(&request.id).ok_or_else(|| {
                    ActionError::Refused(format!("the shell offers no action {}", request.id))
                })?;
                let args = ActionArgs::bind_json(action.params, &request.arguments)
                    .map_err(ActionError::Arguments)?;
                begin(chrome, action, &args)
            });
        match started {
            Ok(InvocationDisposition::Pending(ticket)) => {
                self.link
                    .answer(invocation, InvocationUpdate::Accepted { invocation });
                self.waiting.push(Waiting { invocation, ticket });
            }
            Ok(InvocationDisposition::Accepted(receipt)) => {
                self.link
                    .answer(invocation, update(invocation, Some(Ok(receipt))));
            }
            Err(error) => self
                .link
                .answer(invocation, update(invocation, Some(Err(error)))),
        }
    }
}

/// The update `invocation`'s caller observes for `outcome`.
fn update(
    invocation: InvocationId,
    outcome: Option<Result<ActionReceipt, ActionError>>,
) -> InvocationUpdate {
    match outcome {
        None => InvocationUpdate::Accepted { invocation },
        Some(Ok(receipt)) => match receipt.update {
            InvocationUpdate::Succeeded { output, .. } => {
                InvocationUpdate::Succeeded { invocation, output }
            }
            other => other.with_invocation(invocation),
        },
        Some(Err(error)) => InvocationUpdate::Failed {
            invocation,
            error: ActionFailure {
                code: error.code().into(),
                message: error.to_string(),
                details: None,
                retryable: false,
            },
        },
    }
}
