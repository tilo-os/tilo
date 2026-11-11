//! A hosted workspace's Browser: a window that runs in the workspace's own
//! VM, reached through this host.
//!
//! The relay runs no apps. Browser's page actions exist only on a live
//! Browser window, so a workspace hosted here opens Browser by starting a
//! headless Tilo session in the workspace's VM, the one its Linux programs
//! run in (`docs/hosted-browser.md`). That session is a device of the
//! workspace — a fresh key this host certifies for this one workspace — and it
//! opens the Browser window on the record the open made, so the window is
//! live and published like any other.
//!
//! This module is what the relay does about it:
//!
//! - **Place** ([`HostedBrowsers::place`]): after `app.so.tilo.browser#open`
//!   made a record, certify a device for the session scoped to the
//!   workspace, list it as a member device, ask the hosted-VM service to
//!   start it ([`BrowserSessions::start`]) with that material, and
//!   remember the window it answered. The open answers once the window is
//!   live; a second open attaches to the same session.
//! - **Route** ([`HostedBrowsers::route`]): an action addressed to that
//!   window reaches this host (a door agent's calls go to the workspace's
//!   host device) and is handed to the device holding the window, stamped
//!   with the caller, so it runs there as the caller's: History records the
//!   agent's run for the person it acts for. The relay runs none of it.
//! - **Ask** ([`HostedBrowsers::ask`]): a page action asks the person first,
//!   and nobody stands in front of a window in a VM to ask. The window refuses
//!   such an action for want of their answer; this host puts the question to
//!   their approving devices itself, as it does for its own agents' actions,
//!   and answers its caller that the invocation waits. Allowed, the same
//!   invocation goes to the window again carrying their confirmation —
//!   signed by this host's own device key, for that one invocation — which
//!   the window that certified it accepts for its own person, running it
//!   without asking again; nothing else is a confirmation there. Denied or
//!   unanswered, it ends refused. A caller needs nothing beyond asking and
//!   following what it asked for.
//! - **Meter** ([`BrowserMeter`]): every page action is admitted for the
//!   workspace's account before it is handed over, and counted once the
//!   window answers that it ran. A refused one is answered with the host's
//!   typed [`Refusal`] — the VM path's refusal, not a second one — and one
//!   the person did not allow is never counted.
//!
//! - **Stop** ([`HostedBrowsers::stopped`]): the environment's device is
//!   refused on every plane first — its entry in the relay's register says it
//!   stopped, before anything is delisted, so a delisting that fails leaves it
//!   refused rather than open — then the session in the VM is ended, its
//!   device delisted, and every connection it had is cut. The register says so
//!   on disk too, so a relay that restarts keeps it refused until the
//!   certificate it was given expires.
//!
//! What starts and stops sessions is the hosted-VM service's
//! ([`BrowserSessions`]): this host places, routes and counts, and stops an
//! environment when its session here ends ([`RECONNECT`] after the last one
//! went, unless the device is back by then): the signal is the device's own
//! sessions to this relay, so a session that dies is heard wherever it ran,
//! and the certificate's expiry bounds whatever is missed.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::certification::CertificationProtocol;
use ed25519_dalek::SigningKey;
use iroh::EndpointAddr;
use tilo_actions::remote::host_account::{NextStep, NextStepKind, Reason, Refusal};
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionHostError, Caller, Completer,
    Confirmation, ConfirmedInvocation, InvocationReceipt, InvocationState, InvokeRequest, Pending,
    ReceiptStatus, RemoteActionMessage, RemoteActionTarget, argument_digest, pending,
    provider_workspace,
};
use tilo_actions::selection::logical_resource_id;
use tilo_actions::{ActionFailure, InvocationUpdate};
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tilo_user_identity::{
    Answered, DeviceCapabilities, DeviceCert, HostWitness, Parties, sign_device_cert,
};

/// The app a hosted workspace opens Browser through.
pub const APP: &str = "so.tilo.browser";
/// The resource the window's page actions are actions of (`browser.page`);
/// their provider names the window (`browser@42`).
pub const PAGE_RESOURCE: &str = "browser.page";
/// How long a device certified for a session lasts: as long as the session
/// may run.
pub const DEVICE_LIFETIME: Duration = Duration::from_secs(2 * 60 * 60);
/// How long an environment device whose last session here ended has to come
/// back before its environment stops. A session in a VM that dies is gone; one
/// whose connection dropped is back in a moment, and its window lives on.
pub const RECONNECT: Duration = Duration::from_secs(30);
/// How long the person's answer to one page action stays good for running
/// it: their confirmation of that invocation, not a standing grant.
pub const WITNESS_LIFETIME: u64 = 5 * 60 * 1_000_000_000;
/// How long a page action's question waits for the person.
pub const ANSWER_WAIT: Duration = Duration::from_secs(2 * 60);
/// `answer_wait` in nanoseconds.
fn answer_wait_ns(answer_wait: Duration) -> u64 {
    u64::try_from(answer_wait.as_nanos()).unwrap_or(u64::MAX)
}
/// The label the workspace shows the session's device under.
pub const DEVICE_LABEL: &str = "Browser";
/// The address the sessions in the sessions reach this host at, written
/// `KEY@HOST:PORT` ([`tilo_relay_protocol::hosted::endpoint`]): this host's
/// device key and its **public** address.
///
/// A browser session's network reaches the public internet and nothing
/// else — no cluster-internal, private or link-local address answers it — so
/// a host that places sessions names the address they reach it at, and
/// never its own local one.
pub const RELAY_ENV: &str = "TILO_HOSTED_BROWSER_RELAY";

/// Where the sessions in this host's browser sessions reach it: the
/// address [`RELAY_ENV`] names, which the host must set to serve them.
///
/// # Errors
///
/// Nothing is named, or what is named is not `KEY@HOST:PORT`.
pub fn relay_address() -> Result<EndpointAddr, String> {
    let named = std::env::var(RELAY_ENV).map_err(|_| {
        format!(
            "a host that places Browser sets {RELAY_ENV} to the address its \
             sessions reach it at (KEY@HOST:PORT): their network reaches no private one"
        )
    })?;
    tilo_relay_protocol::hosted::endpoint(&named)
}

/// What a host hands a browser session when it gives it a workspace: the
/// device it certified for it, and where to be that device. The session in
/// the session reads the same type, so the two ends of the hand-off
/// cannot disagree about what it holds.
pub use tilo_relay_protocol::hosted::Material;

/// A browser session that is up, and the window it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// The device of the workspace the session in the VM joined as.
    pub device: [u8; 32],
    /// The provider token the window publishes under (`browser@42`): what a
    /// page action names in place of the app.
    pub provider: String,
}

