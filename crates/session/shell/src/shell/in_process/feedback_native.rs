//! Native feedback delivery on the existing certified shell network and SDK broker.
//! Configuration selects a destination only; current enrolled custody supplies authority.
use super::{
    HashMap, HostContext, HostContextInputs, InProcessApps, InstanceViewStore, LaunchError,
    LaunchTarget, Rc, RecordCheckout, RefCell, RestoreInput,
};
use crate::shell::{network::ShellNetwork, remote_action_link::RemoteActionLink};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tilo_actions::remote::feedback::{FeedbackAnswer, FeedbackDestination, FeedbackQuery};
use tilo_actions::remote::{Pending, PendingPoll, RemoteActionMessage};
use tilo_app::feedback::{Error, Submission};
use tilo_desktop::feedback::transport::{
    Enrollment, Identity, NoCapture, SignedReceipt, Startup, Transport,
};
use tilo_user_identity::DeviceCert;

/// The host owns a model service, not its app's rendering or teardown details.
pub(super) trait Producer {
    fn pump(&mut self, apps: &mut InProcessApps) -> Result<(), LaunchError>;
}

pub(super) struct Delivery {
    connection: Rc<RefCell<Connection>>,
    publisher: Option<ui::App>,
    installed: bool,
}
struct Connection {
    network: Arc<ShellNetwork>,
    destination: FeedbackDestination,
    reporter: DeviceCert,
    owner: DeviceCert,
    serial: u64,
    pending: Vec<(u64, Option<Submission>, Pending<RemoteActionMessage>)>,
    admission: Option<(Enrollment, Instant)>,
    queued: Option<Submission>,
    receipts: HashMap<String, SignedReceipt>,
}
impl Connection {
    fn custody(&self) -> Result<(), Error> {
        let room = crate::shell::state::RoomId::parse(&self.destination.room)
            .ok_or(Error::ReceiptMismatch)?;
        let (reporter, owner) = self
            .network
            .feedback_custody(room)
            .map_err(|_| Error::Invalid("native feedback custody unavailable"))?;
        if reporter != self.reporter || owner != self.owner {
            return Err(Error::ReceiptMismatch);
        }
        Ok(())
    }
    fn current(&self) -> Result<Enrollment, Error> {
        self.custody()?;
        let admission = self
            .admission
            .as_ref()
            .filter(|(_, received)| received.elapsed() < Duration::from_secs(5))
            .map(|(admission, _)| admission.clone())
            .ok_or(Error::Invalid("fresh native feedback admission required"))?;
        let room = crate::shell::state::RoomId::parse(&self.destination.room)
            .ok_or(Error::ReceiptMismatch)?;
        self.network
            .native_owner()
            .map_err(|_| Error::NotDurable)?
            .with_current(room, |view| {
                let standing = view
                    .peer()
                    .permissions(
                        self.reporter.device_public_key,
                        std::time::SystemTime::now(),
                    )?
                    .standing();
                // Both halves of the admission must still hold: the reporter's
                // standing (what they may write) and the room's sandbox (what
                // the feedback window's code may touch, its capture and files).
                let sandbox = view.peer().governance().governed_sandbox()?;
                if admission.permissions.standing() != &standing
                    || admission.permissions.capabilities() != sandbox.as_slice()
                {
                    return Err("native feedback policy changed".into());
                }
                Ok(())
            })
            .map_err(|_| Error::Invalid("native feedback policy changed"))?;
        Ok(admission)
    }
    fn ask(&mut self, submission: Option<Submission>) -> Result<(), Error> {
        self.custody()?;
        if self.pending.len() >= 8 {
            return Err(Error::Full);
        }
        self.serial = self.serial.checked_add(1).ok_or(Error::Full)?;
        let workspace = tilo_relay_protocol::room_workspace_id(&self.destination.room)
            .map_err(|_| Error::ReceiptMismatch)?;
        let answer = self
            .network
            .request(
                workspace,
                iroh::EndpointId::from_bytes(&self.owner.device_public_key)
                    .map_err(|_| Error::ReceiptMismatch)?,
                RemoteActionMessage::FeedbackRequest {
                    correlation: self.serial,
                    query: FeedbackQuery {
                        destination: self.destination.clone(),
                        submission: submission.as_ref().map(serde_json::to_value).transpose()?,
                        review: None,
                    },
                },
            )
            .map_err(|_| Error::NotDurable)?;
        self.pending.push((self.serial, submission, answer));
        Ok(())
    }
    fn admit(&self, answer: &FeedbackAnswer) -> Result<Enrollment, Error> {
        self.custody()?;
        let identity = |cert: &DeviceCert| Identity {
            user: cert.user_public_key,
            device: cert.device_public_key,
        };
        Enrollment::admitted(
            answer,
            &self.destination,
            &identity(&self.owner),
            &identity(&self.reporter),
        )
    }
    fn poll(&mut self) {
        let mut waiting = Vec::new();
        for (correlation, submission, mut reply) in std::mem::take(&mut self.pending) {
            match reply.poll_ready() {
                PendingPoll::Waiting => waiting.push((correlation, submission, reply)),
                PendingPoll::Ready(RemoteActionMessage::FeedbackResponse {
                    correlation: received,
                    result,
                }) if received == correlation => {
                    if let Some((answer, admission)) = result.ok().and_then(|answer| {
                        self.admit(&answer)
                            .ok()
                            .map(|admission| (answer, admission))
                    }) {
                        if let Some(submission) = submission
                            && let Some(receipt) = admission.receipt_in(&answer, &submission)
                        {
                            if self.receipts.len() >= 8 {
                                self.receipts.clear();
                            }
                            self.receipts.insert(submission.id, receipt);
                        }
                        self.admission = Some((admission, Instant::now()));
                    } else {
                        self.admission = None;
                        self.receipts.clear();
                    }
                }
                _ => {
                    self.admission = None;
                    self.receipts.clear();
                }
            }
        }
        self.pending = waiting;
    }
}
impl Delivery {
    pub(super) fn new(
        network: Arc<ShellNetwork>,
        destination: tilo_desktop::feedback::Destination,
        author: &str,
    ) -> Result<Self, LaunchError> {
        destination
            .validate()
            .map_err(|error| LaunchError::Operation(error.to_string()))?;
        let room = crate::shell::state::RoomId::parse(&destination.room)
            .ok_or(LaunchError::BindingMismatch)?;
        let (reporter, owner) = network
            .feedback_custody(room)
            .map_err(LaunchError::Operation)?;
        if hex::encode(reporter.user_public_key) != author {
            return Err(LaunchError::BindingMismatch);
        }
        let mut connection = Connection {
            network,
            destination: FeedbackDestination {
                room: destination.room,
                provider: destination.provider,
                channel: destination.channel,
            },
            reporter,
            owner,
            serial: 0,
            pending: Vec::new(),
            admission: None,
            queued: None,
            receipts: HashMap::new(),
        };
        connection
            .ask(None)
            .map_err(|error| LaunchError::Operation(error.to_string()))?;
        Ok(Self {
            connection: Rc::new(RefCell::new(connection)),
            publisher: None,
            installed: false,
        })
    }
    pub(super) fn pump(&mut self, apps: &mut InProcessApps) -> Result<(), LaunchError> {
        self.connection.borrow_mut().poll();
        {
            let mut connection = self.connection.borrow_mut();
            let refresh = connection
                .admission
                .as_ref()
                .is_none_or(|(_, received)| received.elapsed() >= Duration::from_secs(2));
            if refresh
                && !connection
                    .pending
                    .iter()
                    .any(|(_, submission, _)| submission.is_none())
            {
                let _ = connection.ask(None);
            }
        }
        let admission = self
            .connection
            .borrow()
            .current()
            .map_err(|error| LaunchError::Operation(error.to_string()))?;
        if !self.installed {
            let startup = Startup::new(
                admission.clone(),
                Box::new(Session(self.connection.clone())),
                Box::new(NoCapture),
            )
            .map_err(|error| LaunchError::Operation(error.to_string()))?;
            apps.install_feedback_startup(&admission.reporter, startup)?;
            self.installed = true;
        }
        if self.publisher.is_none() {
            self.publisher = Some(publisher(apps, &admission)?);
        }
        let publisher = self
            .publisher
            .as_mut()
            .ok_or(LaunchError::BindingMismatch)?;
        // The original App's model pass applies committed reads and publishes
        // registered local edits through its broker, without a frame or pixels.
        publisher.pump_hosted_model();
        if !publisher.collaboration_is_connected() {
            return Ok(());
        }
        let submission = self.connection.borrow_mut().queued.take();
        if let Some(submission) = submission {
            let result = publisher.invoke_local_resource_action(
                &tilo_actions::ActionAddress::new(
                    tilo_app::feedback::destination::item(
                        tilo_app::feedback::destination::CHANNELS,
                        &admission.destination.channel,
                    ),
                    tilo_app::feedback::destination::SUBMIT,
                ),
                serde_json::json!({"submission":submission})
                    .as_object()
                    .ok_or(LaunchError::BindingMismatch)?,
            );
            result.map_err(|error| LaunchError::Operation(error.to_string()))?;
            self.connection
                .borrow_mut()
                .ask(Some(submission))
                .map_err(|error| LaunchError::Operation(error.to_string()))?;
        }
        Ok(())
    }
}
fn publisher(apps: &mut InProcessApps, admission: &Enrollment) -> Result<ui::App, LaunchError> {
    let definition = chat::definition();
    let connector = apps.broker.as_ref().ok_or(LaunchError::BindingMismatch)?(
        definition.app_id(),
        &admission.destination.room,
    );
    let collaboration = ui::collaboration::CollaborationManager::for_route_host_in_background(
        connector,
        ui::collaboration::RouteSpec {
            app_id: definition.app_id().into(),
            cluster_id: admission
                .destination
                .room
                .parse()
                .map_err(|_| LaunchError::BindingMismatch)?,
            record: None,
        },
        Some(definition.record_kind.as_str().into()),
    );
    publisher_on(apps, admission, collaboration)
}
fn publisher_on(
    apps: &mut InProcessApps,
    admission: &Enrollment,
    collaboration: ui::collaboration::CollaborationManager,
) -> Result<ui::App, LaunchError> {
    let definition = chat::definition();
    let app = ui::App::from_prepared_component_with_collaboration(
        collaboration,
        || {
            // Open inside the original SDK read bridge: cold cache absence never
            // creates a new Chat record or treats local cached bytes as admission.
            let records = apps
                .owners
                .open(definition.app_id(), &admission.destination.room)
                .map_err(LaunchError::Record)?;
            let checkout = RecordCheckout::package_primary(definition.record_kind.clone());
            records
                .try_open_record(&checkout)
                .map_err(LaunchError::Record)?;
            let target = LaunchTarget::Record {
                checkout,
                reveal: None,
            };
            let views = Rc::new(
                InstanceViewStore::with_store(
                    apps.store.clone(),
                    &apps.device_id,
                    &admission.destination.room,
                )
                .map_err(LaunchError::Restore)?
                .without_persistence(),
            );
            let context = HostContext::prepare(
                definition,
                HostContextInputs {
                    records,
                    views,
                    device_id: apps.device_id.clone(),
                    instance_id: 1,
                    permissions: Rc::new(RefCell::new(admission.permissions.clone())),
                    author: apps.author.clone(),
                    action_context: tilo_actions::ActionContext::default(),
                },
                &target,
            )?;
            Ok(ui::application::PreparedComponentInputs {
                context,
                target,
                restore: RestoreInput::None,
                runner_inputs: ui::application::ComponentRunnerInputs::from_native_process(),
            })
        },
        |context, target, restore| definition.instantiate(context, target, restore),
    )?;
    Ok(app)
}

