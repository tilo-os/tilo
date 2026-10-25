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
//! shared windows do: the desktop opens that app on that record.
//!
//! The windows ask during their frame, on the desktop's thread; the desktop
//! opens the apps when it next takes [`Openings::open`]. Until then the ask
//! stays pending, so a window shows its open as in progress, then done or
//! refused, exactly as with a broker.
//!
//! The session also serves the workspace's activity, as a native session's
//! broker does ([`activity`]).

mod activity;

pub use activity::{ActivityAsk, ActivityReads, ActivityRequests};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionHost, ActionHostError, Caller,
    CataloguePage, CatalogueQuery, Completer, InvocationReceipt, InvocationState, InvokeArgument,
    InvokeRequest, Pending, ReceiptStatus, application_provider, application_provider_record,
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

/// An app a window asked to open, on one of its records or on its own,
/// waiting for the desktop.
struct Opening {
    app: String,
    record: Option<String>,
    invocation: InvocationId,
    answer: Answer,
}

/// The asks the desktop has not taken yet.
type Asked = Arc<std::sync::Mutex<Vec<Opening>>>;

/// The session handed to the windows a desktop hosts in process, for the
/// workspace `room`. It serves only what the desktop itself does: opening
/// an app as a window of this desktop.
pub struct DesktopSession {
    room: String,
    asked: Asked,
    activity: Arc<activity::Door>,
    woken: Arc<AtomicBool>,
}

/// The desktop's end of its [`DesktopSession`].
pub struct Openings {
    asked: Asked,
    activity: ActivityRequests,
}

/// The session for the workspace `room`, and the desktop's end of it.
/// `woken` is raised whenever a window asks, so the desktop's host draws a
/// frame that takes the ask.
#[must_use]
pub fn serve(room: impl Into<String>, woken: Arc<AtomicBool>) -> (Arc<DesktopSession>, Openings) {
    let asked = Asked::default();
    let activity = activity::Door::new(Arc::clone(&woken));
    (
        Arc::new(DesktopSession {
            room: room.into(),
            asked: Arc::clone(&asked),
            activity: Arc::clone(&activity),
            woken,
        }),
        Openings {
            asked,
            activity: activity.requests(),
        },
    )
}

impl DesktopSession {
    /// The app `request` opens in this desktop's workspace.
    ///
    /// # Errors
    /// A request for another workspace, for something other than opening an
    /// app, or with content to open in it, which a desktop hosting windows
    /// in process cannot hand over yet.
    fn app_to_open<'a>(
        &self,
        caller: &Caller,
        request: &'a InvokeRequest,
    ) -> Result<(&'a str, Option<&'a str>), ActionHostError> {
        let address = &request.target.address;
        if let Some((room, app)) = application_provider(&request.target.provider) {
            return self
                .record_to_open(caller, request, room)
                .map(|record| (app, record));
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
        if !matches!(request.argument, InvokeArgument::None) {
            return Err(ActionHostError::Refused {
                reason: "Opening something in another app isn't available here yet".into(),
            });
        }
        Ok((app, None))
    }

    /// The record an open of a resource in `room` names, `None` for the
    /// app's own.
    ///
    /// # Errors
    /// Another action than opening, another workspace, or a caller from
    /// outside it.
    fn record_to_open<'a>(
        &self,
        caller: &Caller,
        request: &'a InvokeRequest,
        room: &str,
    ) -> Result<Option<&'a str>, ActionHostError> {
        if request.target.address.action != APPLICATION_OPEN_ACTION || caller.room().is_none() {
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
        let (app, record) = match self.app_to_open(caller, &request) {
            Ok((app, record)) => (app.to_owned(), record.map(str::to_owned)),
            Err(error) => return Pending::ready(Err(error)),
        };
        let (answer, receipt) = pending();
        self.asked.lock_recover().push(Opening {
            app,
            record,
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

    /// Open every app the windows asked for since this was last taken, as
    /// launches of the desktop `handle` (navigation `generation`), each one
    /// `may_open` admits; one asked on a record opens on it
    /// ([`record_launch`]). Whether anything was asked, so the host lays out
    /// again.
    pub fn open(
        &self,
        handle: &DesktopHandle,
        generation: &str,
        may_open: impl Fn(&str) -> Result<(), ActionHostError>,
    ) -> bool {
        let asked = std::mem::take(&mut *self.asked.lock_recover());
        let any = !asked.is_empty();
        for Opening {
            app,
            record,
            invocation,
            answer,
        } in asked
        {
            let opened = may_open(&app).and_then(|()| {
                let app = match record {
                    Some(record) => record_launch(&app, &record),
                    None => app,
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
