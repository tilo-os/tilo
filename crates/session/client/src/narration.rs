//! An agent's actions narrated on the live plane: the semantic work its
//! device publishes on a record's stream as each action goes ([`frame`]),
//! and what a page admits of it ([`admit`]). A person watching sees the
//! agent the way they see a colleague: its cursor travelling to the control
//! each action is bound to.

use collab_types::presence::Shown;
use tilo_actions::{
    ActionAddress, ActionApp, ActionResource, ActorKind, INTERACTION_VERSION, InteractionAction,
    InteractionActor, InteractionAnchor, InteractionPhase, InvocationId, SemanticInteraction,
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
            step: None,
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
    interaction.actor = InteractionActor::new(publisher, ActorKind::Agent);
    interaction.validate().ok()?;
    Some(interaction)
}

/// The longest line said beside a run's face, the ellipsis included: what a
/// person reads at a glance where the roster draws it.
const SHOWN_CHARS: usize = 64;

/// What a run is at, in a person's words, said beside its face: the host's
/// own word for each request a run makes ([`looking`], [`reading`],
/// [`searching`], [`doing`]), cut where a person stops reading.
pub mod lines {
    use collab_types::presence::MAX_ACTIVITY_BYTES;
    use serde_json::Value;
    use tilo_actions::StepKind;
    use tilo_actions::step::{COMMAND_PARAMS, Step};

    /// An app as a person reads it: the last part of its id, capitalised
    /// (`Sheets`).
    fn short(app: &str) -> String {
        let mut letters = app.rsplit('.').next().unwrap_or(app).chars();
        letters
            .next()
            .map(|first| first.to_uppercase().chain(letters).collect())
            .unwrap_or_default()
    }

    /// Connected, and asked nothing yet: an agent thinking, or waiting on
    /// its person.
    #[must_use]
    pub fn waiting() -> String {
        "Waiting".to_owned()
    }

    /// Looking at the apps, or at the one `app` names.
    #[must_use]
    pub fn looking(app: Option<&str>) -> String {
        match app.map(short).filter(|app| !app.is_empty()) {
            Some(app) => clip(&format!("Looking at {app}")),
            None => "Looking at the apps".to_owned(),
        }
    }

    /// Reading what `app` shows.
    #[must_use]
    pub fn reading(app: &str) -> String {
        let record = short(app);
        clip(&ui::step::line(&Step::new(StepKind::Read, Some(&record))))
    }

    /// Searching for `needle`.
    #[must_use]
    pub fn searching(needle: &str) -> String {
        clip(&verb_line("Searching for", needle))
    }

