#[allow(
    clippy::wildcard_imports,
    reason = "one part of the broker module: the shared state, its imports and the other parts reach this through the parent glob"
)]
use super::*;

pub(super) struct BrokerThreadShutdown {
    state: Arc<AppBrokerState>,
    fanout: Arc<FanoutState>,
}

/// Every host-owned handle the broker state is built from: durable channel
/// state, the peer publisher, workspace membership, the trusted device keys,
/// host services and the native peer configuration.
pub(crate) struct BrokerWiring<'a> {
    pub(crate) broker_channels: Arc<Mutex<BrokerChannels>>,
    pub(crate) publisher: Arc<PeerPublisher>,
    pub(crate) membership: Option<WorkspaceTreeHandle>,
    pub(crate) trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
    /// The authority a harness admits its enrolled devices under, when
    /// neither a peer configuration nor the host's services supply one.
    pub(crate) certificate_authority:
        Option<Arc<dyn crate::certificate_authority::CertificateAuthority>>,
    pub(crate) services: ProcessServices,
    pub(crate) config: Option<&'a crate::NativePeerBrokerConfig>,
}

impl Drop for BrokerThreadShutdown {
    fn drop(&mut self) {
        self.fanout.shutdown_app_broker(&self.state);
    }
}

/// Say where the broker listens.
pub(super) fn log_bound(socket_path: &Path) {
    tracing::info!(
        "collaboration broker: bound {} for native SDK access",
        socket_path.display(),
    );
}

pub(crate) fn spawn_configured(
    socket_path: &Path,
    fanout: &Arc<FanoutState>,
    wiring: BrokerWiring<'_>,
    quit: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    let BrokerWiring {
        broker_channels,
        publisher,
        membership,
        trusted_devices,
        certificate_authority,
        services,
        config,
    } = wiring;
    let (state, bridge_rx) = AppBrokerState::new_configured(
        fanout,
        broker_channels,
        publisher,
        membership,
        trusted_devices,
        config,
        services,
    );
    state.attach_certificate_authority(certificate_authority)?;
    if let Some(path) = state.services.launchable_catalogue.clone() {
        state.launchable.persist_at(&path);
    }
    {
        let journal = state
            .journal
            .lock()
            .map_err(|_| std::io::Error::other("invocation journal unavailable"))?;
        journal
            .as_ref()
            .map_err(|error| std::io::Error::other(error.clone()))?;
    }
    // Bind on the caller's thread: from the worker thread, a bind
    // failure surfaces only via a log line.
    let (std_listener, socket_ownership) = bind_owned_socket(socket_path)?;
    log_bound(socket_path);

    let attached_state = Arc::clone(&state);
    let fanout_for_thread = Arc::clone(fanout);
    let socket_path_for_thread = socket_path.to_path_buf();
    let thread = std::thread::Builder::new()
        .name("tilo-collab-broker".into())
        .spawn(move || {
            // Declaration order is intentional: shutdown first makes every
            // surviving facade inert and releases the journal, then socket
            // ownership removes only this broker's still-owned inode.
            let _socket_ownership = socket_ownership;
            let _shutdown = BrokerThreadShutdown {
                state: Arc::clone(&state),
                fanout: fanout_for_thread,
            };
            let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge_rx));
            loop {
                let listener = match std_listener.try_clone() {
                    Ok(listener) => listener,
                    Err(e) => {
                        tracing::warn!(
                            "collaboration broker: cannot take up its socket again: {e}"
                        );
                        return;
                    }
                };
                let run = {
                    let state = Arc::clone(&state);
                    let directories = Rc::clone(&directories);
                    let quit = Arc::clone(&quit);
                    let socket_path = socket_path_for_thread.clone();
                    tilo_rt::panic_boundary::isolate(move || {
                        let rt = match tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                        {
                            Ok(rt) => rt,
                            Err(e) => {
                                tracing::warn!(
                                    "collaboration broker: tokio runtime init failed: {e}"
                                );
                                return;
                            }
                        };
                        tokio::task::LocalSet::new().block_on(
                            &rt,
                            serve_broker_thread(state, directories, listener, socket_path, quit),
                        );
                    })
                };
                #[cfg(any(test, feature = "test-support"))]
                report_retired_broker_owners(&state, &directories);
                match run {
                    Ok(()) => return,
                    Err(panic) => {
                        if !state.restart_after_unwind(panic.as_ref()) {
                            return;
                        }
                    }
                }
            }
        })?;
    fanout.attach_app_broker(attached_state);

    Ok(thread)
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn report_retired_broker_owners(
    state: &Arc<AppBrokerState>,
    directories: &Rc<LocalActionDirectoryRegistry>,
) {
    if std::env::var_os("TILO_IN_PROCESS_TEST_ROOT").is_some() {
        eprintln!(
            "native broker retired LocalSet: state refs={}, directory refs={}, registered={:?}",
            Arc::strong_count(state),
            Rc::strong_count(directories),
            directories
                .directories
                .try_borrow()
                .map(|entries| entries.len()),
        );
    }
}

