//! Private, one-shot account requests on the component's original admitted route.
//! These replies never enter the document, history or durable event queues.
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
    /// Capture only while the native host is constructing this admitted component.
    /// A later component cannot replace the route held by an existing client.
    pub fn capture() -> Option<Self> {
        service_capture_bridge().map(|bridge| Self {
            bridge,
            busy: Default::default(),
        })
    }
    /// Submit without waiting on the frame thread.
    /// # Errors
    /// Refuses an unavailable or unauthorized original route.
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
