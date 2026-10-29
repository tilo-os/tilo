//! An app's side of the messaging socket: the enrollment and messaging
//! clients Messages is lent, backed by the service.
//!
//! What the app reads without waiting (an account's state, an enrollment's
//! phase) comes from the last snapshot the service sent; everything else is
//! one call. Each account still checks its grant, contacts route and custody
//! in the service on every operation; the snapshot authorizes nothing.

use std::os::fd::FromRawFd as _;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock, Weak};
use std::time::Duration;

use crate::{
    AccountState, EnrollmentApprovalFuture, EnrollmentClient, EnrollmentHost, EnrollmentPhase,
    EnrollmentReviewFuture, GroupActionFuture, GroupChangeRequest, GroupClient, GroupHistoryFuture,
    GroupHost, GroupSendRequest, GroupsFuture, HistoryCursor, HistoryError, HistoryFuture,
    HistoryHost, LinkedAccount, LinkingFuture, MembershipFuture, MessagingClient, MessagingHost,
    PairingFuture, SendError, SendFuture, SendRequest, ServiceAccount, SetupClient, SetupHost,
    SimChoicesFuture,
};
use tilo_contact_directory::routing::Route;

use super::link::{Handler, Responder};
use super::wire::{AppLink, Call, Event, GroupCall, Handoff, PROTOCOL, Proposal, Reply, Snapshot};

/// How long a connection waits for the service's first snapshot.
const FIRST_STATE: Duration = Duration::from_secs(5);
/// How long Passwords waits for the service's permission requests.
const PROPOSALS: Duration = Duration::from_secs(2);

type Observer = Weak<dyn Fn() + Send + Sync>;

struct State {
    snapshot: RwLock<Snapshot>,
    first: (Mutex<bool>, std::sync::Condvar),
    observers: Mutex<Vec<Observer>>,
}

impl State {
    fn notify(&self) {
        let observers: Vec<_> = {
            let mut observers = self
                .observers
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            observers.retain(|observer| observer.strong_count() > 0);
            observers.iter().filter_map(Weak::upgrade).collect()
        };
        for observer in observers {
            observer();
        }
    }
}

impl Handler<Call, Reply, Event> for State {
    fn call(&self, _: Call, reply: Responder<Call, Reply, Event>) {
        reply.send(Reply::Refused);
    }
    fn event(&self, event: Event) {
        match event {
            Event::State(snapshot) => {
                *self
                    .snapshot
                    .write()
                    .unwrap_or_else(PoisonError::into_inner) = snapshot;
                let (first, arrived) = &self.first;
                *first.lock().unwrap_or_else(PoisonError::into_inner) = true;
                arrived.notify_all();
            }
            Event::HistoryChanged => {}
        }
        self.notify();
    }
    fn closed(&self) {
        let mut snapshot = self
            .snapshot
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        for account in &mut snapshot.accounts {
            account.state = AccountState::Unavailable;
        }
        drop(snapshot);
        let (first, arrived) = &self.first;
        *first.lock().unwrap_or_else(PoisonError::into_inner) = true;
        arrived.notify_all();
        self.notify();
    }
}

/// One connection to the service.
pub struct Connection {
    link: AppLink,
    state: Arc<State>,
}

impl Connection {
    /// The connection the launcher handed this app, once per process; `None`
    /// when it was handed none (the service is not running, or the app was
    /// not launched into a session).
    pub fn inherited() -> Option<Arc<Self>> {
        static INHERITED: OnceLock<Option<Arc<Connection>>> = OnceLock::new();
        INHERITED
            .get_or_init(|| {
                let fd: i32 = std::env::var(super::SERVICE_FD_ENV).ok()?.parse().ok()?;
                if fd < 3 {
                    return None;
                }
                // SAFETY: the launcher placed this connected socket at this
                // number for this process alone, and nothing else here
                // takes ownership of it: the value is read once.
                let stream = unsafe { UnixStream::from_raw_fd(fd) };
                Self::over(stream).ok()
            })
            .clone()
    }

    /// Connect to the service at `socket` as `app_id`, writing the handoff
    /// the launcher writes for a launched app. For a trusted host that shows
    /// Messages in its own process.
    ///
    /// # Errors
    /// The service is not there or refused the handoff.
    pub fn connect(socket: &std::path::Path, app_id: &str) -> std::io::Result<Arc<Self>> {
        let mut stream = UnixStream::connect(socket)?;
        write_handoff(&mut stream, app_id)?;
        Self::over(stream)
    }

    /// A connection over a stream whose handoff was written.
    ///
    /// # Errors
    /// The reading thread could not be started.
    pub fn over(stream: UnixStream) -> std::io::Result<Arc<Self>> {
        let state = Arc::new(State {
            snapshot: RwLock::default(),
            first: (Mutex::new(false), std::sync::Condvar::new()),
            observers: Mutex::default(),
        });
        let link = AppLink::start(
            "tilo-messaging-client",
            stream.try_clone()?,
            stream,
            state.clone(),
        )?;
        let (first, arrived) = &state.first;
        drop(
            arrived
                .wait_timeout_while(
                    first.lock().unwrap_or_else(PoisonError::into_inner),
                    FIRST_STATE,
                    |arrived| !*arrived,
                )
                .unwrap_or_else(PoisonError::into_inner),
        );
        Ok(Arc::new(Self { link, state }))
    }

