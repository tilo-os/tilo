//! What came of a person's answer to a question an agent on this server asked tells that agent,
//! once.
//!
//! An agent that needs its person asks ([`super::asks`]) and its turn ends: nothing is done and
//! nothing is paid for while the question waits, an hour or a week, across every restart of
//! this server. Once the answer is carried out (or its person typed a password where no agent
//! reads it), the server asks the agent in its person's name what came of it: an ask of the
//! agent package, made from what the person answered in, under the key `question:<id>`. One
//! key is one ask and one turn however often the server looks and however often it restarts,
//! so an answer tells its asker once and only once. The turn is told what it asked, what its
//! person answered and what came of it, with the conversation around it like any turn.

use crate::asks as tl_asks;

use super::ServerAgents;

/// One turn an answer owes the agent that asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Wake {
    /// The key of the ask: the question's own, so it is asked once.
    key: String,
    /// The person who answered, by their root key, hex: the turn is for them.
    person: String,
    /// The agent that asked, by its id.
    agent: String,
    /// The ask, by its id, whose resource the turn is asked about: the person's answer, or the
    /// ask whose turn asked.
    about: String,
    /// What the agent is told.
    text: String,
}

/// What the agent that asked the question `id` is told: how its person answered `asked` (the
/// server's words for it) and what came of it, asked about what the ask `about` is about.
pub(super) fn carried_out(
    (id, about): (&str, &str),
    (person, agent): (&str, &str),
    asked: &str,
    (answer, came): (&str, &str),
) -> Wake {
    Wake {
        key: super::asks::called(id),
        person: person.to_owned(),
        agent: agent.to_owned(),
        about: about.to_owned(),
        text: format!(
            "Your person answered what you asked them.\nYou asked: {asked}\nThey answered: \
             {answer}\n{came}\nDo not do it again for this answer. Go on from where you asked."
        ),
    }
}

impl ServerAgents {
    /// Ask `wake`'s agent what it is told, in its person's name, about what the ask it names is
    /// about. Asked again under the same key it changes nothing.
    ///
    /// # Errors
    /// The ask it names is gone, or the record refused the ask.
    pub(super) async fn wake(&self, workspace: &str, wake: &Wake) -> Result<(), String> {
        let packages = super::asked::packages()?;
        let person =
            tl_asks::Actor::Member(tilo_action_policy::MemberId::person(wake.person.clone()));
        let agent = tilo_action_policy::MemberId::agent(wake.agent.clone());
        let wake = wake.clone();
        self.workspaces
            .with_tl_record(
                workspace,
                &packages,
                super::asked::viewer(),
                move |opened| {
                    let about = tl_asks::asks(opened)?
                        .into_iter()
                        .find(|asked| asked.at.id_string() == wake.about)
                        .ok_or("what the person answered in is gone")?;
                    let from = about.from.unwrap_or(about.at);
                    tl_asks::ask(opened, &person, &agent, &wake.text, &wake.key, &from).map(drop)
                },
            )
            .await
    }
}