    /// Taking the action `action` in `app` with `args`: what kind of step its
    /// own declaration names, on the app's record, a shell command as the
    /// command line itself. Said in the window's language by the one function
    /// every surface asks ([`ui::step`]).
    #[must_use]
    pub fn doing(app: &str, action: &str, args: Option<&Value>) -> String {
        let command = COMMAND_PARAMS
            .iter()
            .find_map(|key| args.and_then(|args| args.get(*key)))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|command| !command.is_empty());
        let kind = if action == tilo_actions::CREATE_ACTION {
            StepKind::Create
        } else if command.is_some() {
            StepKind::Run
        } else {
            StepKind::Work
        };
        let record = short(app);
        let step = Step::new(kind, Some(&record)).running(command);
        clip(&ui::step::line(&step))
    }

    /// `line`, for an action that failed: never said as one that worked.
    #[must_use]
    pub fn failed(line: &str) -> String {
        clip(&format!("{line} (failed)"))
    }

    /// `step` as a person reads it beside a run's face: the one line every
    /// surface says of a step ([`ui::step`]), cut where a person stops
    /// reading.
    #[must_use]
    pub fn step(step: &Step<'_>) -> String {
        clip(&ui::step::line(step))
    }

    /// `<verb> <what>` as one line, the what left out when there is none.
    fn verb_line(verb: &str, what: &str) -> String {
        let what = what.trim();
        if what.is_empty() {
            verb.to_owned()
        } else {
            format!("{verb} {what}")
        }
    }

    /// `line` as far as a person reads it at a glance, and within the bytes
    /// a presence frame carries.
    fn clip(line: &str) -> String {
        let mut kept = String::new();
        for character in line.chars() {
            if kept.chars().count().saturating_add(2) > super::SHOWN_CHARS
                || kept.len().saturating_add(character.len_utf8()) > MAX_ACTIVITY_BYTES
            {
                kept.push('…');
                return kept;
            }
            kept.push(character);
        }
        kept.trim().to_owned()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Each request is said as what the run is at now, on what.
        #[test]
        fn a_request_is_said_as_what_the_run_is_doing() {
            assert_eq!(looking(None), "Looking at the apps");
            assert_eq!(looking(Some("so.tilo.terminal")), "Looking at Terminal");
            assert_eq!(reading("so.tilo.notes"), "Reading Notes");
            assert_eq!(searching(" invoices "), "Searching for invoices");
            assert_eq!(
                doing("so.tilo.sheets", tilo_actions::CREATE_ACTION, None),
                "Creating in Sheets"
            );
            assert_eq!(
                doing("so.tilo.word-processor", "append-paragraph", None),
                "Working in Word-processor"
            );
            let command = serde_json::json!({"command": "cargo build"});
            assert_eq!(
                doing("so.tilo.terminal", "run", Some(&command)),
                "Running cargo build"
            );
            assert_eq!(
                doing("so.tilo.terminal", "run", None),
                "Working in Terminal"
            );
            assert_eq!(failed("Working in Sheets"), "Working in Sheets (failed)");
        }

        /// A step is said in the catalogue's words for its kind, on the record it acts in:
        /// "Writing in Trip plan", never words assembled from the action's English label.
        #[test]
        fn a_step_is_said_on_the_record_it_acts_in() {
            assert_eq!(
                step(&Step::new(StepKind::Write, Some("Trip plan"))),
                "Writing in Trip plan"
            );
            assert_eq!(
                step(&Step::new(StepKind::Delete, Some("Trip plan"))),
                "Deleting Trip plan"
            );
            let long = step(&Step::new(StepKind::Write, Some(&"x".repeat(200))));
            assert!(long.ends_with('…') && long.chars().count() <= SHOWN_CHARS);
        }

        /// A long line is cut where a person stops reading.
        #[test]
        fn a_long_line_is_cut_where_a_person_stops_reading() {
            let long = searching(&"x".repeat(200));
            assert!(long.ends_with('…') && long.chars().count() <= SHOWN_CHARS);
        }

        use super::super::SHOWN_CHARS;
    }
}

/// An app's name as a person reads it: the last word of its id, capitalised.
fn app_label(app: &str) -> String {
    capitalised(app.rsplit('.').next().unwrap_or(app)).unwrap_or_else(|| app.to_owned())
}

/// An action's id as the words a person reads (`append-paragraph` is
/// "Append paragraph").
#[must_use]
pub fn action_label(action: &str) -> String {
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

    /// Narrated work is drawn only as the agent here that published it: a
    /// person, a stranger, or work naming another execution is not drawn,
    /// and whoever the work claims to be, it is that agent's.
    #[test]
    fn only_an_agent_here_narrates_work_and_only_as_itself() {
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
            admitted.actor.id,
            hex::encode(agent),
            "bound to the publisher"
        );
        assert_eq!(admitted.actor.kind, ActorKind::Agent);
        assert_eq!(admitted.actor.for_person, None);

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

    /// Work the workspace's host narrates for a run is the member it speaks
    /// for that run; the same frame from any device the board did not take
    /// as that run's speaker is not drawn.
    #[test]
    fn the_host_narrates_a_run_as_the_member_it_speaks_for() {
        let (host, other) = ([5; 32], [6; 32]);
        let run = "agent-run:7";
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
        assert_eq!(admitted.actor.id, hex::encode(stand_in));
        assert!(
            admit(&present, other, &event).is_none(),
            "another device does not speak for the host's run"
        );
        let mut unspoken = present;
        unspoken[0].spoken_by = None;
        assert!(admit(&unspoken, host, &event).is_none());
    }
}