    fn snapshot<T>(&self, read: impl FnOnce(&Snapshot) -> T) -> T {
        read(
            &self
                .state
                .snapshot
                .read()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// The person the service holds accounts for.
    pub fn principal(&self) -> String {
        self.snapshot(|snapshot| snapshot.principal.clone())
    }

    /// Account enrollment, as Messages is lent it.
    pub fn enrollment(self: &Arc<Self>) -> EnrollmentClient {
        EnrollmentClient::new(self.principal(), Arc::new(Enrollment(self.clone())))
    }

    /// The running accounts, as Messages is lent them.
    pub fn messaging(self: &Arc<Self>) -> MessagingClient {
        MessagingClient::new(self.principal(), Arc::new(Messaging(self.clone())))
    }

    /// Call `observer` whenever the accounts' state or history changed.
    /// Dropping the returned guard stops it.
    pub fn observe(
        &self,
        observer: Arc<dyn Fn() + Send + Sync>,
    ) -> Box<dyn std::any::Any + Send + Sync> {
        self.state
            .observers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::downgrade(&observer));
        Box::new(observer)
    }

    /// The permission requests the service waits on; for Passwords.
    pub fn proposals(&self) -> Vec<Proposal> {
        match self.link.call_blocking(Call::Proposals, PROPOSALS) {
            Some(Reply::Proposals(proposals)) => proposals,
            _ => Vec::new(),
        }
    }

    /// How many times the requests changed: what a window that shows them
    /// compares to know to look again.
    pub fn proposals_revision(&self) -> u64 {
        self.snapshot(|snapshot| snapshot.proposals)
    }

    /// The person's answer to one request; for Passwords.
    ///
    /// # Errors
    /// The request is gone or the service refused the answer.
    pub fn decide(&self, proposal: &str, decision: super::wire::Decision) -> Result<(), SendError> {
        match self.link.call_blocking(
            Call::Decide {
                proposal: proposal.into(),
                decision,
            },
            PROPOSALS,
        ) {
            Some(Reply::Done) => Ok(()),
            _ => Err(SendError::Revoked),
        }
    }

    fn ask(&self, call: Call) -> impl Future<Output = Option<Reply>> + Send + 'static {
        let answer = self.link.call(call);
        async move { answer.await.ok() }
    }

    fn tell(&self, call: Call) {
        drop(self.link.call(call));
    }
}

/// Write the launcher's handoff for `app_id`.
///
/// # Errors
/// The stream refused the write.
pub fn write_handoff(stream: &mut UnixStream, app_id: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let body = serde_json::to_vec(&Handoff {
        protocol: PROTOCOL,
        app_id: app_id.into(),
    })
    .map_err(std::io::Error::other)?;
    super::wire::HANDOFF_FRAMING
        .write(stream, &body)
        .map_err(std::io::Error::other)?;
    stream.flush()
}

macro_rules! answer {
    ($connection:expr, $call:expr, $variant:ident, $unavailable:expr) => {{
        let answer = $connection.ask($call);
        Box::pin(async move {
            match answer.await {
                Some(Reply::$variant(result)) => result,
                _ => Err($unavailable),
            }
        })
    }};
}

struct Enrollment(Arc<Connection>);

impl EnrollmentHost for Enrollment {
    fn prepare(&self, service: String) -> EnrollmentReviewFuture {
        answer!(
            self.0,
            Call::Prepare(service),
            Review,
            SendError::Unavailable
        )
    }
    fn begin(&self, service: String) -> PairingFuture {
        answer!(
            self.0,
            Call::Begin(service),
            Pairing,
            SendError::Unavailable
        )
    }
    fn finish(&self, account: ServiceAccount, attempt: String) -> EnrollmentReviewFuture {
        answer!(
            self.0,
            Call::Finish(account, attempt),
            Review,
            SendError::Unavailable
        )
    }
    fn review(&self, account: ServiceAccount, name: String) -> EnrollmentApprovalFuture {
        answer!(
            self.0,
            Call::Review(account, name),
            Proposal,
            SendError::Unavailable
        )
    }
    fn phase(&self, account: &ServiceAccount) -> EnrollmentPhase {
        self.0.snapshot(|snapshot| {
            snapshot
                .enrollments
                .iter()
                .find(|enrollment| &enrollment.account == account)
                .map_or(EnrollmentPhase::Empty, |enrollment| enrollment.phase)
        })
    }
    fn linked(&self, account: &ServiceAccount) -> Option<LinkedAccount> {
        self.0.snapshot(|snapshot| {
            snapshot
                .enrollments
                .iter()
                .find(|enrollment| &enrollment.account == account)
                .and_then(|enrollment| enrollment.linked.clone())
        })
    }
    fn cancel(&self, account: &ServiceAccount) {
        self.0.tell(Call::Cancel(account.clone()));
    }
    fn supports_sim_selection(&self) -> bool {
        true
    }
    fn sim_choices(&self) -> SimChoicesFuture {
        answer!(self.0, Call::SimChoices, Choices, SendError::Unavailable)
    }
    fn select_sim(&self, selection: String) -> EnrollmentReviewFuture {
        answer!(
            self.0,
            Call::SelectSim(selection),
            Review,
            SendError::Unavailable
        )
    }
    fn cancel_sim_selection(&self) {
        self.0.tell(Call::CancelSimSelection);
    }
}

