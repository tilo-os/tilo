//! A change feed asked across the API: answered as its provider answers it, and what a provider
//! keeping none says.
use super::*;

/// A provider's change feed is asked across the API as any request is, answered from the
/// provider's own changes; a provider keeping none says so in the words a watch tells apart.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one provider answering a watch, and one keeping none, in order"
)]
fn changes_cross_the_api_as_the_host_answers_them() {
    struct Changed;
    impl ChangeFeed for Changed {
        fn since(
            &self,
            _caller: &Caller,
            after: Option<&str>,
        ) -> Pending<Result<ChangePage, ActionCallError>> {
            Pending::ready(Ok(ChangePage {
                changes: vec![ResourceChanged {
                    app: "so.tilo.notes".into(),
                    record: String::new(),
                    entry: serde_json::json!({"change": "abc", "invocation": "i"}),
                }],
                cursor: format!("{}+1", after.unwrap_or("start")),
            }))
        }
    }
    /// A provider answering the four calls, and nothing else of its own.
    #[derive(Clone)]
    struct Bare;
    impl ActionProvider for Bare {
        fn query(
            &self,
            _caller: &Caller,
            _query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            Pending::ready(Ok(CataloguePage::default()))
        }
        fn invoke(
            &self,
            _caller: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            Pending::ready(Ok(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Completed {
                    output: ActionOutput::default(),
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
    #[derive(Clone)]
    struct Watching(Bare);
    impl ActionProvider for Watching {
        fn query(
            &self,
            caller: &Caller,
            query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            self.0.query(caller, query)
        }
        fn invoke(
            &self,
            caller: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            self.0.invoke(caller, request)
        }
        fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
            self.0.observe(caller, id)
        }
        fn cancel(
            &self,
            caller: &Caller,
            id: &InvocationId,
        ) -> Pending<Result<(), ActionCallError>> {
            self.0.cancel(caller, id)
        }
        fn change_feed(&self) -> Option<std::sync::Arc<dyn ChangeFeed>> {
            Some(std::sync::Arc::new(Changed))
        }
    }
    let watching = Watching(Bare);
    let changes = RemoteActionMessage::ChangesRequest {
        correlation: 8,
        after: Some("c".into()),
    };
    let Some(RemoteActionMessage::ChangesResponse {
        correlation: 8,
        result: Ok(page),
    }) = serve(&watching, &alice(), changes.clone()).map(now)
    else {
        panic!("a watch is answered with the provider's changes");
    };
    assert_eq!((page.cursor.as_str(), page.changes.len()), ("c+1", 1));
    // A provider keeping none refuses it, in the words a watch tells from a failure.
    let bare = Bare;
    let Some(RemoteActionMessage::ChangesResponse {
        result: Err(ActionCallError::Unavailable { reason }),
        ..
    }) = serve(&bare, &alice(), changes).map(now)
    else {
        panic!("a provider with no feed refuses a watch of it");
    };
    assert_eq!(reason, CHANGE_FEED_UNAVAILABLE);
}
