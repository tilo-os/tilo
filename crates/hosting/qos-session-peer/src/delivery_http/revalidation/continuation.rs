//! Private original-request continuation, not a public replay cursor or authority.
use super::*;

/// Captured only from a real admission and its live revalidation borrow. It
/// carries no detached authorization; every use requires a fresh registry check.
pub struct AdmissionContinuation {
    native_time: Option<qos_action_broker::peer_actions::PolicyTime>,
    operation: Option<OwnerDataOperation>,
    session: [u8; 16],
    binding: auth::Binding,
    capabilities: u64,
    expires: u64,
    entry: u64,
    digest: [u8; 32],
    deadline: Instant,
    cancellation: watch::Receiver<bool>,
}
pub struct PendingOperation {
    ticket: AdmissionContinuation,
    original: BufferedRequest,
}
fn digest(original: &BufferedRequest) -> Result<[u8; 32]> {
    let request = Request::new(
        original.method().as_str(),
        original.target(),
        original.body(),
    )?;
    let proof = original.proof().context("missing original proof")?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo-delivery-v3-operation-continuation-v1\0");
    for bytes in [
        original.method().as_str().as_bytes(),
        request.target().as_bytes(),
        request.body(),
        proof,
    ] {
        hash.update(&(bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    Ok(*hash.finalize().as_bytes())
}
impl OperationRevalidation<'_> {
    pub(in crate::delivery_http) fn capture_continuation(
        &self,
        admitted: &VerifiedRequest<'_>,
        original: &BufferedRequest,
    ) -> Result<AdmissionContinuation> {
        ensure!(
            !cancelled(&self.cancellation, self.deadline, &self.clock),
            "original request cancelled or expired"
        );
        ensure!(
            self.deadline == original.deadline()
                && self.cancellation.same_channel(&original.cancellation()),
            "original request lifecycle changed"
        );
        let exact = Request::new(
            original.method().as_str(),
            original.target(),
            original.body(),
        )?;
        ensure!(
            exact.method() == admitted.request().method()
                && exact.target() == admitted.request().target()
                && exact.body() == admitted.request().body(),
            "original request bytes changed"
        );
        let session = admitted.session();
        ensure!(
            self.seed.entry != 0
                && self.seed.session == session.session_id()
                && self.seed.pins.boot == session.binding().server_boot_id
                && self.seed.pins.descriptor == session.binding().descriptor_digest,
            "admission binding changed"
        );
        let mut ticket = AdmissionContinuation::capture(
            admitted,
            original,
            self.seed.entry,
            Some(self.operation),
        )?;
        ticket.native_time = self.seed.pins.native_time.clone();
        Ok(ticket)
    }
}
impl AdmissionContinuation {
    fn capture(
        admitted: &VerifiedRequest<'_>,
        original: &BufferedRequest,
        entry: u64,
        operation: Option<OwnerDataOperation>,
    ) -> Result<Self> {
        ensure!(
            !original.is_cancelled() && entry != 0,
            "original request unavailable"
        );
        let exact = Request::new(
            original.method().as_str(),
            original.target(),
            original.body(),
        )?;
        ensure!(
            exact.method() == admitted.request().method()
                && exact.target() == admitted.request().target()
                && exact.body() == admitted.request().body(),
            "original request bytes changed"
        );
        let session = admitted.session();
        Ok(Self {
            native_time: None,
            operation,
            session: session.session_id(),
            binding: session.binding().clone(),
            capabilities: session.capabilities(),
            expires: session.expires_at_ns(),
            entry,
            digest: digest(original)?,
            deadline: original.deadline(),
            cancellation: original.cancellation(),
        })
    }
    pub fn session_id(&self) -> [u8; 16] {
        self.session
    }
    pub fn park(self, original: BufferedRequest) -> Result<PendingOperation> {
        ensure!(
            !original.is_cancelled(),
            "original request cancelled or expired"
        );
        ensure!(
            self.digest == digest(&original)?
                && self.deadline == original.deadline()
                && self.cancellation.same_channel(&original.cancellation()),
            "original request replaced"
        );
        Ok(PendingOperation {
            ticket: self,
            original,
        })
    }
}
impl PendingOperation {
    pub fn original(&self) -> &BufferedRequest {
        &self.original
    }
    pub fn into_request(self) -> BufferedRequest {
        self.original
    }
    fn check_session(&self, session: &auth::VerifiedSession, entry: u64, now: u64) -> Result<()> {
        ensure!(
            entry == self.ticket.entry
                && session.session_id() == self.ticket.session
                && session.binding() == &self.ticket.binding
                && session.capabilities() == self.ticket.capabilities
                && session.expires_at_ns() == self.ticket.expires
                && now < self.ticket.expires,
            "operation session replaced"
        );
        Ok(())
    }
}
impl HttpSessions {
    pub(in crate::delivery_http) fn resume_operation<'a>(
        &'a mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        budget: &HostBytes,
        pending: &'a PendingOperation,
    ) -> Result<(
        &'a auth::VerifiedSession,
        OperationRevalidation<'a>,
        OwnerResourceTurn,
    )> {
        let time = pending
            .ticket
            .native_time
            .clone()
            .context("original native clock custody missing")?;
        let registered = self
            .native_time
            .as_ref()
            .context("native owner clock not registered")?;
        ensure!(
            time.same_origin(registered),
            "original native clock origin changed"
        );
        self.resume_operation_clock(owner, current, budget, pending, Clock::Native(time))
    }
    #[cfg(test)]
    pub(in crate::delivery_http) fn resume_operation_at<'a>(
        &'a mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        budget: &HostBytes,
        pending: &'a PendingOperation,
        clock: &TestClock,
    ) -> Result<(
        &'a auth::VerifiedSession,
        OperationRevalidation<'a>,
        OwnerResourceTurn,
    )> {
        self.resume_operation_clock(owner, current, budget, pending, Clock::Test(clock.clone()))
    }
    fn resume_operation_clock<'a>(
        &'a mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        budget: &HostBytes,
        pending: &'a PendingOperation,
        clock: Clock,
    ) -> Result<(
        &'a auth::VerifiedSession,
        OperationRevalidation<'a>,
        OwnerResourceTurn,
    )> {
        let operation = pending
            .ticket
            .operation
            .context("native operation custody missing")?;
        let turn = owner.prepare_data_turn(budget, operation)?;
        let now = clock.wall()?;
        self.advance(now)?;
        let original = pending.original();
        let cancellation = original.cancellation();
        ensure!(
            !cancelled(&cancellation, original.deadline(), &clock),
            "original operation cancelled or expired"
        );
        let id = pending.ticket.session;
        let live = self.live.get(&id).context("operation session replaced")?;
        let seed = ContextSeed {
            pins: ContextPins::capture(self, live.identity),
            session: id,
            entry: live.entry,
        };
        let guard = current.read().map_err(anyhow::Error::msg)?;
        let now = clock.sample(guard.view())?;
        self.advance(now)?;
        let context = seed
            .pins
            .context(owner, guard.view(), &clock, &mut self.high_water)?;
        let session = self
            .live
            .get_mut(&id)
            .context("operation session replaced")?
            .admission
            .revalidate(&context.verification())?;
        pending.check_session(session, seed.entry, context.now)?;
        let check = OperationRevalidation {
            operation,
            high_water: &mut self.high_water,
            seed,
            deadline: original.deadline(),
            cancellation,
            clock,
        };
        Ok((session, check, turn))
    }
}

