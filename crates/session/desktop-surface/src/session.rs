//! The session a desktop serves the windows it hosts in process.
//!
//! A window never opens another app itself: it asks its session to open it
//! ([`ui::session_door`]), the same request a native window hands its
//! workspace broker. A desktop that hosts its windows in process, such as a
//! page, answers that request here: [`serve`] makes the session the windows
//! are handed ([`DesktopSession`], a remote action host) and the desktop's
//! end of it ([`Openings`]), which opens each admitted app as the desktop's
//! own launch.
//!
//! A window may also ask to open a resource another app keeps, by its
//! record ([`tilo_actions::remote::application_record_target`]), as Chat's
//! shared windows do: the desktop opens that app on that record. Any other
//! action of an app's resource
//! ([`tilo_actions::remote::application_action_target`]) is delivered the
//! same way: the desktop opens the app, and once its window is up the host
//! invokes the action there ([`Reveal`]), as the workspace broker delivers
//! it to an app it launched.
//!
//! The windows ask during their frame, on the desktop's thread; the desktop
//! opens the apps when it next takes [`Openings::open`]. Until then the ask
//! stays pending, so a window shows its open as in progress, then done or
//! refused, exactly as with a broker.
//!
//! The session also serves the workspace's activity, as a native session's
//! broker does (`activity`).

mod activity;

pub use activity::{ActivityAsk, ActivityReads, ActivityRequests, activity_requests};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionHost, ActionHostError, Caller,
    CataloguePage, CatalogueQuery, Completer, InvocationReceipt, InvocationState, InvokeArgument,
    InvokeRequest, NODE_PERMITS_ACTION, NODE_RESOURCE_PREFIX, Pending, ReceiptStatus,
    SHARE_NODE_ACTION, UNSHARE_NODE_ACTION, application_provider, application_provider_record,
    pending, workspace_activity as wire, workspace_provider_room,
};

use tilo_lock::MutexRecover as _;

use crate::DesktopHandle;
use crate::navigation::Command;

/// What one admitted open answers to.
type Answer = Completer<Result<InvocationReceipt, ActionHostError>>;

/// The launch a [`crate::TileSource`] reads as opening `app` on its record
/// `record`: `{RECORD_LAUNCH}{app}:{record}` ([`record_launch`]).
pub const RECORD_LAUNCH: &str = "record:";

/// The launch that opens `app` on its record `record`.
#[must_use]
pub fn record_launch(app: &str, record: &str) -> String {
    format!("{RECORD_LAUNCH}{app}:{record}")
}

/// The app and record a [`record_launch`] names, or `None` for a plain
/// launch.
#[must_use]
pub fn launched_record(launch: &str) -> Option<(&str, &str)> {
    launch
        .strip_prefix(RECORD_LAUNCH)?
        .split_once(':')
        .filter(|(app, record)| !app.is_empty() && !record.is_empty())
}

/// The launch a [`crate::TileSource`] reads as opening `app` with the
/// content whose hash is `hash`: `{CONTENT_LAUNCH}{app}:{hash}`
/// ([`content_launch`]).
pub const CONTENT_LAUNCH: &str = "content:";

/// The launch that opens `app` with the content whose hash is `hash`, as
/// the workspace broker opens an app with content another app handed over
/// (a capture, a recording).
#[must_use]
pub fn content_launch(app: &str, hash: &str) -> String {
    format!("{CONTENT_LAUNCH}{app}:{hash}")
}

/// The app and content hash a [`content_launch`] names, or `None` for any
/// other launch.
#[must_use]
pub fn launched_content(launch: &str) -> Option<(&str, &str)> {
    launch
        .strip_prefix(CONTENT_LAUNCH)?
        .split_once(':')
        .filter(|(app, hash)| !app.is_empty() && content_hash(hash))
}

/// Whether `hash` is a content address: 64 lowercase hex digits.
fn content_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// An app a window asked to open, on one of its records, with content, or
/// on its own, waiting for the desktop.
struct Opening {
    app: String,
    record: Option<String>,
    /// The content the app opens, by hash, which the asking window holds.
    content: Option<String>,
    /// The resource the open names in the record, which the app's window
    /// is to show once it is open.
    resource: Option<String>,
    /// What the window is to do with the resource once it is open: show it
    /// ([`APPLICATION_OPEN_ACTION`]) or another of its actions.
    action: String,
    arguments: serde_json::Map<String, serde_json::Value>,
    invocation: InvocationId,
    answer: Answer,
}

