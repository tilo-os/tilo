//! Root-owned custody for an ordinary app consuming a retained Home projection.
//! The installed person proof and source/target workers stay alive until the
//! actual consuming PID namespace has stopped. No public dispatcher is added.
use super::{
    installed_retained_actor::InstalledRetainedLaunchInput,
    retained_consumer_selection::ReviewedConsumerSelection,
    retained_projection_session::RetainedProjectionSession,
};
use launcher::{
    ordinary_child::ManagedConsumingBootstrap,
    ordinary_namespace::NamespaceWitness,
    owned_service::ServiceCancellation,
    retained_view::{RetainedViewMode, ReviewedViewIssue, RootConsumerChannel},
};
use std::{
    io,
    os::unix::net::UnixStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// Root preparation owns the projection before any view descriptor is handed
/// to a launcher. An interrupted handoff cannot turn absence of a witness into
/// evidence that an app never began reading the source.
pub(super) struct PendingRetainedConsumer {
    input: Option<Arc<InstalledRetainedLaunchInput>>,
    projection: Option<RetainedProjectionSession>,
    managed: Option<Arc<Mutex<ManagedConsumingBootstrap>>>,
    exposure: Option<launcher::owned_service::target_projection::HeldViewExposure>,
    exported: bool,
    transport: Option<UnixStream>,
    consumer: Option<NamespaceWitness>,
}
impl PendingRetainedConsumer {
    pub(super) fn new(
        input: Arc<InstalledRetainedLaunchInput>,
        projection: RetainedProjectionSession,
        cancel: &AtomicBool,
        managed: Arc<Mutex<ManagedConsumingBootstrap>>,
    ) -> io::Result<Self> {
        input.revalidate(cancel)?;
        Ok(Self {
            input: Some(input),
            projection: Some(projection),
            managed: Some(managed),
            exposure: None,
            exported: false,
            transport: None,
            consumer: None,
        })
    }

    /// Canonical channel transport only: retain the exact lease stream and
    /// receive the real namespace immediately after the view's transport ACK,
    /// before the projection's post-operation authority check can fail.
    pub(super) fn export_view(
        &mut self,
        selection: &ReviewedConsumerSelection,
        cancel: &AtomicBool,
        channel: RootConsumerChannel,
        cancellation: &ServiceCancellation,
    ) -> io::Result<()> {
        let input = self.input.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        if !selection.uses_input(input) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        selection.check(cancel)?;
        let (uid, gid) = input.target_credentials(cancel)?;
        let binding = selection.binding();
        let context_sha256 = binding.context_sha256()?;
        let projection = self
            .projection
            .as_ref()
            .ok_or(io::ErrorKind::NotConnected)?;
        if self.exposure.is_some() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        self.exposure = Some(projection.prepare_consumer_exposure(
            Arc::clone(self.managed.as_ref().ok_or(io::ErrorKind::NotConnected)?),
            channel.retain_peer(),
            binding,
        )?);
        let exposure = self.exposure.as_mut().ok_or(io::ErrorKind::NotConnected)?;
        exposure.place()?;
        let exported = &mut self.exported;
        let transport = &mut self.transport;
        let consumer = &mut self.consumer;
        let expected_source_view_sha256 = projection.expected_source_view_sha256()?;
        projection.with_consumer_observation(|observation| {
            let view = observation.file()?;
            let (stream, peer) = match channel.issue(
                ReviewedViewIssue {
                    exposure: Some(&mut *exposure),
                    original_home_alias: input.original_home_alias(cancel)?,
                    expected_source_view_sha256,
                    observation: Some(observation),
                    view,
                    uid,
                    gid,
                    mode: RetainedViewMode::Home,
                    context_sha256,
                },
                binding,
                cancellation,
            ) {
                Ok(issued) => {
                    *exported = true;
                    issued
                }
                Err(failure) => {
                    // Only the canonical sender can mint this observation. A
                    // receipt-only failure is not possible view delivery.
                    *exported = failure.view_may_have_been_delivered();
                    exposure.observe_failed_issue(&failure);
                    let (error, stream) = failure.into_parts();
                    *transport = Some(stream);
                    return Err(error);
                }
            };
            *transport = Some(stream);
            if !Arc::ptr_eq(&peer, &selection.retained_launcher()) {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            *consumer = Some(NamespaceWitness::receive(
                transport.as_mut().ok_or(io::ErrorKind::NotConnected)?,
                &peer,
                cancellation,
            )?);
            exposure.adopt_consumer(consumer.as_ref().ok_or(io::ErrorKind::NotConnected)?)?;
            Ok(())
        })
    }

    pub(super) fn observe_consumer(
        mut self,
        selection: ReviewedConsumerSelection,
    ) -> Result<RetainedConsumerLease, Box<Self>> {
        if self.consumer.is_none() {
            return Err(Box::new(self));
        }
        // Transfer all actual custody before any fallible validation. The root
        // driver retains the returned owner even when its check fails.
        Ok(RetainedConsumerLease {
            input: self.input.take(),
            projection: self.projection.take(),
            managed: self.managed.take(),
            exposure: self.exposure.take(),
            consumer: self.consumer.take(),
            transport: self.transport.take(),
            selection: Some(selection),
            consumer_stopped: false,
            exported: std::mem::take(&mut self.exported),
        })
    }

    /// Fixed root helper worker only. A missing consuming namespace witness
    /// cannot become evidence of absence when an FD may already be delivered.
    /// Revoke bytes immediately, retain this exact owner and remain alive. No
    /// receipt or terminal status is emitted for this incomplete handoff.
    pub(super) fn retain_failed_handoff(&mut self, cancel: &AtomicBool) {
        cancel.store(true, Ordering::Release);
        if let Some(input) = &self.input {
            input.revoke();
        }
        if self.exported {
            loop {
                std::thread::park_timeout(Duration::from_secs(1));
            }
        }
        // Even an unexported projection needs positively observed retirement.
        // A failed inspection or kernel operation preserves the same owner.
        while self.retire_unexported().is_err() {
            std::thread::park_timeout(Duration::from_millis(50));
        }
    }

    pub(super) fn retire_unexported(&mut self) -> io::Result<()> {
        if self.exported {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if let Some(exposure) = &mut self.exposure {
            exposure.retire()?;
        }
        self.exposure.take();
        if let Some(projection) = &mut self.projection {
            projection.retire()?;
        }
        self.projection.take();
        if let Some(managed) = &self.managed {
            managed
                .lock()
                .map_err(|_| io::Error::other("Managed consuming helper poisoned"))?
                .retire_before_exit();
        }
        self.managed.take();
        self.transport.take();
        self.consumer.take();
        self.input.take();
        Ok(())
    }
}
impl Drop for PendingRetainedConsumer {
    fn drop(&mut self) {
        if let Some(input) = &self.input {
            input.revoke();
        }
        if self.exported && self.consumer.is_none() {
            // Possible view with no genuine init witness remains unknown.
            // Keep this actual root owner alive; never release through exit.
            loop {
                std::thread::park_timeout(Duration::from_secs(1));
            }
        }
        if self.exported {
            if let Some(consumer) = &self.consumer {
                loop {
                    let _ = consumer.cancel_and_wait(Duration::from_secs(5));
                    if consumer.observed_stopped().is_ok_and(|stopped| stopped) {
                        break;
                    }
                    std::thread::park_timeout(Duration::from_millis(50));
                }
            }
            self.exported = false;
        }
        while self.projection.is_some() || self.exposure.is_some() || self.managed.is_some() {
            if self.retire_unexported().is_ok() {
                break;
            }
            std::thread::park_timeout(Duration::from_millis(50));
        }
    }
}

/// Construct only after the root actor has authenticated the real launcher,
/// replayed its signed package and attached the exact joint projection.
/// The opaque witness is minted by the canonical ordinary namespace owner;
/// an app PID, a Shell child exit or a wire status cannot replace it.
pub(super) struct RetainedConsumerLease {
    input: Option<Arc<InstalledRetainedLaunchInput>>,
    projection: Option<RetainedProjectionSession>,
    managed: Option<Arc<Mutex<ManagedConsumingBootstrap>>>,
    exposure: Option<launcher::owned_service::target_projection::HeldViewExposure>,
    consumer: Option<NamespaceWitness>,
    transport: Option<UnixStream>,
    selection: Option<ReviewedConsumerSelection>,
    consumer_stopped: bool,
    exported: bool,
}

impl RetainedConsumerLease {
    /// Actual kernel stop observation remains available after Files proof ends.
    /// Missing custody never becomes an observed successful stop.
    pub(super) fn consumer_observed_stopped(&self) -> io::Result<bool> {
        if self.consumer_stopped {
            return Ok(true);
        }
        self.consumer
            .as_ref()
            .ok_or(io::ErrorKind::NotConnected)?
            .observed_stopped()
    }
    /// True only after the real namespace stop and every owned resource release.
    pub(super) fn retirement_complete(&self) -> bool {
        self.consumer_stopped
            && self.input.is_none()
            && self.projection.is_none()
            && self.managed.is_none()
            && self.exposure.is_none()
            && self.consumer.is_none()
            && self.transport.is_none()
            && self.selection.is_none()
    }

    pub(super) fn check(&self, cancel: &AtomicBool) -> io::Result<()> {
        if self.consumer_stopped {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let consumer = self.consumer.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        if consumer.observed_stopped()? {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let selection = self.selection.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        let input = self.input.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        if !self.exported
            || self.transport.is_none()
            || self.managed.is_none()
            || !selection.uses_input(input)
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        consumer.validate_for_launcher(selection.launcher())?;
        self.exposure
            .as_ref()
            .ok_or(io::ErrorKind::NotConnected)?
            .check()?;
        selection.check(cancel)
    }

    /// Watch the Files role until its consuming namespace stops, then retire
    /// its source. Live custody is rechecked between the stops; the launcher's
    /// control channel ending retires Files, as does cancellation or a lost
    /// custody check. The root helper cannot return while the original actor
    /// lacks positively observed native retirement.
    pub(super) fn supervise(
        lease: &Arc<Mutex<Self>>,
        cancel: &Arc<AtomicBool>,
        grace: Duration,
        control: &UnixStream,
        original_actor: &mut Option<
            Arc<super::installed_retained_actor::ReviewedInstalledRetainedLaunch>,
        >,
        terminal: &mut Option<RetirementGate>,
    ) -> io::Result<()> {
        let mut refresh_at = Instant::now();
        let mut refusal: Option<io::Error> = None;
        let mut stop_files = false;
        let mut files_retired = false;
        let mut original_retired = false;
        let mut original_retirement_unknown = false;
        loop {
            let mut files_stopped = files_retired;
            if !files_retired {
                let inspection = (|| {
                    let owner = lease
                        .lock()
                        .map_err(|_| io::Error::other("Files lease poisoned"))?;
                    let stopped = owner.consumer_observed_stopped()?;
                    if !stopped && !stop_files && Instant::now() >= refresh_at {
                        owner.check(cancel)?;
                        refresh_at = Instant::now()
                            .checked_add(Duration::from_secs(2))
                            .ok_or(io::ErrorKind::InvalidInput)?;
                    }
                    Ok::<bool, io::Error>(stopped)
                })();
                match inspection {
                    Ok(stopped) => files_stopped = stopped,
                    Err(error) => {
                        refusal.get_or_insert(error);
                        stop_files = true;
                    }
                }
            }
            match launcher_control_ended(control) {
                Ok(false) => {}
                Ok(true) => stop_files = true,
                Err(error) => {
                    refusal.get_or_insert(error);
                    stop_files = true;
                }
            }
            if cancel.load(Ordering::Acquire) {
                refusal.get_or_insert_with(|| io::ErrorKind::Interrupted.into());
                stop_files = true;
            }
            if stop_files && !files_stopped {
                let mut owner = lease
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(input) = &owner.input {
                    input.revoke();
                }
                if owner.stop_consumer(grace).is_ok() {
                    files_stopped = true;
                }
            }
            // Workers may still hold original-source pins: their actual cleanup
            // must land before the original actor retires.
            if files_stopped && !files_retired {
                let mut owner = lease
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Err(error) = owner.retire(grace) {
                    refusal.get_or_insert(error);
                }
                files_retired = owner.retirement_complete();
            }
            if files_retired && !original_retired && !original_retirement_unknown {
                let unique = original_actor
                    .as_ref()
                    .is_some_and(|actor| Arc::strong_count(actor) == 1);
                if unique {
                    let Some(actor) = original_actor.take() else {
                        original_retirement_unknown = true;
                        cancel.store(true, Ordering::Release);
                        continue;
                    };
                    match actor.retire() {
                        Ok(()) => original_retired = true,
                        Err(error) => {
                            // A consuming retirement refusal can retain mounted
                            // native custody, so remain the live root process
                            // owner; never retry a DTO or turn an empty slot into
                            // observed retirement.
                            refusal.get_or_insert(error);
                            original_retirement_unknown = true;
                            cancel.store(true, Ordering::Release);
                        }
                    }
                }
            }
            if files_retired && original_retired {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        *terminal = Some(RetirementGate { _private: () });
        refusal.map_or(Ok(()), Err)
    }

    fn stop_consumer(&mut self, grace: Duration) -> io::Result<()> {
        if self.consumer_stopped {
            return Ok(());
        }
        let consumer = self.consumer.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        if !consumer.observed_stopped()? {
            consumer.cancel_and_wait(grace)?;
        }
        if !consumer.observed_stopped()? {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.consumer_stopped = true;
        Ok(())
    }

    /// Root helper error path. Never return to the process exit boundary while
    /// this actual consumer or either filesystem worker remains unobserved.
    /// Native cancellation/inspection errors are kept as failure; eventual
    /// successful teardown allows the caller to return the original failure.
    pub(super) fn retire_before_exit(&mut self, cancel: &AtomicBool) {
        cancel.store(true, Ordering::Release);
        if let Some(input) = &self.input {
            input.revoke();
        }
        while self.retire(Duration::from_secs(5)).is_err() {
            std::thread::park_timeout(Duration::from_millis(50));
        }
    }

    /// Off the UI thread. Timeout or inspection failure retains ALL custody.
    /// An actual app exit ends source callbacks before the target/source
    /// projection is retired; worker stop failures are preserved separately.
    pub(super) fn retire(&mut self, grace: Duration) -> io::Result<()> {
        if !self.consumer_stopped {
            let consumer = self.consumer.as_ref().ok_or(io::ErrorKind::NotConnected)?;
            if !consumer.observed_stopped()? {
                consumer.cancel_and_wait(grace)?;
            }
            if !consumer.observed_stopped()? {
                return Err(io::Error::other(
                    "Consuming namespace stop was not observed; retained source custody held",
                ));
            }
            self.consumer_stopped = true;
        }
        if let Some(exposure) = &mut self.exposure {
            exposure.retire()?;
        }
        self.exposure.take();
        if let Some(projection) = &mut self.projection {
            projection.retire()?;
        }
        self.projection.take();
        if let Some(managed) = &self.managed {
            managed
                .lock()
                .map_err(|_| io::Error::other("Managed consuming helper poisoned"))?
                .retire_before_exit();
        }
        self.managed.take();
        self.consumer.take();
        self.transport.take();
        self.selection.take();
        self.input.take();
        Ok(())
    }
}

/// Nothing is asked of the launcher's control channel once the app runs, so a
/// byte on it or its end means the launcher's role here is over.
fn launcher_control_ended(control: &UnixStream) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    let mut ready = libc::pollfd {
        fd: control.as_raw_fd(),
        events: libc::POLLIN | libc::POLLRDHUP,
        revents: 0,
    };
    if unsafe { libc::poll(&raw mut ready, 1, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if ready.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
        return Err(io::ErrorKind::BrokenPipe.into());
    }
    Ok(ready.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLRDHUP) != 0)
}

impl Drop for RetainedConsumerLease {
    fn drop(&mut self) {
        if let Some(input) = &self.input {
            input.revoke();
        }
        // This owner is confined to the existing root background job. Preserve
        // actual custody through retries, including transient unmount errors.
        while self.projection.is_some() || self.exposure.is_some() || self.managed.is_some() {
            if self.retire(Duration::from_secs(5)).is_ok() {
                break;
            }
            std::thread::park_timeout(Duration::from_millis(50));
        }
    }
}

/// Nonserializable proof that the genuine Files/actor/editor retirement gate completed.
pub(super) struct RetirementGate {
    _private: (),
}
