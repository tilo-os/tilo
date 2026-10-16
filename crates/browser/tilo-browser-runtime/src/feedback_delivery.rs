//! Feedback custody produced by an existing certified peer. Configuration picks
//! an existing destination; only its authenticated owner supplies admission.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use tilo_actions::remote::feedback::{FeedbackAnswer, FeedbackDestination, FeedbackQuery};
use tilo_actions::remote::{RemoteActionFrame, RemoteActionMessage};
use tilo_app::feedback::{Error, Submission};
use tilo_browser_peer::{PeerEvent, PeerEvents, PeerHandle};
use tilo_desktop::feedback::Destination;
use tilo_desktop::feedback::transport::{
    Enrollment, Identity, NoCapture, SignedReceipt, Startup, Transport,
};
use tilo_user_identity::DeviceCert;

thread_local! { static CONNECTIONS: RefCell<HashMap<u32, Rc<RefCell<Connection>>>> = RefCell::new(HashMap::new()); }
struct Connection {
    peer: PeerHandle,
    events: PeerEvents,
    reporter: DeviceCert,
    owner: DeviceCert,
    destination: FeedbackDestination,
    workspace: u64,
    serial: u64,
    pending: HashMap<u64, (web_time::Instant, Option<Submission>)>,
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
            .retain(|_, (sent, _)| sent.elapsed() < Duration::from_secs(30));
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
            .filter(|(_, received)| received.elapsed() < Duration::from_secs(5))
            .map(|(admission, _)| admission.clone())
            .ok_or(Error::Invalid(
                "fresh authenticated feedback admission required",
            ))
    }
}
pub(crate) fn pump(id: u32) {
    let connection = CONNECTIONS.with_borrow(|connections| connections.get(&id).cloned());
    let Some(connection) = connection else {
        return;
    };
    let events = connection.borrow().events.drain();
    for event in events {
        let mut state = connection.borrow_mut();
        match event {
            PeerEvent::RemoteAction {
                workspace_id,
                from,
                frame,
            } if workspace_id == state.workspace && from == state.owner.device_public_key => {
                let Ok(RemoteActionFrame {
                    message:
                        RemoteActionMessage::FeedbackResponse {
                            correlation,
                            result,
                        },
                    ..
                }) = RemoteActionFrame::decode(&frame)
                else {
                    continue;
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
                                    id,
                                    connection: connection.clone(),
                                }),
                                Box::new(NoCapture),
                            )
                            .map_err(|error| error.to_string())
                            .and_then(|startup| {
                                crate::browser::register_feedback_startup(id, startup)
                            });
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
fn identity(cert: &DeviceCert) -> Identity {
    Identity {
        user: cert.user_public_key,
        device: cert.device_public_key,
    }
}
struct Session {
    id: u32,
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
        let admission = self.connection.borrow().current()?;
        if submission.destination != admission.destination {
            return Err(Error::ReceiptMismatch);
        }
        let id = self.id;
        let submission = submission.clone();
        let connection = self.connection.clone();
        // Run after the composer's effect, through the registered local door.
        wasm_bindgen_futures::spawn_local(async move {
            // The report goes in through Chat's own action, whose code a
            // bundle that splits its apps fetches only when first needed.
            ui::application::registry::load("so.tilo.chat").await;
            match crate::replication::submit_feedback(id, &admission, &submission) {
                Ok(()) => {
                    let _ = connection.borrow_mut().request(Some(submission));
                }
                Err(error) => connection.borrow_mut().error = Some(error),
            }
        });
        Ok(())
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
