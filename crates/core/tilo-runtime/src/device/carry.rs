//! A connection of this computer's person carried to another of their devices: a program that
//! drives a device (`tilo-vm`) names it on its first line ([`Carry`]), and the action frames it
//! sends after are asked of that device by this daemon, as this device, over the one endpoint it
//! keeps under the device key. The program never reads the key and never dials as the device: two
//! processes signing as one device keep two counts, and two endpoints under one key push each other
//! off the network.
//!
//! What is carried is the whole action wire: invocations and their following, cancels, and content
//! puts into the other device's store. The other device decides each request with the grants its
//! workspace gives this device, as it decides any device's.

use std::io::{BufRead, Write as _};
use std::sync::{Arc, Weak};
use std::time::Duration;

use iroh::{EndpointAddr, EndpointId};
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    self, ActionCallError, ActionProvider, Caller, Carry, CarryAnswer, CataloguePage,
    CatalogueQuery, ContentPut, ContentPutAnswer, ContentPutStep, ContentPuts, InvocationReceipt,
    InvocationState, InvokeRequest, Pending, RemoteActionFrame, RemoteActionTarget,
};
use tilo_peer_state::actions::{REPLY_DEADLINE, RemoteDevice};
use tilo_socket::Stream;

use super::services::Daemon;

/// How long past the longest remote wait a carried frame's answer is waited for.
const SLACK: Duration = Duration::from_secs(5);

/// Who may have a connection carried, and why one that may not is refused.
#[derive(Clone)]
pub(crate) enum Carrying {
    /// The person's own connection, carried by their daemon.
    Person(Weak<Daemon>),
    /// Any other connection, refused in these words.
    Refused(&'static str),
}

/// The words a connection that is not the person's own is refused in: an app, an agent, a key
/// of another person signed in on the computer.
pub(crate) const NOT_THE_PERSON: &str = "only this computer's person carries a connection to \
     another of their devices; an app, an agent or another person's key acts on this computer alone";

/// The words a connection the device's own actions answer is refused in: one that comes before
/// the person's services run, or one the device answers alone.
pub(crate) const DEVICE_ONLY: &str = "this connection reaches this computer's own device actions \
     alone, so it is carried to no other device: once Tilo runs its person's part (`tilo status` \
     says how it stands), connect again as the person";

impl Carrying {
    /// The person's own connections, carried by `daemon`.
    pub(crate) fn of(daemon: &Arc<Daemon>) -> Self {
        Self::Person(Arc::downgrade(daemon))
    }

    /// Carry `stream`, which asked for `carry` on its first line, reading its frames from
    /// `input` until it ends; or tell it why not, and close.
    pub(crate) fn serve(&self, mut stream: Stream, carry: &Carry, input: impl BufRead) {
        let daemon = match self {
            Self::Person(daemon) => daemon.upgrade(),
            Self::Refused(why) => return refuse(stream, why),
        };
        let Some(daemon) = daemon else {
            return refuse(stream, "this computer's daemon is stopping");
        };
        // The device's actions run their steps on the process's runtime.
        let _runtime = tilo_rt::app().handle().enter();
        let device = match reach(&daemon, carry) {
            Ok(device) => device,
            Err(why) => return refuse(stream, &why),
        };
        let workspaces = daemon.serving();
        let cert = workspaces.device_cert();
        let caller = Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(&carry.workspace);
        drop(daemon);
        if writeln!(stream, "{}", remote::carry_answer_line(&CarryAnswer::Open)).is_err() {
            return;
        }
        let carried = Carried(device, None);
        let workspace = carry.workspace.clone();
        let uploads = remote::HostedContentPuts::default();
        let tree = move |frame: &RemoteActionFrame| {
            if frame.workspace().is_some_and(|named| named != workspace) {
                return Err(ActionCallError::Refused {
                    reason: format!(
                        "this connection is carried to its device in workspace {workspace} alone"
                    ),
                });
            }
            Ok((carried.clone(), caller.clone()))
        };
        remote::serve_lines(tree, &uploads, None, input, stream, SLACK);
    }
}

/// The device `carry` names, asked from `daemon`'s endpoint as its device.
fn reach(daemon: &Daemon, carry: &Carry) -> Result<RemoteDevice, String> {
    let key: [u8; 32] = hex::decode(&carry.device)
        .ok()
        .and_then(|key| key.try_into().ok())
        .ok_or_else(|| format!("{} is not a device key", carry.device))?;
    let workspaces = daemon.serving();
    if key == workspaces.device_cert().device_public_key {
        return Err(
            "that is this computer's own device: ask it here, without carrying the connection"
                .into(),
        );
    }
    let mut addr = EndpointAddr::new(
        EndpointId::from_bytes(&key)
            .map_err(|_| format!("{} is not a device key", carry.device))?,
    );
    for address in &carry.addresses {
        let socket = address
            .parse()
            .map_err(|_| format!("{address} is not a socket address"))?;
        addr = addr.with_ip_addr(socket);
    }
    if let Some(relay) = &carry.relay {
        addr = addr.with_relay_url(
            relay
                .parse()
                .map_err(|_| format!("{relay} is not a relay's address"))?,
        );
    }
    Ok(workspaces
        .copies()
        .action_service()
        .remote(&workspaces.endpoint(), &addr, &carry.workspace))
}

/// Tell `stream` why it is not carried, and close.
fn refuse(mut stream: Stream, why: &str) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let answer = CarryAnswer::Refused {
        reason: why.to_owned(),
    };
    let _ = writeln!(stream, "{}", remote::carry_answer_line(&answer));
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// The other device, with its content store reached through the same frames: the one the action
/// it names keeps an upload in, when it names one.
#[derive(Clone)]
struct Carried(RemoteDevice, Option<RemoteActionTarget>);

impl ActionProvider for Carried {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        self.0.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.0.invoke(caller, request)
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.0.observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.0.next_change(caller, id)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        self.0.cancel(caller, id)
    }

