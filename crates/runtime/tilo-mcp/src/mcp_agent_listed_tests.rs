//! What a typed listing says of the actions on what it lists: each by its name, and what one
//! takes in full where that one action is asked for.
use super::*;

/// Every action a typed listing names: the listed thing's own, then those of each kind of
/// thing on the page.
fn named_in(listing: &Value) -> Vec<&Value> {
    let own = listing["actions"].as_array().into_iter().flatten();
    let of_kinds = listing["kinds"]
        .as_object()
        .into_iter()
        .flat_map(|kinds| kinds.values())
        .flat_map(|rows| rows.as_array().into_iter().flatten());
    own.chain(of_kinds).collect()
}

/// A typed listing names its rows first, then each action by its id, its label, what it may
/// reach and what it takes by name and type. What a parameter means, its choices and its
/// schema are answered where that one action is asked for: said of every action of every kind
/// on a page they were nine tenths of a listing, and its rows stood behind them.
#[tokio::test]
async fn a_typed_listing_names_each_action_and_one_action_asked_for_says_what_it_takes() {
    let notes = Notes::new(true);
    let server = agent(&notes);
    // Whether this build keeps an answer's fields in the order they were written.
    let ordered = json!({"b": 1, "a": 2}).to_string().starts_with("{\"b\"");
    for path in ["notes", "notes/Todo"] {
        let asked = json!({ "path": path });
        let (listing, failed) = typed_calls::answer(&server, "tilo_ls", asked).await;
        assert!(!failed, "{listing}");
        let written = listing.to_string();
        assert!(
            !ordered || written.find("\"rows\"") < written.find("\"actions\""),
            "{path}: the rows stand before the actions: {written}"
        );
        let named = named_in(&listing);
        assert!(!named.is_empty(), "{path} names actions: {listing}");
        for action in named {
            assert!(
                action["id"].is_string()
                    && action["label"].is_string()
                    && action["kind"].is_string(),
                "{path}: {action}"
            );
            for param in action["params"].as_array().into_iter().flatten() {
                let mut said: Vec<&str> = param
                    .as_object()
                    .into_iter()
                    .flat_map(|fields| fields.keys())
                    .map(String::as_str)
                    .collect();
                said.sort_unstable();
                assert_eq!(said, ["name", "required", "ty"], "{path}: {action}");
            }
        }
    }
    // The one action asked for says everything it takes.
    let rename = json!({"path": "notes/Todo#rename"});
    let (one, failed) = typed_calls::answer(&server, "tilo_ls", rename).await;
    assert!(!failed, "{one}");
    let title = &one["actions"][0]["params"][0];
    assert_eq!(title["name"], "title", "{one}");
    assert!(title.get("description").is_some(), "{one}");
}

/// A typed listing names no action its text does not: what a caller is listed is the same in
/// both forms, so the typed one tells a caller of nothing the lines it reads do not.
#[tokio::test]
async fn a_typed_listing_names_no_action_its_text_does_not() {
    let notes = Notes::new(true);
    let server = agent(&notes);
    let asked = json!({"path": "notes/Todo", "limit": 200});
    let (text, failed) = tool(&server, "tilo_ls", asked.clone()).await;
    assert!(!failed, "{text}");
    let (listing, failed) = typed_calls::answer(&server, "tilo_ls", asked).await;
    assert!(!failed, "{listing}");
    let own = listing["actions"].as_array().expect("its actions");
    assert!(!own.is_empty(), "{listing}");
    for action in own {
        let id = action["id"].as_str().unwrap_or_default();
        assert!(
            !id.is_empty() && text.contains(id),
            "{id} is named typed and not in the text:\n{text}"
        );
    }
}