impl HttpSessions {
    pub fn observe_time(&mut self, now: u64) -> Result<()> {
        self.advance(now)
    }
    /// Reserve concrete owner resources before calling. This borrows genuine
    /// request admission and creates custody only for the exact buffered request.
    pub fn admit_buffered_in<'a>(
        &'a mut self,
        proof: &RequestProof,
        exact: &'a Request<'a>,
        original: &BufferedRequest,
        context: &auth::VerificationContext<'_>,
    ) -> Result<(VerifiedRequest<'a>, AdmissionContinuation)> {
        self.advance(context.now_ns)?;
        ensure!(!original.is_cancelled(), "original request unavailable");
        ensure!(
            Some(proof.encode()?.as_slice()) == original.proof(),
            "original proof changed"
        );
        let entry = self
            .live
            .get(&proof.session_id())
            .context("unknown session")?
            .entry;
        let admitted = self.admit_in(proof, exact, context)?;
        let ticket = AdmissionContinuation::capture(&admitted, original, entry, None)?;
        Ok((admitted, ticket))
    }
    /// Concrete owner must reserve and reconstruct its context before this call.
    /// The pending original request remains borrowed through synchronous use.
    pub fn resume_buffered_in<'a>(
        &'a mut self,
        pending: &'a PendingOperation,
        context: &auth::VerificationContext<'_>,
    ) -> Result<&'a auth::VerifiedSession> {
        self.advance(context.now_ns)?;
        ensure!(
            !pending.original.is_cancelled(),
            "original request unavailable"
        );
        let id = pending.ticket.session;
        let entry = self
            .live
            .get(&id)
            .context("operation session replaced")?
            .entry;
        let session = self.revalidate_in(id, context)?;
        pending.check_session(session, entry, context.now_ns)?;
        Ok(session)
    }
}
impl PendingOperation {
    pub fn session_id(&self) -> [u8; 16] {
        self.ticket.session
    }
}