/// The asks the desktop has not taken yet.
type Asked = Arc<std::sync::Mutex<Vec<Opening>>>;

/// What a window asked of one node of the workspace's tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeAction {
    /// Grant the members `access` (`view` or `edit`) on it, as share
    /// `share`.
    Share { share: String, access: String },
    /// Take back what share `share` granted.
    Unshare { share: String },
    /// Whether the caller may change it.
    Permits,
}

/// A window's ask of a node, which the desktop answers through its room
/// ([`NodeAsk::answer`]): the room decides it, as it decides everything a
/// grant gives.
pub struct NodeAsk {
    /// The node, in its text form.
    pub node: String,
    pub action: NodeAction,
    invocation: InvocationId,
    answer: Answer,
}

impl NodeAsk {
    /// Answer the window: the room's output, or why it refused.
    pub fn answer(self, outcome: Result<serde_json::Value, String>) {
        self.answer.complete(Ok(InvocationReceipt {
            invocation: self.invocation,
            status: match outcome {
                Ok(value) => ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput {
                        value: Some(value),
                        ..tilo_actions::ActionOutput::default()
                    },
                },
                Err(reason) => ReceiptStatus::Rejected {
                    error: ActionHostError::Refused { reason },
                },
            },
        }));
    }
}

/// What a window asked of `node`, from its request's action and arguments.
fn node_action(request: &InvokeRequest) -> Result<NodeAction, ActionHostError> {
    let text = |name: &str| match &request.argument {
        InvokeArgument::Json(arguments) => arguments
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| ActionHostError::Refused {
                reason: format!("`{name}` is missing"),
            }),
        _ => Err(ActionHostError::Refused {
            reason: format!("`{name}` is missing"),
        }),
    };
    Ok(match request.target.address.action.as_str() {
        SHARE_NODE_ACTION => NodeAction::Share {
            share: text("share")?,
            access: text("access")?,
        },
        UNSHARE_NODE_ACTION => NodeAction::Unshare {
            share: text("share")?,
        },
        NODE_PERMITS_ACTION => NodeAction::Permits,
        _ => return Err(ActionHostError::UnknownAction),
    })
}

/// The session handed to the windows a desktop hosts in process, for the
/// workspace `room`. It serves only what the desktop itself does: opening
/// an app as a window of this desktop.
pub struct DesktopSession {
    room: String,
    asked: Asked,
    nodes: Arc<std::sync::Mutex<Vec<NodeAsk>>>,
    activity: Arc<activity::Door>,
    woken: Arc<AtomicBool>,
}

/// The desktop's end of its [`DesktopSession`].
pub struct Openings {
    asked: Asked,
    nodes: Arc<std::sync::Mutex<Vec<NodeAsk>>>,
    activity: ActivityRequests,
    /// What the windows opened since [`Openings::take_reveals`] are to show.
    reveals: std::cell::RefCell<Vec<Reveal>>,
}

/// An action a window asked of a resource in an app's record: once the
/// window on that record is up, the host invokes it there, as the
/// workspace broker delivers it to an app it launched. Most often the
/// resource's `open`, which shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reveal {
    /// The app, by package id.
    pub app: String,
    /// The record, as its route names it: empty for the app's own record of
    /// the workspace.
    pub record: String,
    /// The resource the action is of.
    pub resource: String,
    /// The action ([`APPLICATION_OPEN_ACTION`] to show the resource).
    pub action: String,
    /// What the action is invoked with.
    pub arguments: serde_json::Map<String, serde_json::Value>,
}

