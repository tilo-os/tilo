//! Serving a ticket: the certify action a computer with no certificate yet calls on the device
//! that issued its ticket ([`tilo_device_identity::login`]).
//!
//! [`ServedTicket`] answers [`CERTIFY`] alone, under [`PROVIDER`], on the scope an
//! [`ActionService`](crate::actions::ActionService) admits a device with no certificate to
//! ([`ActionService::admit_uncertified`](crate::actions::ActionService::admit_uncertified) with
//! [`SCOPE`](tilo_device_identity::login::SCOPE)). The call is checked here, in this order: it
//! names the certify action; its input is a [`CertifyInput`] whose key is the key the connection
//! came from; it spends the ticket's secret, once, while the ticket is live. The computer is then
//! told its call was accepted and follows it, while the [`Approver`] is asked. Its answer is
//! checked against the approver's own certificate ([`check_approval`]), and only then does the
//! invocation succeed with the [`CertifyAnswer`]. A decline, or an answer that does not check,
//! fails it with that sentence.
//!
//! A device serves every ticket through its one [`Tickets`], whoever issued the ticket there.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending,
};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use tilo_device_identity::login::{
    Approver, CERTIFY, CertifyAnswer, CertifyInput, Entry, PROVIDER, RESOURCE, Secret, Ticket,
    asked, check_approval,
};
use tilo_lock::MutexRecover as _;
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::watch;

mod tickets;

pub use tickets::{LookAgain, PageHost, Refusal, Tickets};

/// How long a follower waiting for the approval is held before it looks again.
const LOOK_AGAIN: Duration = Duration::from_secs(1);

/// The ticket a device issued, served on its action service until a computer is certified with
/// it.
#[derive(Clone)]
pub struct ServedTicket(Arc<Inner>);

struct Inner {
    ticket: Ticket,
    secret: Secret,
    /// The person whose root certifies the computer.
    user_public_key: [u8; 32],
    /// Where the certified computer enters.
    enter: Entry,
    approver: Arc<dyn Approver>,
    /// What the computer becomes: a member device, or one that administers the person's others.
    capability: tilo_user_identity::DeviceCapabilities,
    /// Whether it adds a browser, which a pairing link adds: a member device of the person's
    /// for a browser's lifetime, whatever system it says it runs.
    browser: bool,
    revocations: RevocationList,
    /// Each call taken, by invocation: the device that made it, and where it is.
    calls: Mutex<HashMap<InvocationId, ([u8; 32], InvocationState)>>,
    /// The certificate given, once one is: what the device that issued the ticket waits on.
    certified: watch::Sender<Option<DeviceCert>>,
    /// Whether the computer read how its call ended: until then the issuer keeps serving.
    told: watch::Sender<bool>,
    runtime: tokio::runtime::Handle,
}

impl std::fmt::Debug for ServedTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServedTicket")
            .field("issuer", &hex::encode(self.0.ticket.issuer.endpoint))
            .finish_non_exhaustive()
    }
}

impl ServedTicket {
    /// Serve `ticket`, live until `expires_at_ns`, for a computer to become a member device of
    /// `user_public_key`, entering at `enter`, once `approver` approves it. A certificate whose
    /// approver `revocations` refuses is never handed on. A ticket served again after its issuer
    /// restarted keeps the expiry it was issued with.
    ///
    /// # Panics
    ///
    /// Outside a runtime context, where the approval could not be waited for.
    #[must_use]
    pub fn new(
        ticket: Ticket,
        user_public_key: [u8; 32],
        enter: Entry,
        capability: tilo_user_identity::DeviceCapabilities,
        approver: Arc<dyn Approver>,
        revocations: RevocationList,
        expires_at_ns: u64,
    ) -> Self {
        Self::serving(
            ticket,
            user_public_key,
            enter,
            (capability, false),
            approver,
            revocations,
            expires_at_ns,
        )
    }

