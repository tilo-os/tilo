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

/// A typed listing says which of its rows a read answers as a resource of
/// its own: a note its notebook reads one at a time is one, though it offers
/// no read itself, and a place inside what lists it (an option, a choice) is
/// not, so a walk that reads resources one by one reads only the rows that
/// are.
#[tokio::test]
async fn a_typed_listing_says_which_rows_a_read_answers() {
    let notes = Notes::new(true);
    let mut rows = notes.rows("1");
    // The reading settings beside the notes, offering nothing a read answers
    // of its own, with a font inside them.
    rows.push(notes.row(
        NOTEBOOK,
        "settings",
        "settings",
        "Reading settings",
        None,
        true,
    ));
    rows.push(notes.row("settings", "font:sans", "font", "Sans", None, false));
    *notes.published_kinds.lock().unwrap() = Some(rows);
    let server = agent(&notes);
    let (listing, failed) =
        typed_calls::answer(&server, "tilo_ls", json!({"path": "notes/Notebook"})).await;
    assert!(!failed, "{listing}");
    let reads = |listing: &Value, name: &str| {
        listing["rows"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["name"] == name))
            .map(|row| row["reads"].clone())
    };
    assert_eq!(
        reads(&listing, "Todo"),
        Some(json!(true)),
        "the notebook reads its notes one at a time: {listing}"
    );
    let (fonts, failed) = typed_calls::answer(
        &server,
        "tilo_ls",
        json!({"path": "notes/Notebook/Reading settings"}),
    )
    .await;
    assert!(!failed, "{fonts}");
    assert_eq!(
        reads(&fonts, "Sans"),
        Some(json!(false)),
        "a font is part of the settings' read: {fonts}"
    );
}

/// A path naming a place inside a resource answers no read of its own: the
/// root resources are read once to find the read that could answer such a
/// path, not once for every place a walk asks about.
#[tokio::test]
async fn a_read_of_a_place_answers_no_read_without_reading_the_apps_root_again() {
    let notes = Notes::new(true);
    let mut rows = vec![
        notes.row("", "font", "place", "Fonts", None, false),
        notes.row("", "theme", "place", "Themes", None, false),
        notes.row("", "folder", "place", "Folders", None, false),
        notes.row("", "voice", "place", "Voices", None, false),
    ];
    rows.extend(notes.rows("1"));
    *notes.published_kinds.lock().unwrap() = Some(rows);
    let server = agent(&notes);
    let (_, _) = tool(&server, "tilo_read", json!({"path": "notes/font"})).await;
    notes.take_asked();
    let (_, _) = tool(&server, "tilo_read", json!({"path": "notes/theme"})).await;
    let asked = notes.take_asked();
    for place in ["font", "folder", "voice"] {
        let id = notes.id(place);
        assert!(
            !asked.iter().any(|ask| ask.contains(&format!("under {id}"))),
            "{place} was read again looking for a read it does not answer: {asked:?}"
        );
    }
}