struct Session(Rc<RefCell<Connection>>);
impl Transport for Session {
    fn enrollment(&mut self) -> Result<Enrollment, Error> {
        let mut connection = self.0.borrow_mut();
        let current = connection.current();
        if !connection
            .pending
            .iter()
            .any(|(_, submission, _)| submission.is_none())
        {
            let _ = connection.ask(None);
        }
        current
    }
    fn enqueue(&mut self, submission: &Submission) -> Result<(), Error> {
        let mut connection = self.0.borrow_mut();
        let admission = connection.current()?;
        if submission.destination != admission.destination {
            return Err(Error::ReceiptMismatch);
        }
        if connection.queued.is_some() {
            return Err(Error::Full);
        }
        connection.queued = Some(submission.clone());
        drop(connection);
        Ok(())
    }
    fn receipt(&mut self, submission: &Submission) -> Result<Option<SignedReceipt>, Error> {
        let mut connection = self.0.borrow_mut();
        connection.current()?;
        if let Some(receipt) = connection.receipts.remove(&submission.id) {
            return Ok(Some(receipt));
        }
        if !connection
            .pending
            .iter()
            .filter_map(|(_, submission, _)| submission.as_ref())
            .any(|pending| pending.id == submission.id)
        {
            connection.ask(Some(submission.clone()))?;
        }
        drop(connection);
        Ok(None)
    }
}

