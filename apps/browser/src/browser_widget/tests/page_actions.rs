//! Page actions as an agent reaches them: published on `browser.page`,
//! admitted through the resource operation lane, answered by the engine and
//! reported through the invocation's terminal update.
//!
//! A scenario file of its own: each phase is large enough that the widget
//! module would pass its size ceiling with them inline.
#![allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
#![allow(
    clippy::items_after_statements,
    reason = "scenario phases keep their helpers beside the fixture they serve"
)]

use super::super::tests::synced;
use super::super::*;
use crate::effects::BrowserEffect;
use std::sync::Arc;
use ui::tilo_actions::ActionSpec;
use ui::tilo_resource::Effect as _;
use ui::webview::WebViewEngine as _;

fn effect_operation(payload: &[u8]) -> BrowserOperation {
    BrowserEffect::from_payload(payload)
        .expect("a browser effect")
        .operation()
}

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
            assert!(action.contract.is_programmatic(), "{id}");
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
        assert_eq!(page_requests(&engine), vec![PageRequest::Outline]);
        widget.sync(&doc, &root);
        let Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
            widget.poll_invocation(invocation)
        else {
            panic!("an answered read must succeed its invocation");
        };
        let read = page_action(&widget, &doc, "read-page");
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
        assert_eq!(page_requests(&engine), vec![PageRequest::Outline]);
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
                },
                PageRequest::Select {
                    element: "d3e8".into(),
                    values: vec!["NL".into(), "Belgium".into()],
                },
                PageRequest::Press {
                    element: None,
                    key: PageKey::parse("Shift+Tab").unwrap(),
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
                PageRequest::Changes,
                PageRequest::At { x: 12.5, y: 40.0 },
                PageRequest::Settle {
                    quiet_ms: 500,
                    timeout_ms: 10_000,
                },
                PageRequest::Settle {
                    quiet_ms: 200,
                    timeout_ms: 3000,
                },
                PageRequest::Console { since: 4 },
                PageRequest::Network { since: 0 },
                PageRequest::Fill {
                    element: "d1e3".into(),
                    text: "hunter2".into(),
                    secret: true,
                },
                PageRequest::Submit {
                    element: "d1e9".into(),
                    cross_origin: false,
                },
                PageRequest::Submit {
                    element: "d1e9".into(),
                    cross_origin: true,
                },
            ],
            "a form leaves for another site only when the caller says so"
        );
        for value in &answers {
            assert_eq!(value["provenance"], "web-content", "{value}");
        }
        assert!(
            answers[0]["outline"].is_string(),
            "changes answer outline lines"
        );
        assert_eq!(answers[1]["data"]["x"], 12.5);
        assert_eq!(answers[2]["data"]["settled"], true);
        assert_eq!(answers[4]["data"]["entries"], serde_json::json!([]));
        assert_eq!(answers[5]["data"]["next"], 0);
    }

    // Phase `an_action_that_navigates_completes_on_the_page_it_opened`.
    {
        let (engine, mut widget, mut doc, root) = page_on("https://shop.test/cart");
        engine.push_page_reply(Ok(PageReply {
            url: "https://shop.test/cart".into(),
            message: "Clicked button \"Checkout\"".into(),
            outline: None,
            data: None,
            navigated: true,
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
        assert_eq!(value["navigated"], true);
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

    // Phase `a_pages_dialog_is_answered_and_freezes_everything_else`.
    {
        let (engine, mut widget, mut doc, root) = page_on("https://shop.test/");
        let read = start(
            &mut widget,
            &mut doc,
            &root,
            "read-page",
            &serde_json::json!({}),
        )
        .unwrap();
        widget.apply_engine_events();
        assert!(matches!(
            widget.poll_invocation(read),
            Some(ui::tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
        // A click confirms: the page asks its question before it answers.
        // The engine says the request met a dialog first; the dialog itself
        // reaches the tab just after.
        engine.push_page_reply(Err(PageFailure::new(
            "dialog_open",
            "The page is waiting on its confirm dialog",
        )));
        let click = start(
            &mut widget,
            &mut doc,
            &root,
            "click-page",
            &serde_json::json!({"element": "d1e4"}),
        )
        .unwrap();
        widget.apply_engine_events();
        engine.push_event(WebViewEvent::JsDialog {
            id: 3,
            kind: JsDialogKind::Confirm,
            message: "Are you sure?".into(),
            default_prompt: String::new(),
            origin: "https://shop.test".into(),
        });
        widget.apply_engine_events();
        // The click did happen: it succeeds, and carries the dialog the page
        // opened, so the agent reads the question without another read.
        let Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
            widget.poll_invocation(click)
        else {
            panic!("the action whose input opened a dialog succeeds");
        };
        let value = output.value.unwrap();
        let message = value["message"].as_str().unwrap();
        assert!(message.contains("Clicked element d1e4"), "{message}");
        assert!(message.contains("opened a confirm dialog"), "{message}");
        assert!(
            message.contains("alert") || message.contains("confirm"),
            "{message}"
        );
        assert!(message.contains("Are you sure?"), "{message}");
        assert!(message.contains("answer-page-dialog"), "{message}");
        assert_eq!(value["dialog"]["kind"], "confirm", "{value}");
        assert_eq!(value["dialog"]["message"], "Are you sure?", "{value}");
        assert_eq!(value["provenance"], "web-content", "{value}");
        assert_eq!(widget.page_dialog.as_ref().unwrap().id, 3);

        // Reads answer at once, with the dialog above the page's last outline.
        let read = start(
            &mut widget,
            &mut doc,
            &root,
            "read-page",
            &serde_json::json!({}),
        )
        .unwrap();
        widget.apply_engine_events();
        let Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
            widget.poll_invocation(read)
        else {
            panic!("a read answers while a dialog is open");
        };
        let outline = output.value.unwrap()["outline"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            outline.starts_with("- dialog confirm \"Are you sure?\""),
            "{outline}"
        );
        assert!(outline.contains("test page"), "{outline}");
        let changes = start(
            &mut widget,
            &mut doc,
            &root,
            "read-page-changes",
            &serde_json::json!({}),
        )
        .unwrap();
        widget.apply_engine_events();
        assert!(matches!(
            widget.poll_invocation(changes),
            Some(ui::tilo_actions::InvocationUpdate::Succeeded { .. })
        ));

        // Every other page action says what to answer first, and nothing
        // of it reaches the page.
        let before = page_requests(&engine).len();
        let refused = start(
            &mut widget,
            &mut doc,
            &root,
            "click-page",
            &serde_json::json!({"element": "d1e4"}),
        )
        .unwrap_err();
        assert!(refused.contains("confirm dialog"), "{refused}");
        assert!(refused.contains("Are you sure?"), "{refused}");
        assert!(refused.contains("answer-page-dialog"), "{refused}");
        assert_eq!(page_requests(&engine).len(), before);

        // Answering it releases the page, and its answer says what happened
        // next rather than leaving the caller to guess. The answer is done,
        // not pending: it settles with the action.
        let action = page_action(&mut widget, &mut doc, "answer-page-dialog");
        let encoded = action
            .build_json(serde_json::json!({"accept": false}).as_object().unwrap())
            .unwrap();
        let done = widget
            .admit(
                ui::tilo_actions::InvocationId::new(),
                effect_operation(&encoded.payload),
            )
            .unwrap();
        let ui::framework::Effected::Done(output) = done else {
            panic!("answering a dialog settles at once");
        };
        let Some(serde_json::Value::String(answered)) = output.value else {
            panic!("the answer says what happened next, not nothing");
        };
        assert!(answered.contains("confirm"), "{answered}");
        assert!(answered.contains("Cancel"), "{answered}");
        assert!(answered.contains("read-page"), "{answered}");
        assert!(widget.page_dialog.is_none());
        assert!(
            engine
                .commands()
                .contains(&WebViewCommand::JsDialogDecision {
                    id: 3,
                    accept: false,
                    text: String::new(),
                }),
            "the answer reaches the engine"
        );
        assert!(
            start(
                &mut widget,
                &mut doc,
                &root,
                "read-page",
                &serde_json::json!({})
            )
            .is_ok(),
            "the page takes work again"
        );
    }

    // Phase `a_request_blocked_by_an_open_dialog_fails_naming_it`.
    {
        let (engine, mut widget, mut doc, root) = page_on("https://shop.test/");
        // The page is still loading, so the click cannot reach it yet; the
        // dialog it opened earlier is already there.
        engine.push_event(WebViewEvent::JsDialog {
            id: 4,
            kind: JsDialogKind::Alert,
            message: "Draft saved.".into(),
            default_prompt: String::new(),
            origin: "https://shop.test".into(),
        });
        widget.apply_engine_events();
        widget.set_loading_progress(0.4);
        let before = page_requests(&engine).len();
        let refused = start(
            &mut widget,
            &mut doc,
            &root,
            "click-page",
            &serde_json::json!({"element": "d1e4"}),
        )
        .unwrap_err();
        assert!(refused.contains("alert dialog"), "{refused}");
        assert!(refused.contains("Draft saved."), "{refused}");
        assert!(refused.contains("answer-page-dialog"), "{refused}");
        assert_eq!(
            page_requests(&engine).len(),
            before,
            "nothing reached the page"
        );
    }

    // Phase `a_prompt_is_typed_into_and_a_question_says_what_it_leaves`.
    {
        let (engine, mut widget, mut doc, root) = page_on("https://shop.test/");
        engine.push_event(WebViewEvent::JsDialog {
            id: 9,
            kind: JsDialogKind::Prompt,
            message: "Your name?".into(),
            default_prompt: "Ada".into(),
            origin: "https://shop.test".into(),
        });
        widget.apply_engine_events();
        let outline = widget.page_dialog.as_ref().unwrap().line();
        assert_eq!(
            outline,
            "- dialog prompt \"Your name?\" default \"Ada\" from https://shop.test"
        );
        // A word is text, not a key: press-page says which action types it.
        let refused = start(
            &mut widget,
            &mut doc,
            &root,
            "press-page",
            &serde_json::json!({"key": "Grace"}),
        )
        .unwrap_err();
        assert!(refused.contains("type-page"), "{refused}");
        assert!(refused.contains("is 5 characters"), "{refused}");
        assert!(refused.contains("text rather than a key"), "{refused}");

        // type-page types without a ref, which is how a dialog's own
        // field is filled, and starts no page request of its own.
        let before = page_requests(&engine).len();
        widget
            .perform_browser_operation(BrowserOperation::TypePage {
                text: "Grace".into(),
                element: None,
            })
            .unwrap();
        assert_eq!(page_requests(&engine).len(), before);
        assert!(engine.commands().iter().any(|command| matches!(
            command,
            WebViewCommand::TypePage { text, .. } if text == "Grace"
        )));

        // A prompt answers with its default when the person says nothing.
        widget
            .perform_browser_operation(BrowserOperation::AnswerPageDialog {
                accept: true,
                text: None,
            })
            .unwrap();
        assert!(
            engine
                .commands()
                .contains(&WebViewCommand::JsDialogDecision {
                    id: 9,
                    accept: true,
                    text: "Ada".into(),
                })
        );
    }
}
