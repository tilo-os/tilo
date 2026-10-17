//! The private feedback composer as a desktop shows it when Feedback is
//! opened from a Notes window: before review, and reviewed with the app
//! included as context and an email address masked, in dark and light.
//!
//! Run:  `cargo run -p tilo-desktop --example feedback_shots -- [OUT_DIR]`
//! Out:  `OUT_DIR` (default `target/look/feedback`):
//!       `<open|reviewed>-<dark|light>.png`

#![allow(
    clippy::expect_used,
    clippy::print_stdout,
    reason = "a capture run reports where it wrote and treats a failed setup as fatal"
)]

use std::path::PathBuf;
use std::rc::Rc;

use serde_json::json;
use tilo_app::feedback::{
    Destination, Error, FeedbackHost, MaskedScreenshot, MemoryStore, Receipt, ReviewText, Subject,
    Submission,
};
use tilo_desktop::feedback::PrivateFeedback;
use ui::headless::FrameHarness;
use ui::theme::Theme;
use ui::time::Duration;

/// A destination that is never reached: the shots stop before sending.
struct Offline;
impl FeedbackHost for Offline {
    fn offers_screenshots(&self) -> bool {
        false
    }
    fn context(&mut self) -> Result<Vec<ReviewText>, Error> {
        Ok(Vec::new())
    }
    fn capture(&mut self) -> Result<MaskedScreenshot, Error> {
        Err(Error::CaptureProtected)
    }
    fn persist_screenshot(&mut self, _: &MaskedScreenshot) -> Result<(), Error> {
        Err(Error::CaptureProtected)
    }
    fn send(&mut self, _: &Submission) -> Result<(), Error> {
        Err(Error::Invalid("offline"))
    }
    fn receipt(&mut self, _: &Submission) -> Result<Option<Receipt>, Error> {
        Ok(None)
    }
}

fn call(frame: &mut FrameHarness, action: &str, arguments: &serde_json::Value) {
    frame.relayout();
    let resource = frame
        .resources()
        .into_iter()
        .find(|resource| resource.kind == "tilo.feedback-draft")
        .expect("the composer's draft resource");
    frame
        .invoke_bound_action(
            &ui::bound_dispatch::BoundActionAddress {
                instance_id: 1,
                address: ui::tilo_actions::ActionAddress::new(resource.id.0, action),
            },
            arguments.as_object().expect("arguments"),
        )
        .expect(action);
}

fn main() {
    ui::i18n::I18nManager::init("en-US");
    let out = std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from("target/look/feedback"), PathBuf::from);
    std::fs::create_dir_all(&out).expect("output directory");
    for (variant, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        let feedback = PrivateFeedback::new(
            Rc::new(MemoryStore::new()),
            Destination {
                room: "shots".into(),
                provider: "record:so.tilo.chat:".into(),
                channel: "feedback".into(),
            },
            Box::new(Offline),
        )
        .expect("feedback custody");
        feedback
            .about(Some(Subject {
                app: "so.tilo.notes".into(),
                name: "Notes".into(),
            }))
            .expect("subject");
        let component = feedback.prepare("device", "person", 1).expect("composer");
        let mut frame = FrameHarness::from_component(
            component,
            (560, 760),
            ui::application::ComponentRunnerInputs::from_native_process(),
        )
        .expect("composer frame");
        frame.set_theme(theme);
        frame.settle_for_capture(Duration::from_secs(2));
        let path = out.join(format!("open-{variant}.png"));
        frame.render_png(&path);
        println!("{}", path.display());

        call(
            &mut frame,
            "field-edit",
            &json!({"edit": {"path": [{"field": "issue"}], "op": "replace",
                "text": "Saving lost my last edit. Write to jana@example.org"}}),
        );
        call(
            &mut frame,
            "field-edit",
            &json!({"edit": {"path": [{"field": "include_context"}], "op": "set", "value": true}}),
        );
        call(&mut frame, "review", &json!({}));
        frame.settle_for_capture(Duration::from_secs(2));
        let path = out.join(format!("reviewed-{variant}.png"));
        frame.render_png(&path);
        println!("{}", path.display());
    }
}
