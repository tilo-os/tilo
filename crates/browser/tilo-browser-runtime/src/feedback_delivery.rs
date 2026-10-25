//! Feedback custody produced by an existing certified peer. Configuration picks
//! an existing destination; only its authenticated owner supplies admission.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use tilo_actions::remote::feedback::{FeedbackAnswer, FeedbackDestination, FeedbackQuery};
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
    RemoteActionTarget,
};
use tilo_app::feedback::{Error, Submission};
use tilo_app::feedback::{MaskedScreenshot, ReviewText};
use tilo_browser_peer::{PeerEvent, PeerEvents, PeerHandle};
use tilo_desktop::feedback::Destination;
use tilo_desktop::feedback::transport::{
    Capture, Enrollment, Identity, NoCapture, SignedReceipt, Startup, Transport,
};
use tilo_user_identity::DeviceCert;

/// How long the owner's admission of this reporter stands: long enough to
/// write and send a report after reviewing it, short enough that a person
/// the room removes cannot file for long after. While the page is admitted
/// it asks again before then ([`ADMISSION_RENEW`]), so a Send never meets a
/// lapsed admission.
const ADMISSION_FRESH: Duration = Duration::from_secs(60);
/// How old an admission is when the page asks the owner for a new one.
const ADMISSION_RENEW: Duration = Duration::from_secs(40);
/// How long a question to the owner is waited for before it is asked again.
const ASK_TIMEOUT: Duration = Duration::from_secs(30);

thread_local! { static CONNECTIONS: RefCell<HashMap<u32, Rc<RefCell<Connection>>>> = RefCell::new(HashMap::new()); }
thread_local! {
    static STARTUPS: RefCell<HashMap<u32, Startup>> = RefCell::new(HashMap::new());
    static WAKES: RefCell<HashMap<u32, std::sync::Arc<dyn Fn() + Send + Sync>>> =
        RefCell::new(HashMap::new());
}

struct Connection {
    peer: PeerHandle,
    events: PeerEvents,
    reporter: DeviceCert,
    owner: DeviceCert,
    destination: FeedbackDestination,
    workspace: u64,
    serial: u64,
    pending: HashMap<u64, (web_time::Instant, Option<Submission>)>,
    /// Reports filed through the owner, by the invocation that files them.
    filing: HashMap<tilo_actions::InvocationId, (web_time::Instant, Submission)>,
    admission: Option<(Enrollment, web_time::Instant)>,
    receipts: HashMap<String, SignedReceipt>,
    installed: bool,
    error: Option<String>,
}

