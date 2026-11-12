//! A write of this device into its workspaces' engine, awaited on the session's loop: polled as it
//! is made, when the desktop's own key signs at once and it is done, and again whenever its waker
//! wakes the loop, so a signer that answers later, or another write of this device still under way,
//! never holds the loop.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use tilo_desktop_model::WorkspaceId;

use crate::engine::Engine;

/// What a write came to: the write as the workspace took it, or why it took none.
pub(crate) type Written = Result<tilo_peer_state::owner::AppWrite, String>;

/// One write under way ([`Engine::write_record`]).
pub(crate) struct Writing {
    write: Pin<Box<dyn Future<Output = Written>>>,
    waker: Waker,
}

impl Writing {
    /// Write `changes` into `record` of `app` in `room`, founding it when `founding`, sealed under
    /// `sealed` where the workspace's records are sealed; `wake` wakes the loop when it moves on.
    pub(crate) fn start(
        engine: Arc<Engine>,
        wake: tilo_service::Wake,
        (room, app, record): (WorkspaceId, String, String),
        changes: Vec<u8>,
        founding: bool,
        sealed: Option<channel_crypto::ChannelContentKeys>,
    ) -> Self {
        Self {
            write: Box::pin(async move {
                engine
                    .write_record(room, (&app, &record), &changes, founding, sealed.as_ref())
                    .await
            }),
            waker: Waker::from(Arc::new(Wakes(wake))),
        }
    }

    /// What the write came to, once it is done; `None` while it is under way. A write that
    /// answered is not asked again.
    pub(crate) fn done(&mut self) -> Option<Written> {
        match self
            .write
            .as_mut()
            .poll(&mut Context::from_waker(&self.waker))
        {
            Poll::Ready(written) => Some(written),
            Poll::Pending => None,
        }
    }
}

/// The loop's wake, as a write's waker.
struct Wakes(tilo_service::Wake);

impl std::task::Wake for Wakes {
    fn wake(self: Arc<Self>) {
        (self.0)();
    }
}