/// Most broker threads a session starts after one unwound, before the
/// broker stays down for the rest of the session.
pub(super) const BROKER_THREAD_RESTARTS: u64 = 5;

/// When the broker's turn must wake with nothing arriving: the earlier of
/// its next policy re-check and its next invocation expiry, and never while
/// neither is due.
pub(super) fn broker_turn_deadline(
    recheck: Option<std::time::Instant>,
    invocation: Option<std::time::Instant>,
) -> Option<std::time::Instant> {
    recheck.into_iter().chain(invocation).min()
}

/// Sleep until `deadline`; only polled while there is one.
pub(super) async fn sleep_until(deadline: Option<std::time::Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
    }
}

/// One run of the broker thread: start the broker's own services, then
/// accept and drive connections until asked to quit.
#[expect(
    clippy::future_not_send,
    reason = "the broker state and its Rc-backed action-directory registry run only on the broker LocalSet"
)]
pub(super) async fn serve_broker_thread(
    state: Arc<AppBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    std_listener: tilo_socket::Listener,
    socket_path_for_thread: PathBuf,
    quit: Arc<AtomicBool>,
) {
    let listener = match tilo_socket::tokio::listener(std_listener) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(
                "collaboration broker: adopting {} into the reactor failed: {e}",
                socket_path_for_thread.display()
            );
            return;
        }
    };
    // Membership surfaces and native context subscribers are re-checked when
    // an input announces a change or a registered expiry comes due, not on a
    // clock. The watch wakes this turn for the first announcement
    // after it is armed; the schedule coalesces the rest.
    let policy_turn = Arc::new(tokio::sync::Notify::new());
    let policy_watch = {
        let turn = Arc::clone(&policy_turn);
        tilo_identity::policy_change::Watch::new(move || turn.notify_one())
    };
    let mut policy = crate::policy_schedule::PolicySchedule::default();
    loop {
        let now = tilo_clock::now();
        let epoch = tilo_identity::policy_change::epoch();
        if policy.due(now, epoch).is_some_and(|due| due <= now) {
            let clocked = state.refresh_policy_surfaces();
            policy.checked(now, epoch, clocked);
        }
        let workers = state
            .workers
            .lock()
            .map(|mut workers| std::mem::take(&mut *workers))
            .unwrap_or_default();
        for worker in workers {
            let state = state.clone();
            let directories = directories.clone();
            let peer_id = state.next_peer_id();
            tokio::task::spawn_local(async move {
                let result = async {
                    let stream =
                        tilo_socket::tokio::from_std(worker.stream).map_err(BrokerError::from)?;
                    drive_connection(
                        stream,
                        state,
                        directories,
                        peer_id,
                        Some(worker.binding),
                        None,
                    )
                    .await
                }
                .await;
                if let Err(error) = result {
                    tracing::warn!(
                        peer_id,
                        phase = "local_worker_rpc",
                        error_class = error.kind.as_str(),
                        "collaboration broker: local worker disconnected"
                    );
                }
            });
        }
        state.take_turn(&directories);
        state.drain_offloads();
        if quit.load(Ordering::Relaxed) {
            break;
        }
        // Sleep until something here is due: the next policy re-check, or an
        // invocation about to expire. Everything else the turn handles wakes
        // it at once: queued directory work, in-process api calls and quit
        // through `local_turn`, admitted workers through `worker_admitted`,
        // policy announcements, and with them runs to deliver, through
        // `policy_turn`; connected apps run as
        // tasks of this LocalSet.
        let now = tilo_clock::now();
        let mut recheck = policy.due(now, tilo_identity::policy_change::epoch());
        let armed = policy_watch.arm();
        if recheck.is_none() && armed != policy.epoch() {
            // Announced between the checks above and arming: nothing will
            // wake the turn for it.
            recheck = Some(now);
        }
        let deadline = broker_turn_deadline(recheck, state.next_invocation_deadline());
        let stream = tokio::select! {
            biased;
            () = sleep_until(deadline), if deadline.is_some() => {
                // Expiration must not depend on a caller polling
                // status: completion waiters deserve a terminal
                // answer even after an app goes silent.
                state.expire_stale_invocations();
                continue;
            },
            conn = listener.accept() => conn,
            () = state.local_turn.notified() => continue,
            () = state.worker_admitted.notified() => continue,
            () = policy_turn.notified() => continue,
        };
        let (stream, _) = match stream {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("collaboration broker: accept failed: {e}; retrying");
                continue;
            }
        };
        let state = Arc::clone(&state);
        let directories = Rc::clone(&directories);
        let peer_id = state.next_peer_id();
        tokio::task::spawn_local(async move {
            if let Err(e) = drive_connection(stream, state, directories, peer_id, None, None).await
            {
                tracing::warn!("collaboration broker: peer-{peer_id} disconnected: {e}");
            }
        });
    }
}

