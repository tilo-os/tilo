//! Feedback's custody and delivery, the code a page fetches once it is
//! configured with a destination (`deferred-feedback`; linked otherwise).
//!
//! Everything that sends reports, asks the destination's owner for
//! admission and keeps the approved outbox (the delivery connection, the
//! startup handoff's installation and the private custody it installs) is
//! reached only through [`FeedbackCode`], declared with `ui::lazy_code!`, so
//! the browser desktop keeps it out of its start module. Until it is in a
//! configured destination waits here and reads as awaiting admission, which
//! is what it is about to do; the startups the delivery registers are kept
//! here too, as they hand over between the page and its desktop.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use tilo_actions::remote::feedback::FeedbackDestination;
use tilo_app::feedback::{DurableStore, Error, PickedReport, Subject};
use tilo_app::feedback_composer::FeedbackServices;
use tilo_browser_peer::PeerHandle;
use tilo_desktop::feedback::transport::Startup;
use tilo_user_identity::DeviceCert;
use ui::models::Models;

/// A profile's private feedback custody, as the desktop holds it.
pub(crate) trait Custody {
    /// When storage or transport next wants servicing.
    fn next_wake_deadline(&self) -> Option<ui::time::Instant>;
    /// Service storage and transport.
    fn pump(&self) -> Result<(), Error>;
    /// Name the window the next report is about.
    fn about(&self, subject: Option<Subject>) -> Result<(), Error>;
    /// The services the composer's window borrows.
    fn services(&self) -> FeedbackServices;
    /// The model that looks reports over before they go.
    fn set_models(&self, models: Models, name: String);
    /// Start a report from what the person picked on the page.
    fn begin_report(&self, picked: PickedReport) -> Result<(), Error>;
}

impl Custody for tilo_desktop::feedback::PrivateFeedback {
    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        Self::next_wake_deadline(self)
    }
    fn pump(&self) -> Result<(), Error> {
        Self::pump(self)
    }
    fn about(&self, subject: Option<Subject>) -> Result<(), Error> {
        Self::about(self, subject)
    }
    fn services(&self) -> FeedbackServices {
        Self::services(self)
    }
    fn set_models(&self, models: Models, name: String) {
        Self::services(self).set_models(models, name);
    }
    fn begin_report(&self, picked: PickedReport) -> Result<(), Error> {
        Self::services(self).begin_report(picked)
    }
}

type Configure =
    fn(u32, PeerHandle, DeviceCert, DeviceCert, FeedbackDestination) -> Result<(), String>;
type Install = fn(Startup, Rc<dyn DurableStore>) -> Result<Box<dyn Custody>, String>;

/// Feedback's code: what the module a page fetches hands over.
#[derive(Clone, Copy)]
pub struct FeedbackCode {
    configure: Configure,
    status: fn(u32) -> String,
    pump: fn(u32),
    readmit: fn(u32) -> bool,
    forget: fn(u32),
    owner_model: fn(u32) -> Option<(Models, String)>,
    install: Install,
}

/// The feedback code this build links.
#[must_use]
pub fn builtin_feedback() -> FeedbackCode {
    FeedbackCode {
        configure: crate::feedback_delivery::configure,
        status: crate::feedback_delivery::status,
        pump: crate::feedback_delivery::pump,
        readmit: crate::feedback_delivery::readmit,
        forget: crate::feedback_delivery::forget,
        owner_model: crate::feedback_review::owner_model,
        install: |startup, store| {
            let mut slot = None;
            startup
                .install(store, &mut slot)
                .map_err(|error| error.to_string())?;
            slot.map(|custody| Box::new(custody) as Box<dyn Custody>)
                .ok_or_else(|| "feedback custody was not installed".into())
        },
    }
}

ui::lazy_code! {
    /// Feedback's custody and delivery.
    pub static FEEDBACK: FeedbackCode = builtin_feedback, feature = "deferred-feedback";
}

/// A destination configured before the code was in.
struct Pending {
    peer: PeerHandle,
    reporter: DeviceCert,
    owner: DeviceCert,
    destination: FeedbackDestination,
}

thread_local! {
    /// Peers whose page configured a destination: only they need the code.
    static CONFIGURED: RefCell<std::collections::HashSet<u32>> =
        RefCell::new(std::collections::HashSet::new());
    static PENDING: RefCell<HashMap<u32, Pending>> = RefCell::new(HashMap::new());
    static STARTUPS: RefCell<HashMap<u32, Startup>> = RefCell::new(HashMap::new());
    static WAKES: RefCell<HashMap<u32, Arc<dyn Fn() + Send + Sync>>> =
        RefCell::new(HashMap::new());
}

