//! A host's coding agents, kept by the real keeper against the real launcher
//! on this computer, with the stand-in for Claude Code as their harness.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_lock::MutexRecover as _;

use super::*;
use crate::coding_agents::harness::Start;
use crate::coding_agents::run::{Conversation, Serves};
use crate::coding_agents::testing::Local;
use crate::coding_agents::tests::{ADA, Written, host_of_ada};
use crate::workspaces::Workspaces;
use crate::workspaces::person_model::scripted::Reply;

/// Ada's lasting yes for every workspace.
struct Yes;

impl Permits for Yes {
    fn stands(&self, _: &CodingAgent) -> bool {
        true
    }
}

/// Ada's host keeping coding agents on this computer.
struct Kept {
    agents: CodingAgents,
    local: Arc<Local>,
    /// What the stand-in was started for, each time.
    starts: Arc<Mutex<Vec<Start>>>,
    serves: Serves,
}

impl Kept {
    fn on(workspaces: &Workspaces, idle: Duration) -> Self {
        Self::permitted(workspaces, idle, Arc::new(Yes))
    }

    /// The same, with `permits` saying where a start needs no asking.
    fn permitted(workspaces: &Workspaces, idle: Duration, permits: Arc<dyn Permits>) -> Self {
        let local = Local::new();
        let (harnesses, starts) = local.stand_in();
        let agents = CodingAgents::new(Arc::clone(&local) as Arc<dyn Machines>, harnesses, permits)
            .resting_after(idle);
        let models = workspaces.clone();
        Self {
            agents,
            local,
            starts,
            serves: Serves {
                model: Arc::new(move |kind| models.person_model(ADA, kind)),
                tilo: Arc::new(|_| None),
                language: None,
            },
        }
    }

    /// Give `agent` `words` under its person's lasting yes, what it says
    /// going to `written`.
    fn tell(
        &self,
        agent: &CodingAgent,
        words: &str,
        written: &Arc<Written>,
    ) -> Result<bool, NotTold> {
        self.let_tell(agent, words, written, Permit::Lasting)
    }

    fn let_tell(
        &self,
        agent: &CodingAgent,
        words: &str,
        written: &Arc<Written>,
        permit: Permit,
    ) -> Result<bool, NotTold> {
        self.agents.tell(
            agent,
            words,
            self.serves.clone(),
            Arc::clone(written) as Arc<dyn Conversation>,
            permit,
        )
    }

    fn started(&self) -> usize {
        self.local.started()
    }
}

