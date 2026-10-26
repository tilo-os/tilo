//! A read of content that is a picture, as the planning model is shown it:
//! the picture, where the model sees and the agent may show it, or a plain
//! refusal saying why not.

use super::tests::{Looks, called, look_context, pictures, results, scripted, text_of};
use crate::planner::{PlanContext, Planner as _};
use crate::vision::tests::png;
use crate::{READ_VIEW, Reads};

/// Reads whose every read answers that the content it was asked to read
/// is the picture `scan`, named `passport.jpg`; looks as [`Looks`].
struct ScanReads {
    looks: Looks,
    scan: String,
}

impl ScanReads {
    fn new() -> Self {
        let mut looks = Looks::new('a');
        let scan = looks.keep('b', png(1600, 1200));
        Self { looks, scan }
    }
}

impl Reads for ScanReads {
    fn read(
        &mut self,
        _: &tilo_browser_peer::DocumentRoute,
        _: &tilo_actions::ActionAddress,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_actions::ActionOutput, String> {
        Ok(tilo_actions::ActionOutput {
            value: Some(
                serde_json::json!({"file": "passport.jpg", "format": "picture",
                "picture": self.scan, "media_type": "image/png", "bytes": 4096,
                "width": 1600, "height": 1200}),
            ),
            resources: Vec::new(),
        })
    }

    fn look(&mut self, hash: &str) -> Result<Vec<u8>, String> {
        self.looks.look(hash)
    }
}

fn read_scan() -> serde_json::Value {
    serde_json::json!({"operation": "canvas.artwork#look", "arguments": {}})
}

fn no_steps() -> serde_json::Value {
    serde_json::json!({"steps": [{"operation": "canvas.artwork#draw",
        "arguments": {"marks": []}}], "then": ""})
}

/// The tool result a read of the scan answered, and how many pictures the
/// turn after it carried, for a model that `sees` in `context`.
fn read_once(sees: bool, context: &PlanContext) -> (String, usize, ScanReads) {
    let (mut planner, sent) = scripted(
        vec![
            called("r1", READ_VIEW, &read_scan()),
            called("p1", tilo_agent::tools::PLAN, &no_steps()),
        ],
        sees,
    );
    let mut reads = ScanReads::new();
    planner
        .plan_reading("which passport is valid?", context, &mut reads)
        .unwrap();
    let requests = sent.lock().unwrap().clone();
    let answered = results(&requests[1])[0].clone();
    let shown = requests[1].messages.iter().map(pictures).sum();
    (answered, shown, reads)
}

/// Allowed, the scan a read answered with is shown, written out again
/// within the limits, and the read says so; nothing is refused.
#[test]
fn a_read_of_a_picture_shows_it_to_a_model_that_sees() {
    let mut context = look_context();
    context.room_pictures = true;
    let (answered, shown, reads) = read_once(true, &context);
    assert_eq!(shown, 1, "{answered}");
    assert!(answered.contains("Shown as a picture."), "{answered}");
    assert!(
        answered.contains(&format!("{} picture shown below, 1024×768", reads.scan)),
        "shrunk to the limits: {answered}"
    );
    assert!(!answered.contains("Not read"), "{answered}");
}

/// Where the agent may not show the room's pictures, or its model sees
/// none, the read is refused saying so, and a blind model's read fetches
/// nothing.
#[test]
fn a_read_of_a_picture_is_refused_plainly_where_it_is_not_shown() {
    let (answered, shown, _) = read_once(true, &look_context());
    assert_eq!(shown, 0);
    assert!(
        answered.starts_with("Not read: passport.jpg (tilo.content:")
            && answered.contains("is a picture, and this agent is not shown the room's pictures"),
        "{answered}"
    );

    let mut context = look_context();
    context.room_pictures = true;
    let (answered, shown, reads) = read_once(false, &context);
    assert_eq!(shown, 0);
    assert!(
        answered.contains("is a picture, and this agent's model is not shown pictures"),
        "{answered}"
    );
    assert!(
        reads.looks.looked.is_empty(),
        "nothing fetched for a blind model"
    );
}

/// The room's pictures a task's steps read are shown on the re-plan's
/// first turn, where the agent may show them.
#[test]
fn a_replan_shows_the_pictures_its_steps_read() {
    for allowed in [true, false] {
        let (mut planner, sent) = scripted(
            vec![called("p1", tilo_agent::tools::PLAN, &no_steps())],
            true,
        );
        let mut reads = ScanReads::new();
        let mut context = look_context();
        context.request = Some("read the scans, then say which passport is valid".into());
        context.read_pictures = vec![reads.scan.clone()];
        context.room_pictures = allowed;
        planner
            .plan_reading("say which passport is valid", &context, &mut reads)
            .unwrap();
        let first = sent.lock().unwrap()[0].messages[1].clone();
        if allowed {
            assert_eq!(pictures(&first), 1);
            let text = text_of(&first);
            assert!(
                text.contains(&format!("{} picture shown below", reads.scan)),
                "{text}"
            );
        } else {
            assert_eq!(pictures(&first), 0, "the room's pictures stay withheld");
        }
    }
}
