//! Actions between the devices of a workspace, over an iroh connection with
//! no host, store or relay in between.
//!
//! A request is one signed [`SignedDeviceAction`] on a connection of its
//! own, ALPN [`ACTIONS_ALPN`], and its answer is the [`RemoteActionMessage`]
//! the device's one dispatcher
//! ([`ActionHost`](tilo_actions::remote::ActionHost): the broker, or a
//! runtime's action host) gives, on the same bi-stream. The stream is the
//! correlation: a request in, one answer out, then the stream finishes, so
//! nothing has to be matched up afterwards and a lost answer is a lost
//! request, never another caller's.
//!
//! This serves a device, not a replica: one request names its workspace, and
//! own-device actions span the person. The device answering checks, in this
//! order, that
//!
//! 1. the connection id is the action's `source`, so a device may only ask
//!    as itself;
//! 2. the signature is the source's ([`SignedDeviceAction::verify`]);
//! 3. the addressed device is this one;
//! 4. the request is fresh and not one already admitted
//!    ([`ActionReplayWindow`]);
//! 5. this device holds the workspace it names;
//! 6. the source is a device of that workspace's roster: certified,
//!    unrevoked, unexpired, its person a member, its delegation live
//!    ([`StatePeer::permissions`]).
//!
//! What is left is the [`Caller`] the local callers of the same dispatcher
//! get, and the dispatcher's own policy
//! ([`tilo_action_policy`](tilo_actions::remote)) is the only policy check.
//! Nothing is queued: a device that cannot be reached answers
//! [`ActionHostError::Unavailable`] at once, and the caller decides what to do
//! about it. Nothing is remembered here either: the dispatcher's durable
//! journal is the one ledger, keyed by invocation, so a retried request
//! reports the original invocation.

use crate::direct::Replica;
use ed25519_dalek::SigningKey;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr};
use neighbour_wire::actions::{ActionReplayWindow, DeviceAction, SignedDeviceAction};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tilo_actions::remote::{
    ActionHost, Caller, InvocationReceipt, InvocationState, ReceiptStatus, RemoteActionFrame,
    RemoteActionMessage, UpdateMark,
};
use tilo_lock::MutexRecover as _;
use web_time::SystemTime;

/// ALPN of the device-to-device action plane.
pub const ACTIONS_ALPN: &[u8] = b"tilo-actions/1";

/// The largest frame either side writes.
const MAX_FRAME_BYTES: usize = neighbour_wire::actions::MAX_ACTION_BYTES + 8 * 1024;

/// How long a device is given to answer a request it was asked directly. A
/// device that is not reachable at all fails faster than this; one that is
/// reachable and does not answer is given up on, so the caller learns.
pub const REPLY_DEADLINE: Duration = Duration::from_secs(30);

/// How long one long-poll asks the answering device to hold its answer;
/// well inside [`REPLY_DEADLINE`], so a held answer is never taken for a
/// lost one.
pub const CHANGE_WAIT_MS: u32 = 10_000;

/// How long an invocation is followed on another device before it is given
/// up as unanswered.
pub const FOLLOW_DEADLINE: Duration = Duration::from_secs(120);

/// How many changing requests one person may send per window, and how many
/// at once. Reading (a catalogue, an observation) spends none: a caller that
/// is following work must keep being able to look at it.
#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    /// Changes refilled per window.
    pub changes_per_window: u32,
    /// The window, in seconds.
    pub window_secs: u32,
    /// Changes a person may send at once.
    pub burst: u32,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            changes_per_window: 20,
            window_secs: 10,
            burst: 40,
        }
    }
}

/// Buckets kept before idle, full ones are forgotten.
const KEPT_CALLERS: usize = 4096;

/// Most invocations one device follows at a time.
const MAX_FOLLOWED: usize = 1024;

/// A device's action plane: it serves the requests other devices of its
/// workspaces make, and asks them through [`Self::remote`].
#[derive(Clone)]
pub struct ActionPlane(Arc<Inner>);

struct Inner {
    /// The device whose signature every request it makes carries.
    device: SigningKey,
    /// The workspaces this device holds, by room id.
    replicas: Mutex<BTreeMap<String, Replica>>,
    /// The one place a request is handed to, wherever it came from.
    dispatcher: Arc<dyn ActionHost + Send + Sync>,
    replay: Mutex<ActionReplayWindow>,
    bounds: Bounds,
    callers: Mutex<HashMap<[u8; 32], Bucket>>,
    runtime: tokio::runtime::Handle,
}

