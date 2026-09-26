//! A recorded session to show the walk-through with, for `shots` and
//! tests: someone writes to Ada in Mail, attaches a file, presses Send and
//! it fails. Never used on the live path, which only ever reviews what the
//! recorder recorded.

use session_trace::{
    Environment, Event, InspectKeyframe, Offset, Origin, PresenceChange, PresenceUpdate,
    Redactions, Route, SessionLayer, SessionTrace, TimedEvent,
};
use tilo_actions::{
    ActionApp, ActionResource, InteractionAction, InteractionActor, InteractionActorKind,
    InteractionAnchor, InteractionPhase, InvocationId, SemanticInteraction, Sensitivity,
};
use tilo_reference::inspect::{
    Content, Identity, InspectReply, InspectedNode, NodeState, Pixels, SurfaceRect,
};

const APP: &str = "so.tilo.mail";

fn node(
    id: &str,
    role: &str,
    label: &str,
    value: Option<&str>,
    sensitivity: Sensitivity,
    rect: (f64, f64, f64, f64),
    children: &[&str],
) -> InspectedNode {
    let bounds = SurfaceRect {
        x: rect.0,
        y: rect.1,
        width: rect.2,
        height: rect.3,
    };
    InspectedNode {
        id: id.into(),
        identity: Identity::Durable,
        role: role.into(),
        label: Some(label.into()),
        value: value.map(Into::into),
        sensitivity,
        content: if value.is_some() {
            Content::Value
        } else {
            Content::Label
        },
        bounds: Some(bounds),
        visible: Some(bounds),
        pixels: Pixels::Described,
        state: NodeState::default(),
        children: children.iter().map(ToString::to_string).collect(),
        reference: None,
    }
}

/// Mail's window, with the draft as far as it had been written.
fn screen(to: &str, subject: &str, body: &str, banner: bool) -> Vec<InspectedNode> {
    let mut children = vec!["t:toolbar", "t:to", "t:subject", "t:body"];
    if banner {
        children.insert(0, "t:banner");
    }
    let top = if banner { 44.0 } else { 0.0 };
    let mut nodes = vec![node(
        "t:window",
        "window",
        "New message",
        None,
        Sensitivity::Public,
        (0.0, 0.0, 720.0, 480.0),
        &children,
    )];
    if banner {
        nodes.push(node(
            "t:banner",
            "alert",
            "Couldn't send: the server refused the attachment",
            None,
            Sensitivity::Public,
            (0.0, 0.0, 720.0, 40.0),
            &[],
        ));
    }
    nodes.extend([
        node(
            "t:toolbar",
            "toolbar",
            "Message",
            None,
            Sensitivity::Public,
            (0.0, top, 720.0, 44.0),
            &["t:send", "t:attach", "t:discard"],
        ),
        node(
            "t:send",
            "button",
            "Send",
            None,
            Sensitivity::Public,
            (12.0, top + 8.0, 72.0, 28.0),
            &[],
        ),
        node(
            "t:attach",
            "button",
            "Attach",
            None,
            Sensitivity::Public,
            (92.0, top + 8.0, 80.0, 28.0),
            &[],
        ),
        node(
            "t:discard",
            "button",
            "Discard",
            None,
            Sensitivity::Public,
            (180.0, top + 8.0, 84.0, 28.0),
            &[],
        ),
        node(
            "t:to",
            "text_input",
            "To",
            Some(to),
            Sensitivity::Personal,
            (12.0, top + 56.0, 696.0, 32.0),
            &[],
        ),
        node(
            "t:subject",
            "text_input",
            "Subject",
            Some(subject),
            Sensitivity::Public,
            (12.0, top + 96.0, 696.0, 32.0),
            &[],
        ),
        node(
            "t:body",
            "text_input",
            "Message",
            Some(body),
            Sensitivity::Public,
            (12.0, top + 140.0, 696.0, 300.0 - top),
            &[],
        ),
    ]);
    nodes
}

fn keyframe(at: u64, nodes: Vec<InspectedNode>) -> TimedEvent {
    TimedEvent {
        at: Offset(at),
        event: Event::Inspect(InspectKeyframe {
            route: Route::new(APP, "room", ""),
            reply: InspectReply {
                nodes,
                ..InspectReply::empty()
            },
        }),
    }
}

fn step(at: u64, invocation: u128, phase: InteractionPhase, label: &str) -> TimedEvent {
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
                anchor: InteractionAnchor::Resource,
                group: None,
            })),
        }),
    }
}

/// The sample session.
pub fn sample_session() -> SessionTrace {
    let body =
        "Hi Ada, the March invoice is attached. Call me on 0171 5550123 if anything looks off.";
    SessionTrace {
        origin: Origin::Explicit,
        started_unix_ms: 0,
        duration: Offset(42_000_000),
        environment: Environment::default(),
        snapshots: Vec::new(),
        timeline: vec![
            keyframe(400_000, screen("", "", "", false)),
            keyframe(
                6_000_000,
                screen("Ada Lovelace", "Invoice for March", body, false),
            ),
            step(9_000_000, 1, InteractionPhase::Applying, "Attach"),
            step(11_000_000, 1, InteractionPhase::Committed, "Attach"),
            step(
                31_000_000,
                2,
                InteractionPhase::Failed {
                    code: None,
                    message: "the server refused the attachment".into(),
                },
                "Send",
            ),
            keyframe(
                31_500_000,
                screen("Ada Lovelace", "Invoice for March", body, true),
            ),
        ],
        tags: Vec::new(),
        layer: SessionLayer::Logic,
        redactions: Redactions::default(),
        truncated: false,
    }
}