struct Messaging(Arc<Connection>);

impl HistoryHost for Messaging {
    fn observe(
        &self,
        changed: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<Box<dyn std::any::Any + Send + Sync>> {
        Some(self.0.observe(changed))
    }
    fn read(&self, route: Route, cursor: Option<HistoryCursor>, limit: usize) -> HistoryFuture {
        answer!(
            self.0,
            Call::History {
                route,
                cursor,
                limit
            },
            History,
            HistoryError::Unavailable
        )
    }
}

impl MessagingHost for Messaging {
    fn groups(&self, account: &ServiceAccount) -> Option<GroupClient> {
        self.0
            .snapshot(|snapshot| {
                snapshot
                    .accounts
                    .iter()
                    .any(|view| &view.account == account && view.groups)
            })
            .then(|| {
                GroupClient::new(
                    self.0.principal(),
                    account.clone(),
                    Arc::new(Groups(self.0.clone(), account.clone())),
                )
            })
    }
    fn setup(&self, account: &ServiceAccount) -> Option<SetupClient> {
        let generation = self.0.snapshot(|snapshot| {
            snapshot
                .accounts
                .iter()
                .find(|view| &view.account == account)?
                .setup
                .as_ref()
                .map(|setup| setup.generation)
        })?;
        Some(SetupClient::new(
            self.0.principal(),
            account.clone(),
            generation,
            Arc::new(Setup(self.0.clone(), account.clone())),
        ))
    }
    fn account_state(&self, account: &ServiceAccount) -> AccountState {
        self.0.snapshot(|snapshot| {
            snapshot
                .accounts
                .iter()
                .find(|view| &view.account == account)
                .map_or(AccountState::Unavailable, |view| view.state)
        })
    }
    fn supports(&self, route: &Route) -> bool {
        let account = ServiceAccount::new(&route.service, route.sender.as_str());
        self.account_state(&account) == AccountState::Ready
    }
    fn send(&self, request: SendRequest) -> SendFuture {
        answer!(self.0, Call::Send(request), Sent, SendError::Unavailable)
    }
}

struct Groups(Arc<Connection>, ServiceAccount);

impl GroupHost for Groups {
    fn list(&self) -> GroupsFuture {
        answer!(
            self.0,
            Call::Group(self.1.clone(), GroupCall::List),
            Groups,
            SendError::Unavailable
        )
    }
    fn read(&self, group: String) -> MembershipFuture {
        answer!(
            self.0,
            Call::Group(self.1.clone(), GroupCall::Read(group)),
            Membership,
            SendError::Unavailable
        )
    }
    fn change(&self, request: GroupChangeRequest) -> GroupActionFuture {
        answer!(
            self.0,
            Call::Group(self.1.clone(), GroupCall::Change(request)),
            GroupReceipt,
            SendError::Unavailable
        )
    }
    fn send(&self, request: GroupSendRequest) -> GroupActionFuture {
        answer!(
            self.0,
            Call::Group(self.1.clone(), GroupCall::Send(request)),
            GroupReceipt,
            SendError::Unavailable
        )
    }
    fn history(&self, group: String) -> GroupHistoryFuture {
        answer!(
            self.0,
            Call::Group(self.1.clone(), GroupCall::History(group)),
            GroupHistory,
            SendError::Unavailable
        )
    }
}

struct Setup(Arc<Connection>, ServiceAccount);

impl SetupHost for Setup {
    fn state(&self) -> AccountState {
        self.0.snapshot(|snapshot| {
            snapshot
                .accounts
                .iter()
                .find(|view| view.account == self.1)
                .and_then(|view| view.setup.as_ref())
                .map_or(AccountState::Unavailable, |setup| setup.state)
        })
    }
    fn wake_after(&self) -> Option<Duration> {
        self.0.snapshot(|snapshot| {
            snapshot
                .accounts
                .iter()
                .find(|view| view.account == self.1)
                .and_then(|view| view.setup.as_ref())
                .and_then(|setup| setup.wake_after_ms)
                .map(Duration::from_millis)
        })
    }
    fn begin(&self) -> PairingFuture {
        answer!(
            self.0,
            Call::SetupBegin(self.1.clone()),
            Pairing,
            SendError::Unavailable
        )
    }
    fn finish(&self, attempt: String) -> LinkingFuture {
        answer!(
            self.0,
            Call::SetupFinish(self.1.clone(), attempt),
            Linked,
            SendError::Unavailable
        )
    }
    fn cancel(&self) {
        self.0.tell(Call::SetupCancel(self.1.clone()));
    }
}