impl std::fmt::Debug for ActionPlane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionPlane")
            .field(
                "device",
                &hex::encode(self.0.device.verifying_key().as_bytes()),
            )
            .field("workspaces", &self.0.replicas.lock_recover().len())
            .finish_non_exhaustive()
    }
}

impl ActionPlane {
    /// A plane for the device whose signing key is `device` (and whose iroh
    /// endpoint's secret key it is), handing every request it answers to
    /// `dispatcher`.
    ///
    /// Call it where a runtime runs: answers are followed on that runtime,
    /// so a caller never blocks on one.
    ///
    /// # Panics
    ///
    /// Outside a runtime context, where there is nothing to answer on.
    #[must_use]
    pub fn new(device: SigningKey, dispatcher: Arc<dyn ActionHost + Send + Sync>) -> Self {
        Self::with_bounds(device, dispatcher, Bounds::default())
    }

    /// [`Self::new`] with per-person bounds of the caller's choosing.
    #[must_use]
    pub fn with_bounds(
        device: SigningKey,
        dispatcher: Arc<dyn ActionHost + Send + Sync>,
        bounds: Bounds,
    ) -> Self {
        Self(Arc::new(Inner {
            device,
            replicas: Mutex::default(),
            dispatcher,
            replay: Mutex::default(),
            bounds,
            callers: Mutex::default(),
            runtime: tokio::runtime::Handle::current(),
        }))
    }

    /// This device's public key, as the other devices see it.
    #[must_use]
    pub fn device(&self) -> [u8; 32] {
        self.0.device.verifying_key().to_bytes()
    }

    /// Answer requests for `replica`'s workspace.
    pub fn hold(&self, replica: Replica) {
        self.0
            .replicas
            .lock_recover()
            .insert(replica.room(), replica);
    }

    /// Answer requests for the workspace `sync` syncs.
    pub fn share(&self, sync: &crate::direct::DirectSync) {
        self.hold(sync.replica());
    }

    /// A host for another device's actions in `workspace`, asked as this
    /// device: every callers' [`ActionHost`] methods work through it as
    /// through a local host.
    #[must_use]
    pub fn remote(&self, endpoint: Endpoint, addr: EndpointAddr, workspace: &str) -> RemoteDevice {
        let target = *addr.id.as_bytes();
        RemoteDevice(Arc::new(Device {
            endpoint,
            addr,
            workspace: workspace.to_owned(),
            target,
            key: self.0.device.clone(),
            runtime: self.0.runtime.clone(),
            connection: Mutex::new(None),
            next_correlation: AtomicU64::new(1),
            follow: Following::default(),
        }))
    }

    /// Answer one request: what the device's dispatcher made of it.
    async fn answer(
        &self,
        remote: [u8; 32],
        signed: SignedDeviceAction,
    ) -> Option<RemoteActionMessage> {
        let action = signed.action();
        let request = &action.message;
        match self.authorize(remote, &signed) {
            Ok((caller, _workspace)) => self.dispatch(&caller, request).await,
            Err(error) => refusal(request, error),
        }
    }

    /// The caller a verified request acts as, or why it is refused.
    fn authorize(
        &self,
        remote: [u8; 32],
        signed: &SignedDeviceAction,
    ) -> Result<(Caller, String), tilo_actions::remote::ActionHostError> {
        use tilo_actions::remote::ActionHostError;
        let action = signed.action();
        let source = action.source;
        // A device asks as itself and nobody else: the connection already
        // says which device is there.
        if remote != source {
            return Err(ActionHostError::NotPermitted);
        }
        if signed.verify(&source).is_err() {
            return Err(ActionHostError::NotPermitted);
        }
        if action.target != self.device() {
            return Err(ActionHostError::Refused {
                reason: "the request names another device".into(),
            });
        }
        self.0
            .replay
            .lock_recover()
            .admit(signed, now_ns())
            .map_err(|error| ActionHostError::Refused {
                reason: error.to_string(),
            })?;
        let replica = self
            .0
            .replicas
            .lock_recover()
            .get(&action.workspace)
            .cloned()
            .ok_or_else(|| ActionHostError::Refused {
                reason: "this device does not hold that workspace".into(),
            })?;
        let now = SystemTime::now();
        let authority = replica
            .read(|peer| peer.permissions(source, now))
            .map_err(|error| refusal_of(&error))?;
        let certificate = replica
            .read(|peer| {
                peer.certificates()
                    .into_iter()
                    .find(|certificate| certificate.device_public_key == source)
            })
            .ok_or(ActionHostError::NotPermitted)?;
        // The device is its own principal when its person delegated it: an
        // agent is the agent, a bot the bot, never the person.
        let caller = match authority.delegate.map(|bound| bound.subject) {
            Some(tilo_action_policy::Subject::Agent(agent)) => {
                Caller::delegated_agent(agent, certificate.user_public_key, source)
            }
            Some(tilo_action_policy::Subject::Bot(installation)) => {
                Caller::bot(installation, certificate.user_public_key, source)
            }
            _ => Caller::authenticated(certificate.user_public_key, source),
        };
        Ok((
            caller
                .in_room(action.workspace.clone())
                .from_foreign_device(authority.foreign),
            action.workspace.clone(),
        ))
    }

