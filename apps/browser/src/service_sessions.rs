//! Private service credentials are resolved from canonical custody, never document state.
use browser_continuity::sessions::{Service, SessionCookies};
use ui::tilo_resource::Effect;
use ui::webview::{
    ServiceSessionReply, ServiceSessionRequest, SharedEngine, WebViewCommand, WebViewEngine,
};

const ANCHOR: &str = "service:";

#[derive(Clone)]
pub(crate) struct Selection {
    pub id: String,
    pub service: Service,
    binding: String,
}
impl Selection {
    pub fn anchor(&self) -> String {
        format!("{ANCHOR}{}", self.id)
    }
    pub fn label(&self) -> &'static str {
        match self.service {
            Service::Slack => "Slack",
            Service::Discord => "Discord",
        }
    }
    pub fn resource(&self) -> tilo_actions::Resource {
        let mut open = crate::effects::BrowserEffect::OpenServiceSession {
            session_id: self.id.clone(),
        }
        .bound();
        open.label = format!("Open {}", self.label());
        tilo_actions::Resource::new(
            format!("browser.service.{}", self.id),
            "browser.service",
            self.label(),
        )
        .with_action(open)
    }
}

pub(crate) fn identity(person: &str) -> Option<[u8; 32]> {
    if person.bytes().any(|b| b.is_ascii_uppercase()) || person.len() != 64 {
        return None;
    }
    let mut key = [0; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(person.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    (key != [0; 32]).then_some(key)
}

pub(crate) fn selection(
    value: ui::collaboration::browser_service_sessions::BrowserServiceSessionDescriptor,
) -> Selection {
    Selection {
        id: value.id,
        service: value.service,
        binding: value.setup_binding,
    }
}

/// Private admission stays pending until both the broker and native startup settle.
#[cfg(target_os = "linux")]
pub(crate) struct PendingView {
    pub opening: ui::LocalState<Option<String>>,
    id: String,
    destination: Option<crate::service_destinations::Destination>,
    person: [u8; 32],
    client: ui::collaboration::browser_service_sessions::Client,
    request: Option<ui::collaboration::browser_service_sessions::Pending>,
    worker: Option<std::thread::JoinHandle<()>>,
    result: std::sync::Arc<std::sync::Mutex<Option<Result<ServiceView, String>>>>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    shutdown_failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    waker: Option<ui::WakeHandle>,
}
#[cfg(target_os = "linux")]
impl PendingView {
    pub fn new(
        id: String,
        person: [u8; 32],
        client: ui::collaboration::browser_service_sessions::Client,
    ) -> Self {
        Self {
            opening: ui::LocalState::new(None),
            id,
            destination: None,
            person,
            client,
            request: None,
            worker: None,
            result: Default::default(),
            cancelled: Default::default(),
            shutdown_failed: Default::default(),
            waker: None,
        }
    }
    pub fn with_destination(
        mut self,
        destination: Option<crate::service_destinations::Destination>,
    ) -> Self {
        self.destination = destination;
        self
    }
    pub fn attach_waker(&mut self, waker: ui::WakeHandle) {
        self.waker = Some(waker);
    }
    pub fn notice(&self) -> crate::notice::Notice {
        crate::notice::Notice::neutral("Opening your account…")
    }
    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({"session_id": self.id, "status":"opening", "authenticated":false, "engine_shutdown_confirmed":false})
    }
    pub fn poll(&mut self, selections: &[Selection]) -> Option<Result<ServiceView, String>> {
        if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return None;
        }
        if let Some(worker) = &self.worker {
            if let Some(answer) = self.result.try_lock().ok()?.take() {
                return Some(answer);
            }
            return worker
                .is_finished()
                .then(|| Err("This account could not be opened".into()));
        }
        if self.request.is_none() {
            let anchor = self.opening.with(Clone::clone)?;
            if anchor != crate::service_destinations::anchor(&self.id, self.destination.as_ref()) {
                return Some(Err("This account could not be opened".into()));
            }
            let Some(selected) = selections.iter().find(|v| v.id == self.id) else {
                return Some(Err("This account needs to be connected again".into()));
            };
            self.opening.set(None);
            match self.client.request(ui::collaboration::browser_service_sessions::BrowserServiceSessionCommand::Resolve {
                id: self.id.clone(), setup_binding: selected.binding.clone(),
            }, self.waker.clone()) { Ok(request) => self.request = Some(request), Err(error) => return Some(Err(error)) }
        }
        let reply = self.request.as_mut()?.poll()?;
        self.request = None;
        let session = match reply {
            Ok(
                ui::collaboration::browser_service_sessions::BrowserServiceSessionReply::Resolved {
                    session,
                },
            ) => session,
            _ => return Some(Err("This account needs to be connected again".into())),
        };
        let selected = selections.iter().find(|v| v.id == self.id)?.clone();
        let destination = self.destination.clone();
        if destination
            .as_ref()
            .is_some_and(|value| value.service != selected.service)
        {
            return Some(Err("This account could not be opened".into()));
        }
        let result = self.result.clone();
        let person = self.person;
        let waker = self.waker.clone();
        let cancelled = self.cancelled.clone();
        let shutdown_failed = self.shutdown_failed.clone();
        match std::thread::Builder::new()
            .name("browser-service-open".into())
            .spawn(move || {
                let mut answer =
                    ServiceView::from_session(selected, person, session).map(|mut view| {
                        view.destination = destination;
                        view
                    });
                if let Ok(mut slot) = result.lock() {
                    if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                        if let Ok(view) = &mut answer {
                            if view.finish_close().is_err() {
                                shutdown_failed.store(true, std::sync::atomic::Ordering::Release);
                            }
                        }
                    } else {
                        if let (Ok(view), Some(waker)) = (&mut answer, &waker) {
                            view.attach_waker(waker.clone());
                            let wake = waker.clone();
                            view.engine().set_waker(Box::new(move || wake.wake()));
                        }
                        *slot = Some(answer);
                    }
                }
                if let Some(waker) = waker {
                    waker.wake();
                }
            }) {
            Ok(worker) => self.worker = Some(worker),
            Err(_) => return Some(Err("This account could not be opened".into())),
        }
        None
    }
    pub fn request_close(&mut self) -> bool {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        self.request = None;
        if let Ok(mut result) = self.result.try_lock() {
            if let Some(Ok(view)) = result.as_mut() {
                let _ = view.request_close();
            }
        }
        self.close_complete()
    }
    pub fn close_complete(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::Acquire)
            && !self
                .shutdown_failed
                .load(std::sync::atomic::Ordering::Acquire)
            && self
                .worker
                .as_ref()
                .is_none_or(std::thread::JoinHandle::is_finished)
            && self
                .result
                .try_lock()
                .is_ok_and(|result| match result.as_ref() {
                    Some(Ok(view)) => view.close_complete(),
                    _ => true,
                })
    }
    pub fn finish_close(&mut self) -> Result<(), String> {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        self.request = None;
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| "This account could not be closed")?;
        }
        if self
            .shutdown_failed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err("This account could not be closed".into());
        }
        if let Some(Ok(mut view)) = self
            .result
            .lock()
            .map_err(|_| "This account could not be closed")?
            .take()
        {
            view.finish_close()?;
        }
        Ok(())
    }
}
#[cfg(target_os = "linux")]
impl Drop for PendingView {
    fn drop(&mut self) {
        let _ = self.finish_close();
    }
}

