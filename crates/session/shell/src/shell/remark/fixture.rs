//! A recorded session to show the walk-through with, for `shots` and
//! tests: someone writes to Ada in Mail, attaches a file, presses Send and
//! it fails. The screens are a real toolkit window, laid out and answered
//! by the headless driver the way the native host answers an inspection,
//! and its frames are masked by the recorder's own frame strip. Never used
//! on the live path, which only ever reviews what the recorder recorded.

use session_trace::frames::{
    FrameLimits, FrameStrip, FrameStripBuilder, PixelPurpose, SurfaceFrame, keyframe_regions,
};
use session_trace::{
    Environment, Event, InspectKeyframe, Offset, Origin, PresenceChange, PresenceUpdate,
    Redactions, Route, SessionLayer, SessionTrace, TimedEvent,
};
use tilo_actions::{
    ActionApp, ActionResource, InteractionAction, InteractionActor, InteractionActorKind,
    InteractionAnchor, InteractionPhase, InvocationId, SemanticInteraction,
};
use tilo_reference::inspect::{InspectQuery, InspectReply, InspectRequest};
use ui::widget::{Banner, Button, Column, Row, TextInput};

const APP: &str = "so.tilo.mail";
const ROOM: &str = "room";
const VIEWPORT: (u32, u32) = (720, 480);

/// Mail's compose window, with the draft as far as it had been written.
fn compose(to: &str, subject: &str, body: &str, failed: bool) -> Column {
    let window = Column::new()
        .with_gap(8.0)
        .push(Box::new(
            Row::new()
                .with_gap(8.0)
                .push(Box::new(Button::new("send", "Send", None).primary()))
                .push(Box::new(Button::new("attach", "Attach", None)))
                .push(Box::new(Button::new("discard", "Discard", None).ghost())),
        ))
        .push(Box::new(
            TextInput::local()
                .with_id("to")
                .with_accessible_name("To")
                .with_placeholder("To")
                .with_sensitivity(ui::a11y::Sensitivity::Personal)
                .with_initial_value(to),
        ))
        .push(Box::new(
            TextInput::local()
                .with_id("subject")
                .with_accessible_name("Subject")
                .with_placeholder("Subject")
                .with_initial_value(subject),
        ))
        .push(Box::new(
            TextInput::local()
                .with_id("body")
                .multiline(true)
                .with_accessible_name("Message")
                .with_placeholder("Message")
                .with_initial_value(body),
        ));
    // Below the draft, so the fields keep their place when it fails.
    if failed {
        window.push(Box::new(
            Banner::error("Couldn't send").with_message("The server refused the attachment"),
        ))
    } else {
        window
    }
}

/// One screen as the app answers an inspection of it, and as it looked.
struct Screen {
    reply: InspectReply,
    rgba: Vec<u8>,
}

fn screen(theme: &ui::Theme, to: &str, subject: &str, body: &str, failed: bool) -> Screen {
    let driver = ui::headless::Driver::new_with_theme(
        ui::State::new(),
        Box::new(compose(to, subject, body, failed)),
        VIEWPORT,
        theme.clone(),
    );
    let reply = driver.inspect(&InspectRequest::new(
        InspectQuery::Subtree {
            root: None,
            depth: tilo_reference::inspect::MAX_INSPECT_DEPTH,
        },
        tilo_actions::Audience::LocalSession,
    ));
    Screen {
        reply,
        rgba: driver.render_window_rgba(),
    }
}

fn route() -> Route {
    Route::new(APP, ROOM, "")
}

fn keyframe(at: u64, reply: InspectReply) -> TimedEvent {
    TimedEvent {
        at: Offset(at),
        event: Event::Inspect(InspectKeyframe {
            route: route(),
            reply,
        }),
    }
}

