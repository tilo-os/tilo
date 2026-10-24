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
//! The windows ask during their frame, on the desktop's thread; the desktop
//! opens the apps when it next takes [`Openings::open`]. Until then the ask
//! stays pending, so a window shows its open as in progress, then done or
//! refused, exactly as with a broker.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionHost, ActionHostError, Caller,
    CataloguePage, CatalogueQuery, Completer, InvocationReceipt, InvocationState, InvokeArgument,
    InvokeRequest, Pending, ReceiptStatus, pending, workspace_provider_room,
};

use tilo_lock::MutexRecover as _;

use crate::DesktopHandle;
use crate::navigation::Command;

/// What one admitted open answers to.
type Answer = Completer<Result<InvocationReceipt, ActionHostError>>;

/// An app a window asked to open, waiting for the desktop.
struct Opening {
    app: String,
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
    woken: Arc<AtomicBool>,
}

/// The desktop's end of its [`DesktopSession`].
pub struct Openings {
    asked: Asked,
}

/// The session for the workspace `room`, and the desktop's end of it.
/// `woken` is raised whenever a window asks, so the desktop's host draws a
/// frame that takes the ask.
#[must_use]
pub fn serve(room: impl Into<String>, woken: Arc<AtomicBool>) -> (Arc<DesktopSession>, Openings) {
    let asked = Asked::default();
    (
        Arc::new(DesktopSession {
            room: room.into(),
            asked: Arc::clone(&asked),
            woken,
        }),
        Openings { asked },
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
    ) -> Result<&'a str, ActionHostError> {
        let address = &request.target.address;
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
        Ok(app)
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
        let app = match self.app_to_open(caller, &request) {
            Ok(app) => app.to_owned(),
            Err(error) => return Pending::ready(Err(error)),
        };
        let (answer, receipt) = pending();
        self.asked.lock_recover().push(Opening {
            app,
            invocation: request.invocation,
            answer,
        });
        self.woken.store(true, Ordering::Release);
        receipt
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        // Every open is answered whole at admission.
        None
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionHostError>> {
        tilo_actions::remote::Pending::ready(Err(ActionHostError::Refused {
            reason: "Opening an app can't be cancelled".into(),
        }))
    }
}

impl Openings {
    /// Open every app the windows asked for since this was last taken, as
    /// launches of the desktop `handle` (navigation `generation`), each one
    /// `may_open` admits. Whether anything was asked, so the host lays out
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
            invocation,
            answer,
        } in asked
        {
            let opened = may_open(&app).and_then(|()| {
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