/// The service that starts and stops a workspace's browser sessions.
///
/// The hosted-VM service's client implements this: it asks the placer for the
/// host of the workspace's VM, asks that host's member device to start the
/// session in it with the [`Material`], and answers once the session reports
/// its window live.
pub trait BrowserSessions: Send + Sync {
    /// Start the workspace's browser session with `material`, and answer its
    /// window once it is live.
    ///
    /// # Errors
    ///
    /// The words to refuse the open with: no host had room, or the host
    /// would not start it.
    fn start(&self, material: &Material) -> Result<Window, String>;

    /// The workspace's browser session stopped, or must: its device is
    /// forgotten and its window no longer exists.
    fn stop(&self, workspace: &str);
}

/// What a host counts page actions against: its plans and the account's
/// standing.
pub trait BrowserMeter: Send + Sync {
    /// Whether `account` may run one more page action on a Browser this host
    /// placed.
    ///
    /// # Errors
    ///
    /// The typed refusal the action is answered with, which is the VM path's
    /// ([`Refusal`]).
    fn admit(&self, account: &str) -> Result<(), Refusal>;

    /// One page action ran for `account`, in the workspace it is booked to.
    fn record(&self, workspace: &str, account: &str);
}

/// A host whose plans file sells nothing: it counts nothing and refuses
/// nothing, as it does for VMs.
#[derive(Clone, Copy, Debug, Default)]
pub struct Unmetered;

impl BrowserMeter for Unmetered {
    fn admit(&self, _: &str) -> Result<(), Refusal> {
        Ok(())
    }

    fn record(&self, _: &str, _: &str) {}
}

/// What placing needs of the host itself, once it runs: the certificate it
/// makes browser sessions' devices with, where it is reached, the workspaces
/// it serves, and the register that confines each session's device to the one
/// record it was placed for.
pub struct Authority {
    pub user_public_key: [u8; 32],
    /// The person's stand-in approver for this host
    /// ([`tilo_user_identity::prepare_stand_in_cert`]): what a session's
    /// device is certified with, as a member device of the workspace's person
    /// for as long as the session may run. A host the person did not give
    /// one holds no certificate to make a session a device, and placing
    /// one is refused.
    pub cert: DeviceCert,
    /// That stand-in's own signing key.
    pub key: SigningKey,
    /// Where the session joins the workspace.
    pub relay: EndpointAddr,
    pub workspaces: Option<crate::workspaces::Workspaces>,
    /// The relay's environments' devices, confined to the record each was
    /// placed for ([`crate::environment`]): what every plane of the workspace
    /// asks before it lets one in, or hands it a change.
    pub environments: crate::environment::EnvironmentRegistry,
    /// The runtime the workspaces run on: listing a device writes membership
    /// there.
    pub runtime: tokio::runtime::Handle,
    /// Where the person is asked about an action their agent made: the
    /// channel their approving devices answer on.
    pub certification: crate::certification::CertificationProtocol,
    /// Where a page action the person allowed is handed to the window that
    /// refused it, as the caller asked it: the host's own delivery.
    pub hand: crate::app_serving::Hand,
}

/// One workspace's window, while its session runs.
struct Open {
    /// The workspace, by its UUID, as its VM is named.
    workspace: String,
    window: Window,
    /// The device the workspace lists for it, forgotten when it stops.
    device: DeviceCert,
}

/// A page action handed to a window, as the host handed it there: whose it
/// is, which window has it, and how far its answer has come.
struct Handed {
    /// The caller, as the window hears it and the person's question names it.
    caller: Caller,
    window: [u8; 32],
    /// The workspace, by its UUID: whose account it is counted on.
    workspace: String,
    /// The action as the caller asked it: what the person's question shows,
    /// and what runs once they allow it.
    request: InvokeRequest,
    stage: Stage,
    /// Callers asking how it went, waiting for it to move, by the number
    /// that finds them again once their wait is over.
    waiting: Vec<(u64, Completer<RemoteActionMessage>)>,
}

/// How far a handed page action has come.
enum Stage {
    /// With the window, which answers the caller itself — as it answers a
    /// caller asking after it.
    Running,
    /// The window refused it for want of the person's word: this host asks
    /// them, and the caller waits for their answer.
    Asking,
    /// The person allowed it: with the window again, carrying their answer.
    Allowed,
    /// It is over: the update a caller asking after it is answered with.
    Over(InvocationUpdate),
}

impl Stage {
    /// The state an invocation in this stage is answered with when a caller
    /// asks how it went.
    fn state(&self, invocation: tilo_actions::InvocationId) -> InvocationUpdate {
        match self {
            Self::Running | Self::Asking | Self::Allowed => {
                InvocationUpdate::Accepted { invocation }
            }
            Self::Over(update) => update.clone(),
        }
    }
}

/// How many handed page actions are remembered while their windows answer:
/// beyond it, the oldest are forgotten and only go uncounted.
const HANDED: usize = 8192;

/// The Browser this host places for its workspaces, and the page actions it
/// hands to the windows.
pub struct HostedBrowsers {
    sessions: Box<dyn BrowserSessions>,
    meter: Box<dyn BrowserMeter>,
    /// Who pays for the workspaces' VMs, as this host knows it.
    account: String,
    /// The windows up now, by workspace.
    open: Mutex<BTreeMap<u64, Open>>,
    authority: Mutex<Option<Authority>>,
    /// The page actions handed to a window and not yet answered, by
    /// invocation: what a count is owed for once it ran, and what asks first.
    handed: Arc<Mutex<HashMap<tilo_actions::InvocationId, Handed>>>,
    /// The number naming the next caller waiting for an invocation to move.
    next_waiter: AtomicU64,
    /// How long an environment device that disconnected has to come back
    /// before its environment stops ([`RECONNECT`]).
    reconnect: Duration,
    /// How long a page action's question waits for the person
    /// ([`ANSWER_WAIT`]).
    answer_wait: Duration,
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "each method holds one guard for one answer; placing holds it so two opens make one session"
)]
impl HostedBrowsers {
    /// A host that places Browser through `sessions`, counting page
    /// actions of `account` in `meter`.
    pub fn new(
        sessions: Box<dyn BrowserSessions>,
        meter: Box<dyn BrowserMeter>,
        account: String,
    ) -> Self {
        Self {
            sessions,
            meter,
            account,
            open: Mutex::default(),
            authority: Mutex::default(),
            handed: Arc::default(),
            next_waiter: AtomicU64::new(0),
            reconnect: RECONNECT,
            answer_wait: ANSWER_WAIT,
        }
    }

    /// Wait `reconnect` for a disconnected environment device to come back
    /// before its environment stops ([`RECONNECT`]).
    #[must_use]
    pub const fn with_reconnect(mut self, reconnect: Duration) -> Self {
        self.reconnect = reconnect;
        self
    }