fn agent(id: &str) -> CodingAgent {
    CodingAgent {
        workspace: "ws-1".into(),
        id: id.into(),
        harness: "stand-in".into(),
        repository: String::new(),
        copy: None,
        session: None,
        instruction: None,
        person: String::new(),
        under: String::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_coding_agent_runs_while_it_has_work_rests_and_is_taken_up_where_it_stood() {
    let state = tempfile::tempdir().unwrap();
    let (host, _) = host_of_ada(
        state.path(),
        [
            Reply::text("One."),
            Reply::text("Two."),
            Reply::text("Three."),
        ],
    )
    .await;
    let kept = Kept::on(host.workspaces(), Duration::from_millis(50));
    let written = Arc::new(Written::default());
    let coder = agent("coder");

    // Its first words start it; more words reach the run it has.
    kept.tell(&coder, "first", &written).unwrap();
    written.wait_for("finished true One.");
    kept.tell(&coder, "second", &written).unwrap();
    written.wait_for("finished true Two.");
    assert_eq!(kept.started(), 1, "its next words reached the run it had");
    assert!(kept.agents.runs("ws-1", "coder"));
    assert_eq!(
        kept.starts.lock_recover().as_slice(),
        [Start::default()],
        "a first start takes up no earlier conversation"
    );

    // With nothing to do it is put to rest: its input ends and it ends by itself.
    tilo_test_env::wait_until("the coding agent to be put to rest", || {
        kept.agents.rest();
        !kept.agents.runs("ws-1", "coder")
    });
    assert_eq!(written.all().last().unwrap(), "over finished ");
    assert_eq!(kept.agents.count(), 0);

    // Later words start it again, with its own id for the conversation it had.
    let session = written
        .wait_for("session ")
        .strip_prefix("session ")
        .unwrap()
        .to_owned();
    let again = CodingAgent {
        session: Some(session.clone()),
        instruction: Some("Work on a branch of your own.".into()),
        ..coder
    };
    kept.tell(&again, "third", &written).unwrap();
    written.wait_for("finished true Three.");
    assert_eq!(kept.started(), 2);
    assert_eq!(
        kept.starts.lock_recover().last().unwrap(),
        &Start {
            session: Some(session),
            instruction: Some("Work on a branch of your own.".into()),
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn many_coding_agents_run_at_once_and_a_stop_ends_the_one_it_names() {
    let state = tempfile::tempdir().unwrap();
    let (host, _) = host_of_ada(
        state.path(),
        [Reply::text("Here."), Reply::text("Here."), Reply::never()],
    )
    .await;
    let kept = Kept::on(host.workspaces(), Duration::from_secs(3600));
    let (first, second) = (Arc::new(Written::default()), Arc::new(Written::default()));
    kept.tell(&agent("first"), "go", &first).unwrap();
    kept.tell(&agent("second"), "go", &second).unwrap();
    first.wait_for("finished true Here.");
    second.wait_for("finished true Here.");
    assert_eq!(
        (kept.started(), kept.agents.count()),
        (2, 2),
        "each in a run of its own"
    );
    assert_ne!(first.wait_for("session "), second.wait_for("session "));

    // One is at work on something long; a stop ends it and leaves the other.
    kept.tell(&agent("first"), "this takes long", &first)
        .unwrap();
    tilo_test_env::wait_until("the first to be at work again", || {
        first.count("doing ") == 2
    });
    assert!(kept.agents.stop("ws-1", "first"));
    tilo_test_env::wait_until("the stopped one to be over", || {
        !kept.agents.runs("ws-1", "first")
    });
    assert_eq!(first.all().last().unwrap(), "over stopped ");
    kept.agents.rest();
    assert_eq!(kept.agents.count(), 1);
    assert!(kept.agents.runs("ws-1", "second"));
    assert!(
        !kept.agents.stop("ws-1", "nobody"),
        "no run of that name is held"
    );

    // A coding agent this host does not start is refused in words, and nothing starts.
    let unknown = CodingAgent {
        harness: "another-harness".into(),
        ..agent("third")
    };
    let NotTold::Failed(refused) = kept
        .tell(&unknown, "go", &Arc::new(Written::default()))
        .unwrap_err()
    else {
        panic!("an unknown harness is refused in words");
    };
    assert!(
        refused.contains("another-harness") && refused.contains("stand-in"),
        "{refused}"
    );
    assert_eq!(kept.started(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_coding_agent_starts_unasked_only_where_its_persons_yes_stands() {
    let state = tempfile::tempdir().unwrap();
    let (host, scripted) = host_of_ada(state.path(), [Reply::text("Started.")]).await;
    let kept = Kept::permitted(
        host.workspaces(),
        Duration::from_secs(3600),
        Arc::new(NoPermit),
    );
    let written = Arc::new(Written::default());
    let coder = agent("coder");

    // No yes stands: nothing is started, nothing is spent, and the keeper says what it needs.
    assert_eq!(kept.tell(&coder, "go", &written), Err(NotTold::NeedsYes));
    assert_eq!(kept.started(), 0);
    assert!(written.all().is_empty());
    assert!(scripted.asked().is_empty());

    // Its person's yes to this one start starts it; words to a run that is there need none.
    kept.let_tell(&coder, "go", &written, Permit::Once).unwrap();
    written.wait_for("finished true Started.");
    assert_eq!(kept.started(), 1);
    kept.tell(&coder, "list your tilo tools", &written).unwrap();
    written.wait_for("says a note was");
    assert_eq!(kept.started(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_coding_agent_of_another_workspace_gets_nothing_from_this_ones_model() {
    let state = tempfile::tempdir().unwrap();
    let (host, scripted) = host_of_ada(state.path(), [Reply::text("Mine.")]).await;
    let kept = Kept::on(host.workspaces(), Duration::from_secs(3600));
    let (ours, theirs) = (Arc::new(Written::default()), Arc::new(Written::default()));
    let coder = agent("coder");
    let elsewhere = CodingAgent {
        workspace: "ws-2".into(),
        ..agent("coder")
    };

    // A coding agent of each workspace is a run of its own with a model
    // endpoint of its own, though the two share a name.
    kept.tell(&coder, "where is your model", &ours).unwrap();
    kept.tell(&elsewhere, "where is your model", &theirs)
        .unwrap();
    let said = ours.wait_for("says my model is at ");
    let address = said.strip_prefix("says my model is at ").unwrap();
    assert_ne!(said, theirs.wait_for("says my model is at "));
    assert_eq!(kept.agents.count(), 2);

    // The other workspace's run asks this one's endpoint: nothing comes back,
    // and nothing of it reaches this one's person's model or conversation.
    kept.tell(&elsewhere, &format!("ask the model at {address}"), &theirs)
        .unwrap();
    assert_eq!(
        theirs.wait_for("says from there came "),
        "says from there came []"
    );
    assert!(
        scripted.asked().is_empty(),
        "another workspace's run reached this one's model"
    );
    assert_eq!(ours.count("says "), 1);

    // This one's own run is answered as before.
    kept.tell(&coder, "go", &ours).unwrap();
    ours.wait_for("finished true Mine.");
    assert_eq!(scripted.asked().len(), 1);
}
