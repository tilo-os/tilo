//! Page actions, as an agent reaches them: published on `browser.page`,
//! admitted through the resource operation lane, answered by the engine and
//! reported through the invocation's terminal update. Each merged phase runs
//! against a fresh page so the requests it makes stand on their own.

use std::sync::Arc;

use ui::tilo_actions::ActionSpec;
use ui::webview::{FrameId, PageKey, WebViewEngine as _};

use super::tests::{effect_operation, synced};
use super::*;

// The kept fixtures carry handles their phases inspect only where a phase
// names them; the whole scenario runs as one test, as before the split.

/// Page actions, as an agent reaches them: published on `browser.page`,
/// admitted through the resource operation lane, answered by the engine
/// and reported through the invocation's terminal update.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn page_actions_read_and_operate_the_page_through_invocations() {
    fn page_on(url: &str) -> (Arc<TestEngine>, BrowserWidget, AutoCommit, ObjId) {
        let engine = TestEngine::new();
        engine.send(WebViewCommand::Navigate(url.into()));
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        let (doc, root) = synced(&mut widget);
        (engine, widget, doc, root)
    }
    fn page_action(widget: &BrowserWidget, doc: &AutoCommit, id: &str) -> ActionSpec {
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
        let action = page_action(widget, doc, id);
        let encoded = action
            .build_json(args.as_object().ok_or("arguments are an object")?)
            .map_err(|error| error.to_string())?;
        let invocation = ui::tilo_actions::InvocationId::new();
        let _ = (doc, root);
        match widget
            .admit(invocation, effect_operation(&encoded.payload))
            .map(Some)?
        {
            Some(ui::framework::Effected::Pending) => Ok(invocation),
            other => Err(format!("a page action must be accepted, got {other:?}")),
        }
    }
    fn page_action_contract(id: &str) -> ui::tilo_actions::ActionContract {
        let (_, widget, doc, _) = page_on("https://shop.test/");
        page_action(&widget, &doc, id).contract
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
    {
        let (_, widget, doc, _) = page_on("https://shop.test/");
        for id in [
            "read-page",
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
        ] {
            let action = page_action(&widget, &doc, id);
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
                ui::tilo_actions::ExecutionMode::Background { cancellable: false }
            );
        }
        let (_, blank, doc, _) = page_on("about:blank");
        assert!(
            !page_action(&blank, &doc, "read-page")
                .contract
                .availability
                .is_available()
        );
    }

    // Phase `read_returns_the_engine_outline_in_the_declared_shape`.
    {
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
            vec![PageRequest::Outline { frame: None }]
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
    {
        let (engine, mut widget, mut doc, root) = page_on("https://news.test/story");
        let toggle = |widget: &mut BrowserWidget, doc: &mut AutoCommit| {
            let invocation =
                start(widget, doc, &root, "reader-view", &serde_json::json!({})).unwrap();
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
            page_action(&widget, &doc, "reader-view").label,
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
        let (_, blank, doc, _) = page_on("about:blank");
        assert!(
            !page_action(&blank, &doc, "reader-view")
                .contract
                .availability
                .is_available()
        );
    }

    // Phase `a_read_on_a_loading_page_waits_for_it_to_settle`.
    {
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
            vec![PageRequest::Outline { frame: None }]
        );
        widget.sync(&doc, &root);
        assert!(matches!(
            widget.poll_invocation(invocation),
            Some(ui::tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
    }

    // Phase `typed_arguments_reach_the_engine_and_bad_refs_never_do`.
    {
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
    {
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
    {
        let (engine, mut widget, mut doc, root) = page_on("https://shop.test/");
        let secret = page_action(&widget, &doc, "fill-page-secret");
        assert!(
            secret.params().iter().any(
                |param| param.name == "text" && param.ty == ui::tilo_actions::ParamType::Secret
            ),
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
    }

    // Phase `an_action_that_navigates_completes_on_the_page_it_opened`.
    {
        let (engine, mut widget, mut doc, root) = page_on("https://shop.test/cart");
        engine.push_page_reply(Ok(PageReply {
            url: "https://shop.test/cart".into(),
            message: "Clicked button \"Checkout\"".into(),
            outline: None,
            data: None,
            navigation: ui::webview::PageNavigation::Started,
            loading: true,
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
        assert!(
            start(
                &mut widget,
                &mut doc,
                &root,
                "read-page",
                &serde_json::json!({})
            )
            .is_err(),
            "a second page action is refused while one runs"
        );
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
    }

    // Phase `engine_refusals_become_typed_invocation_failures`.
    {
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
    {
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
}
#[test]
fn a_settled_page_result_nobody_waits_for_does_not_hold_the_tab() {
    let mut widget = BrowserWidget::new_headless();
    widget.current_uri = "https://site.test/form".into();
    let abandoned = ui::tilo_actions::InvocationId::new();
    widget
        .start_page_operation(abandoned, PageRequest::Outline { frame: None })
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
        .start_page_operation(next, PageRequest::Outline { frame: None })
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
    // deadline, and refusing the next action while it runs is right.
    let running = ui::tilo_actions::InvocationId::new();
    widget
        .start_page_operation(running, PageRequest::Outline { frame: None })
        .unwrap();
    assert!(
        widget
            .start_page_operation(
                ui::tilo_actions::InvocationId::new(),
                PageRequest::Outline { frame: None }
            )
            .is_err(),
        "a second page action is refused while one runs"
    );
    assert!(
        widget
            .page_operation
            .as_ref()
            .is_some_and(|operation| operation.invocation == running),
        "the running action keeps the slot"
    );
}