    /// Wait `answer_wait` for the person's answer to a page action's question
    /// before it is refused ([`ANSWER_WAIT`]).
    #[must_use]
    pub const fn with_answer_wait(mut self, answer_wait: Duration) -> Self {
        self.answer_wait = answer_wait;
        self
    }

    /// The Browser this host's environment names, when it serves Browser
    /// at all: where they start and stop (a placer and its VM hosts,
    /// [`crate::placed_browsers::PlacedBrowsers`]), the ledger their page
    /// actions are counted in ([`crate::browser_actions::BrowserMeter`]), and
    /// the address the sessions reach this host at.
    ///
    /// # Errors
    ///
    /// Sessions are named but cannot be started, or their page actions
    /// cannot be counted: a host that runs browsers without counting them
    /// would serve them free of charge, so the start fails rather than run
    /// half of it.
    pub fn from_env() -> Result<Option<Self>, String> {
        let Some(sessions) = crate::placed_browsers::PlacedBrowsers::from_env()? else {
            return Ok(None);
        };
        let meter = crate::browser_actions::BrowserMeter::from_env()?.ok_or_else(|| {
            format!(
                "{} names where Browser sessions start, so {} must name the ledger their \
                 page actions are counted in",
                crate::placed_shells::PLACER_ENV,
                tilo_host_account::browser_actions::LEDGER_ENV
            )
        })?;
        // The address their sessions reach this host at is part of serving
        // them: a host that cannot say it fails here, not at the first open.
        relay_address()?;
        let account = sessions.account().to_owned();
        Ok(Some(Self::new(
            Box::new(sessions),
            Box::new(meter),
            account,
        )))
    }

    /// Name the host's identity and workspaces, once it runs: needed to
    /// certify a session's device and to list it in its workspace. It is also
    /// where this host hears that one of its environment's sessions ended
    /// ([`Self::session_ended`]).
    pub fn serve_from(self: &Arc<Self>, authority: Authority) {
        let browsers = Arc::downgrade(self);
        authority
            .environments
            .when_ended(Arc::new(move |scope, device| {
                if let Some(browsers) = browsers.upgrade() {
                    browsers.session_ended(scope.workspace, device);
                }
            }));
        *self.authority.lock_recover() = Some(authority);
    }

    /// The window `workspace_id`'s Browser is in, when one is up.
    #[must_use]
    pub fn window(&self, workspace_id: u64) -> Option<Window> {
        self.open
            .lock_recover()
            .get(&workspace_id)
            .map(|open| open.window.clone())
    }

    /// Where `workspace_id`'s page action goes: to the device holding its
    /// window, once the meter admits it for the workspace's account; answered
    /// here with the host's typed refusal otherwise. `None` for an action
    /// that is not a page action of that window, which this host serves
    /// itself.
    ///
    /// A caller asking how one of those went is answered here too
    /// ([`Self::observe`]): by the window while it runs there, and by this
    /// host while the person is being asked and once it is over.
    #[must_use]
    pub fn route(
        &self,
        workspace_id: u64,
        caller: &Caller,
        message: &RemoteActionMessage,
    ) -> Option<crate::app_serving::Routing> {
        let RemoteActionMessage::Invoke(request) = message else {
            return self.asked_after(caller, message);
        };
        let (provider, window) = {
            let open = self.open.lock_recover();
            let up = open.get(&workspace_id)?;
            (up.window.provider.clone(), up.window.device)
        };
        if !page_action(&request.target, &provider) {
            return None;
        }
        let device = *caller.device_public_key()?;
        let permitted = match &request.confirmation {
            None => true,
            Some(Confirmation::Witnessed(token)) => {
                self.witnessed(token, workspace_id, device, request)
            }
            Some(_) => false,
        };
        if !permitted {
            return answered_with(message, ActionHostError::NotPermitted);
        }
        // A page action the person is already being asked about waits for
        // their answer: a caller that asks again is told so, not handed over
        // twice.
        if request.confirmation.is_none() && self.waits_for_the_person(request, device) {
            return Some(waiting(request.invocation));
        }
        match self.meter.admit(&self.account) {
            Ok(()) => {
                self.hand_over(workspace_id, caller.clone(), window, request);
                Some(crate::app_serving::Routing::Device(window))
            }
            Err(refusal) => answered_with(
                message,
                ActionHostError::Refused {
                    reason: refusal.to_reason(),
                },
            ),
        }
    }

    /// The window's answer to a page action this host handed it: it ran
    /// (`Completed`), so it is counted — once, when it ran — and its caller is
    /// answered with the page's receipt; the window asks the person first
    /// (`ConfirmationRequired`), so this host puts the question to them and
    /// answers the caller that its invocation waits, rather than refused.
    /// Anything else ran nothing and is counted not at all.
    pub fn relayed(
        &self,
        workspace_id: u64,
        from: [u8; 32],
        message: &RemoteActionMessage,
    ) -> Option<RemoteActionMessage> {
        let RemoteActionMessage::Receipt(receipt) = message else {
            return None;
        };
        if !self.open.lock_recover().contains_key(&workspace_id) {
            return None;
        }
        let mut handed = self.handed.lock_recover();
        let entry = handed.get_mut(&receipt.invocation)?;
        if entry.window != from {
            return None;
        }
        match &receipt.status {
            ReceiptStatus::Completed { .. } => {
                // The action ran where the window says it did, and nowhere
                // else: a count is owed once.
                let allowed = matches!(entry.stage, Stage::Allowed);
                let ran = allowed || matches!(entry.stage, Stage::Running);
                let workspace = entry.workspace.clone();
                answered(entry, &receipt.status);
                let update = entry.stage.state(receipt.invocation);
                drop(handed);
                if ran {
                    self.meter.record(&workspace, &self.account);
                }
                // The caller's own invocation was answered with "accepted,
                // it waits": what ran is told to it as the state it follows,
                // not as a second receipt for the same invocation.
                allowed.then(|| state_message(receipt.invocation, update))
            }
            ReceiptStatus::Rejected {
                error: ActionHostError::ConfirmationRequired,
            } if !matches!(entry.stage, Stage::Over(_)) => {
                entry.stage = Stage::Asking;
                let caller = entry.caller.clone();
                let request = entry.request.clone();
                drop(handed);
                self.ask(workspace_id, &caller, &request);
                Some(waiting_receipt(request.invocation))
            }
            // Running, or to be followed: the answer is still to come, and
            // what it settles is counted then.
            ReceiptStatus::Accepted => None,
            ReceiptStatus::Rejected { .. }
            | ReceiptStatus::Failed { .. }
            | ReceiptStatus::Cancelled { .. } => {
                answered(entry, &receipt.status);
                None
            }
        }
    }

