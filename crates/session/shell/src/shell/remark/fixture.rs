//! A recorded session to show the walk-through with, for `shots` and
//! tests: someone writes to Ada in Mail, attaches a file, presses Send and
//! it fails. The screens are a real toolkit window, laid out and answered
//! by the headless driver the way the native host answers an inspection,
//! and its frames are masked by the recorder's own frame strip. Never used
//! on the live path, which only ever reviews what the recorder recorded.

use std::cell::Cell;
use std::rc::Rc;

use session_trace::frames::{
    FrameError, FrameLimits, FrameProvider, FrameStrip, FrameStripBuilder, PixelPurpose,
    SurfaceFrame, keyframe_regions,
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

/// The app the sample session records.
pub const APP: &str = "so.tilo.mail";
const ROOM: &str = "room";
const VIEWPORT: (u32, u32) = (720, 480);

/// How the failure is shown: as the app draws it, or as a replay whose
/// renderer loses the banner's message, which is what a problem below the
/// app looks like.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Failure {
    None,
    Shown,
    LostBelowTheApp,
}

/// Mail's compose window, with the draft as far as it had been written.
/// `on_send` is what pressing Send does, in a live window.
fn compose(
    to: &str,
    subject: &str,
    body: &str,
    failure: Failure,
    on_send: Option<Rc<Cell<bool>>>,
) -> Column {
    let send = Button::new("send", "Send", None).primary();
    let send = match on_send.clone() {
        Some(pressed) => send.on_effect(move || pressed.set(true)),
        None => send,
    };
    let window = Column::new()
        .with_gap(8.0)
        .push(Box::new(
            Row::new()
                .with_gap(8.0)
                .push(Box::new(send))
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
    if let Some(sent) = on_send {
        return window.push(Box::new(SendFailure {
            banner: Banner::error("Couldn't send")
                .with_message("The server refused the attachment")
                .hidden(),
            sent,
        }));
    }
    match failure {
        Failure::None => window,
        Failure::Shown => window.push(Box::new(
            Banner::error("Couldn't send").with_message("The server refused the attachment"),
        )),
        Failure::LostBelowTheApp => window.push(Box::new(Banner::error("Couldn't send"))),
    }
}

/// Mail's compose window as a live app, for end-to-end tests: the sample
/// session's draft, ready to send. Pressing Send fails, and the window
/// then says why under the draft.
pub fn mail_window() -> Column {
    compose(
        "Ada Lovelace",
        "Invoice for March",
        BODY,
        Failure::None,
        Some(Rc::new(Cell::new(false))),
    )
}

/// The banner saying why sending failed, shown once Send was pressed.
struct SendFailure {
    banner: Banner,
    sent: Rc<Cell<bool>>,
}

impl ui::widget::Decorator for SendFailure {
    type Inner = Banner;

    fn inner(&self) -> &Banner {
        &self.banner
    }

    fn inner_mut(&mut self) -> &mut Banner {
        &mut self.banner
    }

    fn update(&mut self, _state: &mut ui::widget::AppModel, _path: &ui::widget::ModelPath) -> bool {
        if self.sent.get() && self.banner.is_dismissed() {
            self.banner.set_shown(true);
            return true;
        }
        false
    }
}

/// One screen as the app answers an inspection of it, and as it looked.
struct Screen {
    reply: InspectReply,
    rgba: Vec<u8>,
}

fn screen(theme: &ui::Theme, to: &str, subject: &str, body: &str, failure: Failure) -> Screen {
    let driver = ui::headless::Driver::new_with_theme(
        ui::State::new(),
        Box::new(compose(to, subject, body, failure, None)),
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

const BODY: &str =
    "Hi Ada, the March invoice is attached. Call me on 0171 5550123 if anything looks off.";

/// The session's screens, by when they were shown, the failure drawn as
/// `failure`.
fn screens(theme: &ui::Theme, failure: Failure) -> [(u64, Screen); 3] {
    [
        (400_000, screen(theme, "", "", "", Failure::None)),
        (
            6_000_000,
            screen(
                theme,
                "Ada Lovelace",
                "Invoice for March",
                BODY,
                Failure::None,
            ),
        ),
        (
            31_500_000,
            screen(theme, "Ada Lovelace", "Invoice for March", BODY, failure),
        ),
    ]
}

/// The sample session, its screens drawn in `theme`.
pub fn sample_recording(theme: &ui::Theme) -> SampleRecording {
    let screens = screens(theme, Failure::Shown);
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

/// Replays the sample session the way this device's offscreen replay
/// would, except that its renderer loses the failure banner's message: the
/// check finds the frames differ there, below the app.
pub struct SampleReplay {
    theme: ui::Theme,
}

impl SampleReplay {
    pub const fn new(theme: ui::Theme) -> Self {
        Self { theme }
    }
}

impl super::OffscreenReplay for SampleReplay {
    fn start(&self, _record: &ui::record::RecordId, _frames: &FrameStrip) -> Result<u64, String> {
        Ok(1)
    }

    fn take(&self, _check: u64) -> Option<Result<Box<dyn FrameProvider + Send>, String>> {
        let frames = screens(&self.theme, Failure::LostBelowTheApp)
            .into_iter()
            .map(|(at, screen)| {
                SurfaceFrame::new(
                    Offset(at),
                    route(),
                    VIEWPORT.0,
                    VIEWPORT.1,
                    1.0,
                    screen.rgba,
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string());
        Some(frames.map(|frames| Box::new(Replayed { frames }) as Box<dyn FrameProvider + Send>))
    }
}

struct Replayed {
    frames: Vec<SurfaceFrame>,
}

impl FrameProvider for Replayed {
    fn frames_at(&mut self, at: Offset) -> Result<Vec<SurfaceFrame>, FrameError> {
        Ok(self
            .frames
            .iter()
            .rfind(|frame| frame.at <= at)
            .cloned()
            .into_iter()
            .collect())
    }
}

/// What the AI hub offers for review on a laptop with an RTX 4070 beside a
/// Radeon 780M: vLLM on the card, then llama.cpp on the integrated GPU, the
/// processor and the card. Sample data lives only here.
#[must_use]
pub fn sample_offer() -> Vec<super::review::SetUpChoice> {
    let choice = |rank: usize, label: &str, terms: &str, fit: &str, speed: &str| {
        super::review::SetUpChoice {
            action: if rank == 0 {
                "set-up-on-device".into()
            } else {
                format!("set-up-on-device-{rank}")
            },
            label: format!("Set up {label} for Feedback review"),
            terms: terms.into(),
            fit: Some(fit.into()),
            speed: Some(speed.into()),
        }
    };
    vec![
        choice(
            0,
            "Qwen3 4B (AWQ) with vLLM on NVIDIA GeForce RTX 4070 Laptop GPU",
            "2.5 GiB download, 2.5 GiB on disk · License Apache-2.0 · Runs on this device · Needs 6.0 of 8.0 GiB graphics memory · Fast here, about 90 words a second",
            "fit-good",
            "speed-interactive",
        ),
        choice(
            1,
            "Qwen3 4B (GGUF) with llama.cpp on AMD Radeon 780M Graphics",
            "2.3 GiB download, 2.3 GiB on disk · License Apache-2.0 · Runs on this device · Needs 4.4 GiB of the memory the desktop also uses, 15.8 GiB to spare · Slow here, about 29 seconds an answer",
            "fit-excellent",
            "speed-slow",
        ),
        choice(
            2,
            "Qwen3 4B (GGUF) with llama.cpp on the processor",
            "2.3 GiB download, 2.3 GiB on disk · License Apache-2.0 · Runs on this device · Needs 3.8 GiB of the memory the desktop also uses, 18.0 GiB to spare · Slow here, about 66 seconds an answer",
            "fit-excellent",
            "speed-slow",
        ),
        choice(
            3,
            "Qwen3 4B (GGUF) with llama.cpp on NVIDIA GeForce RTX 4070 Laptop GPU",
            "2.3 GiB download, 2.3 GiB on disk · License Apache-2.0 · Runs on this device · Needs 4.4 of 8.0 GiB graphics memory · Slow here, about 78 seconds an answer",
            "fit-good",
            "speed-slow",
        ),
    ]
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

    #[test]
    fn a_replay_that_loses_the_banner_message_is_found_below_the_app_on_the_failed_step() {
        use super::super::OffscreenReplay as _;
        let theme = ui::Theme::dark();
        let recording = sample_recording(&theme);
        let frames = recording.frames.expect("frames");
        let replay = SampleReplay::new(theme);
        let id = ui::record::RecordId::parse("uuid:5a3e0000-0000-4000-8000-000000000001").unwrap();
        let check = replay.start(&id, &frames).unwrap();
        let mut provider = replay.take(check).expect("ready").expect("replayed");
        let report = session_trace::divergence::check(
            &recording.trace,
            &frames,
            provider.as_mut(),
            &session_trace::divergence::Tolerance::default(),
        )
        .unwrap();
        assert_eq!(report.layer, SessionLayer::BelowToolkit);
        // The banner is drawn differently, and only there do pixels differ.
        let differs: Vec<Offset> = report
            .steps
            .iter()
            .filter(|step| {
                step.regions.iter().any(|region| {
                    matches!(
                        region.divergence,
                        session_trace::divergence::RegionDivergence::Differs { .. }
                    )
                })
            })
            .map(|step| step.at)
            .collect();
        assert_eq!(differs, vec![Offset(31_500_000)]);
        assert_eq!(report.steps.len(), 1, "a real window is described whole");
        assert_eq!(
            super::super::review::diverging_places(&report).len(),
            1,
            "the banner is one place, not also the window around it: {report:?}"
        );

        let mut review =
            super::super::review::Review::from_trace("uuid:1", &recording.trace, &|_| {
                "Mail".into()
            });
        let failed = review
            .steps
            .iter()
            .find(|step| step.failed())
            .map(|step| step.index)
            .unwrap();
        review.verdict = super::super::review::Verdict::Checked(report);
        assert!(!review.divergent_regions(failed).is_empty());
        let differs_at = |step: u32| {
            review.divergent_regions(step).iter().any(|(_, region)| {
                matches!(
                    region.divergence,
                    session_trace::divergence::RegionDivergence::Differs { .. }
                )
            })
        };
        assert!(differs_at(failed));
        assert!(!differs_at(0));
        assert_eq!(review.verdict.layer(), Some(SessionLayer::BelowToolkit));
    }
}
