//! `tilo_view`: what a person's window shows, as the lines an agent reads — and the
//! picture of it where the host can hand one over.
use super::*;

/// The row the live Notes publishes for the action every app's window answers with.
pub(super) fn outline_row(host: &Notes) -> CatalogueRow {
    host.row(
        "",
        NOTEBOOK,
        "notes.notebook",
        "Notebook",
        Some((
            "outline",
            "Read the view",
            vec![ActionParam::optional("anchor", ParamType::Text, "Text")],
            tilo_actions::OperationKind::View,
        )),
        false,
    )
}

/// What Notes answers reading its window: the lines, and the picture when `arguments` ask.
pub(super) fn outline_answer(
    invocation: tilo_actions::InvocationId,
    arguments: &serde_json::Map<String, Value>,
) -> InvocationUpdate {
    let mut value = json!({
        "outline": "document\n  static_text \"Todo\"\n  button \"Rename\"\n",
        "tree": [
            {"role": "static_text", "name": "Todo", "rect": [0.0, 0.0, 40.0, 20.0]},
            {"role": "button", "name": "Rename", "rect": [0.0, 24.0, 60.0, 24.0]},
        ],
        "nodes": 2,
        "omitted": 0,
        "viewport": [1280.0, 760.0],
        "window_origin": [0.0, 40.0],
        "theme": "Dark",
        "revision": "3f8777cc627c",
    });
    let mut resources = Vec::new();
    if arguments.get("picture").and_then(Value::as_bool) == Some(true) {
        let fields = value.as_object_mut().expect("an object");
        fields.insert("content".into(), json!("tilo.content:look"));
        fields.insert("width".into(), json!(1280));
        fields.insert("height".into(), json!(760));
        resources.push("tilo.content:look".to_owned());
    }
    InvocationUpdate::Succeeded {
        invocation,
        output: tilo_actions::ActionOutput {
            value: Some(value),
            resources,
        },
    }
}

impl Notes {
    /// What the window answers a read: a loading view while `loading_outline` lasts, its
    /// lines after.
    pub(super) fn view_read(
        &self,
        id: &tilo_actions::InvocationId,
        arguments: &serde_json::Map<String, Value>,
    ) -> InvocationUpdate {
        if self.loading_outline.load(Ordering::Relaxed) > 0 {
            self.loading_outline.fetch_sub(1, Ordering::Relaxed);
            loading_answer(*id)
        } else {
            outline_answer(*id, arguments)
        }
    }
}

/// What the app answers while its window is still loading what it shows: a busy state and
/// nothing of what arrived later.
pub(super) fn loading_answer(invocation: tilo_actions::InvocationId) -> InvocationUpdate {
    InvocationUpdate::Succeeded {
        invocation,
        output: tilo_actions::ActionOutput {
            value: Some(json!({
                "outline": "document\n  group \"Files activity\" [busy]\n",
                "tree": [
                    {"role": "group", "name": "Files activity", "value": "Loading the folder", "state": ["busy"], "rect": [0.0, 0.0, 40.0, 20.0]},
                ],
                "nodes": 1,
                "omitted": 0,
                "viewport": [1280.0, 760.0],
                "window_origin": [0.0, 40.0],
                "theme": "Dark",
                "revision": "000000000000",
            })),
            resources: Vec::new(),
        },
    }
}

/// A phase's async body on a runtime of its own: the same runtime the other test modules
/// build, under a name the glossary keeps clear.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
        .block_on(future)
}

