use super::*;
use crate::network::actor::HostIdentity;
use crate::shell::native_workspace_owner::NativeCommittedView;
use protocol::{CurrentView, OwnerView, PreparedRequest, Step};
use tilo_relay::delivery_v3::descriptor;

#[derive(Default)]
pub(crate) struct Registry {
    tickets: Vec<Weak<Ticket>>,
}
impl Registry {
    pub(crate) fn handle(&mut self, command: Command, identity: Option<&HostIdentity>) {
        if command.reply.is_closed() {
            return;
        }
        self.tickets.retain(|ticket| ticket.strong_count() != 0);
        let result = match identity {
            None => Err("native device identity unavailable".into()),
            Some(identity) => match command.operation {
                Operation::Open { owner, target } => self.open(owner, target, identity),
                Operation::Advance {
                    ticket,
                    serial,
                    body,
                } => self
                    .known(&ticket)
                    .and_then(|()| advance(&ticket, serial, &body, identity))
                    .inspect_err(|_| ticket.stop()),
                Operation::Act { ticket, action } => self
                    .known(&ticket)
                    .and_then(|()| act(&ticket, action, identity))
                    .inspect_err(|_| ticket.stop()),
                Operation::End { ticket } => self
                    .known_pointer(&ticket)
                    .and_then(|()| end(&ticket, identity)),
            },
        };
        // Dropping a successful Open reply also drops the only strong ticket.
        let _ = command.reply.send(result);
    }
    fn known(&self, ticket: &Arc<Ticket>) -> Result<(), String> {
        ticket.completion_live()?;
        self.known_pointer(ticket)
    }
    fn known_pointer(&self, ticket: &Arc<Ticket>) -> Result<(), String> {
        if !self
            .tickets
            .iter()
            .any(|known| known.ptr_eq(&Arc::downgrade(ticket)))
        {
            return Err("foreign native media ticket".into());
        }
        Ok(())
    }
    fn open(
        &mut self,
        owner: NativeWorkspaceOwner,
        target: CallTarget,
        identity: &HostIdentity,
    ) -> Result<Reply, String> {
        if self.tickets.len() >= 16 {
            return Err("native media capacity".into());
        }
        let room = RoomId::parse(&target.workspace).ok_or("invalid native room route")?;
        let (call, origin, wall, continuity, custody_expires) =
            owner.with_current(room, |view| {
                let anchor = Instant::now();
                let wall = now_ns()?;
                check_identity(view, identity, wall)?;
                let custody_expires = [Some(view.governor()), Some(view.device()), view.host()]
                    .into_iter()
                    .flatten()
                    .filter_map(|certificate| certificate.expires_at_ns)
                    .min()
                    .map(|expires| {
                        deadline(
                            anchor,
                            expires.checked_sub(wall).ok_or("native custody expired")?,
                        )
                    })
                    .transpose()?;
                let binding = view.peer().resolve_media_binding(
                    &target.app,
                    &target.record,
                    &target.channel,
                )?;
                let policy = view
                    .peer()
                    .validated_media_policy()?
                    .ok_or("native call policy missing")?;
                let mut calls = policy.calls.iter().filter(|(_, call)| {
                    call.binding == binding
                        && call.phase == qos_peer_state::room::media::CallPhaseV1::Active
                });
                let (id, _) = calls.next().ok_or("native governed call unavailable")?;
                if calls.next().is_some() {
                    return Err("ambiguous native governed call".into());
                }
                let locator =
                    tilo_relay::workspace_join::DeliveryOriginLocatorV1::from_transport_hint(
                        view.delivery_locator()
                            .ok_or("native discovery locator missing")?,
                        view.host().ok_or("native discovery host missing")?,
                    )
                    .map_err(|error| error.to_string())?;
                Ok((
                    hex::encode(id),
                    locator.origin,
                    wall,
                    view.continuity(),
                    custody_expires,
                ))
            })?;
        let ticket = Arc::new(Ticket {
            owner,
            room,
            target,
            alive: AtomicBool::new(true),
            leaving: AtomicBool::new(false),
            own_bootstrap: AtomicBool::new(false),
            own_continuity: AtomicU64::new(0),
            fences: AtomicU64::new(0),
            base: Instant::now(),
            capture_until: AtomicU64::new(0),
            state: Mutex::new(State {
                phase: Phase::Discover {
                    origin: origin.clone(),
                },
                serial: 0,
                pending: None,
                call,
                grant: None,
                carrier: None,
                generation: 0,
                wall_high_water: wall,
                bootstrap_at: Instant::now(),
                descriptor_expires: Instant::now(),
                custody_expires,
            }),
            stops: Mutex::new(Stops::default()),
            subscription: Mutex::new(None),
        });
        // Register while the actual owner publication is read-locked. A fence
        // cannot slip between successful admission and listener installation.
        ticket.owner.with_current(room, |view| {
            check_identity(view, identity, now_ns()?)?;
            if view.continuity() != continuity {
                return Err("native owner changed before intent installation".into());
            }
            let weak = Arc::downgrade(&ticket);
            let observer = Arc::new(move |changed, continuity| {
                if changed == room {
                    if let Some(ticket) = weak.upgrade() {
                        ticket.fence(continuity);
                    }
                }
            });
            *ticket.subscription.lock() = Some(ticket.owner.observe_fences(observer));
            Ok(())
        })?;
        self.tickets.push(Arc::downgrade(&ticket));
        let work = prepare(
            &mut ticket.state.lock(),
            PreparedRequest {
                id: 0,
                origin,
                target: "/peer/v3/descriptor",
                body: Vec::new(),
                proof: None,
            },
        )?;
        Ok(Reply::Opened(ticket, work))
    }
}