    /// Ask the person `caller` acts for whether `request` — a page action of
    /// `workspace_id`'s window — may run, once. Their answer runs it as the
    /// caller asked it, carrying the witness the window accepts; a denial, or
    /// a question nobody answers, ends the invocation refused and nothing
    /// runs.
    fn ask(&self, workspace_id: u64, caller: &Caller, request: &InvokeRequest) {
        let Some(window) = self.window_of(workspace_id, request) else {
            return;
        };
        let (Some(&person), Some(&device)) = (caller.user_public_key(), caller.device_public_key())
        else {
            return;
        };
        let (Some((certification, key)), Some(hand)) = (self.certifying(), self.hand()) else {
            return;
        };
        let asked = Asked {
            person,
            window,
            device,
            request: request.clone(),
            until_ns: tilo_clock::unix_now_ns().saturating_add(answer_wait_ns(self.answer_wait)),
        };
        let question = asked.question(caller);
        let invocation = request.invocation;
        let mut run = request.clone();
        let handed = Arc::clone(&self.handed);
        tokio::runtime::Handle::current().spawn(async move {
            let confirmed =
                asked.confirmed(certification.confirm_for(person, question).await, &key);
            let mut handed = handed.lock_recover();
            let Some(entry) = handed.get_mut(&invocation) else {
                // The caller stopped waiting while the person decided: the
                // action runs not at all.
                return;
            };
            match confirmed {
                Ok(confirmed) => {
                    confirmed.apply(&mut run);
                    entry.stage = Stage::Allowed;
                    drop(handed);
                    // The invocation runs once, as the caller asked it, with
                    // the person's answer: the window that refused it accepts
                    // this one, and answers the caller itself.
                    hand.hand(
                        workspace_id,
                        (device, person),
                        window,
                        RemoteActionMessage::Invoke(run),
                    );
                }
                Err(error) => over(entry, update_of_error(invocation, &error)),
            }
        });
    }

    /// Whether `request` of `device`'s is one this host is already asking the
    /// person about, or has handed over with their answer.
    fn waits_for_the_person(&self, request: &InvokeRequest, device: [u8; 32]) -> bool {
        self.handed
            .lock_recover()
            .get(&request.invocation)
            .is_some_and(|entry| {
                entry.caller.device_public_key() == Some(&device)
                    && matches!(entry.stage, Stage::Asking | Stage::Allowed)
            })
    }

    /// Answer a caller asking how one of a window's invocations went
    /// ([`RemoteActionMessage::Observe`]) or stopping one
    /// ([`RemoteActionMessage::Cancel`]).
    fn asked_after(
        &self,
        caller: &Caller,
        message: &RemoteActionMessage,
    ) -> Option<crate::app_serving::Routing> {
        match message {
            RemoteActionMessage::Observe {
                invocation,
                wait_ms,
                ..
            } => self.observe(caller, *invocation, *wait_ms),
            RemoteActionMessage::Cancel { invocation } => self.cancel(caller, *invocation),
            _ => None,
        }
    }

    /// How `invocation` of `caller`'s went: its state now, or — for a caller
    /// that waits — as soon as it moves or its wait is over. `None` for one
    /// this host does not hold, which is answered as it always was.
    fn observe(
        &self,
        caller: &Caller,
        invocation: tilo_actions::InvocationId,
        wait_ms: Option<u32>,
    ) -> Option<crate::app_serving::Routing> {
        let device = *caller.device_public_key()?;
        let mut handed = self.handed.lock_recover();
        let entry = handed.get_mut(&invocation)?;
        if entry.caller.device_public_key() != Some(&device) {
            return None;
        }
        match &entry.stage {
            // The window holds it, and answers whoever asks after it.
            Stage::Running => return Some(crate::app_serving::Routing::Device(entry.window)),
            Stage::Over(update) => {
                return Some(state_routing(invocation, update.clone()));
            }
            Stage::Asking | Stage::Allowed => {}
        }
        let wait = Duration::from_millis(u64::from(wait_ms.unwrap_or_default()));
        if wait.is_zero() {
            return Some(state_routing(invocation, entry.stage.state(invocation)));
        }
        let token = self.next_waiter.fetch_add(1, Ordering::Relaxed);
        let (completer, answer) = pending();
        entry.waiting.push((token, completer));
        let handed = Arc::clone(&self.handed);
        tokio::runtime::Handle::current().spawn(async move {
            tokio::time::sleep(wait).await;
            let found = {
                let mut handed = handed.lock_recover();
                handed.get_mut(&invocation).and_then(|entry| {
                    let at = entry
                        .waiting
                        .iter()
                        .position(|(found, _)| *found == token)?;
                    let (_, waiting) = entry.waiting.remove(at);
                    Some((waiting, entry.stage.state(invocation)))
                })
            };
            if let Some((waiting, state)) = found {
                waiting.complete(state_message(invocation, state));
            }
        });
        Some(crate::app_serving::Routing::Answer(answer))
    }

    /// Stop `invocation` of `caller`'s: while the person is asked it never
    /// runs, and while the window has it the window stops it.
    fn cancel(
        &self,
        caller: &Caller,
        invocation: tilo_actions::InvocationId,
    ) -> Option<crate::app_serving::Routing> {
        let device = *caller.device_public_key()?;
        let mut handed = self.handed.lock_recover();
        let entry = handed.get_mut(&invocation)?;
        if entry.caller.device_public_key() != Some(&device) {
            return None;
        }
        let stopped = match &entry.stage {
            Stage::Running | Stage::Allowed => {
                return Some(crate::app_serving::Routing::Device(entry.window));
            }
            Stage::Asking => Ok(()),
            Stage::Over(_) => Err(ActionHostError::NotCancellable {
                reason: "it is over".to_owned(),
            }),
        };
        if stopped.is_ok() {
            handed.remove(&invocation);
        }
        Some(crate::app_serving::Routing::Answer(Pending::ready(
            RemoteActionMessage::CancelResponse {
                invocation,
                result: stopped,
            },
        )))
    }

    /// The window `workspace_id`'s page action `request` is about: `None` for
    /// anything that is not a page action of a window up here.
    fn window_of(&self, workspace_id: u64, request: &InvokeRequest) -> Option<[u8; 32]> {
        let open = self.open.lock_recover();
        let up = open.get(&workspace_id)?;
        page_action(&request.target, &up.window.provider).then_some(up.window.device)
    }

    /// Where the person is asked about a page action, and the key their answer
    /// is witnessed with: this host's own, once it runs.
    fn certifying(&self) -> Option<(CertificationProtocol, SigningKey)> {
        let authority = self.authority.lock_recover();
        let authority = authority.as_ref()?;
        Some((authority.certification.clone(), authority.key.clone()))
    }