pub(crate) fn configure(
    id: u32,
    peer: PeerHandle,
    reporter: DeviceCert,
    owner: DeviceCert,
    destination: FeedbackDestination,
) -> Result<(), String> {
    let workspace = tilo_relay_protocol::room_workspace_id(&destination.room)
        .map_err(|_| "feedback room must be a uuid")?;
    Destination {
        room: destination.room.clone(),
        provider: destination.provider.clone(),
        channel: destination.channel.clone(),
    }
    .validate()
    .map_err(|error| error.to_string())?;
    if crate::replication::device_of(id).as_deref()
        != Some(hex::encode(reporter.device_public_key).as_str())
        || !crate::replication::has_record(id, "so.tilo.chat", "")
    {
        return Err("feedback requires an already joined peer and existing Chat source".into());
    }
    CONNECTIONS.with_borrow_mut(|connections| {
        if connections.contains_key(&id) {
            return Err("feedback delivery already configured".into());
        }
        let mut connection = Connection {
            events: peer.events(),
            peer,
            reporter,
            owner,
            destination,
            workspace,
            serial: 0,
            pending: HashMap::new(),
            filing: HashMap::new(),
            admission: None,
            receipts: HashMap::new(),
            installed: false,
            error: None,
        };
        connection.request(None)?;
        connections.insert(id, Rc::new(RefCell::new(connection)));
        Ok(())
    })
}
pub(crate) fn forget(id: u32) {
    CONNECTIONS.with_borrow_mut(|connections| {
        connections.remove(&id);
    });
}
pub(crate) fn status(id: u32) -> String {
    CONNECTIONS.with_borrow(|connections| {
        connections.get(&id).map_or_else(
            || "unconfigured".into(),
            |connection| {
                let connection = connection.borrow();
                connection.error.clone().unwrap_or_else(|| {
                    if connection.installed {
                        "ready".into()
                    } else {
                        "awaiting authenticated admission".into()
                    }
                })
            },
        )
    })
}
impl Connection {
    fn request(&mut self, submission: Option<Submission>) -> Result<(), String> {
        self.pending
            .retain(|_, (sent, _)| sent.elapsed() < ASK_TIMEOUT);
        if self.pending.len() >= 8 {
            return Err("feedback owner requests pending".into());
        }
        self.serial = self
            .serial
            .checked_add(1)
            .ok_or("feedback correlation exhausted")?;
        let frame = RemoteActionFrame::new(RemoteActionMessage::FeedbackRequest {
            correlation: self.serial,
            query: FeedbackQuery {
                destination: self.destination.clone(),
                submission: submission
                    .as_ref()
                    .map(serde_json::to_value)
                    .transpose()
                    .map_err(|error| error.to_string())?,
            },
        })
        .encode()
        .map_err(|error| error.to_string())?;
        self.peer
            .send_remote_action(self.workspace, self.owner.device_public_key, frame)?;
        self.pending
            .insert(self.serial, (web_time::Instant::now(), submission));
        Ok(())
    }
    /// File `submission` through the owner's own submit action. It runs
    /// there as this reporter, so filing needs only their right to comment,
    /// and is done when the owner's durable store holds it.
    fn file(&mut self, submission: &Submission) -> Result<(), String> {
        self.filing
            .retain(|_, (sent, _)| sent.elapsed() < Duration::from_secs(30));
        if self.filing.len() >= 8 {
            return Err("feedback reports pending".into());
        }
        let invocation = tilo_actions::InvocationId::new();
        let arguments = serde_json::json!({ "submission": submission });
        let request = InvokeRequest {
            invocation,
            target: RemoteActionTarget {
                provider: self.destination.provider.clone(),
                address: tilo_actions::ActionAddress::new(
                    tilo_app::feedback::destination::item(
                        tilo_app::feedback::destination::CHANNELS,
                        &self.destination.channel,
                    ),
                    tilo_app::feedback::destination::SUBMIT,
                ),
            },
            argument: InvokeArgument::Json(
                arguments
                    .as_object()
                    .cloned()
                    .ok_or("invalid submission arguments")?,
            ),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: Some(20_000),
            appearance: None,
        };
        let frame = RemoteActionFrame::new(RemoteActionMessage::Invoke(request))
            .encode()
            .map_err(|error| error.to_string())?;
        self.peer
            .send_remote_action(self.workspace, self.owner.device_public_key, frame)?;
        self.filing
            .insert(invocation, (web_time::Instant::now(), submission.clone()));
        Ok(())
    }
    fn admit(&self, answer: &FeedbackAnswer) -> Result<Enrollment, String> {
        Enrollment::admitted(
            answer,
            &self.destination,
            &identity(&self.owner),
            &identity(&self.reporter),
        )
        .map_err(|error| format!("feedback admission refused: {error}"))
    }
    fn current(&self) -> Result<Enrollment, Error> {
        let status = self.peer.status();
        if !status.authenticated || status.rooms.get(&self.workspace) != Some(&true) {
            return Err(Error::Invalid("feedback session admission unavailable"));
        }
        self.admission
            .as_ref()
            .filter(|(_, received)| received.elapsed() < ADMISSION_FRESH)
            .map(|(admission, _)| admission.clone())
            .ok_or(Error::Invalid(
                "fresh authenticated feedback admission required",
            ))
    }
}
impl Connection {
    /// The owner's answer to filing a report: once it is saved there, ask
    /// for the signed receipt; a refusal stays visible until a retry.
    fn filed(&mut self, receipt: tilo_actions::remote::InvocationReceipt) {
        let Some((_, submission)) = self.filing.remove(&receipt.invocation) else {
            return;
        };
        match receipt.status {
            ReceiptStatus::Completed { .. } => {
                self.error = None;
                if let Err(error) = self.request(Some(submission)) {
                    self.error = Some(error);
                }
            }
            // Still running: the receipt request after it settles answers.
            ReceiptStatus::Accepted => {}
            ReceiptStatus::Failed { error } => {
                self.error = Some(format!(
                    "feedback owner could not file it: {}",
                    error.message
                ));
            }
            ReceiptStatus::Rejected { error } => {
                self.error = Some(format!("feedback owner refused it: {error}"));
            }
            ReceiptStatus::Cancelled { .. } => {
                self.error = Some("feedback filing was cancelled".into());
            }
        }
    }
}

