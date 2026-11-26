//! The device's services an app's connection reaches with its own frames: its sound output, and
//! its place in a call.
//!
//! An app's connection to the daemon is bound to the app process the launcher attested: the
//! context here is who the connection is, checked again before every command, and the services
//! are this device's, reached in the app's name. No command names its app, workspace or record:
//! the context is the launcher's.

use std::sync::Arc;

use tilo_actions::remote::native_media::{Request as CallRequest, Snapshot as CallSnapshot};

/// Immutable route plus a live connection/process/admission check. There is no
/// public constructor: only this broker's admitted channel can produce one.
#[derive(Clone)]
pub struct MediaRequestContext {
    pub(super) peer_id: u64,
    pub(super) app_id: String,
    pub(super) workspace_id: String,
    pub(super) document_id: String,
    /// The caller the device opened this view for, when it opened one for a caller: a page's
    /// view is opened for its person, and the browser that shows the page carries its call.
    pub(super) opened_for: Option<tilo_actions::remote::Caller>,
    pub(super) revalidate: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
}

impl MediaRequestContext {
    /// The context of a connection this device's broker admitted, as the seat answers a call the
    /// broker forwarded to it (`crate::seat`): only the broker reaches the seat, and it
    /// checks the connection again before every call it forwards.
    #[must_use]
    pub fn seat_forwarded(
        peer_id: u64,
        app_id: String,
        workspace_id: String,
        document_id: String,
    ) -> Self {
        Self {
            peer_id,
            app_id,
            workspace_id,
            document_id,
            opened_for: None,
            revalidate: Arc::new(|| Ok(())),
        }
    }

    /// The context of an app's own connection to this device's daemon: the launcher placed it
    /// for process `peer_id` running `app_id` in `workspace_id`, and `current` says whether it is
    /// still that process. Only the daemon builds one, for a connection it serves as an app
    /// ([`crate::collab_app_broker::AudioOutputConnection`]); it checks the process again
    /// before every command it forwards.
    #[must_use]
    pub fn daemon_line(
        peer_id: u64,
        app_id: String,
        workspace_id: String,
        current: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    ) -> Self {
        Self {
            peer_id,
            app_id,
            workspace_id,
            document_id: String::new(),
            opened_for: None,
            revalidate: current,
        }
    }

    pub fn peer_id(&self) -> u64 {
        self.peer_id
    }
    pub fn app_id(&self) -> &str {
        &self.app_id
    }
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
    pub fn document_id(&self) -> &str {
        &self.document_id
    }
    /// The caller the device opened this view for, when it opened one for a caller.
    pub const fn opened_for(&self) -> Option<&tilo_actions::remote::Caller> {
        self.opened_for.as_ref()
    }
    /// Re-checks that the admitted connection, process and route are still live.
    pub fn revalidate(&self) -> Result<(), String> {
        (self.revalidate)()
    }
}

pub type CallMediaFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<CallSnapshot, String>> + Send>>;
pub type AudioOutputFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<tilo_actions::remote::audio_output::Snapshot, String>,
            > + Send,
    >,
>;

pub trait AudioOutputServices: Send + Sync {
    fn output(
        &self,
        context: MediaRequestContext,
        request: tilo_actions::remote::audio_output::Request,
        samples: Vec<i16>,
    ) -> AudioOutputFuture;
    /// Fence output belonging to this exact connected process.
    fn disconnect(&self, peer_id: u64);
}

/// An app's sound output as its own connection to the device's daemon reaches it: the service
/// that plays it, and the context of the app process the launcher placed that connection for,
/// which is checked again before every command.
pub struct AudioOutputConnection {
    service: Arc<dyn AudioOutputServices>,
    context: MediaRequestContext,
}

impl AudioOutputConnection {
    #[must_use]
    pub fn new(service: Arc<dyn AudioOutputServices>, context: MediaRequestContext) -> Self {
        Self { service, context }
    }

    /// Play one command of the app's output, as the connection it was opened for is still the
    /// app's.
    #[must_use]
    pub fn output(
        &self,
        request: tilo_actions::remote::audio_output::Request,
        samples: Vec<i16>,
    ) -> AudioOutputFuture {
        self.service.output(self.context.clone(), request, samples)
    }

    /// The app's process ended: nothing it opened sounds any more.
    pub fn disconnect(&self) {
        self.service.disconnect(self.context.peer_id());
    }

    /// The process the connection was placed for, as this output's owner.
    #[must_use]
    pub fn peer_id(&self) -> u64 {
        self.context.peer_id()
    }
}

pub trait CallMediaServices: Send + Sync {
    /// One command of an app's call, answered with the call's status or why it was refused.
    fn command(&self, context: MediaRequestContext, request: CallRequest) -> CallMediaFuture;
    /// Fence pending work and stop local capture for this exact connected process.
    fn disconnect(&self, peer_id: u64);
}

/// An app's place in a call as its own connection to the device's daemon reaches it: the call
/// service this device holds, and the context of the app process the launcher placed the
/// connection for, which is checked again before every command.
pub struct CallMediaConnection {
    service: Arc<dyn CallMediaServices>,
    context: MediaRequestContext,
}

impl CallMediaConnection {
    #[must_use]
    pub fn new(service: Arc<dyn CallMediaServices>, context: MediaRequestContext) -> Self {
        Self { service, context }
    }

    /// One command of the app's call, as the connection it was opened for is still the app's.
    #[must_use]
    pub fn command(&self, request: CallRequest) -> CallMediaFuture {
        self.service.command(self.context.clone(), request)
    }

    /// The app's process ended: nothing it joined stays, and nothing it captures runs.
    pub fn disconnect(&self) {
        self.service.disconnect(self.context.peer_id());
    }

    /// The process the connection was placed for, as this call's owner.
    #[must_use]
    pub fn peer_id(&self) -> u64 {
        self.context.peer_id()
    }
}

impl Clone for CallMediaConnection {
    fn clone(&self) -> Self {
        Self {
            service: Arc::clone(&self.service),
            context: self.context.clone(),
        }
    }
}