    /// Where a frame this host hands over after its own answer goes: the
    /// relay's own delivery to the device holding the window.
    fn hand(&self) -> Option<crate::app_serving::Hand> {
        let authority = self.authority.lock_recover();
        Some(authority.as_ref()?.hand.clone())
    }

    /// Whether `token` is this host's answer for exactly this page action:
    /// the answer of the host that certified `workspace_id`'s window, for the
    /// person it serves, naming this caller, this invocation, this action and
    /// these arguments, and still good.
    fn witnessed(
        &self,
        token: &str,
        workspace_id: u64,
        caller: [u8; 32],
        request: &InvokeRequest,
    ) -> bool {
        let Some(witness) = HostWitness::decode(token) else {
            return false;
        };
        let open = self.open.lock_recover();
        let Some(up) = open.get(&workspace_id) else {
            return false;
        };
        witness.stands_for(&up.device, tilo_clock::unix_now_ns())
            && witness.caller_device_public_key == caller
            && witness.invocation == *request.invocation.0.as_bytes()
            && witness.provider == request.target.provider
            && witness.resource == request.target.address.resource
            && witness.action == request.target.address.action
            && witness.argument == argument_digest(request.argument.to_slot().as_deref())
    }

    /// Whether `workspace_id` has a page action with a window, whose answer
    /// this host counts.
    #[must_use]
    pub fn tracks(&self, workspace_id: u64) -> bool {
        self.open.lock_recover().contains_key(&workspace_id)
            && !self.handed.lock_recover().is_empty()
    }

    /// Remember that `invocation` of `caller`'s is with the window: what its
    /// answer is counted by, and what its caller is told about.
    fn hand_over(
        &self,
        workspace_id: u64,
        caller: Caller,
        window: [u8; 32],
        request: &InvokeRequest,
    ) {
        let workspace = self
            .open
            .lock_recover()
            .get(&workspace_id)
            .map(|up| up.workspace.clone())
            .unwrap_or_default();
        let mut handed = self.handed.lock_recover();
        if handed.len() >= HANDED && !handed.contains_key(&request.invocation) {
            handed.clear();
        }
        handed.insert(
            request.invocation,
            Handed {
                caller,
                window,
                workspace,
                request: request.clone(),
                stage: Stage::Running,
                waiting: Vec::new(),
            },
        );
    }

    /// The UUID of the workspace this host carries as `workspace_id`, when it
    /// serves one: what a page action and an session's device name it by.
    #[must_use]
    pub fn workspace_name(&self, workspace_id: u64) -> Option<String> {
        let authority = self.authority.lock_recover();
        let authority = authority.as_ref()?;
        let workspaces = authority.workspaces.as_ref()?;
        authority
            .runtime
            .block_on(workspaces.workspace_identity(workspace_id))
    }

    /// Place `workspace_id`'s Browser — the workspace whose UUID is
    /// `workspace` — on the record `record`, and answer the window once it is
    /// live. A Browser already up is the same window: a second open attaches
    /// to it.
    ///
    /// # Errors
    ///
    /// The words to refuse the open with: this host is not running, no device
    /// could be certified, or the session would not start.
    pub fn place(
        &self,
        workspace: &str,
        workspace_id: u64,
        record: &str,
    ) -> Result<Window, String> {
        let mut open = self.open.lock_recover();
        if let Some(up) = open.get(&workspace_id) {
            return Ok(up.window.clone());
        }
        let (device, material) = {
            let authority = self.authority.lock_recover();
            let authority = authority.as_ref().ok_or("this host is not running yet")?;
            let (device, secret) = certified(&authority.cert, &authority.key)?;
            let material = Material {
                workspace: workspace.to_owned(),
                workspace_id,
                record: record.to_owned(),
                device: device.clone(),
                secret,
                relay: authority.relay.clone(),
                user_public_key: authority.user_public_key,
                account: self.account.clone(),
            };
            (device, material)
        };
        // Confine the device to that record before it can ask for anything
        // and before the workspace lists it: the register is the relay's own
        // state, written down here, so a relay that restarts still confines
        // it. The device itself is asked nothing.
        let until_ns = device
            .expires_at_ns
            .unwrap_or_else(|| tilo_clock::unix_now_ns().saturating_add(lifetime_ns()));
        if let Err(error) = self.confine(
            &device,
            crate::environment::EnvironmentScope::for_record(workspace_id, APP, record),
            until_ns,
        ) {
            return Err(format!("Browser did not open: {error}"));
        }
        let window = match self.sessions.start(&material) {
            Ok(window) => window,
            Err(error) => {
                // The device never left this host, so nothing holds it: it is
                // forgotten rather than left a member of the workspace.
                self.forget(&device);
                return Err(error);
            }
        };
        self.list(workspace_id, &device, true);
        open.insert(
            workspace_id,
            Open {
                workspace: workspace.to_owned(),
                window: window.clone(),
                device,
            },
        );
        Ok(window)
    }

    /// The workspace's browser session stopped: refuse its device on every
    /// plane, end the session in the VM, forget the device and its window, and
    /// cut every connection the device has here.
    ///
    /// The refusal comes first and is written down, so a stop leaves the
    /// device refused even when what follows fails, and keeps it refused until
    /// the certificate it was given expires.
    pub fn stopped(&self, workspace_id: u64) {
        let stopped = self.open.lock_recover().remove(&workspace_id);
        let devices = {
            let authority = self.authority.lock_recover();
            authority.as_ref().map_or_else(Vec::new, |authority| {
                authority.environments.placed_in(workspace_id)
            })
        };
        self.refuse(
            devices
                .iter()
                .map(|(device, _, until_ns)| (*device, *until_ns)),
        );
        if let Some(stopped) = &stopped {
            self.sessions.stop(&stopped.workspace);
            self.list(workspace_id, &stopped.device, false);
        }
        // Every connection the devices had here audits on this: the planes
        // that carried them refuse them, and their sessions end.
        crate::authority_epoch::bump();
    }

    /// One environment's device stopped — its session ended and did not come
    /// back: refuse it on every plane, forget its window, end what is left of
    /// the session in the VM and delist it. A device placed for the same
    /// workspace since is another environment, and is not this stop's.
    ///
    /// The refusal comes first and is written down, so what follows may fail:
    /// the device stays refused until its certificate expires.
    fn stopped_device(&self, workspace_id: u64, device: [u8; 32], until_ns: u64) {
        let stopped = {
            let mut open = self.open.lock_recover();
            match open.get(&workspace_id) {
                Some(up) if up.window.device == device => open.remove(&workspace_id),
                Some(_) | None => None,
            }
        };
        self.refuse(std::iter::once((device, until_ns)));
        if let Some(stopped) = &stopped {
            self.sessions.stop(&stopped.workspace);
            self.list(workspace_id, &stopped.device, false);
        }
        crate::authority_epoch::bump();
    }