    fn content_puts(&self) -> Option<Arc<dyn ContentPuts>> {
        Some(Arc::new(self.clone()))
    }

    fn content_puts_for(
        &self,
        target: Option<&RemoteActionTarget>,
    ) -> Option<Arc<dyn ContentPuts>> {
        Some(Arc::new(Self(self.0.clone(), target.cloned())))
    }
}

impl Carried {
    /// The other device's answer to `step`.
    fn step(&self, step: ContentPutStep) -> Result<ContentPutAnswer, ActionCallError> {
        match self.0.content_put(step).wait_timeout(REPLY_DEADLINE) {
            Ok(Some(answer)) => answer,
            Ok(None) | Err(_) => Err(ActionCallError::Unanswered),
        }
    }
}

/// What the other device answered a step where it should have answered something else.
fn unexpected(answer: &ContentPutAnswer) -> ActionCallError {
    ActionCallError::Unavailable {
        reason: format!("the device answered an upload step with {answer:?}"),
    }
}

impl ContentPuts for Carried {
    fn begin(&self, _caller: &Caller, size: u64) -> Result<Box<dyn ContentPut>, ActionCallError> {
        match self.step(ContentPutStep::Begin {
            size,
            target: self.1.clone(),
        })? {
            ContentPutAnswer::Begun { upload } => Ok(Box::new(CarriedPut {
                device: self.clone(),
                upload,
                size,
                written: 0,
            })),
            other => Err(unexpected(&other)),
        }
    }

    fn have(&self, _caller: &Caller, hashes: &[String]) -> Result<Vec<bool>, ActionCallError> {
        match self.step(ContentPutStep::Have {
            hashes: hashes.to_vec(),
        })? {
            ContentPutAnswer::Have { present } if present.len() == hashes.len() => Ok(present),
            other => Err(unexpected(&other)),
        }
    }
}

/// One upload the other device keeps open.
struct CarriedPut {
    device: Carried,
    upload: u64,
    size: u64,
    written: u64,
}

impl ContentPut for CarriedPut {
    fn size(&self) -> u64 {
        self.size
    }

    fn written(&self) -> u64 {
        self.written
    }

    fn append(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionCallError> {
        match self
            .device
            .step(ContentPutStep::append(self.upload, offset, chunk))?
        {
            ContentPutAnswer::Appended { written } => {
                self.written = written;
                Ok(written)
            }
            other => Err(unexpected(&other)),
        }
    }

    fn finish(self: Box<Self>) -> Result<String, ActionCallError> {
        match self.device.step(ContentPutStep::Finish {
            upload: self.upload,
        })? {
            ContentPutAnswer::Finished { content } => Ok(content),
            other => Err(unexpected(&other)),
        }
    }
}
