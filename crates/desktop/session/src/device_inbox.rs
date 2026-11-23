//! What another device addresses to this one.
//!
//! A window this session holds is reached through the workspace's host, which hands
//! it what a caller asks of it: a listing of what the window offers, a page action.
//! The frame arrives on this session's own connection with the host, stamped with
//! the caller the host authenticated. Nothing else on this device takes such a
//! frame, so this is where it is served: the session's broker answers for its
//! windows, and the answer goes back to the device that asked, from this one.
//!
//! The caller is the device the host named, certified by the person it named: the
//! session decides nothing about who they are beyond what the host authenticated,
//! and the broker admits the request as it admits any other from a remote device.

use std::sync::Arc;

use collab_types::WorkspaceId;
use tilo_actions::InvocationId;
use tilo_actions::remote::{ActionProvider, Caller, RemoteActionFrame};

/// The session's device host, shared: an [`ActionProvider`] that is itself [`Clone`],
/// which the broker's is not — so a request's answer may outlive this pass.
#[derive(Clone)]
struct Shared(Arc<dyn ActionProvider + Send + Sync>);

impl ActionProvider for Shared {
    fn retire_execution(&self, caller: &Caller) {
        self.0.retire_execution(caller);
    }

    fn query(
        &self,
        caller: &Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>,
    > {
        self.0.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        self.0.invoke(caller, request)
    }

    fn observe(
        &self,
        caller: &Caller,
        id: &InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        self.0.observe(caller, id)
    }

    fn next_change(
        &self,
        caller: &Caller,
        id: &InvocationId,
    ) -> Option<tilo_actions::remote::Pending<()>> {
        self.0.next_change(caller, id)
    }

    fn changed(
        &self,
        caller: &Caller,
        scope: tilo_actions::remote::WatchScope,
    ) -> Option<tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>>>
    {
        self.0.changed(caller, scope)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        self.0.cancel(caller, id)
    }

    fn confirm(
        &self,
        caller: &Caller,
        request: &tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::ConfirmedInvocation, tilo_actions::remote::ActionCallError>,
    > {
        self.0.confirm(caller, request)
    }

    fn content_puts(&self) -> Option<Arc<dyn tilo_actions::remote::ContentPuts>> {
        self.0.content_puts()
    }

    fn content_gets(&self) -> Option<Arc<dyn tilo_actions::remote::ContentGets>> {
        self.0.content_gets()
    }

    fn here(&self, caller: &Caller) -> Option<Vec<tilo_actions::remote::Here>> {
        self.0.here(caller)
    }

    fn resource_reads(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        self.0.resource_reads()
    }

    fn resource_changes(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceChanges>> {
        self.0.resource_changes()
    }

    fn resource_search(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceSearch>> {
        self.0.resource_search()
    }

    fn change_feed(&self) -> Option<Arc<dyn tilo_actions::remote::ChangeFeed>> {
        self.0.change_feed()
    }
}

/// Where an [`Inbox`] says an answer: the frame, to the device that asked, in the
/// workspace it asked in.
type Say = Arc<dyn Fn(WorkspaceId, [u8; 32], Vec<u8>) + Send + Sync>;

/// What answers a frame the host routed here: the session's broker as a device's
/// action host, and where the answer is said.
pub(crate) struct Inbox {
    /// The host the session's broker is for the person's devices.
    provider: Shared,
    /// Say `frame` to the device `to` in `workspace`.
    say: Say,
}

impl Inbox {
    pub(crate) fn new(provider: Arc<dyn ActionProvider + Send + Sync>, say: Say) -> Self {
        Self {
            provider: Shared(provider),
            say,
        }
    }

    /// Serve the frame `from`, certified for `user`, sent this device in `workspace`,
    /// and answer it to that device once the broker has: an answer that takes time
    /// (an invocation the broker holds) is said when it comes, off the session's pass.
    pub(crate) fn take(
        &self,
        workspace: WorkspaceId,
        user: [u8; 32],
        from: [u8; 32],
        frame: &[u8],
    ) {
        let Ok(frame) = RemoteActionFrame::decode(frame) else {
            tracing::warn!(
                device = %hex::encode(from),
                bytes = frame.len(),
                "a device sent this session a frame that is not a remote action"
            );
            return;
        };
        let message = frame.message;
        // An answer is the caller side's to take, and this session asked nothing
        // through the host: only a request is served here.
        if !message.is_request() {
            return;
        }
        let caller = Caller::authenticated(user, from).in_workspace(workspace.to_string());
        let provider = self.provider.clone();
        let say = Arc::clone(&self.say);
        let serve = move || {
            let Some(answer) = tilo_actions::remote::serve(&provider, &caller, message) else {
                return;
            };
            answer.then(move |answer| {
                let Some(answer) = answer else {
                    return;
                };
                let Ok(frame) = RemoteActionFrame::new(answer).encode() else {
                    tracing::warn!(
                        device = %hex::encode(from),
                        "this session's answer to a device did not encode"
                    );
                    return;
                };
                say(workspace, from, frame);
            });
        };
        // A catalogue is assembled and an invocation admitted on a thread of
        // their own, never the one carrying what the host said: that pass is
        // every other frame's too.
        if let Err(error) = std::thread::Builder::new()
            .name("device-inbox".into())
            .spawn(serve)
        {
            tracing::warn!(%error, "a request for this session was not served");
        }
    }
}

impl tilo_session_client::InboundActions for Inbox {
    fn take(&self, workspace: WorkspaceId, user: [u8; 32], from: [u8; 32], frame: Vec<u8>) {
        Self::take(self, workspace, user, from, &frame);
    }
}

/// A host for the tests here and beside them: every listing is an empty page,
/// every invocation completes at once, and who asked is kept.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::Mutex;
    use tilo_actions::InvocationId;
    use tilo_actions::remote::{
        ActionCallError, CataloguePage, CatalogueQuery, InvocationReceipt, InvocationState,
        InvokeRequest, Pending, ReceiptStatus, pending,
    };

    #[derive(Default)]
    pub(crate) struct RecordingHost {
        pub(crate) callers: Mutex<Vec<Caller>>,
    }

    impl RecordingHost {
        pub(crate) fn callers(&self) -> Vec<Caller> {
            self.callers.lock().unwrap().clone()
        }
    }

    impl ActionProvider for RecordingHost {
        fn query(
            &self,
            caller: &Caller,
            _query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            self.callers.lock().unwrap().push(caller.clone());
            let (completer, answer) = pending();
            completer.complete(Ok(CataloguePage::default()));
            answer
        }

        fn invoke(
            &self,
            caller: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            self.callers.lock().unwrap().push(caller.clone());
            Pending::ready(Ok(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput::default(),
                },
            }))
        }

        fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
            None
        }

        fn cancel(
            &self,
            _caller: &Caller,
            _id: &InvocationId,
        ) -> Pending<Result<(), ActionCallError>> {
            Pending::ready(Ok(()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::RecordingHost;
    use super::*;
    use std::sync::Mutex;
    use tilo_actions::remote::{CataloguePage, RemoteActionMessage};

    /// The answers an inbox said: the workspace, the device they went to, and the
    /// frame itself.
    type Said = Arc<Mutex<Vec<(WorkspaceId, [u8; 32], Vec<u8>)>>>;

    /// An inbox whose answers are kept, over `host`.
    fn inbox(provider: Arc<RecordingHost>) -> (Inbox, Said) {
        let said = Arc::new(Mutex::new(Vec::new()));
        let said_to = Arc::clone(&said);
        let inbox = Inbox::new(
            provider,
            Arc::new(move |workspace, to, frame| {
                said_to.lock().unwrap().push((workspace, to, frame));
            }),
        );
        (inbox, said)
    }

    /// The session answers a listing the host routed to it, and says the answer to
    /// the device that asked, from this one.
    #[test]
    fn a_listing_routed_here_is_answered_to_the_device_that_asked() {
        let provider = Arc::new(RecordingHost::default());
        let (inbox, said) = inbox(Arc::clone(&provider));
        let (workspace, user, from) = (WorkspaceId::from_u128(7), [0x11; 32], [0x22; 32]);
        let frame = RemoteActionFrame::new(RemoteActionMessage::CatalogueRequest {
            correlation: 5,
            query: tilo_actions::remote::CatalogueQuery::default(),
        })
        .encode()
        .unwrap();

        inbox.take(workspace, user, from, &frame);

        // Served on a thread of its own: the answer arrives a moment later.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if !said.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let callers = provider.callers();
        let caller = callers.first().expect("the broker was asked");
        assert_eq!(
            (caller.user_public_key(), caller.device_public_key()),
            (Some(&user), Some(&from)),
            "the caller is the device the host authenticated, for the person it named"
        );
        assert_eq!(caller.workspace(), Some(workspace.to_string().as_str()));
        let said = said.lock().unwrap();
        let (answered_in, to, answer) = said
            .first()
            .map(|(w, d, f)| (*w, *d, f.clone()))
            .expect("the answer was said");
        drop(said);
        assert_eq!((answered_in, to), (workspace, from));
        let RemoteActionMessage::CatalogueResponse { correlation, .. } =
            RemoteActionFrame::decode(&answer).unwrap().message
        else {
            panic!("a listing is answered with a page");
        };
        assert_eq!(correlation, 5);
    }

    /// What is not a request is not served: an answer this session never asked for
    /// is dropped, and nothing is said back.
    #[test]
    fn only_a_request_is_served() {
        let provider = Arc::new(RecordingHost::default());
        let (inbox, said) = inbox(Arc::clone(&provider));
        let frame = RemoteActionFrame::new(RemoteActionMessage::CatalogueResponse {
            correlation: 1,
            result: Ok(CataloguePage::default()),
        })
        .encode()
        .unwrap();

        inbox.take(WorkspaceId::from_u128(7), [0x11; 32], [0x22; 32], &frame);

        assert!(provider.callers().is_empty());
        assert!(said.lock().unwrap().is_empty());
    }
}