/// Select peer `id`'s destination, now or once the code is in.
pub(crate) fn configure(
    id: u32,
    peer: PeerHandle,
    reporter: DeviceCert,
    owner: DeviceCert,
    destination: FeedbackDestination,
) -> Result<(), String> {
    CONFIGURED.with_borrow_mut(|configured| configured.insert(id));
    if let Some(code) = FEEDBACK.get() {
        return (code.configure)(id, peer, reporter, owner, destination);
    }
    PENDING.with_borrow_mut(|pending| {
        pending.insert(
            id,
            Pending {
                peer,
                reporter,
                owner,
                destination,
            },
        )
    });
    Ok(())
}

/// The code is in: the destinations configured meanwhile are selected.
pub fn code_arrived() {
    let Some(code) = FEEDBACK.get() else {
        return;
    };
    let pending = PENDING.with_borrow_mut(std::mem::take);
    for (id, pending) in pending {
        // The page waits on its peer's snapshot, which the owner's answer
        // to the admission this asks for moves.
        if let Err(error) = (code.configure)(
            id,
            pending.peer,
            pending.reporter,
            pending.owner,
            pending.destination,
        ) {
            web_sys::console::warn_1(&format!("feedback: {error}").into());
        }
    }
}

/// Whether peer `id`'s page configured a destination.
fn configured(id: u32) -> bool {
    CONFIGURED.with_borrow(|configured| configured.contains(&id))
}

/// Where peer `id`'s feedback stands.
pub(crate) fn status(id: u32) -> String {
    if !configured(id) {
        return "unconfigured".into();
    }
    match FEEDBACK.get() {
        Some(code) => (code.status)(id),
        None if PENDING.with_borrow(|pending| pending.contains_key(&id)) => {
            "awaiting authenticated admission".into()
        }
        None => "unconfigured".into(),
    }
}

/// Service peer `id`'s delivery after its session moved.
pub(crate) fn pump(id: u32) {
    if !configured(id) {
        return;
    }
    let Some(code) = FEEDBACK.get() else {
        return;
    };
    code_arrived();
    (code.pump)(id);
}

/// Ask the owner for a fresh admission; whether it was asked.
pub(crate) fn readmit(peer: u32) -> bool {
    configured(peer) && FEEDBACK.get().is_some_and(|code| (code.readmit)(peer))
}

/// Peer `id` went away.
pub(crate) fn forget(id: u32) {
    PENDING.with_borrow_mut(|pending| pending.remove(&id));
    if CONFIGURED.with_borrow_mut(|configured| configured.remove(&id))
        && let Some(code) = FEEDBACK.get()
    {
        (code.forget)(id);
    }
}

/// The model the owner of peer `peer`'s destination lends its reporters.
pub(crate) fn owner_model(peer: u32) -> Option<(Models, String)> {
    if !configured(peer) {
        return None;
    }
    FEEDBACK.get().and_then(|code| (code.owner_model)(peer))
}

/// Install `startup` over `store` as the profile's private custody.
pub(crate) fn install(
    startup: Startup,
    store: Rc<dyn DurableStore>,
) -> Result<Box<dyn Custody>, String> {
    let code = FEEDBACK
        .get()
        .ok_or_else(|| "feedback is still loading".to_owned())?;
    (code.install)(startup, store)
}

/// Trusted Rust startup handoff, separate from deserialized mount configuration.
/// The certified peer must already exist. This never creates a peer or joins.
pub(crate) fn register_feedback_startup(peer: u32, startup: Startup) -> Result<(), String> {
    let device = startup
        .reporter()
        .device_public_key()
        .ok_or("feedback reporter has no certified device")?;
    if crate::replication::device_of(peer).as_deref() != Some(hex::encode(device).as_str()) {
        return Err("feedback reporter does not own this certified peer".into());
    }
    STARTUPS.with_borrow_mut(|startups| -> Result<(), String> {
        if startups.contains_key(&peer) {
            return Err("feedback startup already registered".into());
        }
        startups.insert(peer, startup);
        Ok(())
    })?;
    // A desktop that mounted first takes it up on its next frame.
    if let Some(wake) = WAKES.with_borrow_mut(|wakes| wakes.remove(&peer)) {
        wake();
    }
    Ok(())
}

/// Call `wake` once a startup is registered for `peer`.
pub(crate) fn wake_on_startup(peer: u32, wake: Arc<dyn Fn() + Send + Sync>) {
    WAKES.with_borrow_mut(|wakes| wakes.insert(peer, wake));
}

pub(crate) fn forget_feedback_startup(peer: u32) {
    STARTUPS.with_borrow_mut(|startups| {
        startups.remove(&peer);
    });
    WAKES.with_borrow_mut(|wakes| {
        wakes.remove(&peer);
    });
}

/// The startup registered for `peer`, for the desktop that mounts with it.
pub(crate) fn take_startup(peer: u32) -> Option<Startup> {
    STARTUPS.with_borrow_mut(|startups| startups.remove(&peer))
}
