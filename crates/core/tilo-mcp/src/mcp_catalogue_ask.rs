//! A catalogue page asked of the action provider, as a bridge's caller: off the runtime's
//! thread when the broker assembles it there ([`off_the_runtime`]), waited for as the tree
//! waits for every page it reads.

use std::sync::Arc;

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, Pending,
};

use crate::{BridgeError, McpServer, timing_done_saying, timing_start};

/// The tree's pauses, on tokio's clock: a test that pauses time pauses the walk's too.
pub(crate) struct TokioPause {
    origin: tokio::time::Instant,
}

impl TokioPause {
    pub(crate) fn new() -> Self {
        Self {
            origin: tokio::time::Instant::now(),
        }
    }
}

impl tilo_actions::tree::Pause for TokioPause {
    fn pause(
        &self,
        how_long: std::time::Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep(how_long))
    }

    fn now(&self) -> std::time::Duration {
        self.origin.elapsed()
    }
}

/// `query` asked of `host` as `caller` the way a bridge asks it ([`asked_off_the_runtime`]),
/// every page the bridge's tree reads: the timing line names what was asked, so a slow listing
/// says which request it waited for, and what a listing asks can be read from its log.
pub(crate) fn off_the_runtime<H: ActionProvider + Send + Sync + 'static>(
    host: &Arc<H>,
    caller: &Caller,
    query: CatalogueQuery,
) -> Pending<Result<CataloguePage, ActionCallError>> {
    let Some(started) = timing_start() else {
        return asked_off_the_runtime(host, caller, query);
    };
    let what = format!(
        "provider={} parent={} rows={}",
        query.provider.as_deref().unwrap_or("-"),
        query.parent_resource.as_deref().unwrap_or("-"),
        query.max_results
    );
    asked_off_the_runtime(host, caller, query).map(move |answer| {
        timing_done_saying("ask.catalogue", started, &what);
        answer
    })
}

/// `query` asked of `host` as `caller`. The whole catalogue's rows
/// and the workspace's own are assembled synchronously by the broker, so they are asked on a
/// blocking thread: every session of an endpoint shares one current-thread runtime, which
/// that work would stall for all of them. Who publishes is cheap, and an app's directory
/// answers later on the broker's own set: asking either costs the runtime nothing.
fn asked_off_the_runtime<H: ActionProvider + Send + Sync + 'static>(
    host: &Arc<H>,
    caller: &Caller,
    query: CatalogueQuery,
) -> Pending<Result<CataloguePage, ActionCallError>> {
    let eager = match query.provider.as_deref() {
        None => query.max_results > 0,
        Some(provider) => tilo_actions::remote::provider_workspace(provider).is_some(),
    };
    let Some(runtime) = tokio::runtime::Handle::try_current().ok().filter(|_| eager) else {
        return host.query(caller, query);
    };
    let host = Arc::clone(host);
    let caller = caller.clone();
    let (completer, answer) = tilo_actions::remote::pending();
    runtime.spawn(async move {
        let asked = tokio::task::spawn_blocking(move || host.query(&caller, query)).await;
        if let Ok(asked) = asked
            && let Some(answered) = asked.await
        {
            completer.complete(answered);
        }
    });
    answer
}

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// One catalogue page from the action provider, as this bridge's caller, timed as every
    /// page its tree reads is ([`off_the_runtime`]).
    pub(crate) async fn ask(&self, query: CatalogueQuery) -> Result<CataloguePage, BridgeError> {
        self.tree.ask(query).await.map_err(BridgeError::from)
    }

    /// The answer `ask` gives the host as this bridge's caller. With
    /// `blocking`, the host is asked on a blocking thread: every session of an
    /// endpoint shares one current-thread runtime, which synchronous broker
    /// work (a catalogue assembled, an admission decided) would stall for
    /// all of them.
    pub(crate) async fn at_host<T: Send + 'static>(
        &self,
        blocking: bool,
        ask: impl FnOnce(&H, &Caller) -> tilo_actions::remote::Pending<T> + Send + 'static,
    ) -> Option<T> {
        let caller = self.caller();
        let answer = if blocking {
            let host = std::sync::Arc::clone(&self.host);
            tokio::task::spawn_blocking(move || ask(&host, &caller))
                .await
                .ok()?
        } else {
            ask(self.host(), &caller)
        };
        answer.await
    }

    /// Wait for the workspace that answers this session to be ready for a call it refused as
    /// not ready yet, the way a peer's request waits: what the session says of itself is the
    /// signal, never a timer, and `until` bounds the wait. `false` once `until` passed, when
    /// the provider cannot say when it moves, or when it moves no more within it — the refusal
    /// is then the answer.
    pub(crate) async fn wait_served(&self, until: tokio::time::Instant) -> bool {
        let caller = self.caller();
        let Some(moved) = self.host().session_state_changed(&caller) else {
            return false;
        };
        tokio::time::timeout_at(until, moved).await.is_ok()
    }
}
