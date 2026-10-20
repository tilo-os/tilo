//! Private service credentials are resolved from canonical custody, never document state.
use browser_continuity::sessions::{Service, SessionCookies};
use ui::tilo_resource::Effect;
use ui::webview::{
    ServiceSessionReply, ServiceSessionRequest, SharedEngine, WebViewCommand, WebViewEngine,
};

mod forget;
pub use forget::PendingForget;

const ANCHOR: &str = "service:";
use crate::CATALOG;

#[derive(Clone)]
pub struct Selection {
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
        let mut forget = crate::effects::BrowserEffect::ForgetServiceSession {
            session_id: self.id.clone(),
        }
        .bound();
        forget.label = CATALOG.t("browser-service-forget");
        let mut retry = crate::effects::BrowserEffect::RetryForgetServiceSession {
            session_id: self.id.clone(),
        }
        .bound();
        retry.label = CATALOG.t("browser-service-forget-retry");
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
        .with_action(forget)
        .with_action(retry)
    }
}

pub fn identity(person: &str) -> Option<[u8; 32]> {
    if person.bytes().any(|b| b.is_ascii_uppercase()) || person.len() != 64 {
        return None;
    }
    let mut key = [0; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(person.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    (key != [0; 32]).then_some(key)
}

pub fn selection(
    value: ui::collaboration::browser_service_sessions::BrowserServiceSessionDescriptor,
) -> Selection {
    Selection {
        id: value.id,
        service: value.service,
        binding: value.setup_binding,
    }
}

/// Private admission stays pending until both the broker and native startup settle.
pub struct PendingView {
    pub opening: ui::LocalState<Option<String>>,
    id: String,
    destination: Option<crate::service_destinations::Destination>,
    person: [u8; 32],
    client: ui::collaboration::browser_service_sessions::Client,
    request: Option<ui::collaboration::browser_service_sessions::Pending>,
    native: native::Opening<ServiceView>,
    waker: Option<ui::WakeHandle>,
}
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
            native: native::Opening::default(),
            waker: None,
        }
    }
    #[must_use]
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
    pub fn notice() -> crate::notice::Notice {
        crate::notice::Notice::neutral("Opening your account…")
    }
    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({"session_id": self.id, "status":"opening", "authenticated":false, "engine_shutdown_confirmed":false})
    }
    pub fn poll(&mut self, selections: &[Selection]) -> Option<Result<ServiceView, String>> {
        if self.native.cancelled() {
            return None;
        }
        if self.native.started() {
            return self.native.poll();
        }
        if self.request.is_none() {
            let anchor = self.opening.with(Clone::clone)?;
            if anchor != crate::service_destinations::anchor(&self.id, self.destination.as_ref()) {
                return Some(Err("This account could not be opened".into()));
            }
            // A stale public anchor still challenges private Resolve. The
            // non-authorizing binding cannot grant custody for an absent row.
            let binding = selections.iter().find(|v| v.id == self.id).map_or_else(
                || "unavailable-selection".into(),
                |selected| selected.binding.clone(),
            );
            self.opening.set(None);
            match self.client.request(ui::collaboration::browser_service_sessions::BrowserServiceSessionCommand::Resolve {
                id: self.id.clone(), setup_binding: binding,
            }, self.waker.clone()) { Ok(request) => self.request = Some(request), Err(error) => return Some(Err(error)) }
        }
        let reply = self.request.as_mut()?.poll()?;
        self.request = None;
        let Ok(ui::collaboration::browser_service_sessions::BrowserServiceSessionReply::Resolved {
            session,
        }) = reply
        else {
            return Some(Err("This account needs to be connected again".into()));
        };
        let Some(selected) = selections.iter().find(|v| v.id == self.id).cloned() else {
            return Some(Err("This account needs to be connected again".into()));
        };
        let destination = self.destination.clone();
        if destination
            .as_ref()
            .is_some_and(|value| value.service != selected.service)
        {
            return Some(Err("This account could not be opened".into()));
        }
        let person = self.person;
        let waker = self.waker.clone();
        let wake = native_wake(waker.clone());
        if let Err(error) = self.native.start(
            move || {
                ServiceView::from_session(selected, person, session).map(|mut view| {
                    view.destination = destination;
                    if let Some(waker) = waker {
                        view.attach_waker(waker.clone());
                        view.engine().set_waker(Box::new(move || waker.wake()));
                    }
                    view
                })
            },
            wake,
        ) {
            return Some(Err(error));
        }
        None
    }
    pub fn request_close(&mut self) -> bool {
        self.request = None;
        self.native.request_close()
    }
    pub fn close_complete(&self) -> bool {
        self.native.close_complete()
    }
    pub fn finish_close(&mut self) -> Result<(), String> {
        self.request = None;
        self.native.finish_close()
    }
}
impl Drop for PendingView {
    fn drop(&mut self) {
        let _ = self.finish_close();
    }
}

