//! A search and a change feed asked across the API: each answered as its provider answers, a
//! narrowed search crossing whole, and what a provider keeping neither says.
use super::*;

/// A provider's search and its change feed are asked across the API as any
/// request is, each answer the provider's own; a provider keeping neither says
/// so in the words a caller tells apart.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one provider answering both asks, and one keeping neither, in order"
)]
fn search_and_changes_cross_the_api_as_the_host_answers_them() {
    struct Held;
    impl ResourceSearch for Held {
        fn find(
            &self,
            _caller: &Caller,
            needle: &str,
        ) -> Pending<Result<Vec<Found>, ActionCallError>> {
            Pending::ready(Ok(vec![Found {
                resource: needle.to_owned(),
                kind: "notes.Note".into(),
                label: "Gooseberry jam".into(),
                open: None,
                arguments: serde_json::Map::new(),
                app: Some("so.tilo.notes".into()),
                provider: None,
                when: None,
                ..Found::default()
            }]))
        }
    }
    impl ChangeFeed for Held {
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
    struct Holding(Bare);
    impl ActionProvider for Holding {
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
        fn resource_search(&self) -> Option<std::sync::Arc<dyn ResourceSearch>> {
            Some(std::sync::Arc::new(Held))
        }
        fn change_feed(&self) -> Option<std::sync::Arc<dyn ChangeFeed>> {
            Some(std::sync::Arc::new(Held))
        }
    }
    let holding = Holding(Bare);
    let invocation = InvocationId::new();
    let search = RemoteActionMessage::Invoke(search::Query::request(
        invocation,
        "note:1",
        &Within::default(),
    ));
    let encoded = RemoteActionFrame::new(search).encode().unwrap();
    let search = RemoteActionFrame::decode(&encoded).unwrap().message;
    assert!(search.is_request());
    let Some(RemoteActionMessage::Receipt(receipt)) =
        serve(&holding, &alice(), search.clone()).map(now)
    else {
        panic!("a search is answered with what the provider found");
    };
    assert_eq!(receipt.invocation, invocation);
    let found = search::found(receipt).unwrap();
    assert_eq!(found[0].resource, "note:1");
    // A search narrowed to a place and a time crosses whole, and a search that cannot
    // narrow says so: it never answers with everything instead.
    let narrowed = RemoteActionMessage::Invoke(search::Query::request(
        InvocationId::new(),
        "note:1",
        &Within {
            place: Some("pad/threads/general".into()),
            after: Some(1),
            before: None,
            app: None,
            about: Vec::new(),
        },
    ));
    let encoded = RemoteActionFrame::new(narrowed.clone()).encode().unwrap();
    assert_eq!(
        RemoteActionFrame::decode(&encoded).unwrap().message,
        narrowed
    );
    let Some(RemoteActionMessage::Receipt(receipt)) = serve(&holding, &alice(), narrowed).map(now)
    else {
        panic!("a search is answered with a receipt");
    };
    let Err(ActionCallError::Unavailable { reason }) = search::found(receipt) else {
        panic!("a search that cannot be narrowed refuses to be");
    };
    assert_eq!(reason, NOT_NARROWED);
    let changes = RemoteActionMessage::ChangesRequest {
        correlation: 8,
        after: Some("c".into()),
    };
    let Some(RemoteActionMessage::ChangesResponse {
        correlation: 8,
        result: Ok(page),
    }) = serve(&holding, &alice(), changes.clone()).map(now)
    else {
        panic!("a watch is answered with the provider's changes");
    };
    assert_eq!((page.cursor.as_str(), page.changes.len()), ("c+1", 1));

    // A provider keeping neither refuses each, the feed in the words a
    // watch tells from a failure.
    let bare = Bare;
    let Some(RemoteActionMessage::Receipt(receipt)) = serve(&bare, &alice(), search).map(now)
    else {
        panic!("a search is answered with a receipt");
    };
    assert!(matches!(
        search::found(receipt),
        Err(ActionCallError::Unavailable { .. })
    ));
    let Some(RemoteActionMessage::ChangesResponse {
        result: Err(ActionCallError::Unavailable { reason }),
        ..
    }) = serve(&bare, &alice(), changes).map(now)
    else {
        panic!("a provider with no feed refuses a watch of it");
    };
    assert_eq!(reason, CHANGE_FEED_UNAVAILABLE);
}