impl AppBrokerState {
    /// After the broker thread's runtime unwound: withdraw the services it
    /// ran and say whether to start it again. Its connections closed with
    /// the runtime; apps reconnect to the same socket.
    pub(super) fn restart_after_unwind(&self, panic: &(dyn std::any::Any + Send)) -> bool {
        let message = tilo_rt::panic_boundary::message(panic);
        let restarts = self
            .thread_restarts
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        self.forget_service_peers();
        if restarts > BROKER_THREAD_RESTARTS || self.stopped.load(Ordering::Acquire) {
            tracing::error!(
                restarts,
                panic = message,
                "collaboration broker: stopped after its thread failed again; apps lose collaboration until the session restarts"
            );
            return false;
        }
        tracing::error!(
            restarts,
            panic = message,
            "collaboration broker: its thread failed; starting it again"
        );
        // A thread that fails at once again backs off rather than spin;
        // tests restart at once.
        if !cfg!(test) {
            let _ = tilo_rt::sleep_thread(Duration::from_millis(
                100_u64.saturating_mul(1 << restarts.min(5)),
            ));
        }
        true
    }
    /// How often the broker thread failed and was started again.
    #[must_use]
    pub fn thread_restarts(&self) -> u64 {
        self.thread_restarts.load(Ordering::Relaxed)
    }
    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
    pub(crate) fn is_accepting(&self) -> bool {
        self.accepting.load(Ordering::Acquire) && !self.is_stopped()
    }
    /// The seat, reached through the providers this broker serves, for a service this host keeps
    /// none of itself; `None` for a host with no providers, or asked off its runtime.
    pub(crate) fn seat(&self) -> Option<crate::seat::Seat> {
        let providers = self.services.providers.clone()?;
        let runtime = tokio::runtime::Handle::try_current().ok()?;
        Some(crate::seat::Seat::new(providers, runtime))
    }
    pub(crate) fn request_stop(&self) {
        self.accepting.store(false, Ordering::Release);
        self.local_turn.notify_one();
        // Held committed-read waits re-check ownership, which now refuses.
        tilo_identity::policy_change::announce();
    }
    /// Hold the broker lifecycle read side for one complete shell-facade
    /// operation. Once shutdown obtains the write side, no stale facade can
    /// admit or publish new work.
    pub(crate) fn while_live<T>(
        &self,
        operation: impl FnOnce(&Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let _live = self
            .lifecycle
            .read()
            .map_err(|_| "broker unavailable".to_string())?;
        if !self.is_accepting() {
            return Err("broker unavailable".into());
        }
        let _call =
            crate::collab_broker::BrokerFacadeCallGuard::enter(std::ptr::from_ref(self).addr());
        operation(self)
    }
    /// Like `while_live`, but discard a newly cloned handle if a callback
    /// requested this exact broker's shutdown before the handle escaped.
    pub(crate) fn while_live_fenced<T>(
        &self,
        operation: impl FnOnce(&Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let result = self.while_live(operation);
        if !self.is_accepting() {
            return Err("broker unavailable".into());
        }
        result
    }
    /// Finalize the shared state after the `LocalSet` and all of its RPC tasks
    /// have been dropped. Surviving Arc-backed facades become inert, and the
    /// journal lock is released before the socket path can be reclaimed.
    pub(crate) fn stop(&self) {
        let _exclusive = match self.lifecycle.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        self.request_stop();
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }

        macro_rules! clear {
            ($field:expr) => {
                match $field.lock() {
                    Ok(mut value) => value.clear(),
                    Err(poisoned) => poisoned.into_inner().clear(),
                }
            };
        }
        // Host-service callbacks can outlive the LocalSet. Serialize their
        // journal transitions with retirement before releasing the writer.
        let _admission = match self.admission_lock.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        self.execution_activity.close();
        clear!(self.host_workers);
        clear!(self.route_holders);
        clear!(self.channels);
        clear!(self.publishers.surfaces);
        clear!(self.surface_tables);
        clear!(self.action_directory_roots);
        clear!(self.native_checkouts);
        clear!(self.publishers.streams);
        clear!(self.pending_invocations);
        clear!(self.served_invocations);
        clear!(self.stable_invocation_subscribers);
        clear!(self.workspace_governance_peers);
        clear!(self.confirmation_grants);
        clear!(self.cancellation_requests);
        clear!(self.host_step_stops);
        clear!(self.workers);
        // A launch waiting for an app to publish learns the broker stopped.
        self.publishers.streams_changed();
        // The broker that would run what is handed over is gone.
        std::mem::take(&mut *self.turn_work.handed.lock_recover());
        self.refresh_action_directory_root_snapshot();

        self.let_go_of_journal();
    }
    /// Allocate a private connected descriptor for an actual host-mounted worker.
    /// The peer must still prove the exact current certificate on every channel join.
    pub fn connect_local_worker(
        &self,
        route: CrdtRoute,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<tilo_socket::Stream, String> {
        self.connect_local_worker_registered(route, certificate, None)
    }
    pub(super) fn connect_local_worker_registered(
        &self,
        route: CrdtRoute,
        certificate: &tilo_user_identity::DeviceCert,
        registration: Option<uuid::Uuid>,
    ) -> Result<tilo_socket::Stream, String> {
        self.owner_health()?;
        if route.app_id.is_empty() || !route.is_publishable() {
            return Err("worker needs an exact app route".into());
        }
        let authority = self
            .certificate_authority
            .get()
            .ok_or("current certificate authority unavailable")?;
        let workspace =
            uuid::Uuid::parse_str(&route.cluster_id).map_err(|_| "invalid worker workspace")?;
        authority.authorize_join(workspace, certificate)?;
        let now = self.policy_now_ns()?;
        tilo_user_identity::verify_device_cert_at(certificate, now)
            .map_err(|_| "worker certificate rejected")?;
        let verified = VerifiedJoinIdentity::of(certificate, certificate.capabilities.foreign);
        self.membership
            .as_ref()
            .ok_or("current workspace authority unavailable")?
            .decide_in_workspace(&route.cluster_id, |workspace| {
                admit_verified_join_at(workspace, &route, verified, now / 1_000_000_000)
            })??;
        let mut workers = self
            .workers
            .lock()
            .map_err(|_| "worker admission unavailable")?;
        if workers.len() >= 32 {
            return Err("worker admission queue full".into());
        }
        let (server, client) = tilo_socket::pair().map_err(|e| e.to_string())?;
        workers.push(PendingWorker {
            stream: server,
            binding: WorkerBinding {
                registration: registration.map(|identity| PrivateWorkerRegistration {
                    identity,
                    route: route.clone(),
                    certificate: certificate.clone(),
                }),
                route,
                certificate: certificate.clone(),
            },
        });
        drop(workers);
        self.worker_admitted.notify_one();
        Ok(client)
    }
    pub(super) const MAX_RETAINED_INVOCATIONS: usize = 256;
    pub(super) const INVOCATION_IDLE_TIMEOUT: std::time::Duration =
        std::time::Duration::from_secs(15 * 60);
    pub(super) const CANCELLATION_GRACE: std::time::Duration = std::time::Duration::from_secs(30);
    /// Broker ceilings are deliberately small: an application must not turn
    /// a palette keystroke into an unbounded RPC.
    pub(super) const DIRECTORY_MAX_RESULTS: u32 = 256;
    pub(super) const DIRECTORY_MAX_BYTES: u32 = 256 * 1024;
    pub(super) const DIRECTORY_MAX_WORK: u32 = 4_096;
    pub(super) const DIRECTORY_MAX_DEPTH: u16 = 32;
    /// An app that has not answered its directory in this long is hung, not
    /// busy: its asker hears it timed out. Remote askers normally give up
    /// sooner, on their own deadline.
    pub(super) const DIRECTORY_OWNER_HUNG: Duration = Duration::from_secs(60);
    pub(super) fn expire_stale_invocations(&self) {
        let now = tilo_clock::now();
        self.expire_stale_invocations_at(
            now,
            Self::INVOCATION_IDLE_TIMEOUT,
            Self::CANCELLATION_GRACE,
        );
        self.expire_workspace_opens(now);
    }
    /// When the next pending invocation can expire. `None` while nothing is
    /// in flight, so the accept loop parks instead of waking every 100 ms.
    pub(super) fn next_invocation_deadline(&self) -> Option<std::time::Instant> {
        let opens = self.workspace_opens.next_deadline();
        let pending = self.pending_invocations.lock().ok()?;
        pending
            .values()
            .filter(|invocation| !self.held_by_job(&invocation.update.invocation()))
            .filter_map(|invocation| {
                let idle_at = invocation
                    .last_activity
                    .checked_add(Self::INVOCATION_IDLE_TIMEOUT);
                let cancel_at = invocation
                    .cancellation_requested_at
                    .and_then(|requested| requested.checked_add(Self::CANCELLATION_GRACE));
                match (cancel_at, idle_at) {
                    (Some(cancel), Some(idle)) => Some(cancel.min(idle)),
                    (cancel, idle) => cancel.or(idle),
                }
            })
            .chain(opens)
            .min()
    }
    pub(super) fn expire_stale_invocations_at(
        &self,
        now: std::time::Instant,
        idle_timeout: std::time::Duration,
        cancellation_grace: std::time::Duration,
    ) {
        let expired = {
            let Ok(mut pending) = self.pending_invocations.lock() else {
                return;
            };
            #[allow(
                clippy::needless_collect,
                reason = "expired ids must be snapshotted before removing from the same pending-invocation map"
            )]
            let ids = pending
                .iter()
                .filter(|(id, _)| !self.committing(**id))
                // An invocation that holds a live engine job is doing work the app waits on:
                // it is never expired as idle while its job runs.
                .filter(|(_, invocation)| !self.held_by_job(&invocation.update.invocation()))
                .filter_map(|(id, invocation)| {
                    let cancellation_expired =
                        invocation
                            .cancellation_requested_at
                            .is_some_and(|requested| {
                                now.saturating_duration_since(requested) >= cancellation_grace
                            });
                    let idle_expired =
                        now.saturating_duration_since(invocation.last_activity) >= idle_timeout;
                    (cancellation_expired || idle_expired).then_some((*id, cancellation_expired))
                })
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|(id, cancellation_expired)| {
                    pending
                        .remove(&id)
                        .map(|invocation| (id, cancellation_expired, invocation))
                })
                .collect::<Vec<_>>()
        };
        for (id, cancellation_expired, pending) in expired {
            pending.cancellation_fence.store(true, Ordering::Release);
            let (code, message) = if cancellation_expired {
                (
                    "action_cancellation_timeout",
                    "application did not acknowledge cancellation before the deadline",
                )
            } else {
                (
                    "action_timeout",
                    "application stopped reporting action progress before the deadline",
                )
            };
            let completion = InvocationCompletion {
                code: code.into(),
                message: message.into(),
            };
            let update = tilo_actions::InvocationUpdate::Failed {
                invocation: pending.update.invocation(),
                error: tilo_actions::ActionFailure {
                    code: completion.code.clone(),
                    message: completion.message.clone(),
                    details: None,
                    retryable: pending.retryable_on_disconnect,
                },
            };
            let (update, completion) = if pending.journal_key.is_some() {
                let update = tilo_invocation_journal::unknown(pending.update.invocation());
                let completion = InvocationCompletion {
                    code: "action_outcome_unknown".into(),
                    message: "application outcome unknown after silence".into(),
                };
                (update, completion)
            } else {
                (update, completion)
            };
            if self.persist_uncertain_outcome(&pending, &update).is_err() {
                self.execution_activity
                    .publish_status(pending.activity.as_ref(), ExecutionStatus::OutcomeUnknown);
                continue;
            }
            self.publish_activity(pending.activity.as_ref(), &update);
            self.record_completion(id, pending.completion_owner(), update.clone());
            // What the app wrote for it may still be on its way, as for an answer.
            self.keep_served(
                pending.peer_id,
                pending.update.invocation(),
                acting_of(&pending),
            );
            if let Ok(mut requests) = self.cancellation_requests.lock() {
                requests.remove(&id);
            }
            let _ = pending.tx.send(completion);
        }
    }
    /// So tests can register inbound state before pushing signed
    /// envelopes through the wire.
    pub fn broker_channels(&self) -> Arc<Mutex<BrokerChannels>> {
        Arc::clone(&self.broker_channels)
    }
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn new(
        fanout: &Arc<FanoutState>,
        broker_channels: Arc<Mutex<BrokerChannels>>,
        publisher: Arc<PeerPublisher>,
        membership: Option<crate::test_support::WorkspaceTreeHandle>,
        trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
    ) -> (
        Arc<Self>,
        std::sync::mpsc::Receiver<ActionDirectoryBridgeRequest>,
    ) {
        let services = membership
            .as_ref()
            .map(crate::test_support::WorkspaceTreeHandle::services)
            .unwrap_or_default();
        Self::new_configured(
            fanout,
            broker_channels,
            publisher,
            membership.map(|tree| tree.authority()),
            trusted_devices,
            None,
            services,
        )
    }
    pub(super) fn configured_journal(
        config: Option<&crate::NativePeerBrokerConfig>,
        services: &ProcessServices,
    ) -> Result<Option<InvocationJournal>, String> {
        config
            .map(|config| config.journal_root.clone())
            .or_else(|| services.journal_root.clone())
            .map(|path| InvocationJournal::open(&path))
            .transpose()
    }
    #[allow(
        clippy::too_many_lines,
        reason = "one constructor naming every registry the broker keeps"
    )]
    pub(super) fn new_configured(
        fanout: &Arc<FanoutState>,
        broker_channels: Arc<Mutex<BrokerChannels>>,
        publisher: Arc<PeerPublisher>,
        membership: Option<WorkspaceTreeHandle>,
        trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
        config: Option<&crate::NativePeerBrokerConfig>,
        services: ProcessServices,
    ) -> (
        Arc<Self>,
        std::sync::mpsc::Receiver<ActionDirectoryBridgeRequest>,
    ) {
        let (bridge_tx, bridge_rx) =
            std::sync::mpsc::sync_channel(ActionDirectoryRequestQueue::CAPACITY);
        let local_turn = std::sync::Arc::new(tokio::sync::Notify::new());
        let journal = Self::configured_journal(config, &services);
        let next_invocation = journal
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .map_or(1, InvocationJournal::next_wire_id);
        (
            Arc::new_cyclic(|state| Self {
                channels: Mutex::new(HashMap::new()),
                committed_waits: Mutex::new(HashMap::new()),
                shown_actions: Mutex::new(Vec::new()),
                lifecycle: RwLock::new(()),
                accepting: AtomicBool::new(true),
                stopped: AtomicBool::new(false),
                fanout: Arc::downgrade(fanout),
                itself: state.clone(),
                broker_channels,
                publisher,
                durable_owner_required: config.is_some(),
                committed_read_budget: Arc::new(committed_document::Budget::default()),
                committed_read_id: AtomicU64::new(1),
                owner_commit_failure: Mutex::new(None),
                owner_commit_order: AsyncMutex::new(()),
                membership,
                trusted_devices,
                certificate_authority: config
                    .map(|config| config.certificate_authority.clone())
                    .or_else(|| services.native_certificates.clone())
                    .map_or_else(std::sync::OnceLock::new, std::sync::OnceLock::from),
                publishers: tilo_broker_core::delivery::Publishers::default(),
                surface_tables: Mutex::new(HashMap::new()),
                launchable: launchable::LaunchableCatalogue::new(),
                workspace_opens: workspace_open::WorkspaceOpens::default(),
                window_instances: Mutex::new(HashMap::new()),
                saves: Mutex::new(HashMap::new()),
                presses: presses::Presses::default(),
                action_directory_bridge: ActionDirectoryRequestQueue::reaching(
                    std::sync::Arc::downgrade(fanout),
                    state.clone(),
                ),
                action_directory_requests: ActionDirectoryRequests {
                    sender: bridge_tx,
                    turn: std::sync::Arc::clone(&local_turn),
                },
                action_directory_roots: Mutex::new(HashMap::new()),
                native_checkouts: Mutex::new(HashMap::new()),
                canonical_directories: Mutex::new(canonical_route::CanonicalDirectories::default()),
                action_directory_root_snapshot: Mutex::new(ActionDirectoryRootSnapshot::default()),
                pending_invocations: Mutex::new(HashMap::new()),
                served_invocations: Mutex::new(HashMap::new()),
                external_moderation_jobs: Mutex::default(),
                remote_invocations: Mutex::new(tilo_actions::remote::InvocationLog::default()),
                child_calls: Mutex::default(),
                ended_invocations: crate::automation::BrokerInvocations::default(),
                app_action_access: RwLock::new(
                    services
                        .app_action_access
                        .clone()
                        .unwrap_or_else(crate::app_actions::nothing_declared),
                ),
                app_resource_kinds: RwLock::new(services.app_resource_kinds.clone()),
                invocation_generation: tokio::sync::watch::Sender::new(0),
                window_generation: tokio::sync::watch::Sender::new(0),
                execution_activity: ActivityBus::default(),
                admission_lock: Mutex::new(()),
                journal: Mutex::new(journal),
                committing_invocations: Mutex::new(HashSet::new()),
                job_invocations: Mutex::new(std::collections::HashSet::new()),
                job_canceller: Mutex::new(None),
                local_report_receipts: Mutex::default(),
                stable_invocation_subscribers: Mutex::new(std::collections::HashSet::new()),
                workspace_governance_peers: Mutex::new(HashMap::new()),
                membership_surface_waiters: Mutex::new(Vec::new()),
                completed_invocations: Mutex::new(HashMap::new()),
                live_views: live_views::LiveViews::default(),
                host_step_stops: Mutex::new(HashMap::new()),
                untrusted_jobs: Mutex::default(),
                confirmation_grants: Mutex::new(HashMap::new()),
                cancellation_requests: Mutex::new(std::collections::HashSet::new()),
                invocations: std::sync::OnceLock::new(),
                records: std::sync::OnceLock::new(),
                asked_reads: std::sync::OnceLock::new(),
                placement: std::sync::OnceLock::new(),
                app_asks: std::sync::OnceLock::new(),
                invocation_id: AtomicU64::new(next_invocation),
                services,
                workers: Mutex::new(Vec::new()),
                worker_admitted: tokio::sync::Notify::new(),
                local_turn: std::sync::Arc::clone(&local_turn),
                offloads: Mutex::default(),
                host_workers: Mutex::default(),
                route_holders: Mutex::default(),
                service_peers: Mutex::new(Vec::new()),
                thread_restarts: AtomicU64::new(0),
                turn_work: std::sync::Arc::new(TurnWork {
                    handed: Mutex::new(Vec::new()),
                    turn: std::sync::Arc::clone(&local_turn),
                }),
                #[cfg(any(test, feature = "test-support"))]
                gone: BrokerGone::default(),
            }),
            bridge_rx,
        )
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_broker_with_idle_directory_queue() -> crate::collab_broker::AppActionBroker {
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = Self::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        );
        crate::collab_broker::AppActionBroker::from_state_for_test(state)
    }
    /// A broker over a host's own workspace authority and services, with no
    /// transport, for host integration tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_broker_with_authority(
        membership: WorkspaceTreeHandle,
        services: ProcessServices,
    ) -> crate::collab_broker::AppActionBroker {
        let (state, _bridge_rx) = Self::test_state(Some(membership), services);
        crate::collab_broker::AppActionBroker::from_state_for_test(state)
    }
    /// [`Self::test_state_configured`] with no native config and no transport,
    /// the host fixture most tests start from.
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn test_state(
        membership: Option<WorkspaceTreeHandle>,
        services: ProcessServices,
    ) -> (
        Arc<Self>,
        std::sync::mpsc::Receiver<ActionDirectoryBridgeRequest>,
    ) {
        Self::test_state_configured(membership, None, PeerPublisher::disabled(), services)
    }
    /// [`Self::test_state`] for a test outside this module, which drains no bridge: the
    /// receiver is dropped here.
    #[cfg(test)]
    pub(crate) fn test_state_only(
        membership: Option<WorkspaceTreeHandle>,
        services: ProcessServices,
    ) -> Arc<Self> {
        let (state, bridge) = Self::test_state(membership, services);
        drop(bridge);
        state
    }
    /// The state [`Self::test_broker_with_authority`] wraps, with the bridge a
    /// test drains itself, its own publisher, or a native config.
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn test_state_configured(
        membership: Option<WorkspaceTreeHandle>,
        config: Option<&crate::NativePeerBrokerConfig>,
        publisher: Arc<PeerPublisher>,
        services: ProcessServices,
    ) -> (
        Arc<Self>,
        std::sync::mpsc::Receiver<ActionDirectoryBridgeRequest>,
    ) {
        Self::new_configured(
            &Arc::new(FanoutState::new()),
            Arc::new(Mutex::new(BrokerChannels::default())),
            publisher,
            membership,
            None,
            config,
            services,
        )
    }
    /// A transport-free fixture with an explicit admission turn. Certified
    /// invocations enqueue inert ownership, just as they do on the live broker.
    #[cfg(any(test, feature = "test-support"))]
    #[allow(
        clippy::expect_used,
        clippy::panic,
        clippy::missing_panics_doc,
        reason = "a test-support fixture: a turn with nothing to service fails the test that asked for it"
    )]
    pub fn test_broker_with_authority_and_invoke_turn(
        membership: WorkspaceTreeHandle,
        services: ProcessServices,
    ) -> (crate::collab_broker::AppActionBroker, impl Fn()) {
        let (state, bridge_rx) = Self::test_state(Some(membership), services);
        let broker = crate::collab_broker::AppActionBroker::from_state_for_test(Arc::clone(&state));
        // What the journal's writer hands the broker's turn (an admission answered once it is
        // durable) runs as the live loop runs it: woken by the turn, with the state, while the
        // broker lives.
        let work = Arc::clone(&state.turn_work);
        let owner = Arc::downgrade(&state);
        std::thread::Builder::new()
            .name("tilo-broker-turn".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .expect("the turn's runtime");
                runtime.block_on(async move {
                    loop {
                        work.turn.notified().await;
                        let Some(state) = owner.upgrade() else {
                            return;
                        };
                        for task in std::mem::take(&mut *work.handed.lock_recover()) {
                            task(&state);
                        }
                    }
                });
            })
            .expect("the broker's turn thread");
        let turn = move || {
            let first =
                tilo_test_env::recv("the certified invocation at the broker turn", &bridge_rx);
            for request in std::iter::once(first).chain(bridge_rx.try_iter()) {
                let ActionDirectoryBridgeRequest::InvokeEager(invoke) = request else {
                    panic!("the transport-free fixture only services eager invocations");
                };
                state.invoke_bridged(*invoke);
            }
        };
        (broker, turn)
    }
    /// Retain a terminal receipt as if an application had reported it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn record_completion_for_test(
        &self,
        id: u64,
        actor: &tilo_action_policy::Actor,
        update: tilo_actions::InvocationUpdate,
    ) {
        self.record_completion(
            id,
            InvocationOwner::Local {
                actor: actor.clone(),
            },
            update,
        );
    }
    /// Stand in for an application's invocation stream on `peer_id`.
    /// The fixture receives the full envelope, including its stable invocation UUID.
    #[cfg(any(test, feature = "test-support"))]
    pub fn capture_invocations_for_test(
        &self,
        peer_id: u64,
    ) -> mpsc::UnboundedReceiver<InvocationEnvelope> {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut subscribers) = self.publishers.streams.lock() {
            subscribers.insert(peer_id, tx);
        }
        if let Ok(mut stable) = self.stable_invocation_subscribers.lock() {
            stable.insert(peer_id);
        }
        self.publishers.streams_changed();
        rx
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn replace_action_directory_roots_for_test(&self, roots: Vec<ActionDirectoryRoot>) {
        if let Ok(mut registered) = self.action_directory_roots.lock() {
            registered.clear();
            registered.extend(roots.into_iter().map(|root| (root.peer_id, root)));
        }
        self.refresh_action_directory_root_snapshot();
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn replace_published_surfaces_for_test(&self, surfaces: Vec<PublishedSurface>) {
        if let Ok(mut published) = self.publishers.surfaces.lock() {
            published.clear();
            published.extend(
                surfaces
                    .into_iter()
                    .map(|surface| (surface.peer_id, surface)),
            );
            self.surfaces_changed();
        }
    }
}