/// The session for the workspace `room`, and the desktop's end of it.
/// `woken` is raised whenever a window asks, so the desktop's host draws a
/// frame that takes the ask.
#[must_use]
pub fn serve(room: impl Into<String>, woken: Arc<AtomicBool>) -> (Arc<DesktopSession>, Openings) {
    let asked = Asked::default();
    let nodes = Arc::<std::sync::Mutex<Vec<NodeAsk>>>::default();
    let activity = activity::Door::new(Arc::clone(&woken));
    (
        Arc::new(DesktopSession {
            room: room.into(),
            asked: Arc::clone(&asked),
            nodes: Arc::clone(&nodes),
            activity: Arc::clone(&activity),
            woken,
        }),
        Openings {
            asked,
            nodes,
            activity: activity.requests(),
            reveals: std::cell::RefCell::default(),
        },
    )
}

impl DesktopSession {
    /// The app `request` opens in this desktop's workspace, the record it
    /// opens it on (`None` for the app's own), and the content it opens in
    /// it, by hash.
    ///
    /// # Errors
    /// A request for another workspace, for something other than an app's
    /// action, or with anything but a content hash to open in it.
    fn app_to_open<'a>(
        &self,
        caller: &Caller,
        request: &'a InvokeRequest,
    ) -> Result<(&'a str, Option<&'a str>, Option<&'a str>), ActionHostError> {
        let address = &request.target.address;
        if let Some((room, app)) = application_provider(&request.target.provider) {
            return self
                .record_to_open(caller, request, room)
                .map(|record| (app, record, None));
        }
        let app = address
            .resource
            .strip_prefix(APPLICATION_RESOURCE_PREFIX)
            .filter(|_| address.action == APPLICATION_OPEN_ACTION)
            .ok_or(ActionHostError::UnknownAction)?;
        // As the broker: opening an app is something a workspace member
        // asks of their workspace.
        if caller.room().is_none() {
            return Err(ActionHostError::UnknownAction);
        }
        if workspace_provider_room(&request.target.provider) != Some(self.room.as_str()) {
            return Err(ActionHostError::Unavailable {
                reason: "That workspace isn't open here".into(),
            });
        }
        // As the broker: the argument is the content to open, by hash
        // (`ApplicationLaunch::argument`).
        match &request.argument {
            InvokeArgument::None => Ok((app, None, None)),
            InvokeArgument::Text(hash) if content_hash(hash) => Ok((app, None, Some(hash))),
            _ => Err(ActionHostError::Refused {
                reason: "That isn't something an app can open".into(),
            }),
        }
    }

    /// The record an action of a resource in `room` names, `None` for the
    /// app's own.
    ///
    /// # Errors
    /// An unnamed action, another workspace, or a caller from outside it.
    fn record_to_open<'a>(
        &self,
        caller: &Caller,
        request: &'a InvokeRequest,
        room: &str,
    ) -> Result<Option<&'a str>, ActionHostError> {
        if request.target.address.action.is_empty() || caller.room().is_none() {
            return Err(ActionHostError::UnknownAction);
        }
        if room != self.room {
            return Err(ActionHostError::Unavailable {
                reason: "That workspace isn't open here".into(),
            });
        }
        Ok(application_provider_record(&request.target.provider))
    }
}

