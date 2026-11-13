//! Looking at a web page goes through the Browser the person can see: the
//! loop tells the model so, and a run that looks at a site reads and
//! captures it there, its capture shown to a model that sees.

use super::*;
use agent_provider_config::protocol::{
    Content, Message, ModelResponse, Role, StopReason, ToolCall, Usage,
};

/// A model that looks at a site the way the loop tells it to: it opens the
/// address in the Browser, reads the page, captures it, and finishes once the
/// capture is in front of it. It records whether the loop's guidance and the
/// capture reached it.
struct LooksAtSites {
    bodies: Mutex<Vec<Value>>,
}

fn browser_op(action: &str) -> String {
    format!("broker:{BROWSER_PEER}:{BROWSER_TAB}#{action}")
}

impl TurnTransport for LooksAtSites {
    fn start(&self, request: TurnRequest) -> Receiver<TurnEvent> {
        let turn = {
            let mut bodies = self.bodies.lock().unwrap();
            bodies.push(request.body);
            bodies.len() - 1
        };
        let invoke = |action: &str, parameters: Value| {
            (
                tools::INVOKE,
                serde_json::json!({"operation": browser_op(action), "parameters": parameters}),
            )
        };
        let call = match turn {
            0 => invoke(
                "navigate",
                serde_json::json!({"url": "https://janedoe.dev"}),
            ),
            1 => invoke("read-page", serde_json::json!({})),
            2 => invoke("capture-page", serde_json::json!({})),
            _ => finish("Jane Doe's portfolio shows two solid projects.", true),
        };
        let response = ModelResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ToolCall {
                    id: format!("t{turn}"),
                    name: call.0.into(),
                    input: call.1,
                    extra_content: None,
                })],
            },
            stop_reason: StopReason::ToolCalls,
            usage: Usage::default(),
            cost_micros: 0,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(TurnEvent::Finished(Ok(
            serde_json::to_string(&response).unwrap()
        )))
        .unwrap();
        rx
    }
}

#[test]
fn looking_at_a_site_reads_and_captures_it_in_the_browser_not_by_fetch() {
    let png = [b"\x89PNG\r\n\x1a\n".as_slice(), b"the page"].concat();
    let mut f = fixture(
        "Look at Jane Doe's portfolio site",
        World::default(),
        Vec::new(),
    );
    let capture = format!("tilo.content:{}", f.session.cfg.content.put(&png).unwrap());
    f.world.borrow_mut().browser = Some(capture.clone());
    f.session.broker_catalogue = super::catalogue_of(&f.world.borrow().surfaces());
    let model = Arc::new(LooksAtSites {
        bodies: Mutex::new(Vec::new()),
    });
    f.session.chooser = Chooser::scripted_model(
        Duration::from_secs(5),
        agent_provider_config::ModelDescriptor {
            vision: true,
            ..agent_provider_config::ModelDescriptor::new("sees")
        },
        Arc::clone(&model) as Arc<dyn TurnTransport>,
    );
    f.session.run_task(&f.run).unwrap();
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "Jane Doe's portfolio shows two solid projects.".into()
        }
    );
    let actions: Vec<String> = f
        .world
        .borrow()
        .steps
        .iter()
        .map(|(action, _)| action.clone())
        .collect();
    assert_eq!(
        actions,
        ["navigate", "read-page", "capture-page"],
        "never fetch"
    );
    let bodies = model.bodies.lock().unwrap().clone();
    let first = bodies[0].to_string();
    assert!(
        first.contains("open its address in the browser"),
        "the loop tells the model to look at pages in the browser: {first}"
    );
    assert!(
        first.contains("`read_page` is only a quick read of text nobody needs to see"),
        "{first}"
    );
    let last = bodies.last().unwrap().to_string();
    assert!(
        last.contains("image/png"),
        "the capture reached the model that sees"
    );
    assert!(last.contains(&capture));
}

/// A model reading a candidate's pages one after another.
struct ReadsManyPages {
    pages: usize,
    bodies: Mutex<Vec<Value>>,
}

impl TurnTransport for ReadsManyPages {
    fn start(&self, request: TurnRequest) -> Receiver<TurnEvent> {
        let turn = {
            let mut bodies = self.bodies.lock().unwrap();
            bodies.push(request.body);
            bodies.len() - 1
        };
        let call = if turn < self.pages {
            let action = if turn % 2 == 0 {
                "read-page"
            } else {
                "capture-page"
            };
            (
                tools::INVOKE,
                serde_json::json!({"operation": browser_op(action), "parameters": {}}),
            )
        } else {
            finish("Read all the pages.", true)
        };
        let response = ModelResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ToolCall {
                    id: format!("t{turn}"),
                    name: call.0.into(),
                    input: call.1,
                    extra_content: None,
                })],
            },
            stop_reason: StopReason::ToolCalls,
            usage: Usage::default(),
            cost_micros: 0,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(TurnEvent::Finished(Ok(
            serde_json::to_string(&response).unwrap()
        )))
        .unwrap();
        rx
    }
}

/// A run walks a list of pages it reads one by one: every page is read
/// once, and the run finishes with what it found.
#[test]
fn the_run_reads_every_page_of_a_list_once() {
    const PAGES: usize = 16;
    let mut f = fixture(
        "Read the candidates' sites",
        World {
            browser: Some(format!("tilo.content:{}", "0".repeat(64))),
            ..World::default()
        },
        Vec::new(),
    );
    let model = Arc::new(ReadsManyPages {
        pages: PAGES,
        bodies: Mutex::new(Vec::new()),
    });
    f.session.chooser = Chooser::scripted_model(
        Duration::from_secs(5),
        agent_provider_config::ModelDescriptor::new("reads"),
        Arc::clone(&model) as Arc<dyn TurnTransport>,
    );
    f.session.run_task(&f.run).unwrap();
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "Read all the pages.".into()
        }
    );
    assert_eq!(f.world.borrow().steps.len(), PAGES, "every page read once");
    // No step here asks the person: reading a page is not an operation that
    // asks first, so none of them waits. A step that did would be held
    // (`native_pipeline::a_step_that_asks_the_person_is_held_until_they_answer`).
    assert!(
        !f.sent.borrow().iter().any(|request| matches!(
            request,
            CollabRequest::InvokeTaskAction { wait: Some(_), .. }
        )),
        "no step waits for a person"
    );
}
