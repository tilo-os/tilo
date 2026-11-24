//! Calendar servers as an outside origin of tilolang packages: the `CalDAV` connector over the
//! calendar engine's protocol code ([`connector`]) and the origin that keeps a package's `caldav`
//! types in step with it ([`origin`]).
//!
//! The accounts are the workspace's, and each one's password is the entry of the workspace's
//! vault the account names, released by the session to the connector in native code
//! ([`sign_ins`]): package code never sees it. An instance runs in a tilolang host, which holds
//! no network and no credential; the connector runs in a process of its own that the system
//! starts for the app ([`serve`]), and the instance's origin reaches it over a socket
//! ([`remote`], [`wire`]).

pub mod connector;
#[cfg(feature = "engine")]
pub mod engine;
// The iCalendar format the servers of calendar accounts speak, and the event,
// task and calendar shapes an entry is read into and written from: the origin's
// own reading of an entry, shared with the workspace host.
pub mod ics;
pub mod model;
pub mod origin;
pub mod remote;
#[cfg(feature = "engine")]
pub mod serve;
// When an event's occurrences fall, as instants and as the wall times its own
// zone writes them.
mod shown;
#[cfg(feature = "engine")]
pub mod sign_ins;
// What a calendar holds between two times, and when its person is free: the
// occurrences a reader asks for and the gaps between them, read by the
// workspace's host and by whoever reads a calendar's entries.
pub mod spans;
pub mod timezone;
pub mod wire;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
#[cfg(feature = "engine")]
use std::sync::atomic::Ordering;

use origin::CalDavOrigin;

/// The name the origin's kinds carry: `caldav.account`, `caldav.calendar`, `caldav.event`.
pub const ORIGIN: &str = "caldav";

/// The origin of an instance in a tilolang host, over the calendar connector process at the far
/// end of `stream`, waking the instance with `wake` when it says something.
pub fn remote_origin(
    stream: tilo_socket::Stream,
    wake: tilolang_tilo::background::Waker,
) -> std::io::Result<Rc<RefCell<CalDavOrigin>>> {
    let woken = Arc::new(AtomicBool::new(true));
    let reconnected = Arc::new(AtomicBool::new(false));
    let connector =
        remote::RemoteConnector::new(stream, Arc::clone(&woken), wake, Arc::clone(&reconnected))?;
    Ok(Rc::new(RefCell::new(
        CalDavOrigin::new(Box::new(connector), woken).reconnecting_on(reconnected),
    )))
}

/// Register the calendar origin's client of the calendar connector with the platform, so a crate
/// that keeps a package's records reaches the connector without naming this one
/// (`tilolang_tilo::origin::remote`).
pub fn register() {
    tilolang_tilo::origin::register_remote(ORIGIN, CONNECTOR_CLIENT);
}

/// The client [`register`] hands the platform.
const CONNECTOR_CLIENT: &Remote = &|stream, wake| {
    remote_origin(stream, wake)
        .map(|origin| origin as tilolang_tilo::origin::SharedOrigin)
        .map_err(|error| format!("the calendar connector's connection: {error}"))
};

/// The client [`register`] hands the platform.
type Remote = dyn Fn(
        tilo_socket::Stream,
        tilolang_tilo::background::Waker,
    ) -> Result<tilolang_tilo::origin::SharedOrigin, String>
    + Send
    + Sync;

/// Set `reconnected` and wake whenever the device comes online again: what could not reach its
/// server then is tried again. An event of the platform's network status, never a timer.
#[cfg(feature = "engine")]
pub(crate) fn follow_the_network(reconnected: Arc<AtomicBool>, wake: engine::Wake) {
    let spawned = std::thread::Builder::new()
        .name("calendar-network".into())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread().build() else {
                return;
            };
            runtime.block_on(async move {
                let mut network = tilo_net::status::watch();
                let mut online = network.last().online();
                loop {
                    let now = network.changed().await.online();
                    if now && !online {
                        reconnected.store(true, Ordering::Release);
                        wake();
                    }
                    online = now;
                }
            });
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "calendars do not follow the network: they retry only when asked");
    }
}

/// The variable the launcher names the origin a connector process connects with.
#[cfg(feature = "engine")]
pub const CONNECTOR_ORIGIN_ENV: &str = runtime_portal::origin_connector::ORIGIN_ENV;

/// Run the connector for `origin` this process was started as (`TILO_TILOLANG_CONNECTOR`), on
/// the socket the launcher handed it, for the accounts of the workspace the session tells it,
/// until the session closes the socket.
#[cfg(feature = "engine")]
pub fn run_connector(origin: &str) -> Result<(), String> {
    use runtime_portal::origin_connector::{self, Handed};
    if origin != ORIGIN {
        return Err(format!("there is no connector for the origin `{origin}`"));
    }
    let socket = origin_connector::take_socket()
        .ok_or_else(|| "the launcher handed the connector no socket".to_owned())?;
    // The session hands its account sign-ins before any reader.
    let sign_ins = match origin_connector::receive(&socket) {
        Ok(Some(Handed::SignIns(sign_ins))) => sign_ins,
        Ok(Some(Handed::Reader(_))) => {
            return Err("the session handed the calendar connector no account sign-ins".to_owned());
        }
        Ok(None) => return Ok(()),
        Err(error) => return Err(format!("the calendar connector's socket: {error}")),
    };
    let attachments = origin_connector::attachments(socket, |failed| match failed {
        None => tracing::info!("the session closed the calendar connector's socket"),
        Some(error) => tracing::error!(%error, "the calendar connector's socket failed for good"),
    });
    serve::serve(sign_ins, attachments);
    Ok(())
}