pub(crate) fn pump(id: u32) {
    let connection = CONNECTIONS.with_borrow(|connections| connections.get(&id).cloned());
    let Some(connection) = connection else {
        return;
    };
    renew_admission(&mut connection.borrow_mut());
    let events = connection.borrow().events.drain();
    for event in events {
        let mut state = connection.borrow_mut();
        match event {
            PeerEvent::RemoteAction {
                workspace_id,
                from,
                frame,
            } if workspace_id == state.workspace && from == state.owner.device_public_key => {
                let Ok(RemoteActionFrame { message, .. }) = RemoteActionFrame::decode(&frame)
                else {
                    continue;
                };
                let (correlation, result) = match message {
                    RemoteActionMessage::FeedbackResponse {
                        correlation,
                        result,
                    } => (correlation, result),
                    RemoteActionMessage::Receipt(receipt) => {
                        state.filed(receipt);
                        continue;
                    }
                    _ => continue,
                };
                let Some((_, submission)) = state.pending.remove(&correlation) else {
                    continue;
                };
                match result
                    .map_err(|error| format!("feedback owner refused: {error:?}"))
                    .and_then(|answer| state.admit(&answer).map(|admission| (answer, admission)))
                {
                    Ok((answer, admission)) => {
                        state.admission = Some((admission.clone(), web_time::Instant::now()));
                        state.error = None;
                        if let Some(submission) = submission
                            && let Some(receipt) = admission.receipt_in(&answer, &submission)
                        {
                            if state.receipts.len() >= 8 {
                                state.receipts.clear();
                            }
                            state.receipts.insert(submission.id, receipt);
                        }
                        if !state.installed {
                            drop(state);
                            let setup = Startup::new(
                                admission,
                                Box::new(Session {
                                    connection: connection.clone(),
                                }),
                                Box::new(PageContent),
                            )
                            .map_err(|error| error.to_string())
                            .and_then(|startup| register_feedback_startup(id, startup));
                            let mut state = connection.borrow_mut();
                            match setup {
                                Ok(()) => state.installed = true,
                                Err(error) => state.error = Some(error),
                            }
                        }
                    }
                    Err(error) => {
                        state.admission = None;
                        state.receipts.clear();
                        state.error = Some(error);
                    }
                }
            }
            PeerEvent::RemoteActionUndeliverable {
                workspace_id, to, ..
            } if workspace_id == state.workspace && to == state.owner.device_public_key => {
                state.admission = None;
                state.pending.clear();
                state.error = Some("feedback owner unavailable".into());
            }
            _ => {}
        }
    }
}
/// Ask the owner again before the admission lapses, once at a time, while
/// the page holds one.
fn renew_admission(connection: &mut Connection) {
    let aging = connection
        .admission
        .as_ref()
        .is_some_and(|(_, received)| received.elapsed() >= ADMISSION_RENEW);
    let asking = connection
        .pending
        .values()
        .any(|(sent, submission)| submission.is_none() && sent.elapsed() < ASK_TIMEOUT);
    if aging && !asking {
        // A refusal to send shows on the next pump as the owner's answer
        // or as undeliverable; the admission held stays until then.
        let _ = connection.request(None);
    }
}

/// Keeps a report's masked picture in the page's content store, as a file
/// posted in Chat is kept: the room's host asks the room for the pictures of
/// the reports filed in its Chat and keeps its own copy, which its receipt
/// waits for. The page captures nothing itself; the picker's masked crops
/// are the only picture a report carries.
struct PageContent;
impl Capture for PageContent {
    fn offers_screenshots(&self) -> bool {
        NoCapture.offers_screenshots()
    }
    fn context(&mut self) -> Result<Vec<ReviewText>, Error> {
        NoCapture.context()
    }
    fn capture(&mut self) -> Result<MaskedScreenshot, Error> {
        NoCapture.capture()
    }
    fn persist(&mut self, image: &MaskedScreenshot) -> Result<(), Error> {
        keep_picture(&crate::page_content::page_content(), image)
    }
}

