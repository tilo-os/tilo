//! An agent's actions narrated on the live plane: the semantic work its
//! device publishes on a record's stream as each action goes ([`frame`]),
//! and what a page admits of it ([`admit`]). A person watching sees the
//! agent the way they see a colleague: its cursor travelling to the control
//! each action is bound to.

use collab_types::presence::Shown;
use tilo_actions::{
    Acting, ActionAddress, ActionApp, ActionResource, INTERACTION_VERSION, InteractionAction,
    InteractionAnchor, InteractionPhase, InvocationId, SemanticInteraction,
};
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
    /// The run the workspace's host narrates this for, when the host does:
    /// an agent's or a key's run it executes.
    pub on_behalf: Option<&'a str>,
}

/// `narrated` as the live plane carries it, numbered `seq` on the agent's
/// stream. `None` for work that does not hold.
#[must_use]
pub fn frame(narrated: &Narrated<'_>, seq: u64, sent_at_ms: u64) -> Option<SemanticFrame> {
    let app = ActionApp {
        id: narrated.app.to_owned(),
        label: tilo_actions::app_label(narrated.app),
    };
    let interaction = SemanticInteraction {
        version: INTERACTION_VERSION,
        invocation: narrated.invocation,
        sequence: seq,
        actor: Acting::agent(narrated.agent),
        phase: narrated.phase.clone(),
        action: InteractionAction {
            id: narrated.action.action.clone(),
            label: tilo_actions::action_label(&narrated.action.action),
            app: Some(app.clone()),
            step: None,
            creates: None,
            did: None,
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
        on_behalf: narrated.on_behalf.map(str::to_owned),
    })
}