pub struct ServiceView {
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
    engine: EngineAdapter,
    pub opening: ui::LocalState<Option<String>>,
    status: &'static str,
    deadline: Option<ui::time::Instant>,
}
impl ServiceView {
    pub fn from_session(
        selection: Selection,
        person: [u8; 32],
        session: SessionCookies,
    ) -> Result<Self, String> {
        session
            .validate()
            .map_err(|_| "This account needs to be connected again")?;
        let selected_id = &selection.id;
        if session.service != selection.service
            || &session.session_id != selected_id
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
            engine: EngineAdapter(engine),
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
        self.engine.0.clone()
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
        let Some(session) = self.session.clone() else {
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
    pub fn apply(&mut self, reply: &ServiceSessionReply) -> bool {
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
                native_wake(self.waker.clone()),
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
    /// Replace the `WebView` engine only after the off-frame restart has settled.
    /// The verification reply belongs to that replacement engine's event queue.
    pub fn poll_engine(&mut self) -> Option<SharedEngine> {
        let restart = self.restart.as_mut()?;
        if let Ok((engine, profile)) = restart.poll()? {
            self.engine = engine;
            self.profile = Some(profile);
            self.restart = None;
            self.status = "verifying";
            self.deadline = Some(ui::time::now() + ui::time::Duration::from_secs(45));
            Some(self.engine())
        } else {
            self.status = "needs-reconnect";
            self.begin_shutdown();
            None
        }
    }
    pub fn poll_timeout(&mut self) -> bool {
        if self
            .deadline
            .is_none_or(|deadline| ui::time::now() < deadline)
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
    pub fn forget(&mut self, id: &str) -> Result<serde_json::Value, String> {
        if self.selection.id != id {
            return Err(CATALOG.t("browser-service-forget-open-first"));
        }
        if matches!(self.status, "forgetting" | "forgotten") {
            return Ok(self.report());
        }
        // A restart owns the lease off-thread; never race its authority.
        let profile = self
            .profile
            .as_mut()
            .ok_or_else(|| CATALOG.t("browser-service-forget-wait"))?;
        profile
            .begin_forget()
            .map_err(|_| CATALOG.t("browser-service-forget-fence-failed"))?;
        self.status = "forgetting";
        self.begin_shutdown();
        Ok(self.report())
    }

    fn take_forget_cleanup(&mut self, id: &str) -> Result<NativeCleanup, String> {
        self.forget(id)?;
        self.cleanup
            .take()
            .ok_or_else(|| CATALOG.t("browser-service-forget-wait"))
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
    pub fn destination_navigation_observed(&self, engine_uri: &str) -> bool {
        self.status == "imported-unverified"
            && self
                .destination
                .as_ref()
                .is_some_and(|target| target.uri == engine_uri)
    }
    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({"service": self.selection.service, "session_id": self.selection.id, "status": if self.status == "forgetting" && self.close_complete() {
                "local-forgotten"
            } else if self.status == "forgetting" && self.cleanup.as_ref().is_some_and(NativeCleanup::settled) {
                "local-forget-incomplete"
            } else { self.status }, "authenticated": false,
            "custody_revoked": self.status == "forgotten", "custody_backend_needed": self.status == "forgetting",
            "local_only": self.status == "forgetting",
            "origin_storage_imported": self.status == "imported-unverified" && self.expected_origins > 0,
            "requires_reconnect": self.status == "needs-reconnect" || self.selection.service == Service::Discord && self.expected_origins == 0,
            "engine_shutdown_confirmed": self.close_complete(),
            "native_restart_readback_verified": self.native_restart_readback_verified})
    }
    pub fn notice(&self) -> Option<crate::notice::Notice> {
        match self.status {
            "forgotten" => Some(crate::notice::Notice::neutral(
                CATALOG.t("browser-service-forgotten"),
            )),
            "forgetting" => Some(crate::notice::Notice::neutral(
                CATALOG.t("browser-service-forgetting"),
            )),
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
impl native::OpeningValue for ServiceView {
    fn request_close(&mut self) {
        let _ = ServiceView::request_close(self);
    }
    fn close_complete(&self) -> bool {
        ServiceView::close_complete(self)
    }
    fn finish_close(&mut self) -> Result<(), String> {
        ServiceView::finish_close(self)
    }
}
impl Drop for ServiceView {
    fn drop(&mut self) {
        self.begin_shutdown();
    }
}

use browser_continuity::session_engine as native;
type NativeRestart = native::NativeRestart<EngineAdapter, crate::service_profiles::Profile>;
type NativeCleanup = native::NativeCleanup<EngineAdapter, crate::service_profiles::Profile>;
#[derive(Clone)]
struct EngineAdapter(std::sync::Arc<ui::webview::chromium::ChromiumEngine>);
impl std::ops::Deref for EngineAdapter {
    type Target = ui::webview::chromium::ChromiumEngine;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl native::Engine for EngineAdapter {
    fn shutdown_service_session(&self) -> Result<(), String> {
        self.0.shutdown_service_session()
    }
    fn reopen(path: &std::path::Path, id: &str) -> Result<Self, String> {
        ui::webview::chromium::ChromiumEngine::with_service_session(path, id).map(Self)
    }
    fn attach_waker(&self, wake: native::Wake) {
        self.0.set_waker(Box::new(move || wake()));
    }
    fn verify(&self, session: SessionCookies) -> Result<(), String> {
        self.0.import_service_session(ServiceSessionRequest {
            request_id: 2,
            verify_only: true,
            session,
        })
    }
}
fn native_wake(waker: Option<ui::WakeHandle>) -> Option<native::Wake> {
    waker.map(wake_callback)
}
fn wake_callback(waker: ui::WakeHandle) -> native::Wake {
    std::sync::Arc::new(move || waker.wake())
}
fn cleanup_after_native_exit(
    engine: EngineAdapter,
    profile: crate::service_profiles::Profile,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    waker: Option<ui::WakeHandle>,
) -> NativeCleanup {
    native::cleanup_after_native_exit(engine, profile, done, native_wake(waker))
}

/// Exercise the consumer's actual private-secret and public-action codecs.
/// Native engine persistence is qualified separately against its signed host.
/// # Errors
/// Custody, public resource/action, or native SDK codec qualification failed.
pub fn service_session_capability() -> Result<serde_json::Value, String> {
    let failure = || "Selected account consumer contract failed".to_owned();
    for selection in tilo_browser_sessions::probe_service_session_codecs()? {
        probe_consumer_session(&Selection {
            id: selection.id,
            service: selection.service,
            binding: selection.binding,
        })?;
    }
    let sdk = ui::webview::chromium::service_session_capability()?;
    if sdk
        .get("service_session_protocol")
        .and_then(serde_json::Value::as_u64)
        != Some(2)
        || sdk
            .get("codec_verified")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
    {
        return Err(failure());
    }
    Ok(
        serde_json::json!({"schema":1,"service_session_protocol":2,"codec_verified":true,
        "custody_binding_verified":true,"resource_action_verified":true}),
    )
}

fn probe_consumer_session(selection: &Selection) -> Result<(), String> {
    let failure = || "Selected account consumer contract failed".to_owned();
    let resource = selection.resource();
    let action = resource.actions.first().ok_or_else(failure)?;
    let encoded = action
        .build_json(&serde_json::Map::new())
        .map_err(|_| failure())?;
    let payload: serde_json::Value =
        serde_json::from_slice(&encoded.payload).map_err(|_| failure())?;
    let decoded = crate::effects::BrowserEffect::decode(
        payload
            .get("effect")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(failure)?,
        payload
            .get("args")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(failure)?,
    )
    .map_err(|_| failure())?;
    if !matches!(decoded, crate::effects::BrowserEffect::OpenServiceSession { session_id } if session_id == selection.id)
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
    fn forget_action_is_destructive_selected_and_never_carries_credentials() {
        let selected = Selection {
            id: "selected-account".into(),
            service: Service::Slack,
            binding: "private-setup-binding".into(),
        };
        let resource = selected.resource();
        assert_eq!(resource.id.0, "browser.service.selected-account");
        let action = resource
            .actions
            .iter()
            .find(|action| action.id == "forget-service-session")
            .unwrap();
        assert_eq!(
            action.contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        );
        assert_eq!(action.contract.kind, tilo_actions::ActionKind::Outside);
        let encoded = action.build_json(&serde_json::Map::new()).unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&encoded.payload).unwrap();
        let decoded = crate::effects::BrowserEffect::decode(
            payload["effect"].as_str().unwrap(),
            payload["args"].as_object().unwrap(),
        )
        .unwrap();
        assert!(
            matches!(decoded, crate::effects::BrowserEffect::ForgetServiceSession {
            session_id
        } if session_id == selected.id)
        );
        assert!(!payload.to_string().contains("private-setup-binding"));
    }

    #[test]
    fn retry_forget_is_selected_destructive_and_contains_no_custody_binding() {
        let selected = Selection {
            id: "selected-account".into(),
            service: Service::Slack,
            binding: "private-setup-binding".into(),
        };
        let resource = selected.resource();
        let action = resource
            .actions
            .iter()
            .find(|action| action.label == CATALOG.t("browser-service-forget-retry"))
            .unwrap();
        assert_eq!(action.contract.kind, tilo_actions::ActionKind::Outside);
        assert_eq!(action.contract.risk, tilo_actions::RiskLevel::Destructive);
        assert_eq!(
            action.contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        );
        let encoded = action.build_json(&serde_json::Map::new()).unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&encoded.payload).unwrap();
        let decoded = crate::effects::BrowserEffect::decode(
            payload["effect"].as_str().unwrap(),
            payload["args"].as_object().unwrap(),
        )
        .unwrap();
        assert!(
            matches!(decoded, crate::effects::BrowserEffect::RetryForgetServiceSession { session_id } if session_id == selected.id)
        );
        assert!(!payload.to_string().contains("private-setup-binding"));
    }

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

    // Fallible lifecycle tests report failed checks as errors, including the
    // compared values, so fixture errors and invariant failures share one path.
    macro_rules! require {
        ($condition:expr $(, $message:literal)? $(,)?) => {
            if !$condition {
                return Err(format!(
                    "assertion failed: {}{}",
                    stringify!($condition),
                    concat!($(": ", $message)?),
                ).into());
            }
        };
    }
    macro_rules! require_eq {
        ($left:expr, $right:expr $(, $message:literal)? $(,)?) => {
            match (&$left, &$right) {
                (left, right) if left != right => {
                    return Err(format!(
                        "assertion failed: {} == {}{}; left: {left:?}, right: {right:?}",
                        stringify!($left), stringify!($right), concat!($(": ", $message)?),
                    ).into());
                }
                _ => {}
            }
        };
    }
    macro_rules! require_ne {
        ($left:expr, $right:expr $(, $message:literal)? $(,)?) => {
            match (&$left, &$right) {
                (left, right) if left == right => {
                    return Err(format!(
                        "assertion failed: {} != {}{}; both: {left:?}",
                        stringify!($left), stringify!($right), concat!($(": ", $message)?),
                    ).into());
                }
                _ => {}
            }
        };
    }
    #[test]
    fn origin_receipt_requires_reaped_restart_and_readonly_verification_before_completion()
    -> Result<(), Box<dyn std::error::Error>> {
        receipt_requires_restart_verification(Service::Discord, true)
    }

    #[test]
    fn cookie_receipt_requires_reaped_restart_and_readonly_verification_before_completion()
    -> Result<(), Box<dyn std::error::Error>> {
        receipt_requires_restart_verification(Service::Slack, true)
    }

    #[test]
    fn cookie_verification_refusal_never_completes_or_navigates_and_retires_profile()
    -> Result<(), Box<dyn std::error::Error>> {
        receipt_requires_restart_verification(Service::Slack, false)
    }

    // Exercises the real Browser lifecycle and real owned subprocess/restart.
    // The wire peer is a fixture, not proof of native CEF/provider acceptance.
    fn receipt_requires_restart_verification(
        service: Service,
        verify_imported: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use browser_continuity::sessions::OriginStorage;
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()?;
        let helper = root.path().join("restart-native-fixture");
        std::fs::write(
            &helper,
            r#"#!/usr/bin/python3
import json,struct,sys,pathlib,os,time
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
        # The test holds the child here while it checks the caller went on.
        while profile.parent.joinpath('test-hold-shutdown').exists():
            time.sleep(0.01)
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
        )?;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))?;
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
        let profile = crate::service_profiles::Profile::prepare_in(root.path(), &session)?;
        let path = profile.path()?.to_owned();
        let receipt = path
            .parent()
            .ok_or("profile has no parent")?
            .join("completed.json");
        if !verify_imported {
            std::fs::write(
                path.parent()
                    .ok_or("profile has no parent")?
                    .join("test-verify-refusal"),
                b"refuse",
            )?;
        }
        let engine = ui::webview::chromium::ChromiumEngine::with_service_session(
            &path,
            &session.session_id,
        )?;
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
            engine: EngineAdapter(engine),
            opening: ui::LocalState::new(Some(format!("service:{id}"))),
            status: "waiting",
            deadline: None,
        });
        require!(view.0.poll_open());
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
        // The old child is held in its shutdown: applying the receipt returns
        // while it is still alive, so it never waits on the restart.
        let hold = path
            .parent()
            .ok_or("profile has no parent")?
            .join("test-hold-shutdown");
        std::fs::write(&hold, b"hold")?;
        require!(view.0.apply(&first.ok_or("missing first receipt")?));
        require_eq!(
            view.0.report().get("status").ok_or("missing status")?,
            "restarting"
        );
        require_eq!(
            view.0
                .report()
                .get("native_restart_readback_verified")
                .ok_or("missing native_restart_readback_verified")?,
            false
        );
        require!(
            !receipt.exists(),
            "first import receipt was treated as durable"
        );
        std::fs::remove_file(&hold)?;
        require!(
            !view
                .0
                .report()
                .get("origin_storage_imported")
                .ok_or("missing origin_storage_imported")?
                .as_bool()
                .ok_or("expected boolean")?
        );
        require!(
            !path.join("observed-navigation").exists(),
            "service navigated before restarted storage verification"
        );
        tilo_test_env::wait_until("actual reaped restart", || view.0.poll_engine().is_some());
        let _replacement_reap = ReapFixture(view.0.engine.0.clone());
        require!(!receipt.exists(), "restart alone completed the profile");
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
        require_ne!(
            std::fs::read(path.join("first-process"))?,
            std::fs::read(path.join("verification-process"))?
        );
        require_eq!(
            verified
                .as_ref()
                .ok_or("missing verification receipt")?
                .request_id,
            2
        );
        require!(
            view.0
                .apply(&verified.ok_or("missing verification receipt")?)
        );
        if !verify_imported {
            require_eq!(
                view.0.report().get("status").ok_or("missing status")?,
                "needs-reconnect"
            );
            require_eq!(
                view.0
                    .report()
                    .get("native_restart_readback_verified")
                    .ok_or("missing native_restart_readback_verified")?,
                false
            );
            require!(
                !view
                    .0
                    .report()
                    .get("authenticated")
                    .ok_or("missing authenticated")?
                    .as_bool()
                    .ok_or("expected boolean")?
            );
            require!(
                !receipt.exists(),
                "failed verification completed the profile"
            );
            require!(!path.join("observed-navigation").exists());
            view.0.finish_close()?;
            require!(view.0.close_complete());
            require!(
                !path.exists(),
                "refused profile survived confirmed shutdown"
            );
            return Ok(());
        }
        require!(receipt.exists());
        require_eq!(
            view.0.report().get("status").ok_or("missing status")?,
            "imported-unverified"
        );
        require_eq!(
            view.0
                .report()
                .get("native_restart_readback_verified")
                .ok_or("missing native_restart_readback_verified")?,
            true
        );
        require_eq!(
            view.0
                .report()
                .get("origin_storage_imported")
                .ok_or("missing origin_storage_imported")?
                .as_bool()
                .ok_or("expected boolean")?,
            is_discord
        );
        require!(
            !view
                .0
                .report()
                .get("authenticated")
                .ok_or("missing authenticated")?
                .as_bool()
                .ok_or("expected boolean")?
        );
        tilo_test_env::wait_until("fixed service navigation after verification", || {
            path.join("observed-navigation").exists()
        });
        require_eq!(
            std::fs::read_to_string(path.join("observed-navigation"))?,
            if is_discord {
                "https://discord.com/channels/123/456"
            } else {
                "https://slack.com/app_redirect?team=T123&channel=C456"
            }
        );
        view.0.finish_close()?;
        require!(view.0.close_complete());
        drop(view);
        let profile = crate::service_profiles::Profile::prepare_in(root.path(), &session)?;
        require!(!profile.needs_import());
        let engine = ui::webview::chromium::ChromiumEngine::with_service_session(
            profile.path()?,
            &session.session_id,
        )?;
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
            engine: EngineAdapter(engine),
            opening: ui::LocalState::new(Some(format!("service:{id}"))),
            status: "waiting",
            deadline: None,
        });
        require!(cached.0.poll_open());
        require_eq!(
            cached.0.report().get("status").ok_or("missing status")?,
            "imported-unverified"
        );
        require_eq!(
            cached
                .0
                .report()
                .get("native_restart_readback_verified")
                .ok_or("missing native_restart_readback_verified")?,
            false,
            "completed-profile reuse is not fresh native restart/readback evidence"
        );
        require!(cached.0.forget("a-different-account").is_err());
        require!(path.exists());
        // Forgetting answers while any child it retires is held in shutdown.
        std::fs::write(&hold, b"hold")?;
        let reply = cached.0.forget(id)?;
        std::fs::remove_file(&hold)?;
        require_eq!(
            reply
                .get("custody_revoked")
                .ok_or("missing custody_revoked")?,
            false
        );
        require_eq!(
            reply
                .get("custody_backend_needed")
                .ok_or("missing custody_backend_needed")?,
            true
        );
        cached.0.finish_close()?;
        require!(cached.0.close_complete());
        require_eq!(
            cached.0.report().get("status").ok_or("missing status")?,
            "local-forgotten"
        );
        require!(
            !path.exists(),
            "completed selected profile survived reaping"
        );
        require!(!receipt.exists());
        require!(
            path.parent()
                .ok_or("profile has no parent")?
                .join("forgetting")
                .exists()
        );
        Ok(())
    }

