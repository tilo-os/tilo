//! A find narrowed by what its query says: to a stretch of time and to a conversation.
use super::*;

/// A search that narrows: what it was asked is kept, and the notes' own search answers.
#[derive(Clone)]
struct Narrowing {
    notes: Notes,
    asked: std::sync::Arc<std::sync::Mutex<Vec<(String, tilo_actions::remote::Within)>>>,
}

/// What a search answers with, when it has.
type Searched = Pending<Result<Vec<tilo_actions::remote::Found>, ActionCallError>>;

impl tilo_actions::remote::ResourceSearch for Narrowing {
    fn find(&self, caller: &Caller, needle: &str) -> Searched {
        self.find_within(caller, needle, &tilo_actions::remote::Within::default())
    }

    fn find_within(
        &self,
        caller: &Caller,
        needle: &str,
        within: &tilo_actions::remote::Within,
    ) -> Searched {
        self.asked
            .lock()
            .unwrap()
            .push((needle.to_owned(), within.clone()));
        tilo_actions::remote::ResourceSearch::find(&self.notes, caller, needle)
    }
}

impl ActionProvider for Narrowing {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        self.notes.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.notes.invoke(caller, request)
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.notes.observe(caller, id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        self.notes.cancel(caller, id)
    }

    fn resource_search(&self) -> Option<std::sync::Arc<dyn tilo_actions::remote::ResourceSearch>> {
        Some(std::sync::Arc::new(self.clone()))
    }
}

/// A find's query narrows it: `after:`, `before:` and `in:` are taken out of its words and
/// asked of the search as what it is narrowed to. A search that cannot narrow refuses, and is
/// never answered with everything instead.
#[test]
fn a_find_is_narrowed_by_what_its_query_says() {
    use tilo_actions::remote::{NOT_NARROWED, Within};
    let notes = Notes::new(true);
    run(async {
        // The tool says how a query narrows, and what a find never reaches.
        let listed = agent(&notes)
            .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
            .await
            .unwrap();
        let said = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "tilo_find")
            .and_then(|tool| tool["description"].as_str())
            .unwrap()
            .to_owned();
        for part in [
            "what you may read here",
            "in:here",
            "after:",
            "before:",
            "Not the web",
        ] {
            assert!(said.contains(part), "{part:?} in {said}");
        }

        // The notes' own search cannot narrow, and says so.
        let query = json!({"query": "Groceries after:2026-11-10"});
        let (text, failed) = tool(&agent(&notes), "tilo_find", query).await;
        assert!(failed, "{text}");
        assert!(text.contains(NOT_NARROWED), "{text}");

        // A search that narrows is asked for the words alone, and what the query narrows to.
        let narrowing = Narrowing {
            notes: notes.clone(),
            asked: std::sync::Arc::default(),
        };
        let query = json!({"query": "after:2026-11-10 Groceries before:2026-11-17T00:00:00Z"});
        let (text, failed) = tool(&agent(&narrowing), "tilo_find", query).await;
        assert!(!failed, "{text}");
        assert_eq!(
            text.lines().next(),
            Some("notes/Notebook/Groceries  notes.page"),
            "{text}"
        );
        let last_week = Within {
            place: None,
            after: Some(1_794_268_800),
            before: Some(1_794_873_600),
        };
        assert_eq!(
            narrowing.asked.lock().unwrap().as_slice(),
            [("Groceries".to_owned(), last_week)]
        );
        // Narrowed to a time, a find needs no words.
        let query = json!({"query": "after:2026-11-10"});
        let (text, failed) = tool(&agent(&narrowing), "tilo_find", query).await;
        assert!(!failed, "{text}");
        assert_eq!(narrowing.asked.lock().unwrap().len(), 2);
        // What names no day is refused in words that show one, and nothing is searched.
        let query = json!({"query": "Groceries after:lastweek"});
        let (text, failed) = tool(&agent(&narrowing), "tilo_find", query).await;
        assert!(failed && text.contains("2026-11-10"), "{text}");
        // `in:here` is the conversation a turn is in: a session in none is told to name one.
        let query = json!({"query": "Groceries in:here"});
        let (text, failed) = tool(&agent(&narrowing), "tilo_find", query).await;
        assert!(failed && text.contains("in:here"), "{text}");
        assert_eq!(narrowing.asked.lock().unwrap().len(), 2);
    });
}