/// Keep `image` in `content`, refusing a store that keeps other bytes.
fn keep_picture(
    content: &tilo_content::ContentStore,
    image: &MaskedScreenshot,
) -> Result<(), Error> {
    let hash = content
        .put(image.png())
        .map_err(|_| Error::Invalid("the page has no room for the picture"))?;
    if hash != image.reference().hash || content.read(&hash).as_deref() != Some(image.png()) {
        return Err(Error::NotDurable);
    }
    Ok(())
}

fn identity(cert: &DeviceCert) -> Identity {
    Identity {
        user: cert.user_public_key,
        device: cert.device_public_key,
    }
}
struct Session {
    connection: Rc<RefCell<Connection>>,
}
impl Transport for Session {
    fn enrollment(&mut self) -> Result<Enrollment, Error> {
        let mut connection = self.connection.borrow_mut();
        let current = connection.current();
        if !connection
            .pending
            .values()
            .any(|(_, pending)| pending.is_none())
        {
            let _ = connection.request(None);
        }
        current
    }
    fn enqueue(&mut self, submission: &Submission) -> Result<(), Error> {
        let mut connection = self.connection.borrow_mut();
        let admission = connection.current()?;
        if submission.destination != admission.destination {
            return Err(Error::ReceiptMismatch);
        }
        connection
            .file(submission)
            .map_err(|_| Error::Invalid("feedback owner unavailable"))
    }
    fn receipt(&mut self, submission: &Submission) -> Result<Option<SignedReceipt>, Error> {
        let mut connection = self.connection.borrow_mut();
        connection.current()?;
        if let Some(receipt) = connection.receipts.remove(&submission.id) {
            return Ok(Some(receipt));
        }
        if !connection
            .pending
            .values()
            .filter(|(sent, _)| sent.elapsed() < Duration::from_secs(30))
            .filter_map(|(_, pending)| pending.as_ref())
            .any(|pending| pending.id == submission.id)
        {
            connection
                .request(Some(submission.clone()))
                .map_err(|_| Error::NotDurable)?;
        }
        Ok(None)
    }
}

/// Trusted Rust startup handoff, separate from deserialized mount configuration.
/// The certified peer must already exist. This never creates a peer or joins.
pub(crate) fn register_feedback_startup(peer: u32, startup: Startup) -> Result<(), String> {
    let device = startup
        .reporter()
        .device_public_key()
        .ok_or("feedback reporter has no certified device")?;
    if crate::replication::device_of(peer).as_deref() != Some(hex::encode(device).as_str()) {
        return Err("feedback reporter does not own this certified peer".into());
    }
    STARTUPS.with_borrow_mut(|startups| -> Result<(), String> {
        if startups.contains_key(&peer) {
            return Err("feedback startup already registered".into());
        }
        startups.insert(peer, startup);
        Ok(())
    })?;
    // A desktop that mounted first takes it up on its next frame.
    if let Some(wake) = WAKES.with_borrow_mut(|wakes| wakes.remove(&peer)) {
        wake();
    }
    Ok(())
}

/// Ask the owner of `peer`'s destination for a fresh admission, which
/// registers a new startup; whether it was asked.
pub(crate) fn readmit(peer: u32) -> bool {
    let Some(connection) = CONNECTIONS.with_borrow(|connections| connections.get(&peer).cloned())
    else {
        return false;
    };
    let Ok(mut state) = connection.try_borrow_mut() else {
        return false;
    };
    state.installed = false;
    state.error = None;
    state.request(None).is_ok()
}

/// Call `wake` once a startup is registered for `peer`.
pub(crate) fn wake_on_startup(peer: u32, wake: std::sync::Arc<dyn Fn() + Send + Sync>) {
    WAKES.with_borrow_mut(|wakes| wakes.insert(peer, wake));
}

pub(crate) fn forget_feedback_startup(peer: u32) {
    STARTUPS.with_borrow_mut(|startups| {
        startups.remove(&peer);
    });
    WAKES.with_borrow_mut(|wakes| {
        wakes.remove(&peer);
    });
}

/// The startup registered for `peer`, for the desktop that mounts with it.
pub(crate) fn take_startup(peer: u32) -> Option<Startup> {
    STARTUPS.with_borrow_mut(|startups| startups.remove(&peer))
}
