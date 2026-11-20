//! A catalogue page asked of the action provider, as a bridge's caller: how long an ask
//! waits for the workspace that answers it, and what a caller hears when it went unanswered.

use tilo_actions::remote::{ActionProvider, Caller, CataloguePage, CatalogueQuery};

use crate::{BridgeError, McpServer, timing_done_saying, timing_start};

/// How long a call waits for a workspace that is not serving its session yet, when what it
/// asked was refused as not ready or went unanswered: a server that restarted serves one again
/// within a minute, and a call that gave up sooner sent its caller after evidence it never had.
pub(crate) const NOT_SERVED_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
/// How a query the host did not answer in time is worded: its code, which a caller reads as
/// one to ask again ([`mcp_answer`] keeps it), and [`McpServer::ask_untimed`] as one that
/// waits for the workspace rather than an answer it will not get.
const UNANSWERED_PREFIX: &str = "unanswered: ";

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// One catalogue page from the action provider, as this bridge's caller: the timing line
    /// names what was asked, so a slow listing says which request it waited for.
    pub(crate) async fn ask(&self, query: CatalogueQuery) -> Result<CataloguePage, BridgeError> {
        let timed = timing_start();
        let what = timed.map(|_| {
            format!(
                "provider={} parent={} rows={}",
                query.provider.as_deref().unwrap_or("-"),
                query.parent_resource.as_deref().unwrap_or("-"),
                query.max_results
            )
        });
        let asked = self.ask_untimed(query).await;
        if let (Some(started), Some(what)) = (timed, what.as_deref()) {
            timing_done_saying("ask.catalogue", started, what);
        }
        asked
    }

    pub(crate) async fn ask_untimed(
        &self,
        query: CatalogueQuery,
    ) -> Result<CataloguePage, BridgeError> {
        // The whole catalogue's rows and the workspace's own are assembled
        // synchronously by the broker. Who publishes is cheap, and an app's
        // directory answers later on the broker's own set: asking either
        // costs this runtime nothing.
        let eager = match query.provider.as_deref() {
            None => query.max_results > 0,
            Some(provider) => tilo_actions::remote::provider_workspace(provider).is_some(),
        };
        // A workspace a server has not started serving yet refuses the ask, and takes it once it
        // does (a server that restarted is serving one again within a minute; until it does, its
        // host may not answer at all): the session is the signal that it moved, and a caller waits
        // for it instead of being sent back. An ask the workspace took is waited for until it
        // answers: a workspace that just started opens the documents a listing reads before it
        // answers, which takes it longer than a query is otherwise given, and an ask given up
        // then made the caller wait for an answer it had already been sent.
        let started = tokio::time::Instant::now();
        let until = started
            .checked_add(NOT_SERVED_WAIT)
            .unwrap_or_else(tokio::time::Instant::now);
        loop {
            let asked = query.clone();
            let answer = match tokio::time::timeout_at(
                until,
                self.at_host(eager, move |host, caller| host.query(caller, asked)),
            )
            .await
            {
                // No answer is not an answer that nothing is there: the caller hears it went
                // unanswered, never that the path names nothing.
                Err(_) => {
                    return Err(BridgeError::from(format!(
                        "{UNANSWERED_PREFIX}the workspace did not answer within {} s; it may \
                         still be opening its documents after it started, so ask again",
                        started.elapsed().as_secs()
                    )));
                }
                Ok(None) => Err(BridgeError::from("the desktop's actions stopped answering")),
                Ok(Some(Ok(page))) => return Ok(page),
                Ok(Some(Err(error))) => Err(BridgeError::Host(error)),
            };
            if matches!(
                &answer,
                Err(BridgeError::Host(
                    tilo_actions::remote::ActionCallError::NotReady
                ))
            ) && self.wait_served(until).await
            {
                continue;
            }
            return answer;
        }
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