/// Optional private-profile configuration chooses no authority and creates no file.
pub(super) fn configured_destination(
    profile: &std::path::Path,
) -> Result<Option<tilo_desktop::feedback::Destination>, LaunchError> {
    use std::io::Read;
    let path = profile.join("feedback-destination.json");
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(LaunchError::Operation(error.to_string())),
    };
    let mut bytes = Vec::new();
    file.take(2049)
        .read_to_end(&mut bytes)
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    if bytes.len() > 2048 {
        return Err(LaunchError::BindingMismatch);
    }
    let wire: FeedbackDestination =
        serde_json::from_slice(&bytes).map_err(|_| LaunchError::BindingMismatch)?;
    let destination = tilo_desktop::feedback::Destination {
        room: wire.room,
        provider: wire.provider,
        channel: wire.channel,
    };
    destination
        .validate()
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    Ok(Some(destination))
}

#[cfg(test)]
#[path = "feedback_native_tests.rs"]
mod tests;

impl Drop for Delivery {
    fn drop(&mut self) {
        if let Some(publisher) = self.publisher.as_mut() {
            publisher.close_components();
        }
    }
}

impl Producer for Delivery {
    fn pump(&mut self, apps: &mut InProcessApps) -> Result<(), LaunchError> {
        Self::pump(self, apps)
    }
}