#[cfg(target_os = "linux")]
pub(crate) struct ServiceView {
    selection: Selection,
    destination: Option<crate::service_destinations::Destination>,
    session: Option<SessionCookies>,
    profile: Option<crate::service_profiles::Profile>,
    expected_cookies: usize,
    expected_origins: usize,
    native_restart_readback_verified: bool,
    restart: Option<NativeRestart>,
    shutdown_done: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    cleanup: Option<NativeCleanup>,
    waker: Option<ui::WakeHandle>,
    engine: std::sync::Arc<ui::webview::chromium::ChromiumEngine>,
    pub opening: ui::LocalState<Option<String>>,
    status: &'static str,
    deadline: Option<ui::time::Instant>,
}
#[cfg(target_os = "linux")]
impl ServiceView {
    pub fn from_session(
        selection: Selection,
        person: [u8; 32],
        session: SessionCookies,
    ) -> Result<Self, String> {
        session
            .validate()
            .map_err(|_| "This account needs to be connected again")?;
        if session.service != selection.service
            || session.session_id != selection.id
            || session.identity_public_key != person
            || session.setup_binding != selection.binding
        {
            return Err("This account needs to be connected again".into());
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|time| i64::try_from(time.as_secs()).ok())
            .ok_or("This account needs to be connected again")?;
        if session.cookies.iter().any(|cookie| {
            cookie
                .expires_unix_seconds
                .is_some_and(|expiry| expiry <= now)
        }) {
            return Err("This account needs to be connected again".into());
        }
        let mut profile = crate::service_profiles::Profile::prepare(&session)
            .map_err(|_| "This account could not be opened".to_owned())?;
        let engine = match ui::webview::chromium::ChromiumEngine::with_service_session(
            profile
                .path()
                .map_err(|_| "This account could not be opened")?,
            &selection.id,
        ) {
            Ok(engine) => engine,
            Err(error) => {
                let _ = profile.cancel_after_engine_exit();
                return Err(error);
            }
        };
        let expected_cookies = session.cookies.len();
        let expected_origins = session.origin_storage.len();
        Ok(Self {
            selection,
            destination: None,
            session: Some(session),
            profile: Some(profile),
            expected_cookies,
            expected_origins,
            native_restart_readback_verified: false,
            restart: None,
            shutdown_done: None,
            cleanup: None,
            waker: None,
            engine,
            opening: ui::LocalState::new(None),
            status: "waiting",
            deadline: None,
        })
    }
    pub fn is_preparing(&self) -> bool {
        matches!(
            self.status,
            "waiting" | "importing" | "restarting" | "verifying"
        )
    }
    pub fn anchor(&self) -> String {
        self.selection.anchor()
    }
    pub fn engine(&self) -> SharedEngine {
        self.engine.clone()
    }
    pub fn poll_open(&mut self) -> bool {
        let Some(anchor) = self.opening.with(Clone::clone) else {
            return false;
        };
        self.opening.set(None);
        if self.status != "waiting" || anchor != self.selection.anchor() {
            return false;
        }
        if self
            .profile
            .as_ref()
            .is_some_and(|profile| !profile.needs_import())
        {
            self.session = None;
            self.status = "imported-unverified";
            self.engine.send(WebViewCommand::Navigate(
                self.destination.as_ref().map_or_else(
                    || self.selection.service.url().to_owned(),
                    |value| value.uri.clone(),
                ),
            ));
            return true;
        }
        let Some(session) = self.session.as_ref().cloned() else {
            return false;
        };
        // Retain selected credentials only through initial restart verification;
        // FlushStore alone does not complete the held destination profile.
        self.status = "importing";
        self.deadline = Some(ui::time::now() + ui::time::Duration::from_secs(45));
        if self
            .engine
            .import_service_session(ServiceSessionRequest {
                request_id: 1,
                verify_only: false,
                session,
            })
            .is_err()
        {
            self.deadline = None;
            self.status = "needs-reconnect";
            self.begin_shutdown();
        }
        true
    }
    pub fn apply(&mut self, reply: ServiceSessionReply) -> bool {
        let request_id = match self.status {
            "importing" => 1,
            "verifying" => 2,
            _ => return false,
        };
        if reply.request_id != request_id
            || reply.session_id != self.selection.id
            || reply.service != self.selection.service
        {
            return false;
        }
        self.deadline = None;
        if !reply.imported
            || reply.cookie_count != self.expected_cookies
            || reply.origin_storage_count != self.expected_origins
        {
            self.status = "needs-reconnect";
            self.begin_shutdown();
            return true;
        }
        if request_id == 1 {
            let Some(profile) = self.profile.take() else {
                self.status = "needs-reconnect";
                self.begin_shutdown();
                return true;
            };
            let Some(session) = self.session.take() else {
                self.profile = Some(profile);
                self.status = "needs-reconnect";
                self.begin_shutdown();
                return true;
            };
            self.status = "restarting";
            self.restart = Some(NativeRestart::start(
                self.engine.clone(),
                profile,
                session,
                self.waker.clone(),
            ));
            return true;
        }
        self.session = None;
        if self
            .profile
            .as_mut()
            .is_some_and(|profile| profile.complete().is_ok())
        {
            // Request 2 is the readonly reply from the actual replacement
            // engine. Cached profile reuse in poll_open never sets this.
            self.native_restart_readback_verified = true;
            self.status = "imported-unverified";
            self.engine.send(WebViewCommand::Navigate(
                self.destination.as_ref().map_or_else(
                    || self.selection.service.url().to_owned(),
                    |value| value.uri.clone(),
                ),
            ));
        } else {
            self.status = "needs-reconnect";
            self.begin_shutdown();
        }
        true
    }
    /// Replace the WebView engine only after the off-frame restart has settled.
    /// The verification reply belongs to that replacement engine's event queue.
    pub fn poll_engine(&mut self) -> Option<SharedEngine> {
        let restart = self.restart.as_mut()?;
        match restart.poll()? {
            Ok((engine, profile)) => {
                self.engine = engine;
                self.profile = Some(profile);
                self.restart = None;
                self.status = "verifying";
                self.deadline = Some(ui::time::now() + ui::time::Duration::from_secs(45));
                Some(self.engine())
            }
            Err(()) => {
                self.status = "needs-reconnect";
                self.begin_shutdown();
                None
            }
        }
    }
    pub fn poll_timeout(&mut self) -> bool {
        if !self
            .deadline
            .is_some_and(|deadline| ui::time::now() >= deadline)
        {
            return false;
        }
        self.deadline = None;
        self.status = "needs-reconnect";
        self.begin_shutdown();
        true
    }
    fn begin_shutdown(&mut self) {
        self.session = None;
        self.deadline = None;
        if let Some(restart) = self.restart.as_mut() {
            restart.request_close();
            return;
        }
        let Some(profile) = self.profile.take() else {
            return;
        };
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.shutdown_done = Some(done.clone());
        self.cleanup = Some(cleanup_after_native_exit(
            self.engine.clone(),
            profile,
            done,
            self.waker.clone(),
        ));
    }
    pub fn attach_waker(&mut self, waker: ui::WakeHandle) {
        self.waker = Some(waker);
    }
    pub fn request_close(&mut self) -> bool {
        self.begin_shutdown();
        if let Some(restart) = &self.restart {
            return restart.close_complete();
        }
        self.shutdown_done
            .as_ref()
            .is_some_and(|done| done.load(std::sync::atomic::Ordering::Acquire))
    }
    pub fn close_complete(&self) -> bool {
        if let Some(restart) = &self.restart {
            return restart.close_complete();
        }
        self.shutdown_done
            .as_ref()
            .is_some_and(|done| done.load(std::sync::atomic::Ordering::Acquire))
    }
    /// Called by the shared component teardown hook, never the paint pass.
    pub fn finish_close(&mut self) -> Result<(), String> {
        self.begin_shutdown();
        if let Some(restart) = self.restart.as_mut() {
            return restart.finish();
        }
        let Some(cleanup) = self.cleanup.as_mut() else {
            return Ok(());
        };
        cleanup.finish()
    }
    /// A public observation of the engine's navigation, not remote login success.
    pub(crate) fn destination_navigation_observed(&self, engine_uri: &str) -> bool {
        self.status == "imported-unverified"
            && self
                .destination
                .as_ref()
                .is_some_and(|target| target.uri == engine_uri)
    }
    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({"service": self.selection.service, "session_id": self.selection.id, "status": self.status, "authenticated": false,
            "origin_storage_imported": self.status == "imported-unverified" && self.expected_origins > 0,
            "requires_reconnect": self.status == "needs-reconnect" || self.selection.service == Service::Discord && self.expected_origins == 0,
            "engine_shutdown_confirmed": self.close_complete(),
            "native_restart_readback_verified": self.native_restart_readback_verified})
    }
    pub fn notice(&self) -> Option<crate::notice::Notice> {
        match self.status {
            "waiting" | "importing" | "restarting" | "verifying" => Some(
                crate::notice::Notice::neutral(format!("Opening {}…", self.selection.label())),
            ),
            "needs-reconnect" => Some(crate::notice::Notice::warning(format!(
                "Sign in to {} again",
                self.selection.label()
            ))),
            "imported-unverified"
                if self.selection.service == Service::Discord && self.expected_origins == 0 =>
            {
                Some(
                    crate::notice::Notice::neutral("Sign in to Discord")
                        .with_message("Sign in to finish connecting this account."),
                )
            }
            _ => None,
        }
    }
}
#[cfg(target_os = "linux")]
impl Drop for ServiceView {
    fn drop(&mut self) {
        self.begin_shutdown();
    }
}