    /// Hand one verified request to the dispatcher and answer it.
    async fn dispatch(
        &self,
        caller: &Caller,
        request: &RemoteActionMessage,
    ) -> Option<RemoteActionMessage> {
        use tilo_actions::remote::ActionHostError;
        let dispatcher = &self.0.dispatcher;
        match request {
            RemoteActionMessage::CatalogueRequest { correlation, query } => {
                let result = await_answer(dispatcher.query(caller, query.clone())).await;
                Some(RemoteActionMessage::CatalogueResponse {
                    correlation: *correlation,
                    result,
                })
            }
            RemoteActionMessage::Invoke(invoke) => {
                let status = match self.spend(caller) {
                    Ok(()) => await_answer(dispatcher.invoke(caller, invoke.clone()))
                        .await
                        .map_or_else(
                            |error| ReceiptStatus::Rejected { error },
                            |receipt| receipt.status,
                        ),
                    Err(error) => ReceiptStatus::Rejected { error },
                };
                Some(RemoteActionMessage::Receipt(InvocationReceipt {
                    invocation: invoke.invocation,
                    status,
                }))
            }
            RemoteActionMessage::Observe {
                invocation,
                wait_ms,
                seen,
            } => {
                let state = self.observe(caller, invocation, *wait_ms, *seen).await;
                Some(RemoteActionMessage::Update {
                    invocation: *invocation,
                    state,
                })
            }
            RemoteActionMessage::Cancel { invocation } => {
                let result = await_answer(dispatcher.cancel(caller, invocation)).await;
                Some(RemoteActionMessage::CancelResponse {
                    invocation: *invocation,
                    result,
                })
            }
            // A request this device does not answer is said so, never left
            // for the caller to wait on.
            other if other.is_request() => refusal(
                other,
                ActionHostError::Unavailable {
                    reason: "this device does not answer that request".into(),
                },
            ),
            _ => None,
        }
    }

    /// The state of `invocation` as `caller` owns it, held up to `wait_ms`
    /// until it moves past `seen` or settles.
    async fn observe(
        &self,
        caller: &Caller,
        invocation: &tilo_actions::InvocationId,
        wait_ms: Option<u32>,
        seen: Option<UpdateMark>,
    ) -> Option<InvocationState> {
        let dispatcher = &self.0.dispatcher;
        let mut state = dispatcher.observe(caller, invocation);
        let Some(wait_ms) = wait_ms else {
            return state;
        };
        let wait =
            Duration::from_millis(u64::from(wait_ms)).min(tilo_actions::remote::MAX_REMOTE_WAIT);
        let deadline = Instant::now()
            .checked_add(wait)
            .unwrap_or_else(Instant::now);
        loop {
            if let Some(current) = &state
                && (current.is_terminal()
                    || seen.is_none_or(|seen| UpdateMark::of(&current.update) != seen))
            {
                return state;
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return dispatcher.observe(caller, invocation);
            };
            match dispatcher.next_change(caller, invocation) {
                Some(changed) => {
                    let _ = tokio::time::timeout(remaining, changed).await;
                }
                // A dispatcher that cannot tell follows its own schedule.
                None => tokio::time::sleep(remaining.min(Duration::from_secs(1))).await,
            }
            let next = dispatcher.observe(caller, invocation);
            if next == state {
                // A wake that found nothing new costs a moment, so a
                // dispatcher that signals at once is not asked in a loop.
                tokio::time::sleep(remaining.min(Duration::from_millis(5))).await;
            }
            state = next;
        }
    }

