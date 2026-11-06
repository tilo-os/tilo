//! YouTube as an outside origin of tilolang packages: the logic that keeps a package's types of
//! the `youtube` kinds (`type Video (origin: youtube.video)`) in step with YouTube, from the
//! searches, channels and trending lists its outside effects ask for.
//!
//! The app holds nothing of it: it declares its types and hands its effects to the
//! capabilities (`outside("youtube.search", query)`), and this origin, in native code the
//! instance runs beside it, holds the network. [`Service`] is what the origin asks, and
//! [`Scripted`] answers it from a script for a test or a development session.

// "YouTube" is the service's own name, as its product writes it, not code.
#![allow(
    clippy::doc_markdown,
    reason = "YouTube is a name, not code: it reads as YouTube in prose, not as `YouTube`"
)]

pub mod origin;
pub mod service;

#[cfg(feature = "live")]
pub mod live;

pub use origin::YouTubeOrigin;
pub use service::{
    ChannelItem, ChannelPage, PlaylistItem, Scripted, SearchPage, Service, ServiceError,
    TrendingPage, VideoItem, scripted,
};

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