#[cfg(target_os = "linux")]
type CleanupCustody = (
    std::sync::Arc<ui::webview::chromium::ChromiumEngine>,
    crate::service_profiles::Profile,
);
#[cfg(target_os = "linux")]
type CleanupSlot = std::sync::Arc<std::sync::Mutex<Option<CleanupCustody>>>;

/// Origin storage is qualified by a cold reopen, not by a write callback in
/// the first process. This worker retains the same profile lease throughout.
#[cfg(target_os = "linux")]
struct NativeRestart {
    slot: CleanupSlot,
    handle: Option<std::thread::JoinHandle<()>>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ready: std::sync::Arc<std::sync::atomic::AtomicBool>,
    failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    cleanup: Option<NativeCleanup>,
    waker: Option<ui::WakeHandle>,
}
#[cfg(target_os = "linux")]
impl NativeRestart {
    fn start(
        engine: std::sync::Arc<ui::webview::chromium::ChromiumEngine>,
        profile: crate::service_profiles::Profile,
        session: SessionCookies,
        waker: Option<ui::WakeHandle>,
    ) -> Self {
        let slot = std::sync::Arc::new(std::sync::Mutex::new(Some((engine, profile))));
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let failed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_slot = slot.clone();
        let worker_cancelled = cancelled.clone();
        let worker_ready = ready.clone();
        let worker_failed = failed.clone();
        let worker_done = done.clone();
        let wake = waker.clone();
        let spawned = std::thread::Builder::new()
            .name("browser-service-verify".into())
            .spawn(move || {
                let result = restart_for_session_verification(
                    worker_slot,
                    session,
                    &worker_cancelled,
                    &worker_done,
                    wake.clone(),
                );
                worker_failed.store(result.is_err(), std::sync::atomic::Ordering::Release);
                worker_ready.store(true, std::sync::atomic::Ordering::Release);
                if let Some(waker) = wake {
                    waker.wake();
                }
            });
        let handle = match spawned {
            Ok(handle) => Some(handle),
            Err(_) => {
                failed.store(true, std::sync::atomic::Ordering::Release);
                ready.store(true, std::sync::atomic::Ordering::Release);
                None
            }
        };
        Self {
            slot,
            handle,
            cancelled,
            ready,
            failed,
            done,
            cleanup: None,
            waker,
        }
    }