    #[test]
    fn mismatched_cookie_receipt_defers_close_without_stalling_frames_and_teardown_reaps_child()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()?;
        let helper = root.path().join("bounded-native-fixture");
        std::fs::write(
            &helper,
            r"#!/usr/bin/python3
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
        # The test holds the child here while it checks paint and sync go on.
        while pathlib.Path(profile).parent.joinpath('test-hold-shutdown').exists():
            time.sleep(0.01)
        pathlib.Path(profile).parent.joinpath('test-child-exited').write_text('native teardown finished')
        break
    if command['op']=='service_session_import':
        send({'event':'service_session_result','tab':1,'request_id':command['request_id'],
            'session_id':command['session_id'],'service':command['service'],'imported':True,'cookie_count':2})
",
        )?;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))?;
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
        let profile = crate::service_profiles::Profile::prepare_in(root.path(), &session)?;
        let path = profile.path()?.to_owned();
        let engine = ui::webview::chromium::ChromiumEngine::with_service_session(
            &path,
            &session.session_id,
        )?;
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
            engine: EngineAdapter(engine.clone()),
            opening: ui::LocalState::new(Some("service:lifecycle-selected-session".into())),
            status: "waiting",
            deadline: None,
            shutdown_done: None,
            cleanup: None,
            waker: None,
        };
        require!(view.poll_open());
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
        // The child is held in its shutdown: the receiver answers while it
        // is still alive, so it never blocks paint on native shutdown.
        let hold = path
            .parent()
            .ok_or("profile has no parent")?
            .join("test-hold-shutdown");
        std::fs::write(&hold, b"hold")?;
        require!(view.apply(&reply.ok_or("missing native reply")?));
        require_eq!(
            view.report().get("status").ok_or("missing status")?,
            "needs-reconnect",
            "mismatched count fabricated successful import"
        );
        require!(
            !view
                .report()
                .get("authenticated")
                .ok_or("missing authenticated")?
                .as_bool()
                .ok_or("expected boolean")?
        );
        let mut window = CloseFixture(crate::BrowserWindow::new(
            crate::BrowserWidget::with_engine(engine, true).with_service_view(view),
            false,
        ));
        require_eq!(
            window.0.on_window_close_requested(),
            WindowCloseAdmission::Defer
        );
        require!(
            !window.0.take_window_close_intent(),
            "close admitted before native child exited"
        );
        let doc = ui::automerge::AutoCommit::new();
        // Still held: a sync that waited for the child would never return.
        window.0.sync(&doc, &ui::automerge::ROOT);
        std::fs::remove_file(&hold)?;
        let reports = window.0.close_components();
        require!(
            reports.iter().all(|report| report.teardown_error.is_none()),
            "actual teardown did not confirm child exit"
        );
        require!(
            !path.exists(),
            "incomplete profile retained after confirmed shutdown"
        );
        require!(
            !path
                .parent()
                .ok_or("profile has no parent")?
                .join("completed.json")
                .exists(),
            "mismatched native receipt was persisted"
        );
        require_eq!(
            std::fs::read(
                path.parent()
                    .ok_or("profile has no parent")?
                    .join("test-child-exited")
            )?,
            b"native teardown finished"
        );
        window.0.sync(&doc, &ui::automerge::ROOT);
        require!(
            window.0.take_window_close_intent(),
            "confirmed shutdown never released deferred close"
        );
        require!(
            !window.0.take_window_close_intent(),
            "close intent repeated"
        );
        Ok(())
    }
}
