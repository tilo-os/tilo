//! Poll local UI futures with the app I/O runtime, not an assumed Tokio UI loop.
use super::{CompletionFuture, ErrorKind, ModelBackend, ModelError, ModelRequest, StreamSink};
use tilo_model_gateway::transport::{FdTransport, ModelClient};

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