    fn settled(&self) -> bool {
        self.ready.load(std::sync::atomic::Ordering::Acquire)
            && self
                .handle
                .as_ref()
                .is_none_or(std::thread::JoinHandle::is_finished)
    }

    fn poll(&mut self) -> Option<Result<CleanupCustody, ()>> {
        if !self.settled() || self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return None;
        }
        if self.failed.load(std::sync::atomic::Ordering::Acquire) {
            return Some(Err(()));
        }
        self.slot.try_lock().ok()?.take().map(Ok)
    }

    fn request_close(&mut self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        if !self.settled() || self.cleanup.is_some() {
            return;
        }
        let payload = self.slot.try_lock().ok().and_then(|mut slot| slot.take());
        if let Some((engine, profile)) = payload {
            self.cleanup = Some(cleanup_after_native_exit(
                engine,
                profile,
                self.done.clone(),
                self.waker.clone(),
            ));
        }
    }

    fn close_complete(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::Acquire)
            && self.done.load(std::sync::atomic::Ordering::Acquire)
            && self
                .handle
                .as_ref()
                .is_none_or(std::thread::JoinHandle::is_finished)
    }

    /// Only component teardown calls this blocking path.
    fn finish(&mut self) -> Result<(), String> {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(handle) = self.handle.take() {
            if handle.join().is_err() {
                retain_unconfirmed_cleanup(self.slot.clone());
                return Err("Account engine shutdown could not be confirmed".into());
            }
        }
        self.request_close();
        if let Some(cleanup) = self.cleanup.as_mut() {
            cleanup.finish()?;
        }
        if self.close_complete() {
            Ok(())
        } else {
            Err("Account engine shutdown could not be confirmed".into())
        }
    }
}
#[cfg(target_os = "linux")]
impl Drop for NativeRestart {
    fn drop(&mut self) {
        self.request_close();
        let empty_settled = self.settled() && self.slot.try_lock().is_ok_and(|slot| slot.is_none());
        if !empty_settled && !self.done.load(std::sync::atomic::Ordering::Acquire) {
            retain_unconfirmed_cleanup(self.slot.clone());
        }
    }
}

/// Unexpected worker failure must preserve authority as well as the engine.
#[cfg(target_os = "linux")]
struct RestartCustody {
    slot: CleanupSlot,
    payload: Option<CleanupCustody>,
}
#[cfg(target_os = "linux")]
impl RestartCustody {
    fn store(&mut self) {
        if let Some(payload) = self.payload.take() {
            *self
                .slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(payload);
        }
    }
}
#[cfg(target_os = "linux")]
impl Drop for RestartCustody {
    fn drop(&mut self) {
        if self.payload.is_some() {
            self.store();
            retain_unconfirmed_cleanup(self.slot.clone());
        }
    }
}

#[cfg(target_os = "linux")]
fn restart_for_session_verification(
    slot: CleanupSlot,
    session: SessionCookies,
    cancelled: &std::sync::atomic::AtomicBool,
    done: &std::sync::atomic::AtomicBool,
    waker: Option<ui::WakeHandle>,
) -> Result<(), ()> {
    use ui::webview::WebViewEngine as _;
    let payload = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let mut custody = RestartCustody { slot, payload };
    let Some((engine, profile)) = custody.payload.as_ref() else {
        return Err(());
    };
    engine.shutdown_service_session().map_err(|_| ())?;
    if cancelled.load(std::sync::atomic::Ordering::Acquire) {
        return finish_cancelled_restart(&mut custody, done);
    }
    let replacement = ui::webview::chromium::ChromiumEngine::with_service_session(
        profile.path().map_err(|_| ())?,
        &session.session_id,
    )
    .map_err(|_| ())?;
    if let Some((engine, _)) = custody.payload.as_mut() {
        *engine = replacement.clone();
    }
    if let Some(waker) = waker {
        replacement.set_waker(Box::new(move || waker.wake()));
    }
    if cancelled.load(std::sync::atomic::Ordering::Acquire) {
        return finish_cancelled_restart(&mut custody, done);
    }
    replacement
        .import_service_session(ServiceSessionRequest {
            request_id: 2,
            verify_only: true,
            session,
        })
        .map_err(|_| ())?;
    if cancelled.load(std::sync::atomic::Ordering::Acquire) {
        return finish_cancelled_restart(&mut custody, done);
    }
    custody.store();
    Ok(())
}

#[cfg(target_os = "linux")]
fn finish_cancelled_restart(
    custody: &mut RestartCustody,
    done: &std::sync::atomic::AtomicBool,
) -> Result<(), ()> {
    let Some((engine, profile)) = custody.payload.as_mut() else {
        return Err(());
    };
    engine.shutdown_service_session().map_err(|_| ())?;
    if profile.needs_import() {
        profile.cancel_after_engine_exit().map_err(|_| ())?;
    }
    custody.payload = None;
    done.store(true, std::sync::atomic::Ordering::Release);
    Ok(())
}