    /// [`Self::new`] for a browser, which a pairing link adds: certified a member of the
    /// person's for a browser's lifetime
    /// ([`tilo_user_identity::CertificationPolicy::browser`]).
    ///
    /// # Panics
    ///
    /// Outside a runtime context, where the approval could not be waited for.
    #[must_use]
    pub fn for_browser(
        ticket: Ticket,
        user_public_key: [u8; 32],
        enter: Entry,
        approver: Arc<dyn Approver>,
        revocations: RevocationList,
        expires_at_ns: u64,
    ) -> Self {
        Self::serving(
            ticket,
            user_public_key,
            enter,
            (tilo_user_identity::DeviceCapabilities::member(), true),
            approver,
            revocations,
            expires_at_ns,
        )
    }

    fn serving(
        ticket: Ticket,
        user_public_key: [u8; 32],
        enter: Entry,
        (capability, browser): (tilo_user_identity::DeviceCapabilities, bool),
        approver: Arc<dyn Approver>,
        revocations: RevocationList,
        expires_at_ns: u64,
    ) -> Self {
        let secret = Secret::of(&ticket, 0, expires_at_ns);
        Self(Arc::new(Inner {
            ticket,
            secret,
            user_public_key,
            enter,
            approver,
            capability,
            browser,
            revocations,
            calls: Mutex::default(),
            certified: watch::Sender::new(None),
            told: watch::Sender::new(false),
            runtime: tokio::runtime::Handle::current(),
        }))
    }

    /// The ticket served.
    #[must_use]
    pub fn ticket(&self) -> &Ticket {
        &self.0.ticket
    }

    /// The certificate this ticket gave, once it gave one.
    #[must_use]
    pub fn certified(&self) -> watch::Receiver<Option<DeviceCert>> {
        self.0.certified.subscribe()
    }

    /// Whether the computer has read how its call ended, once it has: the issuer serves until
    /// then, or the computer would never learn its certificate.
    #[must_use]
    pub fn told(&self) -> watch::Receiver<bool> {
        self.0.told.subscribe()
    }

    /// Whether the ticket still waits for its computer.
    #[must_use]
    pub fn waiting(&self) -> bool {
        self.0.secret.waiting(tilo_clock::unix_now_ns())
    }

    /// Whether this ticket is done with: nothing waits for it, and every computer that called it
    /// read how its call ended.
    fn finished(&self) -> bool {
        !self.waiting()
            && self
                .0
                .calls
                .lock_recover()
                .values()
                .all(|(_, state)| !matches!(state.update, InvocationUpdate::Accepted { .. }))
            && (self.0.calls.lock_recover().is_empty() || *self.0.told.borrow())
    }

    fn has_call(&self, id: &InvocationId) -> bool {
        self.0.calls.lock_recover().contains_key(id)
    }

    fn settle(&self, invocation: InvocationId, update: InvocationUpdate) {
        let state = InvocationState::new(invocation, update);
        if let Some(call) = self.0.calls.lock_recover().get_mut(&invocation) {
            call.1 = state;
        }
        // The followers wake on the next look; the issuer is told at once.
        self.0.certified.send_modify(|_| {});
    }