    /// Spend one of `caller`'s changes, or say the caller has none left.
    fn spend(&self, caller: &Caller) -> Result<(), tilo_actions::remote::ActionHostError> {
        let CallerPrincipalKey::Person(person) = CallerPrincipalKey::of(caller) else {
            return Ok(());
        };
        let now = Instant::now();
        let rate = self.rate();
        let mut callers = self.0.callers.lock_recover();
        if callers.len() >= KEPT_CALLERS && !callers.contains_key(&person) {
            callers.retain(|_, bucket| !bucket.full(rate, now));
        }
        let bucket = callers.entry(person).or_insert(Bucket {
            changes: rate.capacity,
            at: now,
        });
        let spent = bucket.take(rate, now);
        drop(callers);
        if spent {
            Ok(())
        } else {
            Err(tilo_actions::remote::ActionHostError::Refused {
                reason: "too many changes; slow down".into(),
            })
        }
    }

    fn rate(&self) -> Rate {
        let bounds = self.0.bounds;
        Rate {
            capacity: f64::from(bounds.burst),
            per_second: f64::from(bounds.changes_per_window) / f64::from(bounds.window_secs.max(1)),
        }
    }

    /// Serve `connection` until it ends, answering each stream on its own.
    async fn serve(&self, connection: Connection) -> Result<(), String> {
        let remote = *connection.remote_id().as_bytes();
        loop {
            let (send, recv) = match connection.accept_bi().await {
                Ok(stream) => stream,
                Err(iroh::endpoint::ConnectionError::ApplicationClosed(_)) => return Ok(()),
                Err(error) => return Err(error.to_string()),
            };
            let plane = self.clone();
            self.0.runtime.spawn(async move {
                if let Err(error) = plane.answer_stream(remote, send, recv).await {
                    tracing::debug!(%error, "a remote action stream ended in error");
                }
            });
        }
    }

    /// Read one request from `recv`, answer it on `send`, and finish.
    async fn answer_stream(
        &self,
        remote: [u8; 32],
        mut send: SendStream,
        mut recv: RecvStream,
    ) -> Result<(), String> {
        let Some(bytes) = read_frame(&mut recv).await? else {
            return send.finish().map_err(|error| error.to_string());
        };
        let signed: SignedDeviceAction =
            postcard::from_bytes(&bytes).map_err(|error| error.to_string())?;
        if let Some(reply) = self.answer(remote, signed).await {
            let bytes = RemoteActionFrame::new(reply)
                .encode()
                .map_err(|error| error.to_string())?;
            write_frame(&mut send, &bytes).await?;
        }
        send.finish().map_err(|error| error.to_string())
    }
}

impl ProtocolHandler for ActionPlane {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        self.serve(connection)
            .await
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error)))
    }
}

/// The person a caller acts for, when it acts for one.
enum CallerPrincipalKey {
    Person([u8; 32]),
    Other,
}

impl CallerPrincipalKey {
    fn of(caller: &Caller) -> Self {
        caller
            .user_public_key()
            .map_or(Self::Other, |person| Self::Person(*person))
    }
}

#[derive(Clone, Copy)]
struct Bucket {
    changes: f64,
    at: Instant,
}

/// A bucket's size and refill rate per second.
#[derive(Clone, Copy)]
struct Rate {
    capacity: f64,
    per_second: f64,
}

impl Bucket {
    fn take(&mut self, rate: Rate, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        self.changes = elapsed
            .mul_add(rate.per_second, self.changes)
            .min(rate.capacity);
        self.at = now;
        if self.changes >= 1.0 {
            self.changes -= 1.0;
            true
        } else {
            false
        }
    }

    fn full(&self, rate: Rate, now: Instant) -> bool {
        now.saturating_duration_since(self.at)
            .as_secs_f64()
            .mul_add(rate.per_second, self.changes)
            >= rate.capacity
    }
}

