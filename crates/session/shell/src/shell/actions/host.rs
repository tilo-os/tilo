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
    ActionFailure, ActionSpec, EncodedAction, InvocationId, InvocationUpdate, OperationKind,
    ResourceSpec,
};
use tilo_app::framework::{HostedActions, HostedActionsLink, HostedInvocation};
use ui::application::{
    AppComponent, ApplicationDefinition, HostContext, LaunchTarget, RestoreInput,
};

use super::{Action, begin, declared_keywords};
use crate::shell::chrome_state::Chrome;
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

impl Action {
    /// What this action is published as: its address, words, parameters,
    /// contract and kind, as declared.
    pub(crate) fn spec(&'static self) -> ActionSpec {
        let id = self.id;
        let kind = match self.kind {
            OperationKind::View => OperationKind::View,
            // The handler changes the shell's state, not the app's document.
            _ => OperationKind::Effect,
        };
        ActionSpec::typed_operation(
            id,
            t(self.label),
            "",
            kind,
            OPERATION,
            self.params.to_vec(),
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
        .with_keywords(declared_keywords(self.keywords))
        .with_contract(self.contract())
    }
}

/// Every shell action, as the one resource the shell publishes.
pub(crate) fn resource() -> ResourceSpec {
    Action::ALL.iter().fold(
        ResourceSpec::new(RESOURCE, RESOURCE, t("search-kind-actions")),
        |resource, action| resource.with_action(action.spec()),
    )
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
    /// The locale the published labels are in.
    locale: String,
}

impl ShellActionsHost {
    /// Mount the shell's app in `room` through the session's in-process apps.
    pub(crate) fn mount(
        apps: &crate::shell::in_process::SharedInProcessApps,
        room: &str,
    ) -> Option<Self> {
        let link = HostedActionsLink::new(OPERATION);
        link.publish(vec![resource()]);
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
            locale: crate::shell::i18n::current_locale(),
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
            *held = Self::mount(&apps, &room);
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
        let locale = crate::shell::i18n::current_locale();
        if locale != self.locale {
            // Labels are published in the person's language.
            self.link.publish(vec![resource()]);
            self.locale = locale;
        }
        let woken = self.woken.swap(false, Ordering::AcqRel);
        let mut changed = self.app.update(doc, path) | woken;
        for asked in self.link.take_asked() {
            changed = true;
            self.run(chrome, asked);
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

    fn run(&mut self, chrome: &Chrome, asked: HostedInvocation) {
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