/// The semantic work `event` carries, as `from` did it, on a page that has `here` as its members:
/// `None` unless `from` is one of them as an agent and the work is well formed and names the
/// execution its frame does. Its actor is that member, whatever the work claims: a device says only
/// what its own agent does.
///
/// Work the workspace's host narrates for a run it executes ([`SemanticFrame::on_behalf`]) is the
/// member it speaks for that run ([`collab_types::presence::stand_in`]), which the page's board
/// admitted only from the workspace's own host: `None` from any other device.
#[must_use]
pub fn admit(here: &[Shown], from: [u8; 32], event: &SemanticFrame) -> Option<SemanticInteraction> {
    let sender = hex::encode(from);
    let publisher = match &event.on_behalf {
        Some(run) => hex::encode(collab_types::presence::stand_in(from, run)),
        None => sender.clone(),
    };
    let member = here.iter().find(|member| member.publisher == publisher)?;
    member.agent.then_some(())?;
    if event.on_behalf.is_some() && member.spoken_by.as_deref() != Some(sender.as_str()) {
        return None;
    }
    let mut interaction: SemanticInteraction =
        serde_json::from_str(&event.interaction_json).ok()?;
    if interaction.invocation.0 != event.execution {
        return None;
    }
    interaction.actor = Acting::agent(publisher);
    interaction.validate().ok()?;
    Some(interaction)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test the unwrap is the assertion"
    )]

    use super::*;

    /// The members of a page's workspace on which `agent` is here as an
    /// agent and `person` as a person.
    fn here(agent: [u8; 32], person: [u8; 32]) -> Vec<Shown> {
        [(agent, true, "Claude Code"), (person, false, "Bob")]
            .into_iter()
            .map(|(publisher, agent, label)| Shown {
                publisher: hex::encode(publisher),
                epoch: 1,
                agent,
                label: label.into(),
                colour: "#81a8ff".into(),
                ..Shown::default()
            })
            .collect()
    }

    tilo_test_env::scenario! {
        fn narrated_work_is_drawn_only_as_the_member_that_did_it;

        phase an_action_is_narrated_as_the_agents_work_on_its_record;
        // An action is narrated as work a page admits from the agent that did
        // it, named as a person reads it, by the execution it is.
        {
            let invocation = InvocationId::new();
            let action = ActionAddress::new("workbook", "new-sheet");
            let event = frame(
                &Narrated {
                    agent: "Claude Code",
                    invocation,
                    app: "so.tilo.sheets",
                    action: &action,
                    phase: InteractionPhase::Applying,
                    on_behalf: None,
                },
                3,
                1_700_000_000_000,
            )
            .expect("a well-formed action is narrated");
            assert_eq!((event.execution, event.seq), (invocation.0, 3));
            let agent = [7; 32];
            let admitted = admit(&here(agent, [8; 32]), agent, &event).expect("admitted");
            assert_eq!(admitted.invocation, invocation);
            assert_eq!(admitted.action.label, "New sheet");
            assert_eq!(admitted.resource.id, "workbook");
            assert_eq!(admitted.resource.label, "Sheets");
            assert_eq!(admitted.phase, InteractionPhase::Applying);
        }

        phase only_an_agent_here_narrates_work_and_only_as_itself;
        // Narrated work is drawn only as the agent here that published it: a
        // person, a stranger, or work naming another execution is not drawn,
        // and whoever the work claims to be, it is that agent's.
        {
            let (agent, person, stranger) = ([7; 32], [8; 32], [9; 32]);
            let present = here(agent, person);
            let action = ActionAddress::new("workbook", "new-sheet");
            let invocation = InvocationId::new();
            let event = frame(
                &Narrated {
                    agent: "someone-else",
                    invocation,
                    app: "so.tilo.sheets",
                    action: &action,
                    phase: InteractionPhase::Committed,
                    on_behalf: None,
                },
                1,
                1,
            )
            .unwrap();

            let admitted = admit(&present, agent, &event).expect("the agent's work is drawn");
            assert_eq!(
                admitted.actor,
                Acting::agent(hex::encode(agent)),
                "bound to the publisher"
            );

            assert!(
                admit(&present, person, &event).is_none(),
                "a person narrates no work"
            );
            assert!(
                admit(&present, stranger, &event).is_none(),
                "nor does a stranger"
            );
            let mut elsewhere = event;
            elsewhere.execution = InvocationId::new().0;
            assert!(
                admit(&present, agent, &elsewhere).is_none(),
                "work that names another execution is not drawn"
            );
            let mut malformed = elsewhere;
            malformed.execution = invocation.0;
            malformed.interaction_json = "{}".into();
            assert!(admit(&present, agent, &malformed).is_none());
        }

        phase the_host_narrates_a_run_as_the_member_it_speaks_for;
        // Work the workspace's host narrates for a run is the member it speaks
        // for that run; the same frame from any device the board did not take
        // as that run's speaker is not drawn.
        {
            let (host, other) = ([5; 32], [6; 32]);
            let run = "agent-session:7";
            let stand_in = collab_types::presence::stand_in(host, run);
            let present = vec![Shown {
                publisher: hex::encode(stand_in),
                epoch: 1,
                agent: true,
                spoken_by: Some(hex::encode(host)),
                label: "Visa backend".into(),
                ..Shown::default()
            }];
            let action = ActionAddress::new("workbook", "new-sheet");
            let event = frame(
                &Narrated {
                    agent: "Visa backend",
                    invocation: InvocationId::new(),
                    app: "so.tilo.sheets",
                    action: &action,
                    phase: InteractionPhase::Committed,
                    on_behalf: Some(run),
                },
                1,
                1,
            )
            .unwrap();
            let admitted = admit(&present, host, &event).expect("the host's word for its run");
            assert_eq!(admitted.actor, Acting::agent(hex::encode(stand_in)));
            assert!(
                admit(&present, other, &event).is_none(),
                "another device does not speak for the host's run"
            );
            let mut unspoken = present;
            unspoken[0].spoken_by = None;
            assert!(admit(&unspoken, host, &event).is_none());
        }
    }
}