/// Why the workspace's roster leaves a device without authority, as the
/// refusal its caller reads.
fn refusal_of(error: &crate::MembershipError) -> tilo_actions::remote::ActionHostError {
    use crate::MembershipError;
    use tilo_actions::remote::ActionHostError;
    match error {
        MembershipError::Revoked => ActionHostError::Refused {
            reason: "the device is revoked".into(),
        },
        MembershipError::NoMembership => ActionHostError::Refused {
            reason: "the person is not a member of this workspace".into(),
        },
        MembershipError::Delegate(_) => ActionHostError::Refused {
            reason: "the device's delegation is expired or withdrawn".into(),
        },
        MembershipError::Certificate(_) => ActionHostError::Refused {
            reason: "the device's certificate has run out".into(),
        },
        MembershipError::UnknownDevice
        | MembershipError::Absent
        | MembershipError::MalformedIdentity(_)
        | MembershipError::CertificateMismatch
        | MembershipError::GovernanceMissing
        | MembershipError::RevocationRoot
        | MembershipError::Revocation(_) => ActionHostError::NotPermitted,
    }
}

/// The answer to a request the device did not do, in the request's own shape
/// so the caller learns why instead of waiting.
fn refusal(
    request: &RemoteActionMessage,
    error: tilo_actions::remote::ActionHostError,
) -> Option<RemoteActionMessage> {
    Some(match request {
        RemoteActionMessage::CatalogueRequest { correlation, .. } => {
            RemoteActionMessage::CatalogueResponse {
                correlation: *correlation,
                result: Err(error),
            }
        }
        RemoteActionMessage::Invoke(invoke) => RemoteActionMessage::Receipt(InvocationReceipt {
            invocation: invoke.invocation,
            status: ReceiptStatus::Rejected { error },
        }),
        RemoteActionMessage::Observe { invocation, .. } => RemoteActionMessage::Update {
            invocation: *invocation,
            state: None,
        },
        RemoteActionMessage::Cancel { invocation } => RemoteActionMessage::CancelResponse {
            invocation: *invocation,
            result: Err(error),
        },
        RemoteActionMessage::RecordRequest { correlation, .. } => {
            RemoteActionMessage::RecordResponse {
                correlation: *correlation,
                result: Err(error),
            }
        }
        RemoteActionMessage::FeedbackRequest { correlation, .. } => {
            RemoteActionMessage::FeedbackResponse {
                correlation: *correlation,
                result: Err(error),
            }
        }
        RemoteActionMessage::ConfirmationRequest { correlation, .. } => {
            RemoteActionMessage::ConfirmationResponse {
                correlation: *correlation,
                result: Err(error),
            }
        }
        _ => return None,
    })
}

/// The answer a dispatcher's pending gives, or the refusal of one it
/// abandoned.
async fn await_answer<T: Send + 'static>(
    answer: tilo_actions::remote::Pending<Result<T, tilo_actions::remote::ActionHostError>>,
) -> Result<T, tilo_actions::remote::ActionHostError> {
    match answer.await {
        Some(result) => result,
        None => Err(tilo_actions::remote::ActionHostError::Unavailable {
            reason: "the device's action host abandoned the request".into(),
        }),
    }
}

/// One other device, as an [`ActionHost`]: its actions through the same
/// trait a local host's are used through.
#[derive(Clone)]
pub struct RemoteDevice(Arc<Device>);

struct Device {
    endpoint: Endpoint,
    addr: EndpointAddr,
    workspace: String,
    target: [u8; 32],
    key: SigningKey,
    runtime: tokio::runtime::Handle,
    /// The connection to this device, kept while it lasts: one per pair.
    connection: Mutex<Option<Connection>>,
    next_correlation: AtomicU64,
    follow: Following,
}

impl std::fmt::Debug for RemoteDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteDevice")
            .field("device", &hex::encode(self.0.target))
            .field("workspace", &self.0.workspace)
            .finish_non_exhaustive()
    }
}

impl RemoteDevice {
    /// The device these actions are asked of.
    #[must_use]
    pub fn device(&self) -> [u8; 32] {
        self.0.target
    }

