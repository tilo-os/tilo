//! YouTube as an outside origin of tilolang packages: the logic that keeps a package's types of
//! the `youtube` kinds (`type Video (origin: youtube.video)`) in step with YouTube, from the
//! searches, channels and trending lists its outside effects ask for.
//!
//! The app holds nothing of it: it declares its types and hands its effects to the
//! capabilities (`outside("youtube.search", query)`), and this origin, in native code the
//! instance runs beside it, holds the network. [`Service`] is what the origin asks, and
//! [`Scripted`] answers it from a script for a test or a development session.
//!
//! Where an instance runs in a tilolang host, which holds no network, the service is the
//! YouTube connector the session runs for the app ([`connector`]): the instance reaches it over
//! a socket ([`remote::RemoteService`]), and the connector asks YouTube through the
//! platform's InnerTube client ([`Live`]).

// "YouTube" is the service's own name, as its product writes it, not code.
#![allow(
    clippy::doc_markdown,
    reason = "YouTube is a name, not code: it reads as YouTube in prose, not as `YouTube`"
)]

#[cfg(feature = "connector")]
pub mod connector;
pub mod origin;
pub mod remote;
pub mod service;
pub mod wire;

#[cfg(feature = "live")]
pub mod live;

pub use origin::YouTubeOrigin;
pub use remote::RemoteService;
pub use service::{
    ChannelItem, ChannelPage, Live, PlaylistItem, Scripted, SearchPage, Service, ServiceError,
    TrendingPage, VideoItem, scripted,
};

/// The origin kept in step with YouTube over the connector at the far end of `stream`, waking
/// the instance with `wake` when an answer is ready.
pub fn remote_origin(
    stream: std::os::unix::net::UnixStream,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
) -> Result<std::rc::Rc<std::cell::RefCell<YouTubeOrigin>>, String> {
    let woken = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let origin = origin(
        std::sync::Arc::new(RemoteService::new(stream)),
        std::sync::Arc::clone(&woken),
    );
    Ok(waking(origin, wake))
}

/// Register the YouTube origin's client of the YouTube connector with the platform, so a crate
/// that keeps a package's records reaches the connector without naming this one
/// (`tilolang_tilo::origin::remote`).
pub fn register() {
    tilolang_tilo::origin::register_remote("youtube", CONNECTOR_CLIENT);
}

/// The client [`register`] hands the platform: a `const` item, so its own type never has to be
/// named where it is registered.
const CONNECTOR_CLIENT: &Remote = &|stream, wake| {
    remote_origin(stream, wake).map(|origin| origin as tilolang_tilo::origin::SharedOrigin)
};

/// The client [`register`] hands the platform.
type Remote = dyn Fn(
        std::os::unix::net::UnixStream,
        std::sync::Arc<dyn Fn() + Send + Sync>,
    ) -> Result<tilolang_tilo::origin::SharedOrigin, String>
    + Send
    + Sync;

/// The origin over `service`, which sets `woken` when it has an answer.
#[must_use]
pub fn origin(
    service: std::sync::Arc<dyn Service>,
    woken: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> std::rc::Rc<std::cell::RefCell<YouTubeOrigin>> {
    std::rc::Rc::new(std::cell::RefCell::new(YouTubeOrigin::new(service, woken)))
}

/// An origin whose answers wake `wake`, the instance's frame loop: the flag it polls on is
/// shared with the answers' arrival, so one `poll` sees each answer and says it wrote.
#[must_use]
pub fn waking(
    origin: std::rc::Rc<std::cell::RefCell<YouTubeOrigin>>,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
) -> std::rc::Rc<std::cell::RefCell<YouTubeOrigin>> {
    YouTubeOrigin::waking(origin, wake)
}
