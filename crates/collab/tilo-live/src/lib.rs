//! Native `MoQ` adapters for Tilo calls: the tilo-voice/1 and tilo-video/1
//! object profiles and, behind `relay`, `moq-lite` relays and clients over
//! iroh (see [`carrier`] for the admission preface every carrier shares).

#![cfg_attr(
    all(target_arch = "wasm32", target_os = "unknown"),
    allow(
        clippy::future_not_send,
        clippy::arc_with_non_send_sync,
        reason = "a page's MoQ handles to JavaScript stay on the page's one thread"
    )
)]

pub use collab_types::scope;

/// A stream's frames: the track they are sealed under, the sealing and
/// opening, and the scope a frame belongs to.
pub mod frame;

pub mod preface;

/// The devices whose frames a scope opens.
pub mod roster;
pub mod video;
pub mod voice;

#[cfg(feature = "relay")]
pub mod carrier;

/// What a native device presents to be admitted to a scope: the page builds
/// one over iroh too, so it is here wherever a device can dial.
#[cfg(any(feature = "relay", feature = "iroh"))]
pub mod grant;

/// The carrier over a direct iroh connection: the one every device dials,
/// a page included.
#[cfg(feature = "iroh")]
pub mod native;

/// Another relay of one scope, carried into this one: members split across
/// two relays of a workspace still hear each other.
#[cfg(all(feature = "relay", feature = "plane"))]
pub mod bridge;

/// The client half of the carrier (present a grant, run `moq-lite`), which
/// a page runs too: it needs no runtime of its own.
#[cfg(feature = "plane")]
pub mod admit;

#[cfg(feature = "plane")]
pub mod live;

#[cfg(feature = "relay")]
mod mirror;

#[cfg(feature = "relay")]
pub mod relay;

#[cfg(feature = "relay")]
mod authority;

/// One participant's sealed session in one scope, sealed under the scope's
/// channel keys.
#[cfg(feature = "plane")]
pub mod plane;

/// The presence track: what a participant says, and the board a reader keeps
/// of everyone's last word.
#[cfg(feature = "plane")]
pub mod presence;

/// Where a plane's readers and origin drivers run: the device's own executor
/// on a desktop, the page's queue in a browser.
/// Work a plane or a relay started, aborted when the state that owns it is
/// dropped, so no reader or pump outlives what it reads.
///
/// The executor is the target's own: a device's ambient runtime, or the
/// browser's queue in a page. Only the bound on what may be spawned differs —
/// a device's work may move between threads, a page's may not — so the one
/// call is spelled twice, and nothing above this asks which it runs on.
#[cfg(feature = "plane")]
pub(crate) struct Task(Option<n0_future::task::JoinHandle<()>>);

#[cfg(all(feature = "plane", not(target_arch = "wasm32")))]
impl Task {
    pub(crate) fn spawn(future: impl std::future::Future<Output = ()> + Send + 'static) -> Self {
        Self(Some(n0_future::task::spawn(future)))
    }
}

#[cfg(all(feature = "plane", target_arch = "wasm32"))]
impl Task {
    pub(crate) fn spawn(future: impl std::future::Future<Output = ()> + 'static) -> Self {
        Self(Some(n0_future::task::spawn(future)))
    }
}

#[cfg(feature = "relay")]
impl Task {
    pub(crate) fn is_finished(&self) -> bool {
        self.0
            .as_ref()
            .is_none_or(n0_future::task::JoinHandle::is_finished)
    }
    /// Finish owned work without detaching it if this wait is cancelled.
    pub(crate) async fn wait(&mut self) -> Result<(), n0_future::task::JoinError> {
        let result = if let Some(task) = self.0.as_mut() {
            task.await
        } else {
            Ok(())
        };
        self.0.take();
        result
    }
}

#[cfg(feature = "plane")]
impl Drop for Task {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}
