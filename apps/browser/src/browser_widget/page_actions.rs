//! Page actions, as an agent reaches them: published on `browser.page`,
//! admitted through the resource operation lane, answered by the engine and
//! reported through the invocation's terminal update. Each phase is its own
//! test over a fresh page, so the requests it makes stand on their own.

use std::sync::Arc;

use ui::tilo_actions::{ActionInvoker, ActionSpec, InvocationId, InvocationUpdate};
use ui::webview::{FrameId, PageKey, PageNavigation, PageRecordNote, WebViewEngine as _};

use super::page_turns::{MAX_WAITING_PAGE_ACTIONS, PAGE_TURN_WAIT};
use super::tests::{effect_operation, synced};
use super::*;

/// A page that says something only in pixels: a small PNG, as the engine
/// paints a scanned letter. Its bytes are what a capture answers with.
const SCANNED_LETTER_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 12, 0, 0, 0, 6, 8, 2, 0,
    0, 0, 120, 140, 232, 214, 0, 0, 0, 50, 73, 68, 65, 84, 120, 156, 99, 96, 16, 209, 176, 9, 72,
    169, 232, 89, 176, 229, 196, 157, 15, 44, 18, 58, 14, 33, 25, 53, 19, 150, 236, 56, 243, 224,
    11, 135, 140, 129, 75, 68, 78, 195, 148, 21, 123, 24, 134, 170, 34, 0, 164, 221, 97, 81, 182,
    196, 223, 57, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// A page the window has synced, with the document its actions admit into.
fn page_on(url: &str) -> (Arc<TestEngine>, BrowserWidget, AutoCommit, ObjId) {
    let engine = TestEngine::new();
    engine.send(WebViewCommand::Navigate(url.into()));
    let mut widget = BrowserWidget::with_engine(engine.clone(), true);
    let (doc, root) = synced(&mut widget);
    (engine, widget, doc, root)
}
fn page_action(widget: &BrowserWidget, id: &str) -> ActionSpec {
    widget
        .published()
        .children
        .iter()
        .find(|resource| resource.id.0 == "browser.page")
        .unwrap()
        .actions
        .iter()
        .find(|action| action.id == id)
        .unwrap()
        .clone()
}
fn start(
    widget: &mut BrowserWidget,
    doc: &mut AutoCommit,
    root: &ObjId,
    id: &str,
    args: &serde_json::Value,
) -> Result<ui::tilo_actions::InvocationId, String> {
    let _ = (doc, root);
    start_as(widget, None, id, args)
}
/// Start the published page action `id` for the invoker the action door
/// admitted, or for nobody: the person at the window.
fn start_as(
    widget: &mut BrowserWidget,
    invoker: Option<&ActionInvoker>,
    id: &str,
    args: &serde_json::Value,
) -> Result<InvocationId, String> {
    let action = page_action(widget, id);
    let encoded = action
        .build_json(args.as_object().ok_or("arguments are an object")?)
        .map_err(|error| error.to_string())?;
    let invocation = InvocationId::new();
    match widget
        .admit_for(invocation, invoker, effect_operation(&encoded.payload))
        .map(Some)?
    {
        Some(ui::framework::Effected::Pending) => Ok(invocation),
        other => Err(format!("a page action must be accepted, got {other:?}")),
    }
}
/// Who asked the engine for what, in the order it was asked.
fn page_asks(engine: &TestEngine) -> Vec<(Option<String>, &'static str)> {
    engine
        .commands()
        .into_iter()
        .filter_map(|command| match command {
            WebViewCommand::Page { actor, request, .. } => Some((
                actor.map(|actor| actor.as_str().to_owned()),
                request.action(),
            )),
            _ => None,
        })
        .collect()
}
/// The request ids the engine was asked under, in order.
fn page_request_ids(engine: &TestEngine) -> Vec<u32> {
    engine
        .commands()
        .into_iter()
        .filter_map(|command| match command {
            WebViewCommand::Page { request_id, .. } => Some(request_id),
            _ => None,
        })
        .collect()
}
/// The requests the engine was told to stop.
fn page_cancels(engine: &TestEngine) -> Vec<u32> {
    engine
        .commands()
        .into_iter()
        .filter_map(|command| match command {
            WebViewCommand::CancelPage { request_id } => Some(request_id),
            _ => None,
        })
        .collect()
}
/// An engine answer that says `message` and nothing else.
fn answer(message: &str) -> PageReply {
    PageReply {
        url: "https://shop.test/".into(),
        message: message.into(),
        outline: None,
        data: None,
        image: None,
        navigation: PageNavigation::None,
        loading: false,
        record: PageRecordNote::default(),
    }
}
/// Let the engine's answers arrive and read `invocation`'s, as the reply of
/// its own caller does.
fn settled(
    widget: &mut BrowserWidget,
    doc: &AutoCommit,
    root: &ObjId,
    invocation: InvocationId,
) -> InvocationUpdate {
    for _ in 0..4 {
        widget.sync(doc, root);
        if let Some(update) = widget.poll_invocation(invocation) {
            return update;
        }
    }
    panic!("{invocation:?} never settled");
}
/// The value a settled page action succeeded with.
fn succeeded(update: InvocationUpdate) -> serde_json::Value {
    match update {
        InvocationUpdate::Succeeded { output, .. } => output.value.expect("an answer has a value"),
        other => panic!("the page action must succeed, got {other:?}"),
    }
}
/// The failure a settled page action ended with.
fn failed(update: InvocationUpdate) -> ui::tilo_actions::ActionFailure {
    match update {
        InvocationUpdate::Failed { error, .. } => error,
        other => panic!("the page action must fail, got {other:?}"),
    }
}
fn page_action_contract(id: &str) -> ui::tilo_actions::ActionContract {
    let (_, widget, _, _) = page_on("https://shop.test/");
    page_action(&widget, id).contract.clone()
}
fn page_requests(engine: &TestEngine) -> Vec<PageRequest> {
    engine
        .commands()
        .into_iter()
        .filter_map(|command| match command {
            WebViewCommand::Page { request, .. } => Some(request),
            _ => None,
        })
        .collect()
}

// Phase `page_actions_publish_programmatic_write_contracts`.
#[test]
fn page_actions_publish_programmatic_write_contracts() {
    let (_, widget, _, _) = page_on("https://shop.test/");
    for id in [
        "read-page",
        "find-page-element",
        "click-page",
        "fill-page",
        "select-page",
        "fill-page-files",
        "press-page",
        "submit-page",
        "fill-page-secret",
        "read-page-changes",
        "read-page-element-at",
        "wait-page-settled",
        "read-page-console",
        "read-page-network",
        "read-page-record",
        "read-page-fields",
        "find-page-text",
        "read-page-forms",
        "fill-page-form",
        "capture-page",
    ] {
        let action = page_action(&widget, id);
        assert!(action.contract.validate_declaration().is_empty(), "{id}");
        assert!(action.contract.availability.is_available(), "{id}");
        assert!(!action.contract.reaches_people(), "{id}");
        assert!(
            action
                .contract
                .tags
                .iter()
                .any(|tag| tag == ui::tilo_actions::UNTRUSTED_CONTENT_TAG),
            "{id} answers with page content: data, never instructions"
        );
        assert_eq!(
            action.contract.kind,
            ui::tilo_actions::ActionKind::Outside,
            "reading a signed-in page takes the authority to operate it"
        );
        assert_eq!(
            action.contract.execution,
            ui::tilo_actions::ExecutionMode::Background { cancellable: true },
            "{id} may navigate or hold its tab, so its caller can always stop it"
        );
    }
    let (_, blank, _, _) = page_on("about:blank");
    assert!(
        !page_action(&blank, "read-page")
            .contract
            .availability
            .is_available()
    );
}

// Phase `read_returns_the_engine_outline_in_the_declared_shape`.
#[test]
fn read_returns_the_engine_outline_in_the_declared_shape() {
    let (engine, mut widget, mut doc, root) = page_on("https://shop.test/");
    let invocation = start(
        &mut widget,
        &mut doc,
        &root,
        "read-page",
        &serde_json::json!({}),
    )
    .unwrap();
    assert_eq!(
        page_requests(&engine),
        vec![PageRequest::Outline {
            from: None,
            frame: None,
        }]
    );
    widget.sync(&doc, &root);
    let Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
        widget.poll_invocation(invocation)
    else {
        panic!("an answered read must succeed its invocation");
    };
    let value = output.value.unwrap();
    assert_eq!(value["url"], "https://shop.test/");
    assert_eq!(
        value["outline"],
        "- document \"https://shop.test/ — test page\""
    );
}

