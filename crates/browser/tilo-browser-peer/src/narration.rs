//! An agent's actions narrated on the live plane: the semantic work its
//! device publishes on a record's stream as each action goes ([`frame`]),
//! and what a page admits of it ([`admit`]). A person watching sees the
//! agent the way they see a colleague: its cursor travelling to the control
//! each action is bound to.

use tilo_actions::{
    ActionAddress, ActionApp, ActionResource, ActorKind, INTERACTION_VERSION, InteractionAction,
    InteractionActor, InteractionAnchor, InteractionPhase, InvocationId, SemanticInteraction,
};
use tilo_live::presence::Board;
use tilo_relay_protocol::live::SemanticFrame;

/// The instance a narration names as its source: an agent acts through the
/// workspace's host, in no app instance of its own.
const NO_INSTANCE: u64 = 1;

/// One action an agent takes in a record of the app `app`, at `phase`.
pub struct Narrated<'a> {
    /// What the agent calls itself; a receiver binds the work to the member
    /// that published it, whatever this says.
    pub agent: &'a str,
    pub invocation: InvocationId,
    pub app: &'a str,
    /// The action, on the resource it is published on.
    pub action: &'a ActionAddress,
    pub phase: InteractionPhase,
}

/// `narrated` as the live plane carries it, numbered `seq` on the agent's
/// stream. `None` for work that does not hold.
#[must_use]
pub fn frame(narrated: &Narrated<'_>, seq: u64, sent_at_ms: u64) -> Option<SemanticFrame> {
    let app = ActionApp {
        id: narrated.app.to_owned(),
        label: app_label(narrated.app),
    };
    let interaction = SemanticInteraction {
        version: INTERACTION_VERSION,
        invocation: narrated.invocation,
        sequence: seq,
        actor: InteractionActor::new(narrated.agent, ActorKind::Agent),
        phase: narrated.phase.clone(),
        action: InteractionAction {
            id: narrated.action.action.clone(),
            label: action_label(&narrated.action.action),
            app: Some(app.clone()),
        },
        resource: ActionResource {
            id: narrated.action.resource.clone(),
            kind: app.id,
            label: app.label,
        },
        anchor: InteractionAnchor::Resource,
        group: None,
    };
    interaction.validate().ok()?;
    Some(SemanticFrame {
        version: 1,
        seq,
        sent_at_ms: sent_at_ms.max(1),
        execution: narrated.invocation.0,
        peer: NO_INSTANCE,
        interaction_json: serde_json::to_string(&interaction).ok()?,
    })
}

/// The semantic work `event` carries, as `board`'s member `from` did it:
/// `None` unless `from` is here as an agent and the work is well formed and
/// names the execution its frame does. Its actor is that member, whatever
/// the work claims: a device says only what its own agent does.
#[must_use]
pub fn admit(board: &Board, from: [u8; 32], event: &SemanticFrame) -> Option<SemanticInteraction> {
    board
        .member(from)
        .filter(|member| member.here && member.agent())?;
    let mut interaction: SemanticInteraction =
        serde_json::from_str(&event.interaction_json).ok()?;
    if interaction.invocation.0 != event.execution {
        return None;
    }
    interaction.actor = InteractionActor::new(hex::encode(from), ActorKind::Agent);
    interaction.validate().ok()?;
    Some(interaction)
}

/// An app's name as a person reads it: the last word of its id, capitalised.
fn app_label(app: &str) -> String {
    capitalised(app.rsplit('.').next().unwrap_or(app)).unwrap_or_else(|| app.to_owned())
}

/// An action's id as the words a person reads (`append-paragraph` is
/// "Append paragraph").
fn action_label(action: &str) -> String {
    capitalised(action.replace(['-', '_'], " ").trim()).unwrap_or_else(|| action.to_owned())
}

fn capitalised(words: &str) -> Option<String> {
    let mut characters = words.chars();
    let first = characters.next()?;
    Some(first.to_uppercase().chain(characters).collect())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test the unwrap is the assertion"
    )]

    use super::*;
    use collab_types::presence::{Payload, Presence, ShellPresence};

    const WORKSPACE: &str = "ws";

    /// A board on which `agent` is here as an agent and `person` as a
    /// person.
    fn board(agent: [u8; 32], person: [u8; 32]) -> Board {
        let mut board = Board::new(WORKSPACE, [1; 32]);
        for (publisher, is_agent, label) in [(agent, true, "Claude Code"), (person, false, "Bob")] {
            let shell = ShellPresence {
                seq: 1,
                sent_at_ms: 1,
                label: label.into(),
                agent: is_agent,
                colour: "#81a8ff".into(),
                activity: None,
                place: None,
                size: [0, 0],
                windows: Vec::new(),
                selection: None,
                chat: None,
                reaction: None,
            };
            let frame = Presence::desktop(WORKSPACE, Payload::Shell(Box::new(shell)));
            board.hear(publisher, &frame, 1.0);
        }
        board
    }

    /// An action is narrated as work a page admits from the agent that did
    /// it, named as a person reads it, by the execution it is.
    #[test]
    fn an_action_is_narrated_as_the_agents_work_on_its_record() {
        let invocation = InvocationId::new();
        let action = ActionAddress::new("workbook", "new-sheet");
        let event = frame(
            &Narrated {
                agent: "Claude Code",
                invocation,
                app: "so.tilo.sheets",
                action: &action,
                phase: InteractionPhase::Applying,
            },
            3,
            1_700_000_000_000,
        )
        .expect("a well-formed action is narrated");
        assert_eq!((event.execution, event.seq), (invocation.0, 3));
        let agent = [7; 32];
        let admitted = admit(&board(agent, [8; 32]), agent, &event).expect("admitted");
        assert_eq!(admitted.invocation, invocation);
        assert_eq!(admitted.action.label, "New sheet");
        assert_eq!(admitted.resource.id, "workbook");
        assert_eq!(admitted.resource.label, "Sheets");
        assert_eq!(admitted.phase, InteractionPhase::Applying);
    }

    /// Narrated work is drawn only as the agent here that published it: a
    /// person, a stranger, or work naming another execution is not drawn,
    /// and whoever the work claims to be, it is that agent's.
    #[test]
    fn only_an_agent_here_narrates_work_and_only_as_itself() {
        let (agent, person, stranger) = ([7; 32], [8; 32], [9; 32]);
        let board = board(agent, person);
        let action = ActionAddress::new("workbook", "new-sheet");
        let invocation = InvocationId::new();
        let event = frame(
            &Narrated {
                agent: "someone-else",
                invocation,
                app: "so.tilo.sheets",
                action: &action,
                phase: InteractionPhase::Committed,
            },
            1,
            1,
        )
        .unwrap();

        let admitted = admit(&board, agent, &event).expect("the agent's work is drawn");
        assert_eq!(
            admitted.actor.id,
            hex::encode(agent),
            "bound to the publisher"
        );
        assert_eq!(admitted.actor.kind, ActorKind::Agent);
        assert_eq!(admitted.actor.for_person, None);

        assert!(
            admit(&board, person, &event).is_none(),
            "a person narrates no work"
        );
        assert!(
            admit(&board, stranger, &event).is_none(),
            "nor does a stranger"
        );
        let mut elsewhere = event;
        elsewhere.execution = InvocationId::new().0;
        assert!(
            admit(&board, agent, &elsewhere).is_none(),
            "work that names another execution is not drawn"
        );
        let mut malformed = elsewhere;
        malformed.execution = invocation.0;
        malformed.interaction_json = "{}".into();
        assert!(admit(&board, agent, &malformed).is_none());
    }
}
