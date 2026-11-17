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

tilo_test_env::scenario! {
    fn tilo_view_reads_what_the_window_shows;

    phase the_lines_are_the_views_own_semantic_tree_of_the_item_named;
    {
        let host = Notes::new(true).live();
        run(async {
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
        run(async {
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
        run(async {
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
        run(async {
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
}
