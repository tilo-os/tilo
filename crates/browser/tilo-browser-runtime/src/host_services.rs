//! The page's `HostServices`: each platform crate's web backend — the
//! clipboard of trusted clipboard events, `<input type=file>` choosers,
//! downloads, `window.open` and the Notifications API. Speech is silent
//! (`tts::NullSpeaker`); media controls, location and screen capture are
//! unavailable.
//!
//! Applications' notifications go to the desktop that shows their window,
//! as banners and in its notification centre
//! ([`tilo_desktop_surface::AppNotifier`]). The Notifications API shows
//! them too while the page is hidden and the person allows it, and on its
//! own for an application mounted without a desktop.
//!
//! Browsers grant pickers, popups and permission prompts only inside a user
//! gesture. Every call comes from the app's handling of an input event, which
//! winit dispatches from the DOM listener, so it runs within that gesture's
//! activation.
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Model access belongs to a mount, never to the page's ambient services.
/// The embedding host supplies a short-lived model-service session token,
/// not a provider credential. The service enforces its audience and grants.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ModelConfig {
    endpoint: String,
    session_token: String,
    profile_id: String,
    model_id: String,
}

impl ModelConfig {
    pub(crate) fn connect(&self) -> Result<ui::models::Models, String> {
        use tilo_model_gateway::transport::{FetchTransport, ModelClient};
        if self.session_token.trim().is_empty()
            || self.profile_id.trim().is_empty()
            || self.model_id.trim().is_empty()
        {
            return Err("model session and selection are required".into());
        }
        let transport = FetchTransport::new(&self.endpoint, self.session_token.clone())?;
        Ok(ui::models::Models::new(std::rc::Rc::new(ModelClient::new(
            transport,
            self.profile_id.clone(),
            self.model_id.clone(),
            std::time::Duration::from_secs(120),
        ))))
    }
}

use tilo_desktop_surface::{AppInbox, AppNotifier};

thread_local! {
    static NOTIFIER: RefCell<Option<Arc<AppNotifier>>> = const { RefCell::new(None) };
    static BACKGROUND: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
}

/// Install the page's services as the process default. `wake` schedules
/// every desktop's next frame.
pub fn install(wake: Arc<dyn Fn() + Send + Sync>) {
    let system = ui::host::HostServices::system(None);
    let notifier = Arc::new(AppNotifier::new(
        Arc::clone(&system.notifications),
        BACKGROUND.with(Arc::clone),
        wake,
    ));
    NOTIFIER.with_borrow_mut(|slot| *slot = Some(Arc::clone(&notifier)));
    ui::host::install_default(ui::host::HostServices {
        notifications: notifier,
        ..system
    });
}

/// A desktop's inbox of applications' notifications.
pub fn notification_inbox() -> Option<AppInbox> {
    NOTIFIER.with_borrow(|notifier| notifier.as_ref().map(|notifier| notifier.inbox()))
}

/// The page went out of view or came back. Hidden, applications'
/// notifications also go to the browser's, when the person allowed them.
pub fn set_hidden(hidden: bool) {
    let forward = hidden && notification_permission().as_deref() == Some("granted");
    BACKGROUND.with(|background| background.store(forward, Ordering::Release));
}

/// `Notification.permission`, where the page has the API.
fn notification_permission() -> Option<String> {
    let api = js_sys::Reflect::get(&js_sys::global(), &"Notification".into()).ok()?;
    if api.is_undefined() {
        return None;
    }
    js_sys::Reflect::get(&api, &"permission".into())
        .ok()?
        .as_string()
}