fn check_identity(
    view: &NativeCommittedView,
    identity: &HostIdentity,
    now: u64,
) -> Result<(), String> {
    let (cert, entries) = identity.current_native_material(view.workspace())?;
    if &cert != view.device()
        || identity.device_signing_key.verifying_key().to_bytes() != cert.device_public_key
    {
        return Err("native selected device custody changed".into());
    }
    let mut revoked = tilo_user_identity::RevocationList::new();
    for entry in entries {
        revoked.insert(entry).map_err(|error| error.to_string())?;
    }
    if revoked.is_revoked_at(&cert.device_public_key, now) {
        return Err("native selected device revoked".into());
    }
    view.peer().permissions(
        cert.device_public_key,
        SystemTime::UNIX_EPOCH + Duration::from_nanos(now),
    )?;
    Ok(())
}
fn signing_view<'a>(
    view: &'a NativeCommittedView,
    identity: &'a HostIdentity,
    now: u64,
) -> Result<OwnerView<'a>, String> {
    check_identity(view, identity, now)?;
    Ok(OwnerView {
        peer: view.peer(),
        key: &identity.device_signing_key,
        governor: view.governor(),
        host: view.host().ok_or("native delivery host absent")?,
        device: view.device(),
        workspace: view.workspace(),
        continuity: view.continuity(),
        generation: view.descriptor_expectation()?,
        now_ns: now,
    })
}
pub(super) fn current_view(
    view: &NativeCommittedView,
    now: u64,
) -> Result<CurrentView<'_>, String> {
    Ok(CurrentView {
        peer: view.peer(),
        governor: view.governor(),
        host: view.host().ok_or("native delivery host absent")?,
        device: view.device(),
        workspace: view.workspace(),
        continuity: view.continuity(),
        generation: view.descriptor_expectation()?,
        now_ns: now,
    })
}
fn prepare(state: &mut State, request: PreparedRequest) -> Result<Work, String> {
    if state.pending.is_some() {
        return Err("native control operation already pending".into());
    }
    state.serial = state
        .serial
        .checked_add(1)
        .ok_or("native control serial exhausted")?;
    state.pending = Some((state.serial, request.id));
    Ok(Work {
        serial: state.serial,
        request,
    })
}
fn tick_clock(state: &mut State, now: u64) -> Result<(), String> {
    if now < state.wall_high_water {
        return Err("native media clock moved backwards".into());
    }
    state.wall_high_water = now;
    Ok(())
}
fn advance(
    ticket: &Arc<Ticket>,
    serial: u64,
    body: &[u8],
    identity: &HostIdentity,
) -> Result<Reply, String> {
    ticket.completion_live()?;
    let (phase, id) = ticket.owner.with_current(ticket.room, |view| {
        let now = now_ns()?;
        check_identity(view, identity, now)?;
        let mut state = ticket.state.lock();
        tick_clock(&mut state, now)?;
        let (expected, id) = state.pending.take().ok_or("no pending native operation")?;
        if expected != serial {
            return Err("stale native operation completion".into());
        }
        Ok((std::mem::replace(&mut state.phase, Phase::Transition), id))
    })?;
    match phase {
        Phase::Discover { origin } => {
            if body.is_empty() || body.len() > descriptor::MAX_HINT_BYTES {
                return Err("native descriptor response bounds".into());
            }
            let hint: qos_peer_state::join::TransportHint =
                postcard::from_bytes(body).map_err(|error| error.to_string())?;
            let mut canonical = [0; descriptor::MAX_HINT_BYTES];
            if postcard::to_slice(&hint, &mut canonical).map_err(|error| error.to_string())? != body
            {
                return Err("noncanonical native descriptor".into());
            }
            let pending = ticket.owner.begin_bootstrap(
                ticket.room,
                &hint,
                &identity.device_signing_key,
                SystemTime::now(),
            )?;
            if pending.origin() != origin {
                return Err("native descriptor origin changed".into());
            }
            let request = PreparedRequest {
                id: 0,
                origin,
                target: "/peer/v3/bootstrap",
                body: pending
                    .request()
                    .encode()
                    .map_err(|error| error.to_string())?,
                proof: None,
            };
            let mut state = ticket.state.lock();
            ticket.live()?;
            state.phase = Phase::Bootstrap(pending);
            Ok(Reply::Work(prepare(&mut state, request)?))
        }
        Phase::Bootstrap(pending) => {
            let response =
                media_wire::BootstrapResponse::decode(body).map_err(|error| error.to_string())?;
            // Correlation exists only during this exact synchronous opaque
            // finish. A revoke during network await never enters this window.
            finish_owner_bootstrap(ticket, pending, &response)?;
            ticket.owner.with_current(ticket.room, |view| {
                let anchor = Instant::now();
                let now = now_ns()?;
                let signing = signing_view(view, identity, now)?;
                let mut control = protocol::NativeControl::new(
                    &signing,
                    view.hint().ok_or("native descriptor floor missing")?,
                )?;
                let request = control.start_http(&signing)?;
                let mut state = ticket.state.lock();
                tick_clock(&mut state, now)?;
                state.phase = Phase::Control(control);
                let metadata = descriptor::inspect_persisted_hint(
                    view.hint().ok_or("native descriptor missing")?,
                    view.host().ok_or("native host missing")?,
                )
                .map_err(|error| error.to_string())?;
                let remaining = metadata
                    .expires_at_ns
                    .checked_sub(now)
                    .ok_or("native descriptor expired")?;
                state.bootstrap_at = deadline(anchor, (remaining / 2).min(300_000_000_000))?;
                // NativeControl::new above already verified this exact current
                // descriptor. This retained monotonic deadline can only narrow
                // capture lifetime; it does not replace current validation.
                state.descriptor_expires = deadline(anchor, remaining)?;
                Ok(Reply::Work(prepare(&mut state, request)?))
            })
        }
        Phase::Control(mut control) => ticket.owner.with_current(ticket.room, |view| {
            let anchor = Instant::now();
            let now = now_ns()?;
            let signing = signing_view(view, identity, now)?;
            let step = control.advance(&signing, id, body)?;
            let mut state = ticket.state.lock();
            tick_clock(&mut state, now)?;
            let step = if matches!(step, Step::HttpReady) {
                Step::Request(control.start_join(
                    &signing,
                    &state.call,
                    media_wire::MediaDirections {
                        publish: true,
                        subscribe: true,
                    },
                )?)
            } else {
                step
            };
            state.phase = Phase::Control(control);
            apply_step(&mut state, step, anchor)
        }),
        Phase::Transition => Err("native control transition has no completion".into()),
    }
}
fn act(ticket: &Arc<Ticket>, action: Action, identity: &HostIdentity) -> Result<Reply, String> {
    ticket.owner.with_current(ticket.room, |view| {
        let anchor = Instant::now();
        ticket.live()?;
        if ticket.leaving.load(Ordering::SeqCst) {
            return Err("native participation is leaving".into());
        }
        let now = now_ns()?;
        let signing = signing_view(view, identity, now)?;
        let mut state = ticket.state.lock();
        tick_clock(&mut state, now)?;
        if matches!(action, Action::Abandon) {
            if let Some((_, id)) = state.pending.take() {
                if let Phase::Control(control) = &mut state.phase {
                    control.forget(id);
                }
            }
            return Ok(Reply::Closed);
        }
        if state.pending.is_some() {
            return Err("native operation already pending".into());
        }
        if matches!(action, Action::Bootstrap) {
            ticket.pause();
            let locator = tilo_relay::workspace_join::DeliveryOriginLocatorV1::from_transport_hint(
                view.delivery_locator().ok_or("native locator missing")?,
                view.host().ok_or("native host missing")?,
            )
            .map_err(|error| error.to_string())?;
            state.phase = Phase::Discover {
                origin: locator.origin.clone(),
            };
            state.grant = None;
            state.carrier = None;
            return Ok(Reply::Work(prepare(
                &mut state,
                PreparedRequest {
                    id: 0,
                    origin: locator.origin,
                    target: "/peer/v3/descriptor",
                    body: Vec::new(),
                    proof: None,
                },
            )?));
        }
        let handle = state
            .grant
            .as_ref()
            .ok_or("native participation missing")?
            .view
            .handle;
        let carrier = state.carrier.as_ref().map(|carrier| carrier.protocol_id);
        let Phase::Control(control) = &mut state.phase else {
            return Err("native HTTP session missing".into());
        };
        let step = match action {
            Action::Heartbeat => Step::Request(control.heartbeat(&signing, handle)?),
            Action::Renew => Step::Request(control.renew(&signing, handle)?),
            Action::Relay => Step::Request(control.relay_challenge(&signing, handle)?),
            Action::Refresh => Step::Request(control.refresh(
                &signing,
                handle,
                carrier.ok_or("native carrier absent")?,
            )?),
            Action::CloseRelay => Step::Request(control.close_relay(
                &signing,
                handle,
                carrier.ok_or("native carrier absent")?,
            )?),
            Action::Leave => {
                ticket.leaving.store(true, Ordering::SeqCst);
                ticket.pause();
                control.leave(&signing, handle)?
            }
            Action::RetireCarrier => {
                if let Some(carrier) = carrier {
                    control.retire_carrier(carrier);
                }
                state.carrier = None;
                return Ok(Reply::Closed);
            }
            Action::Abandon | Action::Bootstrap => unreachable!("handled before borrowing control"),
        };
        apply_step(&mut state, step, anchor)
    })
}
pub(super) fn finish_owner_bootstrap(
    ticket: &Arc<Ticket>,
    pending: NativeBootstrap,
    response: &media_wire::BootstrapResponse,
) -> Result<(), String> {
    ticket.live()?;
    let expected = ticket.owner.with_current(ticket.room, |view| {
        let changes = match view.descriptor_expectation()? {
            descriptor::GenerationExpectation::Current {
                descriptor_digest, ..
            } => descriptor_digest != pending.request().candidate_digest,
            _ => true,
        };
        if changes {
            Ok(Some(
                view.continuity()
                    .checked_add(1)
                    .ok_or("native continuity exhausted")?,
            ))
        } else {
            Ok(None)
        }
    })?;
    let before = ticket.fences.load(Ordering::SeqCst);
    if let Some(expected) = expected {
        ticket.own_continuity.store(expected, Ordering::SeqCst);
        ticket.own_bootstrap.store(true, Ordering::SeqCst);
    }
    let result = ticket
        .owner
        .finish_bootstrap(pending, response, SystemTime::now());
    ticket.own_bootstrap.store(false, Ordering::SeqCst);
    result?;
    let count = ticket.fences.load(Ordering::SeqCst).saturating_sub(before);
    if count != u64::from(expected.is_some()) {
        return Err("unrelated native fence during bootstrap".into());
    }
    ticket.live()?;
    ticket.owner.with_current(ticket.room, |view| {
        if expected.is_some_and(|expected| expected != view.continuity()) {
            return Err("native owner changed after bootstrap".into());
        }
        Ok(())
    })
}
/// Local teardown has already stopped capture and transport. The only operation
/// allowed after that fence is the exact retained participation's Leave.
fn end(ticket: &Arc<Ticket>, identity: &HostIdentity) -> Result<Reply, String> {
    ticket.stop();
    if ticket.leaving.swap(true, Ordering::SeqCst) {
        return Err("native leave already prepared".into());
    }
    ticket.owner.with_current(ticket.room, |view| {
        let anchor = Instant::now();
        let signing = signing_view(view, identity, now_ns()?)?;
        let mut state = ticket.state.lock();
        let Some(handle) = state.grant.as_ref().map(|grant| grant.view.handle) else {
            return Ok(Reply::Left);
        };
        let pending = state.pending.take();
        let Phase::Control(control) = &mut state.phase else {
            return Ok(Reply::Left);
        };
        if let Some((_, id)) = pending {
            control.forget(id);
        }
        let step = control.leave(&signing, handle)?;
        apply_step(&mut state, step, anchor)
    })
}
fn apply_step(state: &mut State, step: Step, now: Instant) -> Result<Reply, String> {
    match step {
        Step::Request(request) => Ok(Reply::Work(prepare(state, request)?)),
        Step::Grant(view) => {
            let expires = deadline(now, view.expires_in_ns)?;
            let presence = deadline(now, view.presence_in_ns)?;
            state.grant = Some(TimedGrant {
                view,
                expires,
                presence,
            });
            Ok(Reply::Grant)
        }
        Step::Relay(relay) => {
            let grant = state.grant.as_mut().ok_or("native relay grant missing")?;
            if relay.handle != grant.view.handle || relay.namespace != grant.view.namespace {
                return Err("native relay projection changed".into());
            }
            // The signed relay response may contain a newer authorized
            // inventory than Join. NativeControl validated that transition.
            grant.view.inventory = relay.inventory.clone();
            state.generation = state
                .generation
                .checked_add(1)
                .ok_or("native carrier generation exhausted")?;
            let scope = ConnectionScope {
                endpoint: relay.endpoint,
                fingerprints: Vec::new(),
                owner_epoch: media_wire::fresh_nonce().map_err(|error| error.to_string())?,
                carrier_generation: state.generation,
                publication: grant
                    .view
                    .directions
                    .publish
                    .then(|| namespace(relay.namespace)),
                subscriptions: if grant.view.directions.subscribe {
                    relay
                        .inventory
                        .namespaces
                        .into_iter()
                        .map(namespace)
                        .collect()
                } else {
                    Vec::new()
                },
            };
            state.carrier = Some(Carrier {
                protocol_id: relay.carrier,
                scope: scope.clone(),
                expires: grant.expires,
            });
            Ok(Reply::Carrier(Connection {
                scope,
                authorization: relay.setup,
            }))
        }
        Step::Presence(view) => {
            let grant = state
                .grant
                .as_mut()
                .ok_or("native presence grant missing")?;
            grant.presence = deadline(now, view.expires_in_ns)?;
            grant.view.inventory = view.inventory;
            Ok(Reply::Presence)
        }
        Step::Refreshed => {
            if let (Some(carrier), Some(grant)) = (&mut state.carrier, &state.grant) {
                carrier.expires = grant.expires;
            }
            Ok(Reply::Refreshed)
        }
        Step::RelayClosed => {
            state.carrier = None;
            Ok(Reply::Closed)
        }
        Step::Left => {
            state.grant = None;
            state.carrier = None;
            Ok(Reply::Left)
        }
        Step::HttpReady => Err("unexpected native HTTP transition".into()),
    }
}
