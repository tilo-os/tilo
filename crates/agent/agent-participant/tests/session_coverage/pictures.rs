//! A scan the room keeps, read as content by an agent: its model, which
//! sees, is shown the picture where the agent may show the room's pictures,
//! and is told plainly why not where it may not.

use tilo_agent_participant::{ModelPlanner, OpenAiModel};
use tilo_relay_protocol::agents::TaskState;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use super::{
    enrol, message_text, model_endpoint, planning_agent, run_task, start_host, steward, tool_call,
};

const WRITER: &str = "so.tilo.word-processor";

/// A planner at `url` whose model is shown pictures.
fn seeing_planner(url: &str) -> Box<dyn tilo_agent_participant::Planner> {
    Box::new(ModelPlanner::new(Box::new(
        OpenAiModel::new(
            url,
            "canned",
            tilo_agent_participant::ApiKey::None,
            tilo_test_env::hang_bound(),
            tilo_model_gateway::openai::Target::Standard,
        )
        .with_vision(Some(true)),
    )))
}

/// A `width`×`height` grey PNG.
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        width,
        height,
        image::Rgb([0x80, 0x90, 0xa0]),
    ))
    .write_to(
        &mut std::io::Cursor::new(&mut bytes),
        image::ImageFormat::Png,
    )
    .unwrap();
    bytes
}

/// Run a task whose plan runs Writer's `action` (`read-content`, an
/// import) on the scan, then plans the rest, on an agent that may show the
/// room's pictures or not (`allowed`): the task as it ended and the bodies
/// of the requests its model was sent.
async fn on_the_scan(
    action: &str,
    allowed: bool,
) -> (tilo_relay_protocol::agents::AgentTask, Vec<String>) {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let _steward = steward::Steward::start(&host.relay, &root).await;
    let store = tilo_content::ContentStore::memory();
    let scan = format!("tilo.content:{}", store.put(&png(1600, 1200)).unwrap());
    let done = tool_call(
        "p2",
        "plan",
        &serde_json::json!({"steps": [], "then": "", "reply": "The passport is valid."}),
    );
    let (url, sent) = model_endpoint(vec![
        tool_call(
            "p1",
            "plan",
            &serde_json::json!({"steps": [{"operation": format!("word-processor.document#{action}"),
                "arguments": {"content": scan, "name": "passport.png"}}],
                "then": "Say whether the passport is valid"}),
        ),
        done.clone(),
        done,
    ]);
    let (mut agent, control) = planning_agent(
        &root,
        (&host, &host_device),
        &[WRITER],
        seeing_planner(&url),
        store,
    )
    .await;
    agent.allow_room_pictures(allowed);
    agent.open(WRITER, None).unwrap();

    let info = run_task(
        &mut agent,
        &control,
        &format!("Is the passport in the scan {scan} valid?"),
    );
    let requests = sent.try_iter().collect();
    agent.close();
    host.relay.shutdown().await;
    (info, requests)
}

/// Whether the request `body` carries a picture.
fn shows_a_picture(body: &str) -> bool {
    body.contains("data:image/")
}

/// Allowed, the read succeeds and the plan after it is shown the scan,
/// shrunk to the limits; the read is no refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scan_read_as_content_is_shown_to_a_model_that_sees() {
    let (info, requests) = Box::pin(on_the_scan("read-content", true)).await;
    let read = info
        .steps
        .iter()
        .find(|step| step.action == "read-content")
        .unwrap_or_else(|| panic!("{info:?}"));
    assert_eq!(read.state, TaskState::Completed, "{info:?}");
    assert!(requests.len() >= 2, "{info:?}");
    assert!(
        !shows_a_picture(&requests[0]),
        "nothing seen before the read"
    );
    assert!(
        shows_a_picture(&requests[1]),
        "the plan after the read sees it"
    );
    let request: serde_json::Value = serde_json::from_str(&requests[1]).unwrap();
    let turn = message_text(&request["messages"][1]);
    assert!(
        turn.contains("\"format\":\"picture\"") && turn.contains("picture shown below, 1024×768"),
        "{turn}"
    );
}

/// Where the agent may not show the room's pictures, the read is refused
/// saying so, the repair is told why, and no request carries the scan.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scan_read_as_content_is_refused_plainly_where_pictures_are_off() {
    let (info, requests) = Box::pin(on_the_scan("read-content", false)).await;
    let refused = info
        .steps
        .iter()
        .find(|step| step.action == "read-content")
        .and_then(|step| step.error.clone())
        .unwrap_or_else(|| panic!("{info:?}"));
    assert!(
        refused.starts_with("passport.png (tilo.content:")
            && refused.ends_with(
                "is a picture, and this agent is not shown the room's pictures: plan with what \
                 the request and the reads say of it"
            ),
        "{refused}"
    );
    assert!(requests.len() >= 2, "{info:?}");
    let repair: serde_json::Value = serde_json::from_str(&requests[1]).unwrap();
    let repair = message_text(&repair["messages"][1]);
    assert!(
        repair.contains("this agent is not shown the room's pictures"),
        "{repair}"
    );
    assert!(!requests.iter().any(|body| shows_a_picture(body)));
}

/// An import of the scan as a document fails saying it is a picture, and
/// why it is not seen.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_import_of_a_scan_says_it_is_a_picture() {
    let (info, _) = Box::pin(on_the_scan("import-docx-content", false)).await;
    let failed = info
        .steps
        .iter()
        .find(|step| step.action == "import-docx-content")
        .and_then(|step| step.error.clone())
        .unwrap_or_else(|| panic!("{info:?}"));
    assert!(
        failed.contains(
            "is a picture, not a document, and this agent is not shown the room's pictures"
        ),
        "{failed}"
    );
}