// A failed thread start or unconfirmed process wait must retain the lease and
// private profile authority. It must never release them while a child may live.
#[cfg(target_os = "linux")]
fn retain_unconfirmed_cleanup(slot: CleanupSlot) {
    static RETAINED: std::sync::OnceLock<std::sync::Mutex<Vec<CleanupSlot>>> =
        std::sync::OnceLock::new();
    RETAINED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(slot);
}

#[cfg(target_os = "linux")]
fn cleanup_after_native_exit(
    engine: std::sync::Arc<ui::webview::chromium::ChromiumEngine>,
    profile: crate::service_profiles::Profile,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    waker: Option<ui::WakeHandle>,
) -> NativeCleanup {
    let slot = std::sync::Arc::new(std::sync::Mutex::new(Some((engine, profile))));
    let worker_slot = slot.clone();
    let worker_done = done.clone();
    let spawned = std::thread::Builder::new()
        .name("browser-service-close".into())
        .spawn(move || {
            let payload = worker_slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            let mut custody = RestartCustody {
                slot: worker_slot,
                payload,
            };
            if let Some((engine, profile)) = custody.payload.as_mut() {
                if engine.shutdown_service_session().is_ok()
                    && (!profile.needs_import() || profile.cancel_after_engine_exit().is_ok())
                {
                    custody.payload = None;
                    worker_done.store(true, std::sync::atomic::Ordering::Release);
                }
            }
            if let Some(waker) = waker {
                waker.wake();
            }
        });
    NativeCleanup {
        slot,
        handle: spawned.ok(),
        done,
    }
}