    /// Refuse `devices`, on every plane, until the certificate each was given
    /// expires. A refusal that cannot be written down is still refused here,
    /// and a restart confines it no wider than its scope.
    fn refuse(&self, devices: impl Iterator<Item = ([u8; 32], u64)>) {
        let authority = self.authority.lock_recover();
        let Some(authority) = authority.as_ref() else {
            return;
        };
        for (device, until_ns) in devices {
            if let Err(error) = authority.environments.stop(device, until_ns) {
                tracing::error!(
                    %error,
                    device = %hex::encode(device),
                    "a stopped browser environment's device is refused here but was not kept"
                );
            }
        }
    }

    /// One of the workspace's environment devices ended its last session here
    /// and has not come back: its environment stops, and with it the window
    /// that session held.
    ///
    /// A session whose connection dropped is back in a moment and its window
    /// lives on, so the device is given [`RECONNECT`] first, and what decides
    /// is whether a session of it is here then.
    fn session_ended(self: &Arc<Self>, workspace_id: u64, device: [u8; 32]) {
        let (runtime, environments) = {
            let authority = self.authority.lock_recover();
            let Some(authority) = authority.as_ref() else {
                return;
            };
            (authority.runtime.clone(), authority.environments.clone())
        };
        let browsers = Arc::clone(self);
        runtime.spawn(async move {
            tokio::time::sleep(browsers.reconnect).await;
            if environments.connected(&device)
                || crate::environment::environment_of(&environments, &device).is_none()
            {
                return;
            }
            let until_ns = browsers
                .open
                .lock_recover()
                .get(&workspace_id)
                .filter(|up| up.window.device == device)
                .and_then(|up| up.device.expires_at_ns)
                .unwrap_or_else(|| tilo_clock::unix_now_ns().saturating_add(lifetime_ns()));
            // Stopping asks VM hosts and writes workspace membership: never on
            // a runtime thread.
            let _ = tokio::task::spawn_blocking(move || {
                browsers.stopped_device(workspace_id, device, until_ns);
            })
            .await;
        });
    }

    /// Register what a session's device reaches, before it is handed to the
    /// session and listed in its workspace.
    fn confine(
        &self,
        device: &DeviceCert,
        scope: crate::environment::EnvironmentScope,
        until_ns: u64,
    ) -> Result<(), String> {
        let authority = self.authority.lock_recover();
        let Some(authority) = authority.as_ref() else {
            return Err("this host is not running yet".to_owned());
        };
        authority
            .environments
            .place(device.device_public_key, scope, until_ns)
    }

    /// Forget a device that never left this host: one whose session could not
    /// be started.
    fn forget(&self, device: &DeviceCert) {
        let authority = self.authority.lock_recover();
        let Some(authority) = authority.as_ref() else {
            return;
        };
        authority.environments.forget(&device.device_public_key);
    }

    /// List (or forget) the session's device in the workspace it serves,
    /// as a member device of the person the workspace belongs to.
    fn list(&self, workspace_id: u64, device: &DeviceCert, listed: bool) {
        let authority = self.authority.lock_recover();
        let Some(authority) = authority.as_ref() else {
            return;
        };
        let Some(workspaces) = &authority.workspaces else {
            return;
        };
        let listed = authority.runtime.block_on(workspaces.list_device(
            workspace_id,
            device.device_public_key,
            listed,
        ));
        if let Err(error) = listed {
            tracing::warn!(
                %error,
                device = %hex::encode(device.device_public_key),
                "a browser session's device was not listed in its workspace"
            );
        }
    }
}

/// [`DEVICE_LIFETIME`] in nanoseconds: how long a session's certificate lasts.
fn lifetime_ns() -> u64 {
    u64::try_from(DEVICE_LIFETIME.as_nanos()).unwrap_or(u64::MAX)
}