    /// Send one message and hand back what `answer` makes of its reply.
    fn ask<T: Send + 'static>(
        &self,
        message: RemoteActionMessage,
        answer: impl FnOnce(RemoteActionMessage) -> Result<T, tilo_actions::remote::ActionHostError>
        + Send
        + 'static,
    ) -> tilo_actions::remote::Pending<Result<T, tilo_actions::remote::ActionHostError>> {
        let (completer, result) = tilo_actions::remote::pending();
        let device = Arc::clone(&self.0);
        self.0.runtime.spawn(async move {
            let reply = device.request(message).await;
            completer.complete(reply.and_then(answer));
        });
        result
    }

    /// Invoke `request` on the device and follow it until it settles there:
    /// its terminal update, or why it was refused or lost. No thread waits:
    /// the receipt is held by the device until the invocation moves, and each
    /// answer sends the next long-poll.
    pub fn invoke_until_settled(
        self: Arc<Self>,
        mut request: tilo_actions::remote::InvokeRequest,
        follow_for: Duration,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::InvocationUpdate, tilo_actions::remote::ActionHostError>,
    > {
        let now = Instant::now();
        let budget = FollowBudget {
            until: now.checked_add(follow_for).unwrap_or(now),
            rounds: MAX_FOLLOW_ROUNDS,
        };
        let invocation = request.invocation;
        request.wait_ms = Some(CHANGE_WAIT_MS);
        let (done, answer) = tilo_actions::remote::pending();
        let host = Arc::clone(&self);
        self.ask(
            RemoteActionMessage::Invoke(request),
            |answer| match answer {
                RemoteActionMessage::Receipt(receipt) => Ok(receipt),
                _ => Err(unexpected()),
            },
        )
        .then(move |receipt| match receipt {
            Some(Ok(receipt)) => match settled_receipt(invocation, receipt.status) {
                Ok(Some(update)) => done.complete(Ok(update)),
                Ok(None) => follow(&host, invocation, Some(UpdateMark::Accepted), budget, done),
                Err(error) => done.complete(Err(error)),
            },
            Some(Err(error)) => done.complete(Err(error)),
            None => done.complete(Err(unanswered())),
        });
        answer
    }
}

impl Device {
    /// The connection to this device, opened once and kept while it lasts.
    async fn connection(&self) -> Result<Connection, tilo_actions::remote::ActionHostError> {
        if let Some(open) = self
            .connection
            .lock_recover()
            .as_ref()
            .filter(|connection| connection.close_reason().is_none())
        {
            return Ok(open.clone());
        }
        let connection = tokio::time::timeout(
            REPLY_DEADLINE,
            self.endpoint.connect(self.addr.clone(), ACTIONS_ALPN),
        )
        .await
        .map_err(|_| unavailable("the device did not answer"))?
        .map_err(|error| unavailable(&error.to_string()))?;
        *self.connection.lock_recover() = Some(connection.clone());
        Ok(connection)
    }

    /// One signed request and its one answer.
    async fn request(
        &self,
        message: RemoteActionMessage,
    ) -> Result<RemoteActionMessage, tilo_actions::remote::ActionHostError> {
        let signed = SignedDeviceAction::sign(
            &DeviceAction {
                workspace: self.workspace.clone(),
                request_id: uuid::Uuid::new_v4(),
                source: self.key.verifying_key().to_bytes(),
                target: self.target,
                message,
            },
            now_ns(),
            &self.key,
        )
        .map_err(|error| unavailable(&error.to_string()))?;
        let bytes =
            postcard::to_allocvec(&signed).map_err(|error| unavailable(&error.to_string()))?;
        let connection = self.connection().await?;
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|error| unavailable(&error.to_string()))?;
        write_frame(&mut send, &bytes)
            .await
            .map_err(|error| unavailable(&error))?;
        send.finish()
            .map_err(|error| unavailable(&error.to_string()))?;
        let answer = read_frame(&mut recv)
            .await
            .map_err(|error| unavailable(&error))?
            .ok_or_else(|| unavailable("the device closed without answering"))?;
        RemoteActionFrame::decode(&answer)
            .map(|frame| frame.message)
            .map_err(|error| unavailable(&error.to_string()))
    }
}

/// What a peer did not answer, as the caller reads it.
fn unavailable(reason: &str) -> tilo_actions::remote::ActionHostError {
    tilo_actions::remote::ActionHostError::Unavailable {
        reason: reason.to_owned(),
    }
}

fn unexpected() -> tilo_actions::remote::ActionHostError {
    unavailable("the device answered with an unexpected message")
}

fn unanswered() -> tilo_actions::remote::ActionHostError {
    unavailable("the device did not answer in time")
}

/// The terminal update a receipt amounts to; `None` while it runs.
fn settled_receipt(
    invocation: tilo_actions::InvocationId,
    status: ReceiptStatus,
) -> Result<Option<tilo_actions::InvocationUpdate>, tilo_actions::remote::ActionHostError> {
    use tilo_actions::InvocationUpdate;
    match status {
        ReceiptStatus::Accepted => Ok(None),
        ReceiptStatus::Completed { output } => {
            Ok(Some(InvocationUpdate::Succeeded { invocation, output }))
        }
        ReceiptStatus::Failed { error } => Ok(Some(InvocationUpdate::Failed { invocation, error })),
        ReceiptStatus::Cancelled { message } => Ok(Some(InvocationUpdate::Cancelled {
            invocation,
            message,
        })),
        ReceiptStatus::Rejected { error } => Err(error),
    }
}