    /// Take `request` from `device`: what [`ActionProvider::invoke`] answers.
    fn take(
        &self,
        device: [u8; 32],
        request: &InvokeRequest,
    ) -> Result<InvocationReceipt, ActionCallError> {
        let address = &request.target.address;
        if request.target.provider != PROVIDER
            || address.resource != RESOURCE
            || address.action != CERTIFY
        {
            return Err(ActionCallError::UnknownAction);
        }
        if let Some((from, state)) = self.0.calls.lock_recover().get(&request.invocation) {
            // A retry of a call already taken is answered where it is; another device's is not
            // its to see.
            return if *from == device {
                Ok(state.receipt())
            } else {
                Err(ActionCallError::NotPermitted)
            };
        }
        let (input, label) = certify_input(device, request)?;
        let agent = input.agent.trim();
        let now = tilo_clock::unix_now_ns();
        self.0
            .secret
            .spend(&input.secret, now)
            .map_err(|why| ActionCallError::refused(why.to_string()))?;
        let capability = if self.0.browser {
            self.0.capability.clone()
        } else {
            tilo_user_identity::DeviceCapabilities {
                foreign: !input.tilo_os,
                ..self.0.capability.clone()
            }
        };
        let mut asked = asked(
            self.0.user_public_key,
            device,
            &label,
            agent,
            capability,
            &self.0.ticket.secret,
            now,
        )
        .map_err(|error| ActionCallError::refused(error.to_string()))?;
        if self.0.browser {
            let lifetime = tilo_user_identity::CertificationPolicy::browser().lifetime_ns;
            asked.cert.expires_at_ns = Some(asked.cert.issued_at_ns.saturating_add(lifetime));
        }
        let invocation = request.invocation;
        let accepted = InvocationState::new(invocation, InvocationUpdate::Accepted { invocation });
        let receipt = accepted.receipt();
        self.0
            .calls
            .lock_recover()
            .insert(invocation, (device, accepted));
        let served = self.clone();
        self.0.runtime.spawn(async move {
            let answer = served.0.approver.approve(asked.clone()).await;
            let update = match answer {
                Ok(approved) => match check_approval(
                    &asked,
                    &approved,
                    &served.0.revocations,
                    tilo_clock::unix_now_ns(),
                ) {
                    Ok(cert) => {
                        let answer = CertifyAnswer {
                            cert: cert.clone(),
                            enter: served.0.enter.clone(),
                        };
                        served.0.certified.send_replace(Some(cert));
                        InvocationUpdate::Succeeded {
                            invocation,
                            output: ActionOutput {
                                value: serde_json::to_value(&answer).ok(),
                                resources: Vec::new(),
                            },
                        }
                    }
                    Err(why) => failed(invocation, "not-approved", &why.to_string()),
                },
                Err(declined) => failed(invocation, "declined", &declined.0),
            };
            served.settle(invocation, update);
        });
        Ok(receipt)
    }
}

/// What a computer's certify call asks with, checked: its input, and the name it is listed by.
fn certify_input(
    device: [u8; 32],
    request: &InvokeRequest,
) -> Result<(CertifyInput, String), ActionCallError> {
    let InvokeArgument::Json(argument) = &request.argument else {
        return Err(ActionCallError::refused(
            "a computer asks with its key, its agent and the ticket's secret",
        ));
    };
    let input: CertifyInput = serde_json::from_value(serde_json::Value::Object(argument.clone()))
        .map_err(|_| {
        ActionCallError::refused("a computer asks with its key, its agent and the ticket's secret")
    })?;
    let mut key = [0; 32];
    if hex::decode_to_slice(&input.device_public_key, &mut key).is_err() || key != device {
        return Err(ActionCallError::refused(
            "a computer asks to be certified for the key it connects with",
        ));
    }
    let agent = input.agent.trim();
    if agent.is_empty() {
        return Err(ActionCallError::refused(
            "a computer names the agent that runs on it",
        ));
    }
    let label = fitted(input.label.as_deref().map_or(agent, str::trim));
    Ok((input, label))
}

/// Several tickets a device serves at once on the one scope its action service admits computers
/// with no certificate to: a planned machine's, which waits a day for its medium, beside the one
/// `tilo device add` prints now. A call goes to the ticket whose secret it brings, and is followed
/// there.
#[derive(Clone, Default)]
pub(crate) struct ServedTickets(Arc<Mutex<Vec<ServedTicket>>>);

impl std::fmt::Debug for ServedTickets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServedTickets")
            .field("served", &self.0.lock_recover().len())
            .finish()
    }
}

impl ServedTickets {
    /// Serve `ticket` beside the others, dropping those that are done with.
    pub(crate) fn serve(&self, ticket: ServedTicket) {
        let mut served = self.0.lock_recover();
        served.retain(|kept| !kept.finished());
        served.push(ticket);
    }

