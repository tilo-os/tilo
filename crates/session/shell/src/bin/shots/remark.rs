//! Shots of the remark surface: pick mode, the composer, review, the
//! recording pill and the crash notification.
//!
//! Every step goes through the shell's own `remark.*` actions or real
//! input, so a shot shows what a person gets. The only seeded data is what
//! a headless render has no live source for: where a remark can go (there
//! is no community configuration or collaborator here) and the recorded
//! session review walks through.

use serde_json::json;
use shell::shell::chrome_state::ChromeState;
use shell::shell::remark::{CommunityRoom, Directory};

use super::driver::ShellDriver;

/// One step of a remark scenario.
#[derive(Debug, Clone)]
pub enum RemarkStep {
    /// Run a shell action with JSON arguments, as `Ctrl+K` would.
    Invoke(&'static str, &'static str),
    /// Offer the Tilo team, the community and a colleague as destinations.
    SeedDirectory,
    /// Attach the sample recording and open review on it.
    SeedReview,
    /// Start a recording as the composer does, without a broker: the pill.
    SeedRecording,
    /// Post the notification an app that quit unexpectedly leaves.
    SeedCrash,
    /// Pick the node at the centre of the first pickable surface's first
    /// button, and its neighbour with Shift.
    PickTwoButtons,
}

pub fn apply(driver: &mut ShellDriver, step: &RemarkStep) {
    match step {
        RemarkStep::Invoke(id, args) => {
            let args = serde_json::from_str::<serde_json::Value>(args)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default();
            driver.with_chrome(|st: &mut ChromeState| {
                if let Err(error) = shell::shell::search::providers::actions::apply(st, id, &args) {
                    eprintln!("shots: {id} refused: {error}");
                }
            });
        }
        RemarkStep::SeedDirectory => {
            let directory = Directory {
                team: Some(("u_7ea7ea7ea7ea7ea7".into(), "Tilo team".into())),
                community: Some(CommunityRoom {
                    room: "0b2e5696-17ae-8826-9e6b-13da8e10ab38".into(),
                    channel: tilo_reference::FEEDBACK_CHANNEL.into(),
                    label: "Tilo Community #feedback".into(),
                    joined: true,
                }),
                people: vec![
                    tilo_reference::RoomMember::new("u_9a0b1c2d3e4f5061", "Ada Lovelace", false),
                    tilo_reference::RoomMember::new("agent_claude", "Claude", true),
                ],
                rooms: vec![("room-launch".into(), "Launch plan".into())],
            };
            let destination = directory.default_destination();
            driver.host_mut().seed_remark_directory_for_shots(directory);
            driver.with_chrome(|st: &mut ChromeState| st.remark.destination = destination);
        }
        RemarkStep::SeedReview => {
            let trace = shell::shell::remark::fixture::sample_session();
            let summary = trace.summary();
            driver.with_chrome(|st: &mut ChromeState| {
                st.remark.flow = shell::shell::remark::Flow::Attached {
                    record: "uuid:5a3e0000-0000-4000-8000-000000000001".into(),
                    summary,
                };
                st.remark.stage = shell::shell::remark::Stage::Review;
            });
            driver
                .host_mut()
                .begin_remark_review("uuid:5a3e0000-0000-4000-8000-000000000001", &trace);
        }
        RemarkStep::SeedRecording => driver.with_chrome(|st: &mut ChromeState| {
            st.remark.flow = shell::shell::remark::Flow::Recording {
                rooms: vec!["room".into()],
            };
            st.remark.stage = shell::shell::remark::Stage::Recording;
            st.bump();
        }),
        RemarkStep::SeedCrash => driver.with_chrome(|st: &mut ChromeState| {
            st.push_notification_full(shell::shell::remark::crash_notification(
                "Mail",
                "so.tilo.mail",
                true,
            ));
        }),
        RemarkStep::PickTwoButtons => {
            let points = {
                let st = driver.chrome();
                let mut out = Vec::new();
                for surface in &st.pick.scene.surfaces {
                    let pickable = |node: &&shell::shell::remark::pick::PickNode| {
                        node.role == "button"
                            || node
                                .label
                                .as_deref()
                                .is_some_and(|label| label == "Start writing")
                    };
                    for node in surface.nodes.iter().filter(pickable) {
                        out.push(surface.to_screen(node.bounds).center());
                        if out.len() == 2 {
                            break;
                        }
                    }
                    if out.len() == 2 {
                        break;
                    }
                }
                out
            };
            for (index, point) in points.into_iter().enumerate() {
                let args = json!({ "x": point.x, "y": point.y, "add": index > 0 });
                let args = args.as_object().cloned().unwrap_or_default();
                driver.with_chrome(|st: &mut ChromeState| {
                    let _ = shell::shell::search::providers::actions::apply(
                        st,
                        "remark.pick.at",
                        &args,
                    );
                });
            }
        }
    }
}