// Phase `reader_view_toggles_through_the_page_and_ends_with_its_document`.
#[test]
fn reader_view_toggles_through_the_page_and_ends_with_its_document() {
    let (engine, mut widget, mut doc, root) = page_on("https://news.test/story");
    let toggle = |widget: &mut BrowserWidget, doc: &mut AutoCommit| {
        let invocation = start(widget, doc, &root, "reader-view", &serde_json::json!({})).unwrap();
        widget.sync(doc, &root);
        assert!(matches!(
            widget.poll_invocation(invocation),
            Some(ui::tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
    };
    toggle(&mut widget, &mut doc);
    assert_eq!(
        page_requests(&engine),
        vec![PageRequest::Reader { enabled: true }]
    );
    assert!(widget.reader_view);
    assert_eq!(
        page_action(&widget, "reader-view").label,
        "Close reader view"
    );
    toggle(&mut widget, &mut doc);
    assert_eq!(
        page_requests(&engine).last(),
        Some(&PageRequest::Reader { enabled: false })
    );
    assert!(!widget.reader_view);
    toggle(&mut widget, &mut doc);
    assert!(widget.reader_view);
    widget
        .perform_browser_operation(BrowserOperation::Navigate {
            url: "https://news.test/next".into(),
        })
        .unwrap();
    assert!(!widget.reader_view, "the next page opens as itself");
    let (_, blank, _, _) = page_on("about:blank");
    assert!(
        !page_action(&blank, "reader-view")
            .contract
            .availability
            .is_available()
    );
}

// Phase `a_read_on_a_loading_page_waits_for_it_to_settle`.
#[test]
fn a_read_on_a_loading_page_waits_for_it_to_settle() {
    let (engine, mut widget, mut doc, root) = page_on("https://shop.test/");
    widget.set_loading_progress(0.4);
    let invocation = start(
        &mut widget,
        &mut doc,
        &root,
        "read-page",
        &serde_json::json!({}),
    )
    .unwrap();
    widget.sync(&doc, &root);
    assert!(
        page_requests(&engine).is_empty(),
        "read before the load settled"
    );
    assert!(widget.poll_invocation(invocation).is_none());
    engine.send(WebViewCommand::Reload);
    widget.sync(&doc, &root);
    assert_eq!(
        page_requests(&engine),
        vec![PageRequest::Outline {
            from: None,
            frame: None,
        }]
    );
    widget.sync(&doc, &root);
    assert!(matches!(
        widget.poll_invocation(invocation),
        Some(ui::tilo_actions::InvocationUpdate::Succeeded { .. })
    ));
}

// Phase `typed_arguments_reach_the_engine_and_bad_refs_never_do`.
#[test]
fn typed_arguments_reach_the_engine_and_bad_refs_never_do() {
    let (engine, mut widget, mut doc, root) = page_on("https://shop.test/");
    let fill = start(
        &mut widget,
        &mut doc,
        &root,
        "fill-page",
        &serde_json::json!({"element": "d3e7", "text": ""}),
    )
    .unwrap();
    widget.sync(&doc, &root);
    assert!(widget.poll_invocation(fill).is_some());
    let select = start(
        &mut widget,
        &mut doc,
        &root,
        "select-page",
        &serde_json::json!({"element": "d3e8", "values": ["NL", "Belgium"]}),
    )
    .unwrap();
    widget.sync(&doc, &root);
    assert!(widget.poll_invocation(select).is_some());
    let press = start(
        &mut widget,
        &mut doc,
        &root,
        "press-page",
        &serde_json::json!({"key": "Shift+Tab"}),
    )
    .unwrap();
    widget.sync(&doc, &root);
    assert!(widget.poll_invocation(press).is_some());
    assert_eq!(
        page_requests(&engine),
        vec![
            PageRequest::Fill {
                element: "d3e7".into(),
                text: String::new(),
                secret: false,
                frame: None,
            },
            PageRequest::Select {
                element: "d3e8".into(),
                values: vec!["NL".into(), "Belgium".into()],
                frame: None,
            },
            PageRequest::Press {
                element: None,
                key: PageKey::parse("Shift+Tab").unwrap(),
                frame: None,
            },
        ]
    );
    for (id, args) in [
        ("click-page", serde_json::json!({"element": "#checkout"})),
        ("submit-page", serde_json::json!({"element": ""})),
        ("press-page", serde_json::json!({"key": "Hyper+x"})),
    ] {
        let refused = start(&mut widget, &mut doc, &root, id, &args).unwrap_err();
        assert!(!refused.is_empty(), "{id}");
    }
    assert_eq!(
        page_requests(&engine).len(),
        3,
        "refused requests reached the page"
    );
}

// Phase `a_page_file_is_a_checked_absolute_regular_file`.
#[test]
fn a_page_file_is_a_checked_absolute_regular_file() {
    let (engine, mut widget, mut doc, root) = page_on("https://shop.test/claim");
    let folder = std::env::temp_dir().join(format!("tilo-page-files-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let file = folder.join("proof of address.pdf");
    std::fs::write(&file, b"%PDF-1.4").unwrap();
    let shared = file.to_string_lossy().into_owned();
    let chosen = start(
        &mut widget,
        &mut doc,
        &root,
        "fill-page-files",
        &serde_json::json!({"element": "d4e1", "paths": [&shared]}),
    )
    .unwrap();
    widget.sync(&doc, &root);
    assert!(widget.poll_invocation(chosen).is_some());
    assert_eq!(
        page_requests(&engine),
        vec![PageRequest::FillFiles {
            element: "d4e1".into(),
            paths: vec![shared],
            frame: None,
        }],
        "a checked file reaches the page as the path it was named by"
    );
    // What a person's picker could not have produced is refused here,
    // naming the path, so the page never sees a guess.
    for (paths, why) in [
        (vec!["proof of address.pdf".to_string()], "relative"),
        (vec![folder.to_string_lossy().into_owned()], "folder"),
        (
            vec![folder.join("missing.pdf").to_string_lossy().into_owned()],
            "missing",
        ),
    ] {
        let refused = start(
            &mut widget,
            &mut doc,
            &root,
            "fill-page-files",
            &serde_json::json!({"element": "d4e1", "paths": paths}),
        )
        .unwrap_err();
        assert!(refused.contains(&paths[0]), "{why}: {refused}");
    }
    assert_eq!(
        page_requests(&engine).len(),
        1,
        "a refused file reached the page"
    );
    std::fs::remove_dir_all(&folder).unwrap();
}

// Phase `agent_reads_and_consented_writes_reach_the_engine_typed`.
#[test]
fn agent_reads_and_consented_writes_reach_the_engine_typed() {
    let (engine, mut widget, mut doc, root) = page_on("https://shop.test/");
    let secret = page_action(&widget, "fill-page-secret");
    assert!(
        secret
            .params()
            .iter()
            .any(|param| param.name == "text" && param.ty == ui::tilo_actions::ParamType::Secret),
        "a password is a secret parameter, kept out of logs and history"
    );
    let mut answers = Vec::new();
    for (id, args) in [
        ("read-page", serde_json::json!({"frame": "1"})),
        ("read-page-changes", serde_json::json!({})),
        (
            "read-page-element-at",
            serde_json::json!({"x": 12.5, "y": 40}),
        ),
        ("wait-page-settled", serde_json::json!({})),
        (
            "wait-page-settled",
            serde_json::json!({"quiet_ms": 200, "timeout_ms": 3000}),
        ),
        ("read-page-console", serde_json::json!({"since": 4})),
        ("read-page-network", serde_json::json!({})),
        (
            "fill-page-secret",
            serde_json::json!({"element": "d1e3", "text": "hunter2"}),
        ),
        ("submit-page", serde_json::json!({"element": "d1e9"})),
        (
            "submit-page",
            serde_json::json!({"element": "d1e9", "cross_origin": true}),
        ),
        (
            "fill-page",
            serde_json::json!({"element": "d2e4", "text": "42 Linden Street",
                        "frame": "1.2"}),
        ),
        (
            "find-page-element",
            serde_json::json!({"role": "button", "name": "Remove", "nth": 2}),
        ),
    ] {
        let invocation = start(&mut widget, &mut doc, &root, id, &args).unwrap();
        widget.sync(&doc, &root);
        let Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
            widget.poll_invocation(invocation)
        else {
            panic!("{id} must succeed its invocation");
        };
        answers.push(output.value.unwrap());
    }
    assert_eq!(
        page_requests(&engine),
        vec![
            PageRequest::Outline {
                from: None,
                frame: Some(FrameId::parse("1").unwrap()),
            },
            PageRequest::Changes { frame: None },
            PageRequest::At {
                x: 12.5,
                y: 40.0,
                frame: None,
            },
            PageRequest::Settle {
                quiet_ms: 500,
                timeout_ms: 10_000,
                frame: None,
            },
            PageRequest::Settle {
                quiet_ms: 200,
                timeout_ms: 3000,
                frame: None,
            },
            PageRequest::Console { since: 4 },
            PageRequest::Network { since: 0 },
            PageRequest::Fill {
                element: "d1e3".into(),
                text: "hunter2".into(),
                secret: true,
                frame: None,
            },
            PageRequest::Submit {
                element: "d1e9".into(),
                cross_origin: false,
                frame: None,
            },
            PageRequest::Submit {
                element: "d1e9".into(),
                cross_origin: true,
                frame: None,
            },
            PageRequest::Fill {
                element: "d2e4".into(),
                text: "42 Linden Street".into(),
                secret: false,
                frame: Some(FrameId::parse("1.2").unwrap()),
            },
            // A description reaches the engine as it was asked, and the
            // ref it answers is output another step can bind.
            PageRequest::Find {
                role: Some("button".into()),
                name: Some("Remove".into()),
                nth: Some(2),
                frame: None,
            },
        ],
        "a form leaves for another site only when the caller says so"
    );
    // A frame id the outline could not have printed is refused by the
    // action's own text, before it reaches the widget or the page.
    let refused = start(
        &mut widget,
        &mut doc,
        &root,
        "read-page",
        &serde_json::json!({"frame": "#fr"}),
    )
    .unwrap_err();
    assert!(refused.contains("frame"), "{refused}");
    for value in &answers {
        assert_eq!(value["provenance"], "web-content", "{value}");
    }
    assert!(
        answers[1]["outline"].is_string(),
        "changes answer outline lines"
    );
    assert_eq!(answers[2]["data"]["x"], 12.5);
    assert_eq!(answers[3]["data"]["settled"], true);
    assert_eq!(answers[5]["data"]["entries"], serde_json::json!([]));
    assert_eq!(answers[6]["data"]["next"], 0);
    // A find answers the ref of what it named, at the top of the answer
    // where a later step binds it, and the line it matched inside it.
    assert_eq!(
        (
            answers[11]["element"].as_str(),
            answers[11]["role"].as_str()
        ),
        (Some("d1e1"), Some("button")),
    );
    assert_eq!(answers[11]["data"]["element"], "d1e1");
}

// Phase `a_page_picture_is_the_actions_own_content`: what a page shows
// only as pixels reaches the reader who can see it, and nothing else
// carries it.
#[test]
fn a_page_picture_is_the_actions_own_content() {
    let (engine, mut widget, mut doc, root) = page_on("https://letters.test/scan");
    let content = ui::content::ContentStore::memory();
    widget.attach_content(content.clone());
    engine.set_page_capture(SCANNED_LETTER_PNG);

    // The picture is the whole of a viewport capture: no ref, no width,
    // so the engine decides both.
    let invocation = start(
        &mut widget,
        &mut doc,
        &root,
        "capture-page",
        &serde_json::json!({}),
    )
    .unwrap();
    widget.sync(&doc, &root);
    let Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
        widget.poll_invocation(invocation)
    else {
        panic!("a picture of the page succeeds its invocation");
    };
    assert_eq!(
        page_requests(&engine),
        vec![PageRequest::Capture {
            element: None,
            max_width: None,
            frame: None,
        }],
        "the page is asked for a picture of itself, nothing else"
    );
    // The picture travels as content, never as a finding: a reader that
    // cannot see still reads the page's words, one that can sees this.
    assert_eq!(output.resources.len(), 1);
    let hash = ui::tilo_actions::content_hash(&output.resources[0])
        .expect("a picture is content the answer names");
    assert_eq!(content.read(hash).as_deref(), Some(SCANNED_LETTER_PNG));
    let value = output.value.expect("the answer says what it pictured");
    assert_eq!(value["provenance"], "web-content");
    assert_eq!(value["url"], "https://letters.test/scan");
    assert!(
        value["data"].get("capture").is_none(),
        "the pixels are the answer's content, not a base64 finding beside it"
    );

    // An element's own box, at the width the caller asked for.
    let invocation = start(
        &mut widget,
        &mut doc,
        &root,
        "capture-page",
        &serde_json::json!({"element": "d1e5", "max_width": 400}),
    )
    .unwrap();
    widget.sync(&doc, &root);
    let Some(ui::tilo_actions::InvocationUpdate::Succeeded { .. }) =
        widget.poll_invocation(invocation)
    else {
        panic!("a picture of an element succeeds its invocation");
    };
    assert_eq!(
        page_requests(&engine).last(),
        Some(&PageRequest::Capture {
            element: Some("d1e5".into()),
            max_width: Some(400),
            frame: None,
        })
    );

    // A ref the outline never printed is refused by the action's own
    // text, before the page is asked to paint anything.
    let before = page_requests(&engine).len();
    let refused = start(
        &mut widget,
        &mut doc,
        &root,
        "capture-page",
        &serde_json::json!({"element": "not-a-ref"}),
    )
    .unwrap_err();
    assert!(refused.contains("element"), "{refused}");
    assert_eq!(page_requests(&engine).len(), before);

    // A width wider than any engine paints is refused rather than
    // silently cut: the caller asked for something that cannot exist.
    let refused = start(
        &mut widget,
        &mut doc,
        &root,
        "capture-page",
        &serde_json::json!({"max_width": 4000}),
    )
    .unwrap_err();
    assert!(
        refused.contains("arguments the page cannot take"),
        "a width no engine paints is refused before the page is asked: {refused}"
    );
    assert_eq!(page_requests(&engine).len(), before);

    // A window with nowhere to keep a picture says so instead of
    // answering as if the page had none.
    let (engine, mut widget, mut doc, root) = page_on("https://letters.test/scan");
    engine.set_page_capture(SCANNED_LETTER_PNG);
    let invocation = start(
        &mut widget,
        &mut doc,
        &root,
        "capture-page",
        &serde_json::json!({}),
    )
    .unwrap();
    widget.sync(&doc, &root);
    let Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
        widget.poll_invocation(invocation)
    else {
        panic!("the page is still read, even with nowhere to keep its picture");
    };
    assert!(output.resources.is_empty());
    assert!(output.value.unwrap()["picture"].is_string());
}

// Phase `an_action_that_navigates_completes_on_the_page_it_opened`.
#[test]
fn an_action_that_navigates_completes_on_the_page_it_opened() {
    let (engine, mut widget, mut doc, root) = page_on("https://shop.test/cart");
    engine.push_page_reply(Ok(PageReply {
        url: "https://shop.test/cart".into(),
        message: "Clicked button \"Checkout\"".into(),
        outline: None,
        data: None,
        image: None,
        navigation: ui::webview::PageNavigation::Started,
        loading: true,
        record: PageRecordNote::default(),
    }));
    let click = start(
        &mut widget,
        &mut doc,
        &root,
        "click-page",
        &serde_json::json!({"element": "d1e4"}),
    )
    .unwrap();
    widget.sync(&doc, &root);
    assert!(
        widget.poll_invocation(click).is_none(),
        "a click that started a navigation waits for it"
    );
    // A second page action is not refused while one runs: it waits its turn
    // behind the click, and reaches no page before the click's has opened.
    let read = start(
        &mut widget,
        &mut doc,
        &root,
        "read-page",
        &serde_json::json!({}),
    )
    .expect("a second page action waits its turn while one runs");
    assert_eq!(
        page_requests(&engine).len(),
        1,
        "the read waits behind the click"
    );
    assert!(widget.poll_invocation(read).is_none());
    engine.send(WebViewCommand::Navigate(
        "https://shop.test/checkout".into(),
    ));
    widget.sync(&doc, &root);
    let Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
        widget.poll_invocation(click)
    else {
        panic!("the settled navigation must complete the click");
    };
    let value = output.value.unwrap();
    assert_eq!(value["url"], "https://shop.test/checkout");
    assert_eq!(value["message"], "Clicked button \"Checkout\"");
    assert_eq!(value["navigation"], "started");
    // The read ran after the click, on the page the click opened.
    let value = succeeded(settled(&mut widget, &doc, &root, read));
    assert_eq!(value["url"], "https://shop.test/checkout");
    assert_eq!(
        value["outline"],
        "- document \"https://shop.test/checkout — test page\""
    );
    assert_eq!(
        page_requests(&engine),
        vec![
            PageRequest::Click {
                element: "d1e4".into(),
                frame: None,
            },
            PageRequest::Outline {
                from: None,
                frame: None,
            },
        ]
    );
}

// Phase `engine_refusals_become_typed_invocation_failures`.
#[test]
fn engine_refusals_become_typed_invocation_failures() {
    let (engine, mut widget, mut doc, root) = page_on("https://shop.test/");
    engine.push_page_reply(Err(PageFailure::new(
        "stale_element",
        "d1e4 belongs to a page this tab has left; read the page again",
    )));
    let click = start(
        &mut widget,
        &mut doc,
        &root,
        "click-page",
        &serde_json::json!({"element": "d1e4"}),
    )
    .unwrap();
    widget.sync(&doc, &root);
    let Some(ui::tilo_actions::InvocationUpdate::Failed { error, .. }) =
        widget.poll_invocation(click)
    else {
        panic!("a refused click must fail its invocation");
    };
    assert_eq!(error.code, "browser.page.stale_element");
    assert!(!error.retryable);
    assert!(
        start(
            &mut widget,
            &mut doc,
            &root,
            "read-page",
            &serde_json::json!({})
        )
        .is_ok()
    );
}

// Phase `a_page_action_answers_at_most_once_so_its_failure_asks_for_none`.
#[test]
fn a_page_action_answers_at_most_once_so_its_failure_asks_for_none() {
    // Every page action runs once at the page: the wheel it turned, the
    // click it pressed or the text it typed cannot be undone by running it
    // again. A failure that invited a retry would break that promise, so
    // even the refusals a later try could get past are finals: the caller
    // asks again itself, with the page it read by then.
    let scroll = page_action_contract("scroll-page");
    assert_eq!(
        scroll.retry,
        ui::tilo_actions::RetryPolicy::AtMostOnce,
        "a scroll turns the wheel once"
    );
    let (engine, mut widget, mut doc, root) = page_on("https://shop.test/");
    for (code, message) in [
        ("busy", "Another page action is still running in this tab"),
        ("timeout", "The page did not answer in time."),
    ] {
        engine.push_page_reply(Err(PageFailure::new(code, message)));
        let scroll = start(
            &mut widget,
            &mut doc,
            &root,
            "scroll-page",
            &serde_json::json!({"to": "bottom"}),
        )
        .unwrap();
        widget.sync(&doc, &root);
        let Some(ui::tilo_actions::InvocationUpdate::Failed { error, .. }) =
            widget.poll_invocation(scroll)
        else {
            panic!("a refused scroll must fail its invocation");
        };
        assert_eq!(error.code, format!("browser.page.{code}"));
        assert!(
            !error.retryable,
            "{code} must not advertise a retry of an action that already ran"
        );
    }
}

#[test]
fn a_settled_page_result_nobody_waits_for_does_not_hold_the_tab() {
    let mut widget = BrowserWidget::new_headless();
    widget.current_uri = "https://site.test/form".into();
    let abandoned = ui::tilo_actions::InvocationId::new();
    widget
        .start_page_operation(
            abandoned,
            None,
            PageRequest::Outline {
                from: None,
                frame: None,
            },
        )
        .unwrap();
    // Its page answered after the caller was answered `accepted`.
    widget.page_operation = None;
    widget.page_operation_result = Some(ui::tilo_actions::InvocationUpdate::Succeeded {
        invocation: abandoned,
        output: ui::tilo_actions::ActionOutput::default(),
    });
    assert!(
        !widget.reply_outstanding(abandoned),
        "nothing waits on the abandoned invocation"
    );

    // The next action in this tab runs rather than being refused.
    let next = ui::tilo_actions::InvocationId::new();
    widget
        .start_page_operation(
            next,
            None,
            PageRequest::Outline {
                from: None,
                frame: None,
            },
        )
        .expect("an unwatched result must not refuse the next page action");
    assert!(
        widget
            .page_operation
            .as_ref()
            .is_some_and(|operation| operation.invocation == next),
        "the next action took the slot"
    );

    // The decision is the reply set, never the age of the result: a settled
    // result whose invocation a reply still waits on stays for that reply.
    assert!(!widget.reply_outstanding(next));
    widget.page_operation = None;
    widget.page_operation_result = Some(ui::tilo_actions::InvocationUpdate::Succeeded {
        invocation: next,
        output: ui::tilo_actions::ActionOutput::default(),
    });
    widget.abandon_unwatched_page_result();
    assert!(
        widget.page_operation_result.is_none(),
        "a settled result no reply waits on is let go"
    );

    // An operation still running is left alone: it settles at its own
    // deadline, and the next action waits its turn behind it.
    let running = ui::tilo_actions::InvocationId::new();
    widget
        .start_page_operation(
            running,
            None,
            PageRequest::Outline {
                from: None,
                frame: None,
            },
        )
        .unwrap();
    let behind = ui::tilo_actions::InvocationId::new();
    widget
        .start_page_operation(
            behind,
            None,
            PageRequest::Outline {
                from: None,
                frame: None,
            },
        )
        .expect("a second page action waits its turn while one runs");
    assert!(
        widget
            .page_operation
            .as_ref()
            .is_some_and(|operation| operation.invocation == running),
        "the running action keeps the slot"
    );
    assert_eq!(
        widget
            .page_waiting
            .iter()
            .map(|waiting| waiting.invocation)
            .collect::<Vec<_>>(),
        vec![behind],
        "the second waits behind it"
    );
}

#[test]
fn a_page_action_either_finishes_or_says_what_became_of_it() {
    // Giving up on a page action frees the tab's slot and says that what the
    // page did is unknown, whether the caller stopped waiting for it or the
    // action ran out of time: neither claims the page acted or did not.
    let mut widget = BrowserWidget::new_headless();
    widget.current_uri = "https://site.test/form".into();
    let cancelled = ui::tilo_actions::InvocationId::new();
    widget
        .start_page_operation(
            cancelled,
            None,
            PageRequest::Outline {
                from: None,
                frame: None,
            },
        )
        .unwrap();
    let answer = widget
        .answer_a_cancelled_page_operation(cancelled, true)
        .expect("a running page action is stopped when its caller gives up");
    let ui::tilo_actions::InvocationUpdate::Failed { error, .. } = answer else {
        panic!("a stopped page action answers as a failure");
    };
    assert_eq!(error.code, "browser.page.cancelled");
    assert!(error.message.contains("unknown"), "{}", error.message);
    assert!(widget.page_operation.is_none(), "the slot is freed");

    // An invocation that runs nothing here is not answered twice.
    assert!(
        widget
            .answer_a_cancelled_page_operation(cancelled, true)
            .is_none(),
        "nothing is left to stop"
    );

    // Running out of time says the same about what the page did.
    let slow = ui::tilo_actions::InvocationId::new();
    widget
        .start_page_operation(
            slow,
            None,
            PageRequest::Outline {
                from: None,
                frame: None,
            },
        )
        .unwrap();
    let operation = widget.page_operation.take().unwrap();
    let ui::tilo_actions::InvocationUpdate::Failed { error, .. } =
        BrowserWidget::answer_a_timed_out_page_operation(&operation)
    else {
        panic!("a page action that ran out of time answers as a failure");
    };
    assert_eq!(error.code, "browser.page.timeout");
    assert!(error.message.contains("unknown"), "{}", error.message);
}

/// What an agent's run answers a page action with before anyone chose, as
/// the window publishes it: allowed, or asks first.
fn agent_default(widget: &BrowserWidget, id: &str) -> ui::permissions::policy::Answer {
    let action = widget
        .published()
        .children
        .iter()
        .find(|resource| resource.id.0 == "browser.page")
        .and_then(|page| page.actions.iter().find(|action| action.id == id).cloned())
        .unwrap_or_else(|| panic!("{id} is published on the page"));
    ui::permissions::policy::agent_answers::default_answer(
        action.contract.need().kind,
        action.contract.requires_confirmation(),
    )
}

/// The page of a window in `mode`, at `url`.
fn page_in(mode: BrowsingMode, url: &str) -> BrowserWidget {
    let engine = TestEngine::new();
    engine.send(WebViewCommand::Navigate(url.into()));
    let mut widget = BrowserWidget::with_engine_mode(engine, true, mode);
    let _ = synced(&mut widget);
    widget
}

/// Reading a page takes what the window holds. A window opened for an agent
/// holds none of the person's sign-ins: what only looks at its page takes
/// no more than any visitor has, and the agent reads it without asking,
/// while what acts on the page reaches the web and asks first. The person's
/// own window holds their sign-ins: reading its page takes the authority to
/// operate it, so every page action asks first, as it always has.
#[test]
fn a_page_read_asks_first_only_where_the_window_holds_the_persons_sign_ins() {
    use ui::permissions::policy::Answer::{Allow, Ask};
    const OBSERVE: [&str; 12] = [
        "read-page",
        "capture-page",
        "find-page-element",
        "read-page-changes",
        "read-page-element-at",
        "wait-page-settled",
        "read-page-console",
        "read-page-network",
        "read-page-record",
        "read-page-fields",
        "find-page-text",
        "read-page-forms",
    ];
    const ACT: [&str; 13] = [
        "click-page",
        "fill-page",
        "select-page",
        "fill-page-files",
        "press-page",
        "submit-page",
        "fill-page-secret",
        "hover-page",
        "drag-page",
        "pointer-path",
        "pointer-page",
        "scroll-page",
        "fill-page-form",
    ];
    let visitor = page_in(BrowsingMode::Visitor, "https://github.com/ada");
    for id in OBSERVE {
        assert_eq!(
            agent_default(&visitor, id),
            Allow,
            "{id} on a visitor's page"
        );
    }
    for id in ACT {
        assert_eq!(agent_default(&visitor, id), Ask, "{id} on a visitor's page");
    }
    let signed_in = page_in(BrowsingMode::Normal, "https://github.com/ada");
    for id in OBSERVE.into_iter().chain(ACT) {
        assert_eq!(
            agent_default(&signed_in, id),
            Ask,
            "{id} on a page with the person's sign-ins"
        );
    }
}

/// A secret filled into a visitor's page is the person's credential: from
/// then on the page holds a sign-in. Its reads ask first like every other
/// page action, the bar no longer says it is not signed in, and nothing
/// the window does later takes that back.
#[test]
fn a_secret_filled_into_a_visitors_page_makes_it_signed_in_for_good() {
    use ui::permissions::policy::Answer::{Allow, Ask};
    let mut visitor = page_in(BrowsingMode::Visitor, "https://github.com/login");
    assert_eq!(agent_default(&visitor, "read-page"), Allow);
    assert_eq!(
        visitor.window_toolbar().leading.len(),
        1,
        "it says it is not signed in"
    );

    let invocation = ui::tilo_actions::InvocationId::new();
    let fill = crate::effects::BrowserEffect::FillPageSecret {
        element: "d1e5".into(),
        text: "hunter2".into(),
        frame: None,
    };
    let _ = visitor.admit(invocation, fill.operation());
    for id in [
        "read-page",
        "capture-page",
        "find-page-element",
        "click-page",
    ] {
        assert_eq!(
            agent_default(&visitor, id),
            Ask,
            "{id} once a secret was entered"
        );
    }
    assert!(
        visitor.window_toolbar().leading.is_empty(),
        "the bar no longer says it is not signed in"
    );

    // Whatever comes next, the page stays signed in: another page, a
    // reload, a key that is no password.
    visitor
        .engine
        .send(WebViewCommand::Navigate("https://example.org/".into()));
    let _ = synced(&mut visitor);
    assert_eq!(agent_default(&visitor, "read-page"), Ask);
}

/// Run the published page action `id` with `args` on `widget` as an agent's
/// invocation does, and let the engine's answer settle it.
fn run_page_action(widget: &mut BrowserWidget, id: &str, args: &serde_json::Value) {
    let action = widget
        .published()
        .children
        .iter()
        .find(|resource| resource.id.0 == "browser.page")
        .and_then(|page| page.actions.iter().find(|action| action.id == id).cloned())
        .unwrap_or_else(|| panic!("{id} is published on the page"));
    let encoded = action
        .build_json(args.as_object().expect("arguments are an object"))
        .expect("the arguments fit the action");
    widget
        .admit(
            ui::tilo_actions::InvocationId::new(),
            effect_operation(&encoded.payload),
        )
        .expect("the page action is accepted");
    for _ in 0..3 {
        let _ = synced(widget);
    }
    assert!(widget.page_operation.is_none(), "{id} settled");
}

/// What makes a credential is the field it goes into, not the action that
/// put it there: an agent's fill or keys into a password field sign the
/// visitor's page in as a secret does, and the same into any other field
/// does not.
#[test]
fn an_agent_typing_into_a_password_field_signs_a_visitors_page_in() {
    use ui::permissions::policy::Answer::{Allow, Ask};
    let field = |purpose| {
        Some(ui::webview::TextField {
            purpose,
            multiline: false,
            bounds: None,
            composition: None,
        })
    };
    for (id, args) in [
        (
            "press-page",
            serde_json::json!({"key": "a", "element": "d1e5"}),
        ),
        (
            "fill-page",
            serde_json::json!({"element": "d1e5", "text": "hunter2"}),
        ),
    ] {
        let engine = TestEngine::new();
        engine.send(WebViewCommand::Navigate("https://github.com/login".into()));
        let mut visitor =
            BrowserWidget::with_engine_mode(engine.clone(), true, BrowsingMode::Visitor);
        let _ = synced(&mut visitor);

        engine.set_text_field(field(ui::ime::InputPurpose::Email));
        run_page_action(&mut visitor, id, &args);
        assert_eq!(
            agent_default(&visitor, "read-page"),
            Allow,
            "{id} into an email field"
        );

        engine.set_text_field(field(ui::ime::InputPurpose::Password));
        run_page_action(&mut visitor, id, &args);
        assert_eq!(
            agent_default(&visitor, "read-page"),
            Ask,
            "{id} into a password field"
        );
        assert!(visitor.window_toolbar().leading.is_empty(), "{id}");

        engine.set_text_field(None);
        run_page_action(&mut visitor, id, &args);
        assert_eq!(
            agent_default(&visitor, "read-page"),
            Ask,
            "{id}: and it stays signed in"
        );
    }
}

/// An agent the action door admitted, by its principal.
fn agent(principal: &str) -> ActionInvoker {
    ActionInvoker::admitted(&ui::tilo_actions::InteractionActor::new(
        principal,
        ui::tilo_actions::ActorKind::Agent,
    ))
}

/// The engine is told who asks: the principal the action door admitted the
/// invocation for, and nobody for an invocation without one.
#[test]
fn the_admitted_invoker_is_who_asks_the_engine() {
    let (engine, mut widget, doc, root) = page_on("https://shop.test/");
    let ada = agent("agent:ada");
    let read = start_as(&mut widget, Some(&ada), "read-page", &serde_json::json!({})).unwrap();
    succeeded(settled(&mut widget, &doc, &root, read));
    // The person at the window is admitted by no door: the tab's one
    // unnamed actor, as every caller was before a tab had several.
    let own = start_as(&mut widget, None, "read-page", &serde_json::json!({})).unwrap();
    succeeded(settled(&mut widget, &doc, &root, own));
    // An agent acting for a person asks under its own principal, never the
    // person's: what it did is set down as its own.
    let for_ada = ActionInvoker::admitted(
        &ui::tilo_actions::InteractionActor::new(
            "agent:task-7",
            ui::tilo_actions::ActorKind::Agent,
        )
        .acting_for("person:ada"),
    );
    let click = start_as(
        &mut widget,
        Some(&for_ada),
        "click-page",
        &serde_json::json!({"element": "d1e4"}),
    )
    .unwrap();
    succeeded(settled(&mut widget, &doc, &root, click));
    // A principal longer than the engine takes is shortened the same way
    // every time, so one actor keeps one name.
    let long = agent(&format!("agent:{}", "x".repeat(120)));
    for _ in 0..2 {
        let read = start_as(
            &mut widget,
            Some(&long),
            "read-page",
            &serde_json::json!({}),
        )
        .unwrap();
        succeeded(settled(&mut widget, &doc, &root, read));
    }
    // An invoker with no principal is nobody, not an actor called "".
    let nameless = ActionInvoker::default();
    let read = start_as(
        &mut widget,
        Some(&nameless),
        "read-page",
        &serde_json::json!({}),
    )
    .unwrap();
    succeeded(settled(&mut widget, &doc, &root, read));

    let asks = page_asks(&engine);
    assert_eq!(
        asks[..3],
        [
            (Some("agent:ada".to_owned()), "outline"),
            (None, "outline"),
            (Some("agent:task-7".to_owned()), "click"),
        ]
    );
    let shortened = asks[3]
        .0
        .clone()
        .expect("a long principal still names its actor");
    assert_eq!(shortened.encode_utf16().count(), 64);
    assert!(shortened.starts_with("agent:xxxx"), "{shortened}");
    assert_eq!(asks[4].0.as_ref(), Some(&shortened));
    assert_eq!(asks[5], (None, "outline"));
}

/// Several actors act in one tab: an action admitted while another runs is
/// not refused. It waits, runs in the order it came, and answers the
/// invocation that asked for it.
#[test]
fn a_second_actors_action_waits_its_turn_and_answers_its_own_invocation() {
    let (engine, mut widget, doc, root) = page_on("https://shop.test/");
    let (ada, ben, cy) = (agent("agent:ada"), agent("agent:ben"), agent("agent:cy"));
    engine.push_page_reply(Ok(answer("Filled textbox \"Email\"")));
    engine.push_page_reply(Ok(answer("Clicked button \"Send\"")));
    engine.push_page_reply(Ok(answer("Chose Belgium")));
    let fill = start_as(
        &mut widget,
        Some(&ada),
        "fill-page",
        &serde_json::json!({"element": "d1e2", "text": "ada@example.org"}),
    )
    .unwrap();
    let click = start_as(
        &mut widget,
        Some(&ben),
        "click-page",
        &serde_json::json!({"element": "d1e9"}),
    )
    .expect("a second actor's action waits its turn");
    let select = start_as(
        &mut widget,
        Some(&cy),
        "select-page",
        &serde_json::json!({"element": "d1e5", "values": ["Belgium"]}),
    )
    .expect("and a third behind it");
    // One request is with the engine; the others have reached nothing.
    assert_eq!(
        page_asks(&engine),
        vec![(Some("agent:ada".to_owned()), "fill")]
    );
    assert!(widget.poll_invocation(click).is_none());
    assert!(widget.poll_invocation(select).is_none());

    // Each is answered as its own, in the order it came. The third is read
    // first here: its answer is still its own, and it still ran last.
    let filled = succeeded(settled(&mut widget, &doc, &root, fill));
    assert_eq!(filled["message"], "Filled textbox \"Email\"");
    assert_eq!(
        page_asks(&engine).len(),
        1,
        "the next waits until this answer was read"
    );
    widget.sync(&doc, &root);
    assert!(
        widget.poll_invocation(select).is_none(),
        "the third does not overtake the second"
    );
    let clicked = succeeded(settled(&mut widget, &doc, &root, click));
    assert_eq!(clicked["message"], "Clicked button \"Send\"");
    let chosen = succeeded(settled(&mut widget, &doc, &root, select));
    assert_eq!(chosen["message"], "Chose Belgium");
    assert_eq!(
        page_asks(&engine),
        vec![
            (Some("agent:ada".to_owned()), "fill"),
            (Some("agent:ben".to_owned()), "click"),
            (Some("agent:cy".to_owned()), "select"),
        ]
    );
    assert!(widget.page_waiting.is_empty());
    assert!(page_cancels(&engine).is_empty());
}

/// A caller that gives up on an action still waiting its turn takes it out
/// before it ran: the engine never hears of it, it is known to have changed
/// nothing, and the action that is running goes on.
#[test]
fn a_waiting_action_whose_caller_gives_up_never_reaches_the_engine() {
    let (engine, mut widget, doc, root) = page_on("https://shop.test/");
    let (ada, ben) = (agent("agent:ada"), agent("agent:ben"));
    engine.push_page_reply(Ok(answer("Filled textbox \"Email\"")));
    let fill = start_as(
        &mut widget,
        Some(&ada),
        "fill-page",
        &serde_json::json!({"element": "d1e2", "text": "ada@example.org"}),
    )
    .unwrap();
    let click = start_as(
        &mut widget,
        Some(&ben),
        "click-page",
        &serde_json::json!({"element": "d1e9"}),
    )
    .unwrap();
    let stopped = failed(
        widget
            .poll_invocation_cancelling(click, true)
            .expect("a waiting action is answered when its caller gives up"),
    );
    assert_eq!(stopped.code, "browser.page.cancelled");
    assert!(
        stopped.message.contains("never ran and changed nothing"),
        "{}",
        stopped.message
    );
    assert!(!stopped.retryable);
    assert_eq!(stopped.details.unwrap()["outcome"], "not_run");
    assert!(
        widget.poll_invocation_cancelling(click, true).is_none(),
        "it is answered once"
    );
    // Ada's action was not the one given up on: it runs to its answer.
    let filled = succeeded(settled(&mut widget, &doc, &root, fill));
    assert_eq!(filled["message"], "Filled textbox \"Email\"");
    for _ in 0..3 {
        widget.sync(&doc, &root);
    }
    assert_eq!(
        page_asks(&engine),
        vec![(Some("agent:ada".to_owned()), "fill")],
        "the cancelled click never reached the engine"
    );
    assert!(
        page_cancels(&engine).is_empty(),
        "and the engine was told to stop nothing"
    );
}

/// A caller that gives up on the action that is running stops that request
/// by name, so no other actor's action is stopped with it, and the action
/// that waited behind it takes its turn.
#[test]
fn stopping_the_running_action_names_its_request_and_the_next_takes_its_turn() {
    let (engine, mut widget, doc, root) = page_on("https://shop.test/");
    let (ada, ben) = (agent("agent:ada"), agent("agent:ben"));
    let fill = start_as(
        &mut widget,
        Some(&ada),
        "fill-page",
        &serde_json::json!({"element": "d1e2", "text": "ada@example.org"}),
    )
    .unwrap();
    let read = start_as(&mut widget, Some(&ben), "read-page", &serde_json::json!({})).unwrap();
    let stopped = failed(
        widget
            .poll_invocation_cancelling(fill, true)
            .expect("the running action is stopped when its caller gives up"),
    );
    assert_eq!(stopped.code, "browser.page.cancelled");
    assert!(stopped.message.contains("unknown"), "{}", stopped.message);
    let asked = page_request_ids(&engine);
    assert_eq!(asked.len(), 1);
    assert_eq!(
        page_cancels(&engine),
        asked,
        "the cancel names the request it stops"
    );
    // Ben's read was not stopped: it runs next and answers as its own.
    let value = succeeded(settled(&mut widget, &doc, &root, read));
    assert_eq!(
        value["outline"],
        "- document \"https://shop.test/ — test page\""
    );
    assert_eq!(
        page_asks(&engine),
        vec![
            (Some("agent:ada".to_owned()), "fill"),
            (Some("agent:ben".to_owned()), "outline"),
        ]
    );
    assert_eq!(page_cancels(&engine).len(), 1, "nothing else was stopped");
    assert_ne!(page_request_ids(&engine)[1], page_cancels(&engine)[0]);
}

/// The wait is bounded in number: past it an action is turned away at once,
/// told how many wait and who holds the tab, rather than queued without end.
#[test]
fn a_tab_with_its_turns_taken_turns_the_next_action_away_saying_who_holds_it() {
    let (engine, mut widget, _doc, _root) = page_on("https://shop.test/");
    let (ada, ben) = (agent("agent:ada"), agent("agent:ben"));
    start_as(
        &mut widget,
        Some(&ada),
        "fill-page",
        &serde_json::json!({"element": "d1e2", "text": "ada@example.org"}),
    )
    .unwrap();
    for _ in 0..MAX_WAITING_PAGE_ACTIONS {
        start_as(&mut widget, Some(&ben), "read-page", &serde_json::json!({}))
            .expect("an action within the bound waits its turn");
    }
    let refused = start_as(&mut widget, Some(&ben), "read-page", &serde_json::json!({}))
        .expect_err("an action past the bound is turned away");
    assert!(
        refused.contains(&format!("{MAX_WAITING_PAGE_ACTIONS} page actions waiting")),
        "{refused}"
    );
    assert!(
        refused.contains("fill by \"agent:ada\""),
        "it says who holds the tab: {refused}"
    );
    assert_eq!(widget.page_waiting.len(), MAX_WAITING_PAGE_ACTIONS);
    assert_eq!(page_asks(&engine).len(), 1, "only the running one was sent");
}

/// The wait is bounded in time: an action that has waited as long as one may
/// is answered as never run, which is more than a timeout at the page can
/// say, and the actions around it keep their order.
#[test]
fn an_action_that_waited_too_long_is_answered_as_never_run() {
    let (engine, mut widget, doc, root) = page_on("https://shop.test/");
    let (ada, ben, cy) = (agent("agent:ada"), agent("agent:ben"), agent("agent:cy"));
    let fill = start_as(
        &mut widget,
        Some(&ada),
        "fill-page",
        &serde_json::json!({"element": "d1e2", "text": "ada@example.org"}),
    )
    .unwrap();
    let click = start_as(
        &mut widget,
        Some(&ben),
        "click-page",
        &serde_json::json!({"element": "d1e9"}),
    )
    .unwrap();
    let read = start_as(&mut widget, Some(&cy), "read-page", &serde_json::json!({})).unwrap();
    assert!(
        widget.poll_invocation(click).is_none(),
        "within its wait it waits"
    );
    // Ben's click has waited as long as one may.
    let waited = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_millis(1))
        .unwrap();
    widget.page_waiting[0].until = waited;
    let busy = failed(
        widget
            .poll_invocation(click)
            .expect("an action that waited too long is answered"),
    );
    assert_eq!(busy.code, "browser.page.busy");
    assert!(!busy.retryable, "a page action answers at most once");
    assert!(
        busy.message.contains("never ran") && busy.message.contains("changed nothing"),
        "{}",
        busy.message
    );
    assert!(
        busy.message
            .contains(&format!("{} seconds", PAGE_TURN_WAIT.as_secs()))
            && busy.message.contains("fill by \"agent:ada\""),
        "it says how long it waited and behind whom: {}",
        busy.message
    );
    assert_eq!(busy.details.unwrap()["outcome"], "not_run");

    // One nobody asked about again is answered the same when its turn comes,
    // and never runs late.
    let late = start_as(
        &mut widget,
        Some(&ben),
        "click-page",
        &serde_json::json!({"element": "d1e9"}),
    )
    .unwrap();
    widget.page_waiting[1].until = waited;
    succeeded(settled(&mut widget, &doc, &root, fill));
    succeeded(settled(&mut widget, &doc, &root, read));
    let busy = failed(settled(&mut widget, &doc, &root, late));
    assert_eq!(busy.code, "browser.page.busy");
    assert_eq!(
        page_asks(&engine),
        vec![
            (Some("agent:ada".to_owned()), "fill"),
            (Some("agent:cy".to_owned()), "outline"),
        ],
        "neither click reached the engine"
    );
}

/// The tab's record and the review of its fields are page reads like the
/// others: typed requests, answered in the shape every page read has.
#[test]
fn the_record_and_the_fields_are_read_typed_and_answer_in_the_declared_shape() {
    let (engine, mut widget, mut doc, root) = page_on("https://shop.test/form");
    let record = serde_json::json!({
        "entries": [
            {"seq": 1, "time_ms": 1_700_000_000_000_u64, "actor": "agent:ada", "action": "fill",
                "ok": true, "message": "Filled textbox \"Email\"",
                "url": "https://shop.test/form", "navigation": "possible",
                "target": {"role": "textbox", "name": "Email"}, "text": "ada@example.org"},
            {"seq": 2, "time_ms": 1_700_000_000_500_u64, "actor": "agent:ben",
                "action": "select", "ok": true, "url": "https://shop.test/form",
                "target": {"role": "combobox", "name": "Country"}, "values": ["Belgium"]},
        ],
        "next": 2, "more": false, "dropped": 0, "waiting": 0,
    });
    let review = serde_json::json!({"fields": [
        {"role": "textbox", "name": "Email", "value": "ada@example.org", "required": true,
            "set": {"how": "action", "by": "agent:ada", "seq": 1}},
        {"role": "searchbox", "name": "Search", "value": "toaster",
            "set": {"how": "action", "by": "agent:ada", "seq": 5, "changed": true}},
        {"role": "textbox", "name": "Password", "value": "<14 characters hidden>",
            "set": {"how": "input"}},
    ]});
    engine.push_page_reply(Ok(PageReply {
        data: Some(record.clone()),
        ..answer("2 actions")
    }));
    engine.push_page_reply(Ok(PageReply {
        data: Some(serde_json::json!({
            "entries": [], "next": 2, "more": false, "dropped": 0, "waiting": 0,
        })),
        ..answer("0 actions")
    }));
    engine.push_page_reply(Ok(PageReply {
        outline: Some(
            "- textbox \"Email\" = ada@example.org (set by \"agent:ada\", action 1)".into(),
        ),
        data: Some(review.clone()),
        ..answer("3 fields, 1 changed after they were set")
    }));
    let mut answers = Vec::new();
    for (id, args) in [
        ("read-page-record", serde_json::json!({})),
        ("read-page-record", serde_json::json!({"since": 2})),
        ("read-page-fields", serde_json::json!({})),
        (
            "read-page-fields",
            serde_json::json!({"frame": "[frame=1.2]"}),
        ),
    ] {
        let invocation = start(&mut widget, &mut doc, &root, id, &args).unwrap();
        answers.push(succeeded(settled(&mut widget, &doc, &root, invocation)));
    }
    assert_eq!(
        page_requests(&engine),
        vec![
            PageRequest::Ledger { since: 0 },
            PageRequest::Ledger { since: 2 },
            PageRequest::Fields { frame: None },
            PageRequest::Fields {
                frame: Some(FrameId::parse("1.2").unwrap()),
            },
        ]
    );
    for value in &answers {
        assert_eq!(value["provenance"], "web-content", "{value}");
        assert!(value["url"].is_string(), "{value}");
        assert!(value["message"].is_string() && value["title"].is_string());
        assert_eq!(value["navigation"], "none");
    }
    // The record is who did what, by the description that still names the
    // element later, with what was written.
    assert_eq!(answers[0]["message"], "2 actions");
    assert_eq!(answers[0]["data"], record);
    assert_eq!(answers[0]["data"]["entries"][0]["actor"], "agent:ada");
    assert_eq!(
        answers[0]["data"]["entries"][0]["target"],
        serde_json::json!({"role": "textbox", "name": "Email"})
    );
    assert_eq!(answers[1]["data"]["entries"], serde_json::json!([]));
    assert_eq!(answers[1]["data"]["next"], 2);
    // The review is every field with what it holds now and who set it.
    assert_eq!(answers[2]["data"], review);
    assert!(
        answers[2]["outline"]
            .as_str()
            .is_some_and(|outline| outline.contains("set by \"agent:ada\""))
    );
    assert_eq!(
        answers[2]["data"]["fields"][1]["set"],
        serde_json::json!({"how": "action", "by": "agent:ada", "seq": 5, "changed": true})
    );
    assert_eq!(answers[3]["data"]["fields"], serde_json::json!([]));
    // A frame the outline never printed is refused before the page is asked.
    let refused = start(
        &mut widget,
        &mut doc,
        &root,
        "read-page-fields",
        &serde_json::json!({"frame": "#fr"}),
    )
    .unwrap_err();
    assert!(refused.contains("frame"), "{refused}");
    assert_eq!(page_requests(&engine).len(), 4);
}

/// Reading the record takes no turn: it reads nothing of the page, so it is
/// with the engine at once whatever holds the tab, and whoever reviews a tab
/// is never queued behind the actors they are watching.
#[test]
fn reading_the_record_takes_no_turn() {
    let (engine, mut widget, doc, root) = page_on("https://shop.test/");
    let (ada, ben, rev) = (agent("agent:ada"), agent("agent:ben"), agent("person:rev"));
    let fill = start_as(
        &mut widget,
        Some(&ada),
        "fill-page",
        &serde_json::json!({"element": "d1e2", "text": "ada@example.org"}),
    )
    .unwrap();
    let click = start_as(
        &mut widget,
        Some(&ben),
        "click-page",
        &serde_json::json!({"element": "d1e9"}),
    )
    .unwrap();
    let read = start_as(&mut widget, Some(&ben), "read-page", &serde_json::json!({})).unwrap();
    let record = start_as(
        &mut widget,
        Some(&rev),
        "read-page-record",
        &serde_json::json!({"since": 7}),
    )
    .unwrap();
    assert_eq!(
        page_asks(&engine),
        vec![
            (Some("agent:ada".to_owned()), "fill"),
            (Some("person:rev".to_owned()), "ledger"),
        ],
        "the record is asked for while the fill still runs, ahead of what waits"
    );
    assert_eq!(
        widget.page_waiting.len(),
        2,
        "and it took no place among them"
    );
    // It is answered before the fill's answer was read, and says how many
    // actions wait their turn in this tab.
    widget.sync(&doc, &root);
    let value = succeeded(
        widget
            .poll_invocation(record)
            .expect("the record answers whatever runs"),
    );
    assert_eq!(value["data"]["next"], 7);
    assert_eq!(value["data"]["waiting"], 2);
    assert_eq!(value["provenance"], "web-content");
    // The actions it was read beside are untouched, and keep their order.
    succeeded(settled(&mut widget, &doc, &root, fill));
    succeeded(settled(&mut widget, &doc, &root, click));
    succeeded(settled(&mut widget, &doc, &root, read));
    assert_eq!(
        page_asks(&engine)
            .into_iter()
            .map(|(_, action)| action)
            .collect::<Vec<_>>(),
        vec!["fill", "ledger", "click", "outline"]
    );

    // A caller that gives up on a read of the record stops that request and
    // no other.
    let running = start_as(&mut widget, Some(&ada), "read-page", &serde_json::json!({})).unwrap();
    let given_up = start_as(
        &mut widget,
        Some(&rev),
        "read-page-record",
        &serde_json::json!({}),
    )
    .unwrap();
    let stopped = failed(
        widget
            .poll_invocation_cancelling(given_up, true)
            .expect("a read of the record is answered when its caller gives up"),
    );
    assert_eq!(stopped.code, "browser.page.cancelled");
    assert_eq!(stopped.details.unwrap()["outcome"], "not_run");
    let asked = page_request_ids(&engine);
    assert_eq!(page_cancels(&engine), vec![*asked.last().unwrap()]);
    succeeded(settled(&mut widget, &doc, &root, running));
    assert!(widget.poll_invocation(given_up).is_none(), "answered once");
}

/// An actor learns the tab moved under it from the answer it was waiting for
/// anyway: what the others did since it last heard travels in its output,
/// and with a refusal, where it is often the reason.
#[test]
fn what_the_others_did_reaches_the_callers_output() {
    let (engine, mut widget, doc, root) = page_on("https://shop.test/form");
    let ada = agent("agent:ada");
    let theirs = serde_json::json!([
        {"seq": 4, "actor": "agent:ben", "action": "fill", "ok": true,
            "target": {"role": "textbox", "name": "Email"}, "text": "ben@example.org"},
        {"seq": 5, "actor": "agent:ben", "action": "click", "ok": true,
            "target": {"role": "button", "name": "Next"},
            "url": "https://shop.test/form", "to_url": "https://shop.test/pay"},
    ]);
    let others = theirs.as_array().unwrap().clone();
    engine.push_page_reply(Ok(PageReply {
        record: PageRecordNote {
            seq: Some(6),
            others: others.clone(),
            others_earlier: 3,
        },
        ..answer("Filled searchbox \"Search\"")
    }));
    let fill = start_as(
        &mut widget,
        Some(&ada),
        "fill-page",
        &serde_json::json!({"element": "d1e3", "text": "kettle"}),
    )
    .unwrap();
    let value = succeeded(settled(&mut widget, &doc, &root, fill));
    assert_eq!(value["message"], "Filled searchbox \"Search\"");
    assert_eq!(value["seq"], 6, "its own place in the record");
    assert_eq!(value["others"], theirs);
    assert_eq!(value["others_earlier"], 3);
    assert_eq!(
        value["provenance"], "web-content",
        "what the others wrote is page content like the rest"
    );

    // Told once: an answer that carries nothing of the others has no such
    // fields, rather than empty ones to be mistaken for news.
    let read = start_as(&mut widget, Some(&ada), "read-page", &serde_json::json!({})).unwrap();
    let value = succeeded(settled(&mut widget, &doc, &root, read));
    for key in ["seq", "others", "others_earlier"] {
        assert!(value.get(key).is_none(), "{key}: {value}");
    }

    // A refusal is told the same: the element went stale because Ben's
    // click left the page.
    let mut stale = PageFailure::new(
        "stale_element",
        "d1e3 belongs to a page this tab has left; read the page again",
    );
    stale.record = PageRecordNote {
        seq: Some(7),
        others: others[1..].to_vec(),
        others_earlier: 0,
    };
    engine.push_page_reply(Err(stale));
    let click = start_as(
        &mut widget,
        Some(&ada),
        "click-page",
        &serde_json::json!({"element": "d1e3"}),
    )
    .unwrap();
    let refused = failed(settled(&mut widget, &doc, &root, click));
    assert_eq!(refused.code, "browser.page.stale_element");
    assert!(!refused.retryable);
    let details = refused
        .details
        .expect("the refusal says what the others did");
    assert_eq!(details["seq"], 7);
    assert_eq!(details["others"], serde_json::json!([theirs[1]]));
    assert!(details.get("others_earlier").is_none());
}

/// A long page is read on from where a read stopped, and its words are found
/// by searching for them: both are typed reads, answered in the shape every
/// page read has, and a line a search answers is one a read starts from.
#[test]
fn a_long_page_is_read_on_and_its_words_are_found_by_searching() {
    let (engine, mut widget, mut doc, root) = page_on("https://law.test/act");
    engine.push_page_reply(Ok(PageReply {
        outline: Some("1240: - paragraph \"Condition 1240. Within 30 days.\"".into()),
        ..answer("Read lines 1240 to 2001 of 2001")
    }));
    let found = serde_json::json!({
        "matches": [{"line": 1500,
            "text": "Condition 1500. See Section 1500 of the ACT, within 30 days."}],
        "total": 1,
    });
    engine.push_page_reply(Ok(PageReply {
        outline: Some("1500: Condition 1500. See Section 1500 of the ACT, within 30 days.".into()),
        data: Some(found.clone()),
        ..answer("1 line holds those words")
    }));
    let mut answers = Vec::new();
    for (id, args) in [
        ("read-page", serde_json::json!({"from": 1240})),
        (
            "find-page-text",
            serde_json::json!({"text": "Section 1500 of the ACT"}),
        ),
        (
            "find-page-text",
            serde_json::json!({"text": "within 30 days", "frame": "1"}),
        ),
        ("read-page", serde_json::json!({"from": 1500, "frame": "1"})),
        // The words are counted as the engine counts them, whatever their
        // script: 200 of them are 200, however many bytes they take.
        (
            "find-page-text",
            serde_json::json!({"text": "é".repeat(200)}),
        ),
    ] {
        let invocation = start(&mut widget, &mut doc, &root, id, &args).unwrap();
        answers.push(succeeded(settled(&mut widget, &doc, &root, invocation)));
    }
    assert_eq!(
        page_requests(&engine),
        vec![
            PageRequest::Outline {
                from: Some(1240),
                frame: None,
            },
            PageRequest::Search {
                text: "Section 1500 of the ACT".into(),
                frame: None,
            },
            PageRequest::Search {
                text: "within 30 days".into(),
                frame: Some(FrameId::parse("1").unwrap()),
            },
            PageRequest::Outline {
                from: Some(1500),
                frame: Some(FrameId::parse("1").unwrap()),
            },
            PageRequest::Search {
                text: "é".repeat(200),
                frame: None,
            },
        ]
    );
    for value in &answers {
        assert_eq!(value["provenance"], "web-content", "{value}");
    }
    assert_eq!(answers[0]["message"], "Read lines 1240 to 2001 of 2001");
    assert!(
        answers[0]["outline"]
            .as_str()
            .is_some_and(|outline| outline.starts_with("1240: "))
    );
    // A search answers the lines that hold the words, as findings and as the
    // lines themselves; the line is what the next read starts from.
    assert_eq!(answers[1]["data"], found);
    assert_eq!(answers[1]["data"]["matches"][0]["line"], 1500);
    assert_eq!(
        answers[1]["outline"],
        "1500: Condition 1500. See Section 1500 of the ACT, within 30 days."
    );
    assert_eq!(
        answers[2]["data"],
        serde_json::json!({"matches": [], "total": 0})
    );

    // What no engine takes is refused before the page is asked.
    let asked = page_requests(&engine).len();
    for (id, args) in [
        ("read-page", serde_json::json!({"from": 0})),
        ("read-page", serde_json::json!({"from": 1_000_001})),
        ("find-page-text", serde_json::json!({"text": ""})),
        (
            "find-page-text",
            serde_json::json!({"text": "x".repeat(201)}),
        ),
        (
            "find-page-text",
            serde_json::json!({"text": "Total", "frame": "#fr"}),
        ),
    ] {
        let refused = start(&mut widget, &mut doc, &root, id, &args).unwrap_err();
        assert!(!refused.is_empty(), "{id} {args}");
    }
    let refused = start(
        &mut widget,
        &mut doc,
        &root,
        "read-page",
        &serde_json::json!({"from": 1_000_001}),
    )
    .unwrap_err();
    assert!(refused.contains("between 1 and 1000000"), "{refused}");
    let refused = start(
        &mut widget,
        &mut doc,
        &root,
        "find-page-text",
        &serde_json::json!({"text": "x".repeat(201)}),
    )
    .unwrap_err();
    assert!(refused.contains("1 to 200 characters"), "{refused}");
    assert_eq!(page_requests(&engine).len(), asked);

    // A line past the page's end is the engine's to refuse: it knows how
    // long the page is, and its refusal reaches the caller as it was said.
    engine.push_page_reply(Err(PageFailure::new(
        "invalid_request",
        "there is no line 5000: the page has 2001 lines",
    )));
    let beyond = start(
        &mut widget,
        &mut doc,
        &root,
        "read-page",
        &serde_json::json!({"from": 5000}),
    )
    .unwrap();
    let refused = failed(settled(&mut widget, &doc, &root, beyond));
    assert_eq!(refused.code, "browser.page.invalid_request");
    assert!(
        refused.message.contains("no line 5000"),
        "{}",
        refused.message
    );
}

/// A form is read as what it asks and answered by the names it asks under:
/// the answers reach the engine as the object they are, the form is sent
/// with its first button or a named one, and what came of each answer
/// reaches the caller.
#[test]
fn a_pages_forms_are_read_and_answered_by_the_names_they_ask_under() {
    let (engine, mut widget, mut doc, root) = page_on("https://visa.test/apply");
    let forms = serde_json::json!({"forms": [{
        "form": "form1", "label": "Apply for a permit", "method": "GET", "action": "/done",
        "signature": "5f1c2a9be0", "submit": ["Send application"],
        "fields": [
            {"name": "Date of birth: Month", "kind": "number", "required": true, "value": "",
                "element": "d1e4"},
            {"name": "Languages", "kind": "choices", "choices": ["English", "Dutch", "German"],
                "value": [], "elements": ["d1e9", "d1e10", "d1e11"]},
            {"name": "Nationality", "kind": "choice", "choices": ["Kenya", "Other"],
                "value": "", "element": "d1e12"},
            {"name": "Passcode", "kind": "secret", "required": true, "value": "",
                "element": "d1e14"},
        ],
    }]});
    engine.push_page_reply(Ok(PageReply {
        outline: Some("- form1 \"Apply for a permit\" (GET /done) signature=5f1c2a9be0".into()),
        data: Some(forms.clone()),
        ..answer("1 form")
    }));
    let placed = serde_json::json!({
        "form": "form1", "signature": "5f1c2a9be0",
        "results": [
            {"name": "Date of birth: Month", "ok": true},
            {"name": "Languages", "ok": true},
            {"name": "Nationality", "ok": true},
            {"name": "Passcode", "ok": false, "code": "secret_field",
                "message": "\"Passcode\" is a secret: fill it with the secret fill",
                "element": "d1e14"},
        ],
        "set": [
            {"name": "Date of birth: Month", "value": "11"},
            {"name": "Languages", "value": "English, German"},
            {"name": "Nationality", "value": "Other"},
        ],
        "submitted": false,
        "now_asks": ["Which country?"],
        "unanswered": ["Which country?", "Passcode"],
    });
    engine.push_page_reply(Ok(PageReply {
        data: Some(placed.clone()),
        navigation: PageNavigation::Possible,
        record: PageRecordNote {
            seq: Some(1),
            ..PageRecordNote::default()
        },
        ..answer(
            "Placed 3 of 4 answers; the form now also asks \"Which country?\"; not sent, since \
             not every answer was placed",
        )
    }));
    let first_answers = serde_json::json!({
        "Date of birth: Month": 11,
        "Languages": ["English", "German"],
        "Nationality": "Other",
        "Passcode": "0000",
    });
    let mut answers = Vec::new();
    for (id, args) in [
        ("read-page-forms", serde_json::json!({})),
        (
            "fill-page-form",
            serde_json::json!({"form": "form1", "answers": first_answers, "submit": true}),
        ),
        // The form named by its signature, sent with the button a label
        // names, and allowed to leave for another site.
        (
            "fill-page-form",
            serde_json::json!({"form": "5f1c2a9be0",
                "answers": {"Which country?": "Portugal", "Newsletter": false},
                "submit_button": "Send application", "cross_origin": true}),
        ),
        // Answered, and left for its caller to send.
        (
            "fill-page-form",
            serde_json::json!({"form": "form1", "answers": {"Arrival": "2027-02-03"}}),
        ),
        ("read-page-forms", serde_json::json!({"frame": "2"})),
    ] {
        let invocation = start(&mut widget, &mut doc, &root, id, &args).unwrap();
        answers.push(succeeded(settled(&mut widget, &doc, &root, invocation)));
    }
    let object = |value: serde_json::Value| value.as_object().unwrap().clone();
    assert_eq!(
        page_requests(&engine),
        vec![
            PageRequest::Forms { frame: None },
            PageRequest::FillForm {
                form: "form1".into(),
                answers: object(first_answers),
                submit: ui::webview::FormSubmit::First,
                cross_origin: false,
            },
            PageRequest::FillForm {
                form: "5f1c2a9be0".into(),
                answers: object(
                    serde_json::json!({"Which country?": "Portugal", "Newsletter": false})
                ),
                submit: ui::webview::FormSubmit::Button("Send application".into()),
                cross_origin: true,
            },
            PageRequest::FillForm {
                form: "form1".into(),
                answers: object(serde_json::json!({"Arrival": "2027-02-03"})),
                submit: ui::webview::FormSubmit::No,
                cross_origin: false,
            },
            PageRequest::Forms {
                frame: Some(FrameId::parse("2").unwrap()),
            },
        ]
    );
    for value in &answers {
        assert_eq!(value["provenance"], "web-content", "{value}");
    }
    // The forms as what they ask, with the id and the signature an answer
    // names them by.
    assert_eq!(answers[0]["data"], forms);
    assert_eq!(answers[0]["data"]["forms"][0]["signature"], "5f1c2a9be0");
    assert!(
        answers[0]["outline"]
            .as_str()
            .is_some_and(|outline| outline.contains("signature=5f1c2a9be0"))
    );
    // What came of each answer, what the form asks now, and the action's
    // place in the tab's record.
    assert_eq!(answers[1]["data"], placed);
    assert_eq!(answers[1]["data"]["results"][3]["code"], "secret_field");
    assert_eq!(
        answers[1]["data"]["now_asks"],
        serde_json::json!(["Which country?"])
    );
    assert_eq!(answers[1]["data"]["submitted"], false);
    assert_eq!(answers[1]["seq"], 1);
    assert_eq!(answers[1]["navigation"], "possible");
    assert!(
        answers[1]["message"]
            .as_str()
            .is_some_and(|message| message.contains("now also asks \"Which country?\""))
    );
    assert_eq!(answers[3]["data"]["form"], "form1");
    assert_eq!(answers[4]["data"], serde_json::json!({"forms": []}));

    // A call that placed nothing fails with the reason the engine gave, by
    // its own code, and asks for no retry of an action that runs once.
    for (code, message) in [
        (
            "no_form",
            "No form has the signature 5f1c2a9be0 any more; read the forms again",
        ),
        (
            "secret_field",
            "\"Passcode\" is a secret: fill it with the secret fill",
        ),
        ("no_field", "The form does not ask \"Shoe size\""),
    ] {
        engine.push_page_reply(Err(PageFailure::new(code, message)));
        let invocation = start(
            &mut widget,
            &mut doc,
            &root,
            "fill-page-form",
            &serde_json::json!({"form": "5f1c2a9be0", "answers": {"Shoe size": "44"}}),
        )
        .unwrap();
        let refused = failed(settled(&mut widget, &doc, &root, invocation));
        assert_eq!(refused.code, format!("browser.page.{code}"));
        assert_eq!(refused.message, message);
        assert!(!refused.retryable, "{code}");
    }
}

/// What a form cannot take never reaches the page: the bounds the engine
/// accepts are held here, and the action's own types before them.
#[test]
fn a_forms_answers_are_bounded_before_they_reach_the_page() {
    let (engine, mut widget, mut doc, root) = page_on("https://visa.test/apply");
    let too_many: serde_json::Map<String, serde_json::Value> = (0..=200)
        .map(|number| (format!("Question {number}"), serde_json::json!("x")))
        .collect();
    for (why, args) in [
        (
            "a form of no name",
            serde_json::json!({"form": "", "answers": {"Arrival": "x"}}),
        ),
        (
            "a name longer than an id or a signature",
            serde_json::json!({"form": "f".repeat(65), "answers": {}}),
        ),
        (
            "more answers than a form takes",
            serde_json::json!({"form": "form1", "answers": too_many}),
        ),
        (
            "an answer under no name",
            serde_json::json!({"form": "form1", "answers": {"": "x"}}),
        ),
        (
            "an answer that is an object",
            serde_json::json!({"form": "form1", "answers": {"Arrival": {"day": 3}}}),
        ),
        (
            "an answer that is nothing",
            serde_json::json!({"form": "form1", "answers": {"Arrival": null}}),
        ),
        (
            "a choice that is no text",
            serde_json::json!({"form": "form1", "answers": {"Languages": ["English", 3]}}),
        ),
        (
            "more choices than one answer makes",
            serde_json::json!({"form": "form1", "answers": {"Languages": vec!["x"; 65]}}),
        ),
        (
            "a text longer than a field takes",
            serde_json::json!({"form": "form1", "answers": {"Notes": "x".repeat(65_537)}}),
        ),
        (
            "a button named while saying not to send",
            serde_json::json!({"form": "form1", "answers": {}, "submit": false,
                "submit_button": "Send application"}),
        ),
        (
            "a label longer than any button's",
            serde_json::json!({"form": "form1", "answers": {},
                "submit_button": "x".repeat(201)}),
        ),
        // The action's own types refuse these before the bounds are asked.
        (
            "answers that are no object",
            serde_json::json!({"form": "form1", "answers": "Arrival=2027-02-03"}),
        ),
        (
            "a way of sending that is neither yes nor no",
            serde_json::json!({"form": "form1", "answers": {}, "submit": "Send application"}),
        ),
        ("no answers at all", serde_json::json!({"form": "form1"})),
    ] {
        let refused = start(&mut widget, &mut doc, &root, "fill-page-form", &args).expect_err(why);
        assert!(!refused.is_empty(), "{why}");
    }
    assert!(
        page_requests(&engine).is_empty(),
        "a refused form reached the page"
    );
    // A refusal for its bounds says what a form takes.
    let refused = start(
        &mut widget,
        &mut doc,
        &root,
        "fill-page-form",
        &serde_json::json!({"form": "form1", "answers": {"Arrival": {"day": 3}}}),
    )
    .unwrap_err();
    assert!(
        refused.contains("at most 200 answers") && refused.contains("a list of texts"),
        "{refused}"
    );
    // Within them it goes: a form with nothing left to answer is only sent.
    let sent = start(
        &mut widget,
        &mut doc,
        &root,
        "fill-page-form",
        &serde_json::json!({"form": "form1", "answers": {}, "submit": true}),
    )
    .unwrap();
    succeeded(settled(&mut widget, &doc, &root, sent));
    assert_eq!(
        page_requests(&engine),
        vec![PageRequest::FillForm {
            form: "form1".into(),
            answers: serde_json::Map::new(),
            submit: ui::webview::FormSubmit::First,
            cross_origin: false,
        }]
    );
}

/// Answering a form is an action like a click or a submit: it waits its turn
/// behind another actor's, is told what that actor did, and when it sends
/// the form it completes on the page the form opened.
#[test]
fn answering_a_form_takes_its_turn_and_completes_on_the_page_it_sent_to() {
    let (engine, mut widget, doc, root) = page_on("https://visa.test/apply");
    let (ada, ben) = (agent("agent:ada"), agent("agent:ben"));
    engine.push_page_reply(Ok(answer("Clicked checkbox \"Accept terms\"")));
    let sent = serde_json::json!({
        "form": "form1", "signature": "5f1c2a9be0",
        "results": [{"name": "Which country?", "ok": true}],
        "set": [{"name": "Which country?", "value": "Portugal"}],
        "submitted": true, "now_asks": [], "unanswered": [],
    });
    let theirs = serde_json::json!([
        {"seq": 1, "actor": "agent:ada", "action": "click", "ok": true,
            "target": {"role": "checkbox", "name": "Accept terms"}},
    ]);
    engine.push_page_reply(Ok(PageReply {
        url: "https://visa.test/apply".into(),
        data: Some(sent.clone()),
        navigation: PageNavigation::Started,
        loading: true,
        record: PageRecordNote {
            seq: Some(2),
            others: theirs.as_array().unwrap().clone(),
            others_earlier: 0,
        },
        ..answer("Placed 1 answer and sent the form with \"Send application\"")
    }));
    let click = start_as(
        &mut widget,
        Some(&ada),
        "click-page",
        &serde_json::json!({"element": "d1e7"}),
    )
    .unwrap();
    let form = start_as(
        &mut widget,
        Some(&ben),
        "fill-page-form",
        &serde_json::json!({"form": "form1", "answers": {"Which country?": "Portugal"},
            "submit": true}),
    )
    .expect("answering a form waits its turn behind the click");
    assert_eq!(
        page_asks(&engine),
        vec![(Some("agent:ada".to_owned()), "click")],
        "the form's answers have reached nothing yet"
    );
    assert!(widget.poll_invocation(form).is_none());
    succeeded(settled(&mut widget, &doc, &root, click));
    widget.sync(&doc, &root);
    widget.sync(&doc, &root);
    assert_eq!(
        page_asks(&engine),
        vec![
            (Some("agent:ada".to_owned()), "click"),
            (Some("agent:ben".to_owned()), "fill_form"),
        ]
    );
    assert!(
        widget.poll_invocation(form).is_none(),
        "a form that was sent waits for the page it opened"
    );
    engine.send(WebViewCommand::Navigate(
        "https://visa.test/done?other=Portugal".into(),
    ));
    let value = succeeded(settled(&mut widget, &doc, &root, form));
    assert_eq!(value["url"], "https://visa.test/done?other=Portugal");
    assert_eq!(value["navigation"], "started");
    assert_eq!(
        value["message"],
        "Placed 1 answer and sent the form with \"Send application\""
    );
    assert_eq!(value["data"], sent);
    assert_eq!(value["data"]["submitted"], true);
    assert_eq!(value["seq"], 2, "it took a place in the tab's record");
    assert_eq!(
        value["others"], theirs,
        "and was told of the click before it"
    );
}

/// The form action is published like the page's other actions that act: it
/// runs once, its answers are an object, and it is sent by a yes or by a
/// button's label, each a parameter of its own type.
#[test]
fn the_new_page_actions_declare_what_they_take() {
    use ui::tilo_actions::ParamType;
    let (_, widget, _, _) = page_on("https://visa.test/apply");
    let types = |id: &str| -> Vec<(String, ParamType)> {
        page_action(&widget, id)
            .params()
            .iter()
            .map(|param| (param.name.to_string(), param.ty))
            .collect()
    };
    assert_eq!(
        types("fill-page-form"),
        vec![
            ("form".to_owned(), ParamType::Text),
            ("answers".to_owned(), ParamType::Object),
            ("submit".to_owned(), ParamType::Boolean),
            ("submit_button".to_owned(), ParamType::Text),
            ("cross_origin".to_owned(), ParamType::Boolean),
        ]
    );
    assert!(
        types("fill-page-form")
            .iter()
            .all(|(_, ty)| *ty != ParamType::Secret),
        "a form's answers are never secrets: a secret takes the secret fill"
    );
    assert_eq!(
        page_action_contract("fill-page-form").retry,
        ui::tilo_actions::RetryPolicy::AtMostOnce,
        "a form is answered, and sent, once"
    );
    assert_eq!(
        types("read-page"),
        vec![
            ("from".to_owned(), ParamType::Integer),
            ("frame".to_owned(), ParamType::Text),
        ]
    );
    assert_eq!(
        types("find-page-text"),
        vec![
            ("text".to_owned(), ParamType::Text),
            ("frame".to_owned(), ParamType::Text),
        ]
    );
    assert_eq!(
        types("read-page-forms"),
        vec![("frame".to_owned(), ParamType::Text)]
    );
}
