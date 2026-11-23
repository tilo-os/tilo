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

/// A page of a listing read on from the one before it resolves no path again: the app is
/// asked for the rows on that page and nothing else, where a first page asks it for the
/// workspace's installed apps and the app's own directory to find what the path names.
#[tokio::test]
async fn a_later_page_of_a_listing_resolves_no_path_again() {
    let notes = Notes::new(true);
    let mut rows = notes.rows("1");
    rows.push(notes.row("", "settings", "settings", "Reading settings", None, true));
    // More than a page of a directory level holds ([`PAGE_ROWS`]), so the level is read in
    // pages and the listing's next cursor names one.
    for index in 0..300 {
        rows.push(notes.row(
            "settings",
            &format!("option:{index}"),
            "option",
            &format!("Option {index:02}"),
            None,
            false,
        ));
    }
    *notes.published_kinds.lock().unwrap() = Some(rows);
    // The directory answers in pages of a hundred rows, as an app's does.
    let notes = notes.paging_levels(100);
    let server = agent(&notes);
    let (first, failed) = tool(
        &server,
        "tilo_ls",
        json!({"path": "notes/settings", "limit": 200}),
    )
    .await;
    assert!(!failed, "{first}");
    let goes_on = first
        .split("cursor \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("a first page goes on");
    // The page after this one, from the directory page its 201st resource starts in: the
    // cursor says how many rows stand before it (200) and the revision the directory was
    // read at (`v1`), as the app's cursors do.
    assert!(goes_on.starts_with("200.0.7631."), "{goes_on}");
    let asked = notes.take_asked();
    assert!(
        asked.iter().any(|ask| ask.ends_with("under "))
            && asked.iter().any(|ask| ask.contains("named ")),
        "a first page finds what the path names: {asked:?}"
    );
    let (next, failed) = tool(
        &server,
        "tilo_ls",
        json!({"path": "notes/settings", "limit": 200,
            // The directory page the 201st resource starts at, in the fixture's own cursor
            // (a row count), keeping the revision the page was read at.
            "cursor": "200.0.7631.323030"}),
    )
    .await;
    assert!(!failed, "{next}");
    assert!(
        next.contains("Option 299"),
        "the rest of the directory: {next}"
    );
    let asked = notes.take_asked();
    let settings = notes.id("settings");
    assert!(
        asked
            .iter()
            .any(|ask| ask.ends_with(&format!("under {settings}"))),
        "the page reads its rows: {asked:?}"
    );
    assert!(
        !asked.iter().any(|ask| ask.ends_with("under ")),
        "the app's own directory is not resolved again: {asked:?}"
    );
    assert!(
        !asked.iter().any(|ask| ask.contains("named ")),
        "the workspace's installed apps are not asked again: {asked:?}"
    );
}

/// Notes answering a page bounded in resources as a provider that builds the
/// resources its page shows does ([`CatalogueQuery::max_resources`]): the
/// rows of that many, saying more follow past them.
#[derive(Clone)]
struct Bounded(Notes);

impl ActionProvider for Bounded {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let bound = query
            .provider
            .is_some()
            .then(|| query.resource_bound())
            .flatten();
        // The fixture's own cursor: the row it starts at.
        let from = std::str::from_utf8(&query.cursor)
            .ok()
            .and_then(|cursor| cursor.parse::<usize>().ok())
            .unwrap_or(0);
        self.0.query(caller, query).map(move |page| {
            page.map(move |mut page| {
                if let Some(most) = bound {
                    page.rows.drain(..from.min(page.rows.len()));
                    if tilo_actions::remote::truncate_resources(&mut page.rows, most) {
                        page.exhausted = false;
                        page.next_cursor = from
                            .saturating_add(page.rows.len())
                            .to_string()
                            .into_bytes();
                    }
                }
                page
            })
        })
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.0.invoke(caller, request)
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.0.observe(caller, id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        self.0.cancel(caller, id)
    }
}

/// An app's own page lists what its notebook holds, merged into one level and
/// paged by offset. A page is bounded in resources, so the notebook's read
/// holds the notes the page shows and not the notebook; the cursor still
/// leads through every note, and none past the bound is lost.
#[tokio::test]
async fn a_listing_of_an_apps_own_page_walks_its_notebook_whole() {
    const NOTES: usize = 100;
    let server = agent(&Bounded(Notes::new(true).with_notes(NOTES)));
    let mut named: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut args = json!({"path": "notes", "limit": 100});
        if let Some(cursor) = &cursor {
            args["cursor"] = json!(cursor);
        }
        let (page, failed) = typed_calls::answer(&server, "tilo_ls", args).await;
        assert!(!failed, "{page}");
        named.extend(
            page["rows"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|row| row["name"].as_str().map(str::to_owned)),
        );
        match page["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
        assert!(
            named.len() <= NOTES.saturating_add(8),
            "the cursor leads somewhere: {named:?}"
        );
    }
    let noted: Vec<&String> = named
        .iter()
        .filter(|name| name.starts_with("Bench note"))
        .collect();
    assert_eq!(noted.len(), NOTES, "every note is listed: {noted:?}");
    let distinct: std::collections::HashSet<&&String> = noted.iter().collect();
    assert_eq!(distinct.len(), NOTES, "no note is listed twice: {noted:?}");
}

/// An app's listing names the actions its root resource offers, each under the
/// root it is published on, so an agent calls one straight from the app's
/// listing rather than walking into the root first.
#[tokio::test]
async fn an_app_lists_its_roots_actions_under_the_root() {
    // An app whose only actions are on its root resource: a notebook that holds
    // nothing yet, so its level lists no item and its root holds what it offers.
    let notes = Notes::new(true).live();
    let root = notes.row("", NOTEBOOK, "notes.notebook", "Notes", None, false);
    let new_note = notes.row(
        "",
        NOTEBOOK,
        "notes.notebook",
        "Notes",
        Some((
            "new-note",
            "New note",
            vec![ActionParam::optional("title", ParamType::Text, "Text")],
            tilo_actions::OperationKind::Effect,
        )),
        false,
    );
    *notes.published_kinds.lock().unwrap() = Some(vec![root, new_note]);

    let server = agent(&notes);
    let (text, failed) = tool(&server, "tilo_ls", json!({"path": "notes"})).await;
    assert!(!failed, "{text}");
    assert!(text.contains("#new-note"), "{text}");
    assert_eq!(
        text.matches("#new-note").count(),
        1,
        "the app lists its root's action once: {text}"
    );
    // What the app's listing names it by takes its parameters here.
    let (page, failed) = tool(&server, "tilo_ls", json!({"path": "notes#new-note"})).await;
    assert!(!failed && page.contains("title"), "{page}");
    // And it runs where the listing names it: on the root it is published on.
    let (done, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "notes#new-note", "args": {"title": "Plan"}}),
    )
    .await;
    assert!(!failed, "{done}");
    let (resource, action, _) = notes.invoked().last().cloned().expect("new-note ran");
    assert_eq!(resource, notes.id(NOTEBOOK), "{done}");
    assert_eq!(action, "new-note", "{done}");

    // A root that holds its actions in its own level names each once, never beside
    // what the app's listing already took from it.
    let live = Notes::new(true).live();
    let server = agent(&live);
    let (app, failed) = tool(&server, "tilo_ls", json!({"path": "notes"})).await;
    assert!(!failed, "{app}");
    assert_eq!(app.matches("#new-note").count(), 1, "listed once: {app}");
    let (root, failed) = tool(&server, "tilo_ls", json!({"path": "notes/Notebook"})).await;
    assert!(!failed, "{root}");
    assert_eq!(
        root.matches("#new-note").count(),
        1,
        "the root's own: {root}"
    );
}
