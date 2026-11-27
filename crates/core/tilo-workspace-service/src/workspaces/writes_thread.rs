//! A thread of the service's own that carries its writes.
//!
//! A write holds its workspace's lock while it waits (for the key service, for a link, for the
//! disk), and the thread that asked for it is often one that something else waits on: a broker's
//! loop answering a shell, or a runtime whose thread is sending to an app's process. Carrying the
//! write on a thread nothing else uses lets it finish whatever those threads wait for.

/// A thread of its own that carries writes, ended when it is dropped.
pub(crate) struct WritesThread {
    runtime: tokio::runtime::Handle,
    /// Dropped with its holder, which ends the thread.
    _stop: tokio::sync::oneshot::Sender<()>,
}

impl WritesThread {
    /// Start a thread driving a runtime of its own: what every write it carries runs on, and
    /// nothing else does.
    pub(crate) fn start() -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let handle = runtime.handle().clone();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        std::thread::Builder::new()
            .name("tilo-ws-writes".into())
            .spawn(move || {
                runtime.block_on(async {
                    let _ = stopped.await;
                });
            })?;
        Ok(Self {
            runtime: handle,
            _stop: stop,
        })
    }

    /// The runtime the thread drives: what a write is spawned on.
    pub(crate) fn runtime(&self) -> tokio::runtime::Handle {
        self.runtime.clone()
    }
}