tilo_test_env::scenario! {
    fn tilo_view_reads_what_the_window_shows;

    phase the_lines_are_the_views_own_semantic_tree_of_the_item_named;
    {
        let host = Notes::new(true).live();
        block_on(async {
            let (text, failed) =
                tool(&agent(&host), "tilo_view", json!({"path": "notes/Todo"})).await;
            assert!(!failed, "{text}");
            assert!(
                text.contains("static_text \"Todo\"") && text.contains("button \"Rename\""),
                "the window's lines: {text}"
            );
            // The view was asked to show the note the path names, so the lines are of that
            // note and not of whatever the window showed before.
            let asked = host.invoked();
            let (_, _, arguments) = asked
                .iter()
                .find(|(_, action, _)| action == "outline")
                .unwrap_or_else(|| panic!("the view was read: {asked:?}"));
            assert_eq!(
                arguments["anchor"],
                json!(host.id(&Notes::live_key(TODO))),
                "{arguments}"
            );
            // No picture unless one is asked for: a model pays for every image.
            assert_eq!(arguments.get("picture"), None, "{arguments}");
        });
    }

    phase a_picture_is_asked_for_by_name;
    {
        let host = Notes::new(true).live();
        block_on(async {
            let (typed, failed) = answer(
                &agent(&host),
                "tilo_view",
                json!({"path": "notes", "picture": true}),
            )
            .await;
            assert!(!failed, "{typed}");
            assert_eq!(typed["picture"], json!("tilo.content:look"), "{typed}");
            assert_eq!(typed["nodes"], json!(2), "{typed}");
            assert_eq!(typed["window_origin"], json!([0.0, 40.0]), "{typed}");
            assert!(
                typed["tree"].as_array().is_some_and(|tree| tree.len() == 2),
                "{typed}"
            );
            let asked = host.invoked();
            let (_, _, arguments) = asked
                .iter()
                .find(|(_, action, _)| action == "outline")
                .unwrap_or_else(|| panic!("the view was read: {asked:?}"));
            assert_eq!(arguments["picture"], json!(true), "{arguments}");
            // A path naming the app itself names no item: the window is what it shows.
            assert_eq!(arguments.get("anchor"), None, "{arguments}");
        });
    }

    phase a_picture_the_host_can_read_is_sent_as_image_content;
    {
        use base64::Engine as _;
        let host = Notes::new(true).live();
        block_on(async {
            let server = agent(&host).with_media(Some(std::sync::Arc::new(
                crate::test_hosts::StoredByTestApp,
            )));
            let reply = server
                .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                    "params":{"name":"tilo_view",
                        "arguments":{"path":"notes","picture":true},
                        "_meta":{"so.tilo.mcp":{"structured":true}}}}))
                .await
                .unwrap();
            let result = &reply["result"];
            let images: Vec<Value> = result["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|block| block["type"] == "image")
                .cloned()
                .collect();
            assert_eq!(images.len(), 1, "{reply}");
            assert_eq!(images[0]["mimeType"], "image/png");
            assert_eq!(
                base64::engine::general_purpose::STANDARD
                    .decode(images[0]["data"].as_str().unwrap())
                    .unwrap(),
                crate::test_hosts::PNG
            );
            assert_eq!(
                result["structuredContent"]["picture"],
                json!("tilo.content:look"),
                "{reply}"
            );
        });
    }

    phase an_app_that_does_not_run_is_started_without_a_window;
    {
        let host = Notes::new(false).live();
        block_on(async {
            let (text, failed) =
                tool(&agent(&host), "tilo_view", json!({"path": "notes/Todo"})).await;
            assert!(!failed, "{text}");
            assert!(text.contains("static_text \"Todo\""), "{text}");
            let asked = host.invoked();
            assert!(
                asked
                    .iter()
                    .any(|(_, action, _)| action == "start"),
                "the app was started: {asked:?}"
            );
            assert!(
                asked.iter().any(|(_, action, _)| action == "outline"),
                "then its view read: {asked:?}"
            );
        });
    }

    phase a_window_still_loading_is_read_again_until_what_it_shows_arrives;
    {
        // The first two reads answer the window with its folder still opening, as a window
        // read before its resources arrived does.
        let notes = Notes::new(true).live().loading_reads(2);
        block_on(async {
            let (typed, failed) =
                answer(&agent(&notes), "tilo_view", json!({"path": "notes"})).await;
            assert!(!failed, "{typed}");
            assert_eq!(typed["loading"], json!(false), "{typed}");
            assert!(
                typed["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("button \"Rename\"")),
                "the settled view, not the busy one: {typed}"
            );
            let reads = notes
                .invoked()
                .iter()
                .filter(|(_, action, _)| action == "outline")
                .count();
            assert_eq!(reads, 3, "the busy reads and the one that settled");
        });
    }

    phase a_window_that_never_settles_answers_what_it_has_and_says_it_is_loading;
    {
        let notes = Notes::new(true).live().loading_reads(u32::MAX);
        block_on(async {
            let (text, failed) =
                tool(&agent(&notes), "tilo_view", json!({"path": "notes"})).await;
            assert!(!failed, "{text}");
            assert!(text.contains("[busy"), "what it shows now: {text}");
            assert!(
                text.ends_with("(still loading: the window has not finished showing what it holds)"),
                "the answer says it is still loading: {text}"
            );
            let reads = notes
                .invoked()
                .iter()
                .filter(|(_, action, _)| action == "outline")
                .count();
            assert!(
                reads > 1 && reads <= 100,
                "read again, bounded: {reads} reads"
            );
        });
    }

    phase a_picture_of_a_loading_window_is_taken_once_the_view_settled;
    {
        let notes = Notes::new(true).live().loading_reads(1);
        block_on(async {
            let (typed, failed) = answer(
                &agent(&notes),
                "tilo_view",
                json!({"path": "notes", "picture": true}),
            )
            .await;
            assert!(!failed, "{typed}");
            assert_eq!(typed["picture"], json!("tilo.content:look"), "{typed}");
            let asked = notes.invoked();
            let pictured: Vec<bool> = asked
                .iter()
                .filter(|(_, action, _)| action == "outline")
                .map(|(_, _, arguments)| arguments.get("picture") == Some(&json!(true)))
                .collect();
            assert_eq!(
                pictured,
                vec![true, false, true],
                "the loading picture is dropped, and the settled view pictured: {asked:?}"
            );
        });
    }
}