/// What following one invocation on another device has left.
#[derive(Clone, Copy)]
struct FollowBudget {
    until: Instant,
    rounds: u32,
}

/// Most long-polls one followed invocation sends. A device holds each for up
/// to [`CHANGE_WAIT_MS`] unless the invocation moves, so this bounds only a
/// device that answers at once with nothing new.
const MAX_FOLLOW_ROUNDS: u32 = 64;

/// Ask the device for `invocation`'s next move past `seen`, and keep asking
/// until it settles or `budget` runs out.
fn follow(
    host: &Arc<RemoteDevice>,
    invocation: tilo_actions::InvocationId,
    seen: Option<UpdateMark>,
    budget: FollowBudget,
    done: tilo_actions::remote::Completer<
        Result<tilo_actions::InvocationUpdate, tilo_actions::remote::ActionHostError>,
    >,
) {
    if budget.rounds == 0 || Instant::now() >= budget.until {
        done.complete(Err(unavailable(
            "the device did not finish the action in time",
        )));
        return;
    }
    let next = FollowBudget {
        rounds: budget.rounds.saturating_sub(1),
        ..budget
    };
    let again = Arc::clone(host);
    host.ask(
        RemoteActionMessage::Observe {
            invocation,
            wait_ms: Some(CHANGE_WAIT_MS),
            seen,
        },
        |answer| match answer {
            RemoteActionMessage::Update { state, .. } => Ok(state),
            _ => Err(unexpected()),
        },
    )
    .then(move |answer| match answer {
        Some(Ok(Some(state))) if state.is_terminal() => done.complete(Ok(state.update)),
        Some(Ok(Some(state))) => {
            let seen = Some(UpdateMark::of(&state.update));
            follow(&again, invocation, seen, next, done);
        }
        Some(Ok(None)) => done.complete(Err(
            tilo_actions::remote::ActionHostError::UnknownInvocation,
        )),
        Some(Err(error)) => done.complete(Err(error)),
        None => done.complete(Err(unanswered())),
    });
}

/// What following an invocation on another device knows of it.
#[derive(Default)]
struct Following {
    /// How far each running invocation was last seen to get: what a
    /// long-poll asks the device to move past.
    seen: Mutex<HashMap<tilo_actions::InvocationId, UpdateMark>>,
    /// The state a long-poll brought back, for the observe that follows its
    /// wake, so one change costs one round trip, not two.
    fresh: Mutex<HashMap<tilo_actions::InvocationId, InvocationState>>,
    /// Set once the device answered a long-poll at once with nothing new: it
    /// predates waits, so its callers keep their own schedule.
    unheld: std::sync::atomic::AtomicBool,
}

impl Following {
    fn saw(&self, id: tilo_actions::InvocationId, state: Option<&InvocationState>) {
        let Ok(mut seen) = self.seen.lock() else {
            return;
        };
        match state.filter(|state| !state.is_terminal()) {
            Some(state) => {
                if seen.len() >= MAX_FOLLOWED && !seen.contains_key(&id) {
                    seen.clear();
                }
                seen.insert(id, UpdateMark::of(&state.update));
            }
            None => {
                seen.remove(&id);
            }
        }
    }

    fn mark(&self, id: &tilo_actions::InvocationId) -> Option<UpdateMark> {
        self.seen.lock().ok()?.get(id).copied()
    }

    /// Keep what a long-poll brought back. A `None` is not kept: the observe
    /// that follows asks again, and hears the same refusal.
    fn keep_fresh(&self, id: tilo_actions::InvocationId, state: Option<InvocationState>) {
        if let (Some(state), Ok(mut fresh)) = (state, self.fresh.lock()) {
            if fresh.len() >= MAX_FOLLOWED && !fresh.contains_key(&id) {
                fresh.clear();
            }
            fresh.insert(id, state);
        }
    }

    fn take_fresh(&self, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.fresh.lock().ok()?.remove(id)
    }
}

