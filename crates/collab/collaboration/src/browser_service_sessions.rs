//! Private, one-shot requests to the session's services, on the app's own
//! route. These replies never enter the document, history or durable event
//! queues.
//!
//! A window takes its client as it is built ([`Client::capture`]) and uses it
//! whenever it needs the session: while its route is still joining, once
//! the session admitted it, or in a room whose record this device's own
//! channel serves. What the session answers is the session's decision on
//! that route, taken per request; the client only carries the request there.
use super::native_initial::{InitialReadBridge, service_capture_bridge};
pub use runtime_collab::{
    BrowserServiceSessionCommand, BrowserServiceSessionDescriptor, BrowserServiceSessionReply,
};
use std::sync::mpsc::{Receiver, TryRecvError};

#[derive(Clone)]
pub struct Client {
    bridge: InitialReadBridge,
    busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl Client {
    /// Capture this window's connection to the session while the native host
    /// constructs its component: `None` outside a session (a test, a
    /// `--data-dir` profile), which keeps its own stores.
    ///
    /// The captured connection names the app, not a moment: it serves the
    /// window's requests before its route joined, after, and across a record
    /// switch on the same app's route.
    pub fn capture() -> Option<Self> {
        service_capture_bridge().map(|bridge| Self {
            bridge,
            busy: Default::default(),
        })
    }
    /// Submit without waiting on the frame thread.
    /// # Errors
    /// Refuses a request another is already in flight, or a connection that
    /// ended.
    pub fn request(
        &self,
        command: BrowserServiceSessionCommand,
        wake: Option<crate::WakeHandle>,
    ) -> Result<Pending, String> {
        if !command.valid()
            || wake.as_ref().is_some_and(|wake| !wake.is_active())
            || self
                .busy
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                )
                .is_err()
        {
            return Err("This account could not be opened".into());
        }
        let received = match self.bridge.service_request(command, wake) {
            Ok(received) => received,
            Err(error) => {
                self.busy.store(false, std::sync::atomic::Ordering::Release);
                return Err(error);
            }
        };
        Ok(Pending {
            received,
            busy: self.busy.clone(),
            deadline: crate::time::now() + crate::time::Duration::from_secs(10),
            done: false,
        })
    }
}
pub struct Pending {
    busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    received: Receiver<Result<BrowserServiceSessionReply, String>>,
    deadline: crate::time::Instant,
    done: bool,
}
impl Pending {
    /// A reply is returned at most once; dropping this object drops private custody.
    pub fn poll(&mut self) -> Option<Result<BrowserServiceSessionReply, String>> {
        if self.done {
            return None;
        }
        let result = match self.received.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) if crate::time::now() < self.deadline => return None,
            Err(_) => Err("This account could not be opened".into()),
        };
        self.done = true;
        self.busy.store(false, std::sync::atomic::Ordering::Release);
        Some(result)
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if !self.done {
            self.busy.store(false, std::sync::atomic::Ordering::Release);
        }
    }
}
