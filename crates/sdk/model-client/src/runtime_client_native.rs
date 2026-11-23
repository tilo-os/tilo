//! Poll local UI futures with the app I/O runtime, not an assumed Tokio UI loop.
use crate::transport::{FdTransport, ModelClient};
use agent_provider_config::protocol::{
    CompletionFuture, ErrorKind, ModelBackend, ModelError, ModelRequest, StreamSink,
};

#[path = "runtime_admin_native.rs"]
mod administration;
pub(super) use administration::from_transport as admin_from_transport;

pub(super) fn default_from_transport(
    transport: FdTransport,
) -> Result<std::rc::Rc<dyn ModelBackend>, ModelError> {
    Ok(std::rc::Rc::new(NativeModels::new(
        ModelClient::default_model(transport, std::time::Duration::from_secs(120)),
    )?))
}

/// One inherited stream carries both model and administration capabilities.
/// Clones share serialization and cancellation identities, not independent
/// readers of duplicated descriptors.
pub(super) fn launcher_transport() -> Result<FdTransport, ModelError> {
    static TRANSPORT: std::sync::Mutex<Option<FdTransport>> = std::sync::Mutex::new(None);
    let mut slot = TRANSPORT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(transport) = slot.as_ref() {
        return Ok(transport.clone());
    }
    let transport =
        FdTransport::from_launcher().map_err(|_| ModelError::new(ErrorKind::Unavailable))?;
    *slot = Some(transport.clone());
    drop(slot);
    Ok(transport)
}

pub(super) struct NativeModels {
    client: ModelClient<FdTransport>,
    runtime: tokio::runtime::Handle,
}

impl NativeModels {
    pub(super) fn new(client: ModelClient<FdTransport>) -> Result<Self, ModelError> {
        let runtime = tilo_rt::try_app().map_err(|_| ModelError {
            kind: ErrorKind::Unavailable,
            message: "model runtime unavailable".into(),
        })?;
        Ok(Self {
            client,
            runtime: runtime.handle().clone(),
        })
    }

    fn on_runtime<'a>(&'a self, mut future: CompletionFuture<'a>) -> CompletionFuture<'a> {
        Box::pin(std::future::poll_fn(move |context| {
            // Winit polls on the UI thread; the app runtime drives I/O and
            // timers. Enter only while polling, never across a suspension.
            let _entered = self.runtime.enter();
            future.as_mut().poll(context)
        }))
    }
}

impl ModelBackend for NativeModels {
    fn complete<'a>(&'a self, request: &'a ModelRequest) -> CompletionFuture<'a> {
        self.on_runtime(self.client.complete(request))
    }

    fn stream<'a>(
        &'a self,
        request: &'a ModelRequest,
        sink: StreamSink<'a>,
    ) -> CompletionFuture<'a> {
        self.on_runtime(self.client.stream(request, sink))
    }
}

pub(super) fn from_transport(
    transport: FdTransport,
    profile_id: String,
    model_id: String,
) -> Result<std::rc::Rc<dyn ModelBackend>, ModelError> {
    let client = ModelClient::new(
        transport,
        profile_id,
        model_id,
        std::time::Duration::from_secs(120),
    );
    Ok(std::rc::Rc::new(NativeModels::new(client)?))
}