    /// Stop serving `ticket`: another took its place.
    pub(crate) fn withdraw(&self, ticket: &ServedTicket) {
        self.0
            .lock_recover()
            .retain(|kept| !Arc::ptr_eq(&kept.0, &ticket.0));
    }

    /// Whether a ticket served here takes `request`: one took it already, or its secret is the
    /// one the call brings.
    #[must_use]
    pub(crate) fn knows(&self, request: &InvokeRequest) -> bool {
        let secret = secret_of(request);
        self.0
            .lock_recover()
            .iter()
            .any(|kept| kept.has_call(&request.invocation) || kept.0.secret.is(secret))
    }

    /// The ticket the call `request` is for: the one that took it already, else the one whose
    /// secret it brings, else the newest, which refuses it.
    fn for_request(&self, request: &InvokeRequest) -> Option<ServedTicket> {
        let served = self.0.lock_recover();
        if let Some(taken) = served
            .iter()
            .find(|kept| kept.has_call(&request.invocation))
        {
            return Some(taken.clone());
        }
        let secret = secret_of(request);
        served
            .iter()
            .find(|kept| kept.0.secret.is(secret))
            .or_else(|| served.last())
            .cloned()
    }

    fn with_call(&self, id: &InvocationId) -> Option<ServedTicket> {
        self.0
            .lock_recover()
            .iter()
            .find(|kept| kept.has_call(id))
            .cloned()
    }
}

impl ActionProvider for ServedTickets {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let newest = self.0.lock_recover().last().cloned();
        match newest {
            Some(served) => served.query(caller, query),
            None => Pending::ready(Err(ActionCallError::UnknownAction)),
        }
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        match self.for_request(&request) {
            Some(served) => served.invoke(caller, request),
            None => Pending::ready(Err(ActionCallError::refused(
                "no ticket is served here any more",
            ))),
        }
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.with_call(id)?.observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.with_call(id)?.next_change(caller, id)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        match self.with_call(id) {
            Some(served) => served.cancel(caller, id),
            None => Pending::ready(Err(ActionCallError::UnknownAction)),
        }
    }
}

/// The secret a certify call brings, hex: empty when it brings none.
fn secret_of(request: &InvokeRequest) -> &str {
    match &request.argument {
        InvokeArgument::Json(argument) => argument
            .get("secret")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default(),
        _ => "",
    }
}

/// `label` within what a certificate's label carries.
fn fitted(label: &str) -> String {
    let room = tilo_user_identity::certification::MAX_CERTIFICATION_LABEL_BYTES;
    let end = (0..=room.min(label.len()))
        .rev()
        .find(|end| label.is_char_boundary(*end))
        .unwrap_or(0);
    label.get(..end).unwrap_or_default().to_owned()
}

fn failed(invocation: InvocationId, code: &str, message: &str) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation,
        error: ActionFailure {
            code: code.to_owned(),
            message: message.to_owned(),
            details: None,
            retryable: false,
        },
    }
}

impl ActionProvider for ServedTicket {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Err(ActionCallError::Unavailable {
            reason: "a computer with no certificate calls only the certify action".into(),
        }))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let Some(device) = caller.device_public_key().copied() else {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        };
        Pending::ready(self.take(device, &request))
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let device = caller.device_public_key()?;
        let state = self
            .0
            .calls
            .lock_recover()
            .get(id)
            .filter(|(from, _)| from == device)
            .map(|(_, state)| state.clone())?;
        if !matches!(state.update, InvocationUpdate::Accepted { .. }) {
            self.0.told.send_replace(true);
        }
        Some(state)
    }

    fn next_change(&self, _caller: &Caller, _id: &InvocationId) -> Option<Pending<()>> {
        let mut moved = self.0.certified.subscribe();
        let (done, change) = tilo_actions::remote::pending();
        self.0.runtime.spawn(async move {
            let _ = tokio::time::timeout(LOOK_AGAIN, moved.changed()).await;
            done.complete(());
        });
        Some(change)
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::NotCancellable {
            reason: "the person is asked already; their answer ends it".into(),
        }))
    }
}