impl ActionHost for RemoteDevice {
    fn query(
        &self,
        _caller: &Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionHostError>,
    > {
        let correlation = self.0.next_correlation.fetch_add(1, Ordering::Relaxed);
        self.ask(
            RemoteActionMessage::CatalogueRequest { correlation, query },
            |answer| match answer {
                RemoteActionMessage::CatalogueResponse { result, .. } => result,
                _ => Err(unexpected()),
            },
        )
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<InvocationReceipt, tilo_actions::remote::ActionHostError>,
    > {
        self.ask(
            RemoteActionMessage::Invoke(request),
            |answer| match answer {
                RemoteActionMessage::Receipt(receipt) => match receipt.status {
                    ReceiptStatus::Rejected { error } => Err(error),
                    _ => Ok(receipt),
                },
                _ => Err(unexpected()),
            },
        )
    }

    /// The state the long-poll that just woke its caller brought back, or
    /// else the device's answer to asking, waited for as the trait's
    /// synchronous shape requires.
    fn observe(
        &self,
        _caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<InvocationState> {
        let state = self.0.follow.take_fresh(id).or_else(|| {
            let answer = self
                .ask(
                    RemoteActionMessage::Observe {
                        invocation: *id,
                        wait_ms: None,
                        seen: None,
                    },
                    |answer| match answer {
                        RemoteActionMessage::Update { state, .. } => Ok(state),
                        _ => Err(unexpected()),
                    },
                )
                .wait_timeout(REPLY_DEADLINE);
            match answer {
                Ok(Some(Ok(state))) => state,
                _ => None,
            }
        });
        self.0.follow.saw(*id, state.as_ref());
        state
    }

    /// A long-poll: one waiting observe the device answers when the
    /// invocation moves past what was last seen of it, or after
    /// [`CHANGE_WAIT_MS`]. No thread waits for it. `None` for a device that
    /// answered one at once with nothing new, so its callers keep their own
    /// schedule instead of asking as fast as it answers.
    fn next_change(
        &self,
        _caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::Pending<()>> {
        if self.0.follow.unheld.load(Ordering::Relaxed) {
            return None;
        }
        let seen = self.0.follow.mark(id);
        let sent = Instant::now();
        let device = Arc::clone(&self.0);
        let invocation = *id;
        let (changed, change) = tilo_actions::remote::pending();
        self.ask(
            RemoteActionMessage::Observe {
                invocation,
                wait_ms: Some(CHANGE_WAIT_MS),
                seen,
            },
            move |answer| match answer {
                RemoteActionMessage::Update { state, .. } => {
                    let same = state.as_ref().map(|state| UpdateMark::of(&state.update)) == seen
                        && state.is_some();
                    let early =
                        sent.elapsed() < Duration::from_millis(u64::from(CHANGE_WAIT_MS / 2));
                    if same && early {
                        device.follow.unheld.store(true, Ordering::Relaxed);
                    }
                    device.follow.keep_fresh(invocation, state);
                    Ok(())
                }
                _ => Err(unexpected()),
            },
        )
        .then(move |_| changed.complete(()));
        Some(change)
    }

    fn cancel(
        &self,
        _caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionHostError>> {
        self.ask(
            RemoteActionMessage::Cancel { invocation: *id },
            |answer| match answer {
                RemoteActionMessage::CancelResponse { result, .. } => result,
                _ => Err(unexpected()),
            },
        )
    }
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|since| u64::try_from(since.as_nanos()).ok())
        .unwrap_or(u64::MAX)
}

async fn write_frame(send: &mut SendStream, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err("frame too large".into());
    }
    let len = u32::try_from(bytes.len()).map_err(|_| "frame too large".to_string())?;
    send.write_all(&len.to_le_bytes())
        .await
        .map_err(|error| error.to_string())?;
    send.write_all(bytes)
        .await
        .map_err(|error| error.to_string())
}

async fn read_frame(recv: &mut RecvStream) -> Result<Option<Vec<u8>>, String> {
    let mut len = [0_u8; 4];
    match recv.read_exact(&mut len).await {
        Ok(()) => {}
        Err(iroh::endpoint::ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(error) => return Err(error.to_string()),
    }
    let len =
        usize::try_from(u32::from_le_bytes(len)).map_err(|_| "frame too large".to_string())?;
    if len > MAX_FRAME_BYTES {
        return Err("frame too large".into());
    }
    let mut bytes = vec![0_u8; len];
    recv.read_exact(&mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    Ok(Some(bytes))
}