#[cfg(target_os = "linux")]
struct NativeCleanup {
    slot: CleanupSlot,
    handle: Option<std::thread::JoinHandle<()>>,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
#[cfg(target_os = "linux")]
impl NativeCleanup {
    fn finish(&mut self) -> Result<(), String> {
        if let Some(handle) = self.handle.take() {
            handle
                .join()
                .map_err(|_| "Account engine shutdown could not be confirmed")?;
        } else if !self.done.load(std::sync::atomic::Ordering::Acquire) {
            // Thread creation can fail. Teardown owns the tree already and may
            // wait synchronously; render/sync never takes this fallback.
            let payload = self
                .slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            let mut custody = RestartCustody {
                slot: self.slot.clone(),
                payload,
            };
            if let Some((engine, profile)) = custody.payload.as_mut() {
                if engine.shutdown_service_session().is_ok()
                    && (!profile.needs_import() || profile.cancel_after_engine_exit().is_ok())
                {
                    custody.payload = None;
                    self.done.store(true, std::sync::atomic::Ordering::Release);
                }
            }
        }
        if self.done.load(std::sync::atomic::Ordering::Acquire) {
            Ok(())
        } else {
            Err("Account engine shutdown could not be confirmed".into())
        }
    }
}
#[cfg(target_os = "linux")]
impl Drop for NativeCleanup {
    fn drop(&mut self) {
        if !self.done.load(std::sync::atomic::Ordering::Acquire) {
            retain_unconfirmed_cleanup(self.slot.clone());
        }
    }
}

/// Exercise the consumer's actual private-secret and public-action codecs.
/// Native engine persistence is qualified separately against its signed host.
pub fn service_session_capability() -> Result<serde_json::Value, String> {
    use browser_continuity::sessions::{Cookie, OriginStorage, SameSite};
    let failure = || "Selected account consumer contract failed".to_owned();
    let session = SessionCookies {
        schema: 1,
        session_id: "consumer-probe".into(),
        service: Service::Slack,
        identity_public_key: [5; 32],
        setup_binding: "consumer-probe".into(),
        source_snapshot_digest: [6; 32],
        origin_storage: Vec::new(),
        cookies: vec![Cookie {
            host: "app.slack.com".into(),
            domain: "app.slack.com".into(),
            host_only: true,
            name: "__Host-session".into(),
            value: "synthetic-consumer-cookie".into(),
            path: "/".into(),
            expires_unix_seconds: None,
            secure: true,
            http_only: true,
            same_site: SameSite::Lax,
        }],
    };
    probe_consumer_session(&session)?;
    let mut discord = session;
    discord.schema = 2;
    discord.session_id = "consumer-origin-probe".into();
    discord.service = Service::Discord;
    discord.cookies.clear();
    discord.origin_storage = vec![OriginStorage {
        origin: "https://discord.com".into(),
        key: "token".into(),
        value: "\"synthetic-consumer-origin\"".into(),
        source_snapshot_digest: [7; 32],
    }];
    probe_consumer_session(&discord)?;
    let mut foreign = discord.clone();
    foreign.origin_storage[0].origin = "https://discord.com.evil.example".into();
    if account_custody::service_sessions::service_session_secret(&foreign).is_ok() {
        return Err(failure());
    }
    let sdk = ui::webview::chromium::service_session_capability()?;
    if sdk["service_session_protocol"] != 2 || sdk["codec_verified"] != true {
        return Err(failure());
    }
    Ok(
        serde_json::json!({"schema":1,"service_session_protocol":2,"codec_verified":true,
        "custody_binding_verified":true,"resource_action_verified":true}),
    )
}

fn probe_consumer_session(session: &SessionCookies) -> Result<(), String> {
    let failure = || "Selected account consumer contract failed".to_owned();
    let secret = account_custody::service_sessions::service_session_secret(session)
        .map_err(|_| failure())?;
    let id = accounts::AccountId::new(&session.session_id);
    let restored = account_custody::service_sessions::service_session_from_secret(
        &secret,
        &id,
        [5; 32],
        "consumer-probe",
    )
    .map_err(|_| failure())?;
    if restored != *session
        || format!("{secret:?}").contains("synthetic-consumer-")
        || account_custody::service_sessions::service_session_from_secret(
            &secret,
            &id,
            [7; 32],
            "consumer-probe",
        )
        .is_ok()
        || account_custody::service_sessions::service_session_from_secret(
            &secret,
            &id,
            [5; 32],
            "foreign-setup",
        )
        .is_ok()
    {
        return Err(failure());
    }
    let selection = Selection {
        id: session.session_id.clone(),
        service: session.service,
        binding: session.setup_binding.clone(),
    };
    let resource = selection.resource();
    let action = resource.actions.first().ok_or_else(failure)?;
    let encoded = action
        .build_json(&serde_json::Map::new())
        .map_err(|_| failure())?;
    let payload: serde_json::Value =
        serde_json::from_slice(&encoded.payload).map_err(|_| failure())?;
    let decoded = crate::effects::BrowserEffect::decode(
        payload["effect"].as_str().ok_or_else(failure)?,
        payload["args"].as_object().ok_or_else(failure)?,
    )
    .map_err(|_| failure())?;
    if !matches!(decoded, crate::effects::BrowserEffect::OpenServiceSession { session_id } if session_id == session.session_id)
        || action.label != format!("Open {}", selection.label())
        || payload.to_string().contains("synthetic-consumer-")
    {
        return Err(failure());
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod lifecycle_tests {
    use super::*;
    use browser_continuity::sessions::{Cookie, SameSite};
    use std::os::unix::fs::PermissionsExt;
    use ui::component_lifecycle::ComponentLifecycle;
    use ui::webview::WebViewEngine;
    use ui::widget::{Widget, WindowCloseAdmission};

    #[test]
    fn browser_protocol_two_probe_exercises_discord_custody_and_public_resource() {
        let proof = service_session_capability().unwrap();
        assert_eq!(proof["service_session_protocol"], 2);
        assert_eq!(proof["custody_binding_verified"], true);
        assert_eq!(proof["resource_action_verified"], true);
        assert!(!proof.to_string().contains("synthetic-consumer-"));
        assert!(proof.get("authenticated").is_none());
    }

    struct ReapFixture(std::sync::Arc<ui::webview::chromium::ChromiumEngine>);
    impl Drop for ReapFixture {
        fn drop(&mut self) {
            // A receiver assertion can unwind while its cleanup worker owns the
            // supervisor join. Preserve TempDir until the public stop operation
            // observes actual reaping, without replacing the original failure.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(40);
            while self.0.shutdown_service_session().is_err() && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    struct CloseFixture(crate::BrowserWindow);
    impl Drop for CloseFixture {
        fn drop(&mut self) {
            let _ = self.0.close_components();
        }
    }

    struct ViewFixture(ServiceView);
    impl Drop for ViewFixture {
        fn drop(&mut self) {
            let _ = self.0.finish_close();
        }
    }

    #[test]
    fn origin_receipt_requires_reaped_restart_and_readonly_verification_before_completion() {
        receipt_requires_restart_verification(Service::Discord, true);
    }

    #[test]
    fn cookie_receipt_requires_reaped_restart_and_readonly_verification_before_completion() {
        receipt_requires_restart_verification(Service::Slack, true);
    }

    #[test]
    fn cookie_verification_refusal_never_completes_or_navigates_and_retires_profile() {
        receipt_requires_restart_verification(Service::Slack, false);
    }

    // Exercises the real Browser lifecycle and real owned subprocess/restart.
    // The wire peer is a fixture, not proof of native CEF/provider acceptance.
    fn receipt_requires_restart_verification(service: Service, verify_imported: bool) {
        use browser_continuity::sessions::OriginStorage;
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let helper = root.path().join("restart-native-fixture");
        std::fs::write(
            &helper,
            r#"#!/usr/bin/python3
import json,struct,sys,pathlib,os
profile=pathlib.Path(next(a[10:] for a in sys.argv[1:] if a.startswith('--profile=')))
def send(value):
    data=b'\x01'+json.dumps(value).encode()
    sys.stdout.buffer.write(struct.pack('<I',len(data))+data);sys.stdout.buffer.flush()
send({'event':'ready','cef_version':'fixture','chromium_version':'fixture'})
while True:
    prefix=sys.stdin.buffer.read(4)
    if not prefix: break
    count,=struct.unpack('<I',prefix)
    command=json.loads(sys.stdin.buffer.read(count)[1:])
    if command['op']=='shutdown':
        profile.joinpath('reaped-first').write_text(str(os.getpid()))
        break
    if command['op']=='navigate':
        assert profile.joinpath('verification-process').exists()
        profile.joinpath('observed-navigation').write_text(command['url'])
    if command['op']=='service_session_import':
        verify=command.get('verify_only',False)
        if command['service']=='slack':
            assert len(command['cookies'])==1 and command['origin_storage']==[]
            assert command['cookies'][0]['value']=='synthetic-restart-cookie'
        else:
            assert command['cookies']==[] and len(command['origin_storage'])==1
            assert command['origin_storage'][0]['value']=='\"synthetic-raw-dom-value\"'
        if verify:
            assert command['request_id']==2 and profile.joinpath('reaped-first').exists()
            assert profile.joinpath('first-process').read_text()!=str(os.getpid())
            profile.joinpath('verification-process').write_text(str(os.getpid()))
        else:
            assert command['request_id']==1
            profile.joinpath('first-process').write_text(str(os.getpid()))
        send({'event':'service_session_result','tab':1,'request_id':command['request_id'],
            'session_id':command['session_id'],'service':command['service'],
            'imported':not (verify and profile.parent.joinpath('test-verify-refusal').exists()),
            'cookie_count':len(command['cookies']) if not (verify and profile.parent.joinpath('test-verify-refusal').exists()) else 0,
            'origin_storage_count':len(command['origin_storage']) if not (verify and profile.parent.joinpath('test-verify-refusal').exists()) else 0})
"#,
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(ui::webview::chromium::runtime::HOST_OVERRIDE_ENV, &helper);
        let is_discord = service == Service::Discord;
        let id = if is_discord {
            "restart-discord-selected"
        } else {
            "restart-slack-selected"
        };
        let session = SessionCookies {
            schema: if is_discord { 2 } else { 1 },
            session_id: id.into(),
            service,
            identity_public_key: [8; 32],
            setup_binding: "restart-setup".into(),
            source_snapshot_digest: [3; 32],
            cookies: if is_discord {
                vec![]
            } else {
                vec![Cookie {
                    host: "app.slack.com".into(),
                    domain: "app.slack.com".into(),
                    host_only: true,
                    name: "__Host-session".into(),
                    value: "synthetic-restart-cookie".into(),
                    path: "/".into(),
                    expires_unix_seconds: None,
                    secure: true,
                    http_only: true,
                    same_site: SameSite::Lax,
                }]
            },
            origin_storage: if is_discord {
                vec![OriginStorage {
                    origin: "https://discord.com".into(),
                    key: "token".into(),
                    value: "\"synthetic-raw-dom-value\"".into(),
                    source_snapshot_digest: [5; 32],
                }]
            } else {
                vec![]
            },
        };
        let profile = crate::service_profiles::Profile::prepare_in(root.path(), &session).unwrap();
        let path = profile.path().unwrap().to_owned();
        let receipt = path.parent().unwrap().join("completed.json");
        if !verify_imported {
            std::fs::write(
                path.parent().unwrap().join("test-verify-refusal"),
                b"refuse",
            )
            .unwrap();
        }
        let engine =
            ui::webview::chromium::ChromiumEngine::with_service_session(&path, &session.session_id)
                .unwrap();
        let _reap = ReapFixture(engine.clone());
        let mut view = ViewFixture(ServiceView {
            selection: Selection {
                id: session.session_id.clone(),
                service: session.service,
                binding: session.setup_binding.clone(),
            },
            session: Some(session.clone()),
            profile: Some(profile),
            destination: crate::service_destinations::Destination::parse(if is_discord {
                "https://discord.com/channels/123/456"
            } else {
                "https://slack.com/app_redirect?team=T123&channel=C456"
            }),
            expected_cookies: usize::from(!is_discord),
            expected_origins: usize::from(is_discord),
            native_restart_readback_verified: false,
            restart: None,
            shutdown_done: None,
            cleanup: None,
            waker: None,
            engine,
            opening: ui::LocalState::new(Some(format!("service:{id}"))),
            status: "waiting",
            deadline: None,
        });
        assert!(view.0.poll_open());
        let mut first = None;
        tilo_test_env::wait_until("first actual import receipt", || {
            first = view
                .0
                .engine
                .drain_events()
                .into_iter()
                .find_map(|event| match event {
                    ui::webview::WebViewEvent::ServiceSessionResult(reply) => Some(reply),
                    _ => None,
                });
            first.is_some()
        });
        let began = std::time::Instant::now();
        assert!(view.0.apply(first.unwrap()));
        assert!(began.elapsed() < std::time::Duration::from_millis(500));
        assert_eq!(view.0.report()["status"], "restarting");
        assert_eq!(view.0.report()["native_restart_readback_verified"], false);
        assert!(
            !receipt.exists(),
            "first import receipt was treated as durable"
        );
        assert!(
            !view.0.report()["origin_storage_imported"]
                .as_bool()
                .unwrap()
        );
        assert!(
            !path.join("observed-navigation").exists(),
            "service navigated before restarted storage verification"
        );
        tilo_test_env::wait_until("actual reaped restart", || view.0.poll_engine().is_some());
        let _replacement_reap = ReapFixture(view.0.engine.clone());
        assert!(!receipt.exists(), "restart alone completed the profile");
        let mut verified = None;
        tilo_test_env::wait_until("actual readonly verification receipt", || {
            verified = view
                .0
                .engine
                .drain_events()
                .into_iter()
                .find_map(|event| match event {
                    ui::webview::WebViewEvent::ServiceSessionResult(reply) => Some(reply),
                    _ => None,
                });
            verified.is_some()
        });
        assert_ne!(
            std::fs::read(path.join("first-process")).unwrap(),
            std::fs::read(path.join("verification-process")).unwrap()
        );
        assert_eq!(verified.as_ref().unwrap().request_id, 2);
        assert!(view.0.apply(verified.unwrap()));
        if !verify_imported {
            assert_eq!(view.0.report()["status"], "needs-reconnect");
            assert_eq!(view.0.report()["native_restart_readback_verified"], false);
            assert!(!view.0.report()["authenticated"].as_bool().unwrap());
            assert!(
                !receipt.exists(),
                "failed verification completed the profile"
            );
            assert!(!path.join("observed-navigation").exists());
            view.0.finish_close().unwrap();
            assert!(view.0.close_complete());
            assert!(
                !path.exists(),
                "refused profile survived confirmed shutdown"
            );
            return;
        }
        assert!(receipt.exists());
        assert_eq!(view.0.report()["status"], "imported-unverified");
        assert_eq!(view.0.report()["native_restart_readback_verified"], true);
        assert_eq!(
            view.0.report()["origin_storage_imported"]
                .as_bool()
                .unwrap(),
            is_discord
        );
        assert!(!view.0.report()["authenticated"].as_bool().unwrap());
        tilo_test_env::wait_until("fixed service navigation after verification", || {
            path.join("observed-navigation").exists()
        });
        assert_eq!(
            std::fs::read_to_string(path.join("observed-navigation")).unwrap(),
            if is_discord {
                "https://discord.com/channels/123/456"
            } else {
                "https://slack.com/app_redirect?team=T123&channel=C456"
            }
        );
        view.0.finish_close().unwrap();
        assert!(view.0.close_complete());
        drop(view);
        let profile = crate::service_profiles::Profile::prepare_in(root.path(), &session).unwrap();
        assert!(!profile.needs_import());
        let engine = ui::webview::chromium::ChromiumEngine::with_service_session(
            profile.path().unwrap(),
            &session.session_id,
        )
        .unwrap();
        let _cached_reap = ReapFixture(engine.clone());
        let mut cached = ViewFixture(ServiceView {
            selection: Selection {
                id: session.session_id.clone(),
                service: session.service,
                binding: session.setup_binding.clone(),
            },
            session: Some(session),
            profile: Some(profile),
            destination: None,
            expected_cookies: usize::from(!is_discord),
            expected_origins: usize::from(is_discord),
            native_restart_readback_verified: false,
            restart: None,
            shutdown_done: None,
            cleanup: None,
            waker: None,
            engine,
            opening: ui::LocalState::new(Some(format!("service:{id}"))),
            status: "waiting",
            deadline: None,
        });
        assert!(cached.0.poll_open());
        assert_eq!(cached.0.report()["status"], "imported-unverified");
        assert_eq!(
            cached.0.report()["native_restart_readback_verified"],
            false,
            "completed-profile reuse is not fresh native restart/readback evidence"
        );
        cached.0.finish_close().unwrap();
        assert!(cached.0.close_complete());
    }

    #[test]
    fn mismatched_cookie_receipt_defers_close_without_stalling_frames_and_teardown_reaps_child() {
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let helper = root.path().join("bounded-native-fixture");
        std::fs::write(&helper, r#"#!/usr/bin/python3
import json,struct,sys,pathlib,time
profile=next(a[10:] for a in sys.argv[1:] if a.startswith('--profile='))
def send(value):
    data=b'\x01'+json.dumps(value).encode()
    sys.stdout.buffer.write(struct.pack('<I',len(data))+data);sys.stdout.buffer.flush()
send({'event':'ready','cef_version':'fixture','chromium_version':'fixture'})
while True:
    prefix=sys.stdin.buffer.read(4)
    if not prefix: break
    count,=struct.unpack('<I',prefix)
    command=json.loads(sys.stdin.buffer.read(count)[1:])
    if command['op']=='shutdown':
        pathlib.Path(profile,'shutdown-seen').write_text('native teardown begun')
        time.sleep(1)
        pathlib.Path(profile).parent.joinpath('test-child-exited').write_text('native teardown finished')
        break
    if command['op']=='service_session_import':
        send({'event':'service_session_result','tab':1,'request_id':command['request_id'],
            'session_id':command['session_id'],'service':command['service'],'imported':True,'cookie_count':2})
"#).unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(ui::webview::chromium::runtime::HOST_OVERRIDE_ENV, &helper);
        let session = SessionCookies {
            schema: 1,
            session_id: "lifecycle-selected-session".into(),
            service: Service::Slack,
            identity_public_key: [8; 32],
            setup_binding: "lifecycle-setup".into(),
            source_snapshot_digest: [3; 32],
            origin_storage: Vec::new(),
            cookies: vec![Cookie {
                host: "app.slack.com".into(),
                domain: "app.slack.com".into(),
                host_only: true,
                name: "__Host-session".into(),
                value: "synthetic-lifecycle-credential".into(),
                path: "/".into(),
                expires_unix_seconds: None,
                secure: true,
                http_only: true,
                same_site: SameSite::Lax,
            }],
        };
        let profile = crate::service_profiles::Profile::prepare_in(root.path(), &session).unwrap();
        let path = profile.path().unwrap().to_owned();
        let engine =
            ui::webview::chromium::ChromiumEngine::with_service_session(&path, &session.session_id)
                .unwrap();
        let _reap = ReapFixture(engine.clone());
        let mut view = ServiceView {
            selection: Selection {
                id: session.session_id.clone(),
                service: Service::Slack,
                binding: session.setup_binding.clone(),
            },
            destination: None,
            expected_cookies: 1,
            expected_origins: 0,
            native_restart_readback_verified: false,
            restart: None,
            session: Some(session),
            profile: Some(profile),
            engine: engine.clone(),
            opening: ui::LocalState::new(Some("service:lifecycle-selected-session".into())),
            status: "waiting",
            deadline: None,
            shutdown_done: None,
            cleanup: None,
            waker: None,
        };
        assert!(view.poll_open());
        let mut reply = None;
        tilo_test_env::wait_until("actual native service reply", || {
            reply = engine
                .drain_events()
                .into_iter()
                .find_map(|event| match event {
                    ui::webview::WebViewEvent::ServiceSessionResult(reply) => Some(reply),
                    _ => None,
                });
            reply.is_some()
        });
        let started = std::time::Instant::now();
        assert!(view.apply(reply.unwrap()));
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "receiver blocked paint on native shutdown"
        );
        assert_eq!(
            view.report()["status"],
            "needs-reconnect",
            "mismatched count fabricated successful import"
        );
        assert!(!view.report()["authenticated"].as_bool().unwrap());
        let mut window = CloseFixture(crate::BrowserWindow::new(
            crate::BrowserWidget::with_engine(engine, true).with_service_view(view),
            false,
        ));
        assert_eq!(
            window.0.on_window_close_requested(),
            WindowCloseAdmission::Defer
        );
        assert!(
            !window.0.take_window_close_intent(),
            "close admitted before native child exited"
        );
        let doc = ui::automerge::AutoCommit::new();
        let frame_started = std::time::Instant::now();
        window.0.sync(&doc, &ui::automerge::ROOT);
        assert!(
            frame_started.elapsed() < std::time::Duration::from_millis(500),
            "sync waited for native child"
        );
        let reports = window.0.close_components();
        assert!(
            reports.iter().all(|report| report.teardown_error.is_none()),
            "actual teardown did not confirm child exit"
        );
        assert!(
            !path.exists(),
            "incomplete profile retained after confirmed shutdown"
        );
        assert!(
            !path.parent().unwrap().join("completed.json").exists(),
            "mismatched native receipt was persisted"
        );
        assert_eq!(
            std::fs::read(path.parent().unwrap().join("test-child-exited")).unwrap(),
            b"native teardown finished"
        );
        window.0.sync(&doc, &ui::automerge::ROOT);
        assert!(
            window.0.take_window_close_intent(),
            "confirmed shutdown never released deferred close"
        );
        assert!(
            !window.0.take_window_close_intent(),
            "close intent repeated"
        );
    }
}