/// A fresh device for a session, certified by the host's stand-in as a
/// member device of the workspace's person, for as long as the session
/// may run.
fn certified(stand_in: &DeviceCert, key: &SigningKey) -> Result<(DeviceCert, [u8; 32]), String> {
    if !stand_in.capabilities.stand_in {
        return Err(
            "this host holds no stand-in certificate for the workspace's person, so it cannot \
             make a browser session a device"
                .to_owned(),
        );
    }
    let mut secret = [0_u8; 32];
    getrandom::fill(&mut secret).map_err(|error| error.to_string())?;
    let device = SigningKey::from_bytes(&secret);
    let now = tilo_clock::unix_now_ns();
    let mut minted = DeviceCert {
        user_public_key: stand_in.user_public_key,
        device_public_key: device.verifying_key().to_bytes(),
        device_label: DEVICE_LABEL.to_owned(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: now,
        expires_at_ns: Some(now.saturating_add(lifetime_ns())),
        signature: [0; 64],
        issuer: Some(Box::new(stand_in.clone())),
    };
    sign_device_cert(&mut minted, key).map_err(|error| error.to_string())?;
    Ok((minted, secret))
}

/// Whether `target` is an action of the page `provider`'s window publishes:
/// the window's provider ([`Window::provider`]) and the page's resource.
#[must_use]
pub fn page_action(target: &RemoteActionTarget, provider: &str) -> bool {
    target.provider == provider && logical_resource_id(&target.address.resource) == PAGE_RESOURCE
}

/// One page action the person is asked about, and what their answer is
/// turned into for the window that refused it.
struct Asked {
    /// The person asked: their approving devices answer it.
    person: [u8; 32],
    /// The window's device, which their answer names as its target.
    window: [u8; 32],
    /// The caller's device, which it names as the one that asked.
    device: [u8; 32],
    /// The action as the caller asked it: what the question shows, and what
    /// their answer is good for running.
    request: InvokeRequest,
    /// When the question stops waiting for an answer.
    until_ns: u64,
}

impl Asked {
    /// The question this puts to the person, which they answer once.
    fn question(&self, caller: &Caller) -> PendingConfirmation {
        question(
            caller.workspace().unwrap_or_default(),
            caller,
            &self.request,
            self.until_ns,
        )
    }

    /// `answer` as this invocation's confirmation, or the refusal it amounts
    /// to: the witness `key` signs, for exactly this action and these
    /// arguments, naming this caller and this window.
    fn confirmed(
        &self,
        answer: Result<ConfirmationAnswer, String>,
        key: &SigningKey,
    ) -> Result<ConfirmedInvocation, ActionHostError> {
        match answer {
            // The question shows no draft, so nothing in it is changed.
            Ok(ConfirmationAnswer::AllowOnce) => {
                let now = tilo_clock::unix_now_ns();
                let mut witness = HostWitness::unsigned(
                    Parties {
                        person: self.person,
                        host: key.verifying_key().to_bytes(),
                        caller: self.device,
                        target: self.window,
                    },
                    Answered {
                        invocation: *self.request.invocation.0.as_bytes(),
                        provider: self.request.target.provider.clone(),
                        resource: self.request.target.address.resource.clone(),
                        action: self.request.target.address.action.clone(),
                        argument: argument_digest(self.request.argument.to_slot().as_deref()),
                    },
                    (now, now.saturating_add(WITNESS_LIFETIME)),
                );
                witness
                    .sign(key)
                    .map(|()| {
                        ConfirmedInvocation::unchanged(Confirmation::Witnessed(witness.encode()))
                    })
                    .map_err(|error| ActionHostError::Unavailable {
                        reason: format!("the host could not answer for the person: {error}"),
                    })
            }
            Ok(ConfirmationAnswer::Deny) => Err(ActionHostError::NotPermitted),
            Err(_) => Err(ActionHostError::ConfirmationRequired),
        }
    }
}

/// `invocation` accepted, and to be followed: what a caller is answered with
/// while the person is asked about it.
fn waiting_receipt(invocation: tilo_actions::InvocationId) -> RemoteActionMessage {
    RemoteActionMessage::Receipt(InvocationReceipt {
        invocation,
        status: ReceiptStatus::Accepted,
    })
}

/// [`waiting_receipt`], as where a caller's frame goes.
fn waiting(invocation: tilo_actions::InvocationId) -> crate::app_serving::Routing {
    crate::app_serving::Routing::Answer(Pending::ready(waiting_receipt(invocation)))
}

/// `update` as the answer a caller asking after `invocation` receives.
fn state_message(
    invocation: tilo_actions::InvocationId,
    update: InvocationUpdate,
) -> RemoteActionMessage {
    RemoteActionMessage::Update {
        invocation,
        state: Some(InvocationState::new(invocation, update)),
    }
}

/// [`state_message`], as where a caller's frame goes.
fn state_routing(
    invocation: tilo_actions::InvocationId,
    update: InvocationUpdate,
) -> crate::app_serving::Routing {
    crate::app_serving::Routing::Answer(Pending::ready(state_message(invocation, update)))
}

/// `message` — a request of a window's — answered at once with `error`.
fn answered_with(
    message: &RemoteActionMessage,
    error: ActionHostError,
) -> Option<crate::app_serving::Routing> {
    Some(crate::app_serving::Routing::Answer(Pending::ready(
        tilo_actions::remote::refuse(message, error)?,
    )))
}

/// Note that a handed page action is over, as `status` says: what a caller
/// asking after it is answered with, and what the callers waiting on it are
/// answered with now.
fn answered(entry: &mut Handed, status: &ReceiptStatus) {
    over(entry, update_of(entry.request.invocation, status));
}

/// Note that a handed page action is over with `update`, and answer the
/// callers waiting on it.
fn over(entry: &mut Handed, update: InvocationUpdate) {
    entry.stage = Stage::Over(update.clone());
    let state = state_message(entry.request.invocation, update);
    for (_, waiting) in entry.waiting.drain(..) {
        waiting.complete(state.clone());
    }
}

/// How an invocation ends, as `status` says: the update its callers read.
fn update_of(invocation: tilo_actions::InvocationId, status: &ReceiptStatus) -> InvocationUpdate {
    match status {
        ReceiptStatus::Accepted => InvocationUpdate::Accepted { invocation },
        ReceiptStatus::Completed { output } => InvocationUpdate::Succeeded {
            invocation,
            output: output.clone(),
        },
        ReceiptStatus::Failed { error } => InvocationUpdate::Failed {
            invocation,
            error: error.clone(),
        },
        ReceiptStatus::Rejected { error } => update_of_error(invocation, error),
        ReceiptStatus::Cancelled { message } => InvocationUpdate::Cancelled {
            invocation,
            message: message.clone(),
        },
    }
}

/// `error` as the failure a caller reads: its typed code and what it says.
fn update_of_error(
    invocation: tilo_actions::InvocationId,
    error: &ActionHostError,
) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation,
        error: ActionFailure {
            code: error.code().to_owned(),
            message: error.to_string(),
            details: None,
            retryable: false,
        },
    }
}

/// The question `request` puts to the person, as their approving devices
/// draw it: the agent that asked, by the name it goes by, the action as the
/// caller described it, and the workspace it acts in.
fn question(
    workspace: &str,
    caller: &tilo_actions::remote::Caller,
    request: &InvokeRequest,
    until_ns: u64,
) -> PendingConfirmation {
    let mut id = [0_u8; 32];
    let _ = getrandom::fill(&mut id);
    let address = &request.target.address;
    let context = request.context.as_ref();
    let agent_id = match caller.principal() {
        tilo_actions::remote::CallerPrincipal::Agent(agent) => agent.clone(),
        _ => String::new(),
    };
    PendingConfirmation {
        id,
        agent_id,
        agent_label: caller
            .agent_name()
            .map_or_else(|| address.action.clone(), str::to_owned),
        action_label: context
            .and_then(|context| context.purpose.clone())
            .unwrap_or_else(|| address.action.clone()),
        resource_label: context
            .and_then(|context| context.resource.as_ref())
            .map_or_else(
                || address.resource.clone(),
                |resource| resource.label.clone(),
            ),
        risk: context
            .and_then(|context| {
                context
                    .facets
                    .iter()
                    .find(|facet| facet.key == tilo_actions::RISK_FACET)
            })
            .map_or_else(|| "destructive".to_owned(), |facet| facet.value.clone()),
        workspace: workspace.to_owned(),
        expires_at_ns: until_ns,
        ..PendingConfirmation::default()
    }
}

/// [`Reason::NoPaymentMethod`] for an account whose period's free actions are
/// used up, with the one step that would let the next action through.
#[must_use]
pub fn no_payment_method(free: u64) -> Refusal {
    Refusal {
        reason: Reason::NoPaymentMethod,
        message: format!(
            "This period's {free} free Browser actions are used up. A card continues them."
        ),
        next: vec![NextStep {
            kind: NextStepKind::AddPaymentMethod,
            url: None,
        }],
    }
}

/// What a host that places no Browser refuses an open of it with: words that
/// say why, since a Browser window needs a session this host has not.
#[must_use]
pub fn unplaced() -> String {
    "Browser does not open here: this host places no browser session, and a Browser \
     window needs one. Open it on your own device instead."
        .to_owned()
}