fn step(
    at: u64,
    invocation: u128,
    phase: InteractionPhase,
    label: &str,
    control: Option<&str>,
) -> TimedEvent {
    TimedEvent {
        at: Offset(at),
        event: Event::Presence(PresenceUpdate {
            participant: "me".into(),
            room: "room".into(),
            scope: Vec::new(),
            change: PresenceChange::Interaction(Box::new(SemanticInteraction {
                version: tilo_actions::INTERACTION_VERSION,
                invocation: InvocationId(uuid::Uuid::from_u128(invocation)),
                sequence: 1,
                actor: InteractionActor {
                    id: "me".into(),
                    kind: InteractionActorKind::Person,
                },
                phase,
                action: InteractionAction {
                    id: format!("mail.{}", label.to_lowercase()),
                    label: label.into(),
                    app: Some(ActionApp {
                        id: APP.into(),
                        label: "Mail".into(),
                    }),
                },
                resource: ActionResource {
                    id: "mail.draft:1".into(),
                    kind: "mail.draft".into(),
                    label: "Draft".into(),
                    sensitivity: tilo_actions::Sensitivity::Public,
                },
                anchor: control.map_or(InteractionAnchor::Resource, |id| {
                    InteractionAnchor::SemanticControl { id: id.into() }
                }),
                group: None,
            })),
        }),
    }
}

/// The id of the button labelled `label` on a screen.
fn control(screen: &Screen, label: &str) -> Option<String> {
    screen
        .reply
        .nodes
        .iter()
        .find(|node| node.role == "button" && node.label.as_deref() == Some(label))
        .map(|node| node.id.clone())
}

/// The sample session and the frames kept of it.
pub struct SampleRecording {
    pub trace: SessionTrace,
    pub frames: Option<FrameStrip>,
}

/// The sample session, its screens drawn in `theme`.
pub fn sample_recording(theme: &ui::Theme) -> SampleRecording {
    let body =
        "Hi Ada, the March invoice is attached. Call me on 0171 5550123 if anything looks off.";
    let screens = [
        (400_000, screen(theme, "", "", "", false)),
        (
            6_000_000,
            screen(theme, "Ada Lovelace", "Invoice for March", body, false),
        ),
        (
            31_500_000,
            screen(theme, "Ada Lovelace", "Invoice for March", body, true),
        ),
    ];
    let attach = control(&screens[1].1, "Attach");
    let send = control(&screens[1].1, "Send");
    let mut timeline: Vec<TimedEvent> = screens
        .iter()
        .map(|(at, screen)| keyframe(*at, screen.reply.clone()))
        .collect();
    timeline.extend([
        step(
            9_000_000,
            1,
            InteractionPhase::Applying,
            "Attach",
            attach.as_deref(),
        ),
        step(
            11_000_000,
            1,
            InteractionPhase::Committed,
            "Attach",
            attach.as_deref(),
        ),
        step(
            31_000_000,
            2,
            InteractionPhase::Failed {
                code: None,
                message: "the server refused the attachment".into(),
            },
            "Send",
            send.as_deref(),
        ),
    ]);
    timeline.sort_by_key(|event| event.at);
    let trace = SessionTrace {
        origin: Origin::Explicit,
        started_unix_ms: 0,
        duration: Offset(42_000_000),
        environment: Environment::default(),
        snapshots: Vec::new(),
        timeline,
        tags: Vec::new(),
        layer: SessionLayer::Logic,
        redactions: Redactions::default(),
        truncated: false,
    };
    let mut strip = FrameStripBuilder::new(PixelPurpose::Keep, FrameLimits::default());
    for (at, screen) in screens {
        let keyframe = InspectKeyframe {
            route: route(),
            reply: screen.reply,
        };
        let pushed = SurfaceFrame::new(
            Offset(at),
            route(),
            VIEWPORT.0,
            VIEWPORT.1,
            1.0,
            screen.rgba,
        )
        .and_then(|frame| strip.push(frame, &keyframe_regions(&keyframe)));
        if let Err(error) = pushed {
            tracing::warn!(%error, "sample recording: a frame was not kept");
        }
    }
    let frames = strip.finish(&trace).ok();
    SampleRecording { trace, frames }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sample_is_a_real_window_with_its_frames_masked() {
        let recording = sample_recording(&ui::Theme::dark());
        let keyframes: Vec<&InspectKeyframe> = recording
            .trace
            .timeline
            .iter()
            .filter_map(|entry| match &entry.event {
                Event::Inspect(keyframe) => Some(keyframe),
                _ => None,
            })
            .collect();
        assert_eq!(keyframes.len(), 3);
        let to = keyframes[1]
            .reply
            .nodes
            .iter()
            .find(|node| node.label.as_deref() == Some("To"))
            .expect("the To field");
        assert_eq!(to.sensitivity, tilo_actions::Sensitivity::Personal);
        let frames = recording.frames.expect("frames");
        assert_eq!(frames.frames.len(), 3);
        assert!(frames.frames.iter().all(|frame| !frame.masked.is_empty()));
    }
}