impl ActionHost for DesktopSession {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        // The windows find what they may open in their own catalogue; this
        // session only opens it.
        Pending::ready(Ok(CataloguePage {
            exhausted: true,
            ..CataloguePage::default()
        }))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        if request.target.provider == wire::PROVIDER {
            // As the broker: a workspace's activity is its members' to read.
            if caller.room().is_none() {
                return Pending::ready(Err(ActionHostError::UnknownAction));
            }
            return self.activity.invoke(caller, request);
        }
        if let Some(node) = request
            .target
            .address
            .resource
            .strip_prefix(NODE_RESOURCE_PREFIX)
            .filter(|_| workspace_provider_room(&request.target.provider).is_some())
        {
            // As the broker: what a node grants is its workspace members'
            // business, in this workspace only.
            if caller.room().is_none() {
                return Pending::ready(Err(ActionHostError::UnknownAction));
            }
            if workspace_provider_room(&request.target.provider) != Some(self.room.as_str()) {
                return Pending::ready(Err(ActionHostError::Unavailable {
                    reason: "That workspace isn't open here".into(),
                }));
            }
            let action = match node_action(&request) {
                Ok(action) => action,
                Err(error) => return Pending::ready(Err(error)),
            };
            let (answer, receipt) = pending();
            self.nodes.lock_recover().push(NodeAsk {
                node: node.to_owned(),
                action,
                invocation: request.invocation,
                answer,
            });
            self.woken.store(true, Ordering::Release);
            return receipt;
        }
        let (app, record, content) = match self.app_to_open(caller, &request) {
            Ok((app, record, content)) => (
                app.to_owned(),
                record.map(str::to_owned),
                content.map(str::to_owned),
            ),
            Err(error) => return Pending::ready(Err(error)),
        };
        // An action of an app's resource names the resource; an app's own
        // open names the app.
        let resource = application_provider(&request.target.provider)
            .map(|_| request.target.address.resource.clone());
        let arguments = match request.argument {
            InvokeArgument::None => serde_json::Map::new(),
            InvokeArgument::Text(_) if content.is_some() => serde_json::Map::new(),
            InvokeArgument::Json(arguments) => arguments,
            InvokeArgument::Text(_) => {
                return Pending::ready(Err(ActionHostError::Refused {
                    reason: "Asking another app for that isn't available here yet".into(),
                }));
            }
        };
        let (answer, receipt) = pending();
        self.asked.lock_recover().push(Opening {
            app,
            record,
            content,
            resource,
            action: request.target.address.action,
            arguments,
            invocation: request.invocation,
            answer,
        });
        self.woken.store(true, Ordering::Release);
        receipt
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        // Every open is answered whole at admission; what stays open is a
        // view of the activity.
        self.activity.observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.activity.next_change(caller, id)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionHostError>> {
        if self.activity.serves(id) {
            return Pending::ready(self.activity.cancel(caller, id));
        }
        Pending::ready(Err(ActionHostError::Refused {
            reason: "Opening an app can't be cancelled".into(),
        }))
    }
}

impl Openings {
    /// The desktop's end of the activity the session serves.
    #[must_use]
    pub fn activity(&self) -> ActivityRequests {
        self.activity.clone()
    }

    /// What the windows asked of nodes of the workspace's tree since this
    /// was last taken, for the desktop to ask its room.
    #[must_use]
    pub fn take_node_asks(&self) -> Vec<NodeAsk> {
        std::mem::take(&mut *self.nodes.lock_recover())
    }

    /// What the windows opened since this was last taken are to show.
    #[must_use]
    pub fn take_reveals(&self) -> Vec<Reveal> {
        std::mem::take(&mut *self.reveals.borrow_mut())
    }

    /// Open every app the windows asked for since this was last taken, as
    /// launches of the desktop `handle` (navigation `generation`), each one
    /// `may_open` admits (told whether the open shows something that
    /// exists, a record or an item of one); one asked on a record opens on it
    /// ([`record_launch`]). Whether anything was asked, so the host lays out
    /// again.
    pub fn open(
        &self,
        handle: &DesktopHandle,
        generation: &str,
        may_open: impl Fn(&str, bool) -> Result<(), ActionHostError>,
    ) -> bool {
        let asked = std::mem::take(&mut *self.asked.lock_recover());
        let any = !asked.is_empty();
        for Opening {
            app,
            record,
            content,
            resource,
            action,
            arguments,
            invocation,
            answer,
        } in asked
        {
            let existing = record.is_some() || resource.is_some();
            let opened = may_open(&app, existing).and_then(|()| {
                if let Some(resource) = resource {
                    self.reveals.borrow_mut().push(Reveal {
                        app: app.clone(),
                        record: record.clone().unwrap_or_default(),
                        resource,
                        action,
                        arguments,
                    });
                }
                let app = match (record, content) {
                    (Some(record), _) => record_launch(&app, &record),
                    (None, Some(hash)) => content_launch(&app, &hash),
                    (None, None) => app,
                };
                handle
                    .dispatch(
                        &Command::Launch {
                            generation: generation.to_owned(),
                            app,
                        },
                        generation,
                    )
                    .map_err(|reason| ActionHostError::Unavailable { reason })
            });
            answer.complete(opened.map(|()| InvocationReceipt {
                invocation,
                status: ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput::default(),
                },
            }));
        }
        any
    }
}