/// The answer to an open of Browser that ran in `workspace_id`: `message`
/// with the window placed, or the refusal in its place.
///
/// The open answered once its record was made; this places the session on
/// that record, and the open answers once the window is live.
#[must_use]
pub fn placed(
    browsers: &HostedBrowsers,
    workspace_id: u64,
    invocation: tilo_actions::InvocationId,
    message: RemoteActionMessage,
) -> RemoteActionMessage {
    let Some(record) = opened_record(&message) else {
        return message;
    };
    let placement = browsers
        .workspace_name(workspace_id)
        .ok_or_else(|| "this host serves no such workspace".to_owned())
        .and_then(|workspace| browsers.place(&workspace, workspace_id, &record));
    match placement {
        Ok(_) => message,
        Err(reason) => refused(invocation, format!("Browser did not open: {reason}")),
    }
}

/// The answer to `invocation` refused with `reason`.
#[must_use]
pub fn refused(invocation: tilo_actions::InvocationId, reason: String) -> RemoteActionMessage {
    RemoteActionMessage::Receipt(tilo_actions::remote::InvocationReceipt {
        invocation,
        status: tilo_actions::remote::ReceiptStatus::Rejected {
            error: ActionHostError::Refused { reason },
        },
    })
}

/// Whether `request` opens Browser (`app.so.tilo.browser#open`): the open the
/// plane places a session after.
#[must_use]
pub fn opens_browser(request: &InvokeRequest) -> bool {
    provider_workspace(&request.target.provider).is_some()
        && request.target.address.resource == format!("{APPLICATION_RESOURCE_PREFIX}{APP}")
        && request.target.address.action == APPLICATION_OPEN_ACTION
}

/// The record an answered [`opens_browser`] open made, when it made one: the
/// record its answer names.
#[must_use]
pub fn opened_record(message: &RemoteActionMessage) -> Option<String> {
    let RemoteActionMessage::Receipt(receipt) = message else {
        return None;
    };
    let tilo_actions::remote::ReceiptStatus::Completed { output } = &receipt.status else {
        return None;
    };
    output
        .value
        .as_ref()
        .and_then(|value| value.get("record"))
        .and_then(|record| record.as_str())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::ActionAddress;
    use tilo_actions::ActionOutput;

    fn page(provider: &str, resource: &str) -> RemoteActionTarget {
        RemoteActionTarget {
            provider: provider.to_owned(),
            address: ActionAddress::new(resource, "read-page"),
        }
    }

    #[test]
    fn a_windows_page_action_is_the_one_addressed_to_that_window() {
        assert!(page_action(
            &page("browser@42", PAGE_RESOURCE),
            "browser@42"
        ));
        assert!(
            !page_action(&page("browser@42", PAGE_RESOURCE), "browser@7"),
            "another window's page is another device's action"
        );
        assert!(
            !page_action(&page("browser@42", "browser.tab"), "browser@42"),
            "the record's own actions are the host's, not the window's"
        );
    }

    #[test]
    fn an_open_of_browser_is_the_one_that_places_a_session() {
        use tilo_actions::remote::{APPLICATION_OPEN_ACTION, workspace_provider};
        let open = |resource: &str, action: &str| InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            on_behalf_of: None,
            target: RemoteActionTarget {
                provider: workspace_provider("ws"),
                address: ActionAddress::new(resource, action),
            },
            argument: tilo_actions::remote::InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        };
        assert!(opens_browser(&open(
            "app.so.tilo.browser",
            APPLICATION_OPEN_ACTION
        )));
        assert!(!opens_browser(&open(
            "app.so.tilo.notes",
            APPLICATION_OPEN_ACTION
        )));
    }

    #[test]
    fn an_open_that_made_a_record_answers_it_by_its_id() {
        let made = RemoteActionMessage::Receipt(tilo_actions::remote::InvocationReceipt {
            invocation: tilo_actions::InvocationId::new(),
            status: tilo_actions::remote::ReceiptStatus::Completed {
                output: ActionOutput {
                    value: Some(
                        serde_json::json!({"provider": "record:so.tilo.browser:abc", "record": "abc"}),
                    ),
                    resources: Vec::new(),
                },
            },
        });
        assert_eq!(opened_record(&made).as_deref(), Some("abc"));
        let refused = refused(tilo_actions::InvocationId::new(), unplaced());
        assert_eq!(opened_record(&refused), None);
    }

    #[test]
    fn a_host_serves_browsers_only_when_it_names_where_they_start_where_it_is_reached_and_what_it_counts()
     {
        use crate::browser_actions::BrowserMeter;
        use crate::placed_shells::{MEMBERS_ENV, PLACER_ENV};
        use tilo_host_account::{ACCOUNT_ENV, LEDGER_ENV, PLANS_ENV};

        let mut env = tilo_test_env::ScopedEnv::new();
        env.remove(PLACER_ENV)
            .remove(MEMBERS_ENV)
            .remove(ACCOUNT_ENV)
            .remove(LEDGER_ENV)
            .remove(PLANS_ENV)
            .remove(RELAY_ENV);
        assert!(
            HostedBrowsers::from_env().unwrap().is_none(),
            "a host with no placer places no Browser"
        );

        // A placer alone is half a host: without a ledger it would serve
        // Browser free of charge.
        let dir = tempfile::tempdir().unwrap();
        let members = dir.path().join("members.json");
        std::fs::write(&members, "{}").unwrap();
        env.set(PLACER_ENV, "vm-placer:7702")
            .set(MEMBERS_ENV, &members)
            .set(ACCOUNT_ENV, "acct-1");
        let error = HostedBrowsers::from_env().err().unwrap();
        assert!(error.contains(LEDGER_ENV), "{error}");
        assert!(BrowserMeter::from_env().unwrap().is_none());

        // With the ledger but no address, its sessions would be told to reach
        // it where they cannot.
        env.set(LEDGER_ENV, dir.path().join("actions.jsonl"))
            .set(PLANS_ENV, dir.path().join("plans.json"));
        let error = HostedBrowsers::from_env().err().unwrap();
        assert!(error.contains(RELAY_ENV), "{error}");

        // All of them: this host places its workspaces' Browser.
        let key = hex::encode(
            ed25519_dalek::SigningKey::from_bytes(&[7; 32])
                .verifying_key()
                .to_bytes(),
        );
        env.set(RELAY_ENV, format!("{key}@127.0.0.1:7704"));
        assert!(HostedBrowsers::from_env().unwrap().is_some());
    }

    #[test]
    fn a_host_that_places_nothing_says_why_instead_of_naming_an_unknown_app() {
        let words = unplaced();
        assert!(words.contains("browser session"), "{words}");
        assert!(!words.contains("no app"), "{words}");
    }
}
