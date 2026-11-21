//! A coding agent's run, from its host's side, against the real launcher.
//!
//! The machine here is this computer: the test starts `tilo-linux-vm-agent
//! serve-run` as a workspace's machine would, and the harness it serves is a
//! stand-in for Claude Code that behaves as Claude Code does at its edges. It
//! reads one JSON message a line, asks its model at `ANTHROPIC_BASE_URL` in
//! Anthropic's protocol, asks Tilo's MCP server over HTTP, and prints Claude
//! Code's own lines. The person's model is a scripted provider behind the
//! host's own entry, so everything from the person's settings and vault to
//! the answer in the provider's shape is the production path.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tilo_lock::MutexRecover as _;
use ush::served_runs::wire::{FromRun, ToRun};
use ush::served_runs::{Heard, Over};

use super::claude_code::ClaudeCode;
use super::harness::{Harness, Start};
use super::run::{CodingRun, Conversation, Ended, Ending, Machine, Serves};
use super::testing::{STAND_IN, StandIn, TestMachine, git, seen_of};
use super::working_copy::WorkingCopy;
use crate::workspaces::Workspaces;
use crate::workspaces::fixtures::{self, governor};
use crate::workspaces::person_model::scripted::{Linking, Reply, ScriptedProvider};
use crate::workspaces::person_model::{ProviderProtocol, Spender};

/// The workspace Ada's coding agents work in.
pub(super) const WORKSPACE: &str = "0b7e4c2a-9d1f-4e3b-8a6c-5f2d1e0c9b8a";
/// [`WORKSPACE`] as the model store is opened for it.
pub(super) fn workspace_of(name: &str) -> collab_types::WorkspaceId {
    collab_types::WorkspaceId::parse(name).expect("a workspace id")
}
/// Ada's key at her provider. Nothing in the machine may ever hold it.
const ADA_KEY: &str = "sk-ada-KEY-THAT-STAYS-ON-THE-SERVER";

const ANTHROPIC: Linking<'static> = Linking {
    name: "Anthropic",
    protocol: ProviderProtocol::Anthropic,
    endpoint: "https://api.anthropic.com",
    key: Some(ADA_KEY),
    models: &["claude-parts"],
};

/// A sub-agent's conversation, as a list of what was written into it.
#[derive(Default)]
pub(super) struct Written(Mutex<Vec<String>>);

impl Written {
    pub(super) fn all(&self) -> Vec<String> {
        self.0.lock_recover().clone()
    }

    /// Wait until something written starts with `start`, and answer it.
    pub(super) fn wait_for(&self, start: &str) -> String {
        tilo_test_env::wait_for(format!("the conversation to hold {start:?}"), || {
            self.all()
                .into_iter()
                .find(|written| written.starts_with(start))
        })
    }

    /// How many entries start with `start`.
    pub(super) fn count(&self, start: &str) -> usize {
        self.all()
            .iter()
            .filter(|written| written.starts_with(start))
            .count()
    }
}

impl Conversation for Written {
    fn session(&self, id: &str) {
        self.0.lock_recover().push(format!("session {id}"));
    }

    fn says(&self, words: &str) {
        self.0.lock_recover().push(format!("says {words}"));
    }

    fn doing(&self, what: &str) {
        self.0.lock_recover().push(format!("doing {what}"));
    }

    fn finished(&self, ok: bool, words: &str) {
        self.0.lock_recover().push(format!("finished {ok} {words}"));
    }

    fn over(&self, ended: &Ended) {
        self.0
            .lock_recover()
            .push(format!("over {} {}", ended.how.word(), ended.words));
    }
}

/// One coding agent of Ada's on this computer: its machine, its run, its
/// conversation, and what Tilo's MCP server was asked.
struct Coding {
    machine: Arc<TestMachine>,
    run: CodingRun,
    written: Arc<Written>,
    tilo_asked: Arc<Mutex<Vec<Value>>>,
    directory: tempfile::TempDir,
}

impl Coding {
    /// Start one whose model calls are counted on a day with `limit_micros`.
    fn start(workspaces: &Workspaces, limit_micros: Option<u64>) -> Self {
        Self::start_in(workspaces, limit_micros, None)
    }

    /// The same, in a working copy of its own when `copy` names one.
    fn start_in(
        workspaces: &Workspaces,
        limit_micros: Option<u64>,
        copy: Option<WorkingCopy>,
    ) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let script = tilo_test_env::script(directory.path().join("claude"), STAND_IN).unwrap();
        let harness: Arc<dyn Harness> = Arc::new(StandIn {
            script: script.path().to_path_buf(),
            copy,
            starts: Arc::default(),
        });
        let program = harness.program(&Start::default());
        let machine = TestMachine::start(
            &program.argv,
            &program.env,
            &program.endpoints,
            directory.path(),
            Duration::from_secs(20),
        );
        let written = Arc::new(Written::default());
        let tilo_asked = Arc::new(Mutex::new(Vec::new()));
        let (asked, models) = (Arc::clone(&tilo_asked), workspaces.clone());
        let serves = Serves {
            model: Arc::new(move |kind| {
                models
                    .workspace_model(workspace_of(WORKSPACE), kind)
                    .spending_as(Spender {
                        workspace: WORKSPACE.into(),
                        agent: "coder".into(),
                        name: "Coder".into(),
                        day: "2026-11-17".into(),
                        limit_micros,
                    })
            }),
            tilo: Arc::new(move |request| {
                asked.lock_recover().push(request.clone());
                let id = request.get("id")?.clone();
                Some(
                    json!({"jsonrpc": "2.0", "id": id, "result": {"tools": [{"name": "tilo_ls"}]}}),
                )
            }),
            language: None,
        };
        let run = CodingRun::follow(
            Arc::clone(&machine) as Arc<dyn Machine>,
            harness,
            serves,
            Arc::clone(&written) as Arc<dyn Conversation>,
        )
        .unwrap();
        Self {
            machine,
            run,
            written,
            tilo_asked,
            directory,
        }
    }

    fn ended(&self) -> Ended {
        tilo_test_env::wait_for("the run to be over", || self.run.ended())
    }
}

/// Ada's host, her provider linked with her key, answering from `replies`.
pub(super) async fn host_of_ada(
    state: &Path,
    replies: impl IntoIterator<Item = Reply>,
) -> (crate::workspaces::fixtures::TestHost, Arc<ScriptedProvider>) {
    let (key, cert) = governor();
    let host = fixtures::test_host_taking_part(state, &key, &cert).await;
    let scripted = ScriptedProvider::answering(replies);
    host.workspaces().answer_models_with(scripted.clone());
    // Her workspace, whose vault keeps her key.
    host.workspaces()
        .create_workspace("Ada's", Some(WORKSPACE))
        .await
        .unwrap();
    host.workspaces()
        .link_model_for(WORKSPACE, &ANTHROPIC, true)
        .await
        .unwrap();
    (host, scripted)
}

/// Looks for the person's key everywhere a program in the machine could find one.
fn the_key_is_nowhere_in_the_machine(coding: &Coding) {
    // No key in the machine: not in what its programs were started with, not in a file beside
    // them, not in a frame that went into it, not in what it said, not in its conversation.
    let (launcher, harness) = (coding.machine.launcher(), coding.machine.harness());
    let mut everywhere = vec![seen_of(launcher), seen_of(harness)];
    for file in std::fs::read_dir(coding.directory.path()).unwrap() {
        everywhere.push(
            String::from_utf8_lossy(&std::fs::read(file.unwrap().path()).unwrap()).into_owned(),
        );
    }
    everywhere.push(format!("{:?}", coding.machine.told()));
    for frame in &coding.machine.told() {
        if let ToRun::AnswerBody { bytes, .. } | ToRun::In(bytes) = frame {
            everywhere.push(String::from_utf8_lossy(bytes).into_owned());
        }
    }
    for frame in &coding.machine.said() {
        if let FromRun::Out(bytes) | FromRun::Err(bytes) | FromRun::AskedBody { bytes, .. } = frame
        {
            everywhere.push(String::from_utf8_lossy(bytes).into_owned());
        }
    }
    everywhere.extend(coding.written.all());
    assert!(
        everywhere.len() > 12,
        "the places a key could be were looked at"
    );
    for place in &everywhere {
        assert!(
            !place.contains("KEY-THAT-STAYS"),
            "the person's key is in the machine: {place}"
        );
    }
    assert!(
        seen_of(harness).contains("ANTHROPIC_BASE_URL=http://127.0.0.1:"),
        "the harness was started with its endpoint, which is what was looked at"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_coding_agent_works_on_its_persons_own_model_and_its_words_reach_its_conversation() {
    let state = tempfile::tempdir().unwrap();
    let (host, scripted) = host_of_ada(
        state.path(),
        [
            Reply::text("Three deploys since Monday.").costing(1200),
            Reply::text("It is Safari only.").costing(800),
        ],
    )
    .await;
    let coding = Coding::start(host.workspaces(), None);

    // Its job reaches it as its first message; what it does and says is written as it comes.
    assert!(
        coding
            .run
            .message("Daily use fell 18% since Tuesday. Find out why.")
    );
    assert!(
        coding
            .written
            .wait_for("session ")
            .starts_with("session stand-in-")
    );
    coding
        .written
        .wait_for("finished true Three deploys since Monday.");
    assert_eq!(
        coding.written.all().get(1..).unwrap(),
        [
            "doing Bash: git log --since=monday",
            "says Three deploys since Monday.",
            "finished true Three deploys since Monday.",
        ]
    );

    // The model that answered is Ada's own, asked with her key, on the host.
    let asked = scripted.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].key.as_deref(), Some(ADA_KEY));
    assert_eq!(asked[0].model, "claude-parts");
    assert_eq!(asked[0].endpoint, "https://api.anthropic.com");
    let said = serde_json::to_string(&asked[0].request).unwrap();
    assert!(
        said.contains("Daily use fell 18% since Tuesday. Find out why."),
        "{said}"
    );
    assert_eq!(coding.run.spent_micros(), 1200);

    // A note given while it runs reaches it as its next message.
    assert!(coding.run.message("Is it only Safari?"));
    coding.written.wait_for("finished true It is Safari only.");
    assert_eq!(coding.run.spent_micros(), 2000);

    // Tilo's own MCP server is at its other endpoint, asked as posted JSON.
    assert!(coding.run.message("list your tilo tools"));
    let listed = coding.written.wait_for("says a note was");
    assert!(
        listed.contains("202 Accepted"),
        "a message that asks for no answer: {listed}"
    );
    assert!(listed.contains("tilo_ls"), "{listed}");
    assert_eq!(
        coding
            .tilo_asked
            .lock_recover()
            .iter()
            .map(|asked| asked["method"].clone())
            .collect::<Vec<_>>(),
        ["notifications/initialized", "tools/list"]
    );

    the_key_is_nowhere_in_the_machine(&coding);

    // Nothing more to say: its input ends, and it ends by itself.
    assert!(coding.run.close());
    let ended = coding.ended();
    assert_eq!(
        (ended.how, ended.exit, ended.spent_micros),
        (Ending::Finished, Some(0), 2000)
    );
    assert_eq!(coding.written.all().last().unwrap(), "over finished ");
    assert!(
        !coding.run.message("too late"),
        "a run that is over takes no message"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_coding_agent_that_is_stopped_ends_and_its_model_call_is_let_go() {
    let state = tempfile::tempdir().unwrap();
    let (host, scripted) = host_of_ada(state.path(), [Reply::never()]).await;
    let coding = Coding::start(host.workspaces(), None);
    assert!(coding.run.message("Take your time."));
    tilo_test_env::wait_until("the model to be asked", || scripted.asked().len() == 1);

    // An answer that takes long has its stream opened and kept alive, so the harness waits.
    tilo_test_env::wait_until("the stream to be opened and kept alive", || {
        let told = coding.machine.told();
        let opened = told.iter().any(|frame| {
            matches!(frame, ToRun::Answer { status: 200, headers, .. }
                if headers.contains(&("content-type".to_owned(), "text/event-stream".to_owned())))
        });
        let alive = told.iter().any(|frame| {
            matches!(frame, ToRun::AnswerBody { bytes, last: false, .. }
                if bytes.starts_with(b"event: ping"))
        });
        opened && alive
    });

    coding.run.stop();
    let ended = coding.ended();
    assert_eq!(ended.how, Ending::Stopped);
    assert_eq!(
        ended.exit,
        Some(130),
        "it ended itself when it was interrupted"
    );
    assert_eq!(
        coding.written.count("finished"),
        0,
        "it was not done, and says nothing of being done"
    );
    assert_eq!(coding.written.all().last().unwrap(), "over stopped ");
    assert_eq!(coding.run.spent_micros(), 0);
    assert_eq!(
        scripted.asked().len(),
        1,
        "and its model was asked the once"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_coding_agent_stops_at_its_persons_limit_for_the_day_and_says_so() {
    let state = tempfile::tempdir().unwrap();
    let (host, scripted) = host_of_ada(
        state.path(),
        [
            Reply::text("A long first look.").costing(1500),
            Reply::text("never asked"),
        ],
    )
    .await;
    let coding = Coding::start(host.workspaces(), Some(1000));
    assert!(coding.run.message("Look into it."));
    coding.written.wait_for("finished true A long first look.");

    // The first answer used the day up: the next call is refused before the provider is asked.
    assert!(coding.run.message("And further."));
    let refused = coding.written.wait_for("finished false");
    assert!(
        refused.contains("Coder"),
        "the refusal names the agent whose day it is: {refused}"
    );
    assert_eq!(
        scripted.asked().len(),
        1,
        "a call past the limit never reaches the provider"
    );
    assert_eq!(coding.run.spent_micros(), 1500);
    let refusal = coding.machine.told().iter().find_map(|frame| match frame {
        ToRun::Answer { status, .. } if *status != 200 => Some(*status),
        _ => None,
    });
    assert_eq!(
        refusal,
        Some(402),
        "said so that the harness does not ask again"
    );
}

/// A machine that ends at once, as `over` says.
struct Ends(Mutex<Option<Over>>);

impl Machine for Ends {
    fn tell(&self, _: &ToRun) -> bool {
        false
    }

    fn hear(&self, _: Duration) -> Option<Heard> {
        self.0.lock_recover().take().map(Heard::Over)
    }

    fn cut(&self) {}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_machine_that_cannot_serve_a_run_says_why_in_words() {
    let state = tempfile::tempdir().unwrap();
    let (host, _) = host_of_ada(state.path(), []).await;
    let ended = |over: Over| {
        let models = host.workspaces().clone();
        let written = Arc::new(Written::default());
        let run = CodingRun::follow(
            Arc::new(Ends(Mutex::new(Some(over)))),
            Arc::new(ClaudeCode),
            Serves {
                model: Arc::new(move |kind| models.workspace_model(workspace_of(WORKSPACE), kind)),
                tilo: Arc::new(|_| None),
                language: None,
            },
            Arc::clone(&written) as Arc<dyn Conversation>,
        )
        .unwrap();
        let ended = tilo_test_env::wait_for("the run to be over", || run.ended());
        assert_eq!(written.all(), [format!("over failed {}", ended.words)]);
        ended
    };

    // An image from before its agent served runs starts the agent's own program, which ends.
    let old = ended(Over {
        exit: 1,
        refusal: None,
        words: "tilo-linux-vm-agent: listen on vsock port 7110: Address already in use\n".into(),
    });
    assert_eq!(old.how, Ending::Failed);
    assert_eq!(
        old.words,
        "This workspace's machine could not serve a coding agent: tilo-linux-vm-agent: listen on \
         vsock port 7110: Address already in use"
    );

    // A machine the account cannot have yet answers with the host's own typed refusal.
    let refusal = tilo_actions::remote::server_account::Refusal {
        reason: tilo_actions::remote::server_account::Reason::NoPaymentMethod,
        message: "Linux machines need a card on file.".into(),
        next: Vec::new(),
    };
    let unpaid = ended(Over {
        exit: 126,
        refusal: Some(refusal),
        words: "ush: tilo-linux-vm-agent: Linux machines need a card on file.\n".into(),
    });
    assert_eq!(
        (unpaid.how, unpaid.words.as_str()),
        (Ending::Failed, "Linux machines need a card on file.")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn coding_agents_on_one_repository_each_work_in_a_copy_and_a_branch_of_their_own() {
    let state = tempfile::tempdir().unwrap();
    let (host, _) = host_of_ada(state.path(), []).await;
    let code = tempfile::tempdir().unwrap();
    let repository = code.path().join("app");
    std::fs::create_dir_all(&repository).unwrap();
    git(&repository, &["init", "--quiet"]);
    std::fs::write(repository.join("README"), "an app\n").unwrap();
    git(&repository, &["add", "README"]);
    git(&repository, &["commit", "--quiet", "-m", "start"]);
    let repository = repository.display().to_string();

    let first = Coding::start_in(
        host.workspaces(),
        None,
        Some(WorkingCopy::of(&repository, "first")),
    );
    let second = Coding::start_in(
        host.workspaces(),
        None,
        Some(WorkingCopy::of(&repository, "second")),
    );
    for (coding, agent) in [(&first, "first"), (&second, "second")] {
        assert!(coding.run.message("where do you work"));
        assert_eq!(
            coding.written.wait_for("says in "),
            format!("says in {repository}.agents/{agent} on agent/{agent}"),
            "each works in its own copy, on its own branch"
        );
    }
    // What one writes is not in the other's tree, nor in the repository's own.
    std::fs::write(
        Path::new(&format!("{repository}.agents/first")).join("NOTES"),
        "mine",
    )
    .unwrap();
    assert!(
        !Path::new(&format!("{repository}.agents/second"))
            .join("NOTES")
            .exists()
    );
    assert!(!Path::new(&repository).join("NOTES").exists());

    // A third agent asked onto a branch another holds is refused by git, in git's words.
    let third = Coding::start_in(
        host.workspaces(),
        None,
        Some(WorkingCopy {
            directory: format!("{repository}.agents/third"),
            ..WorkingCopy::of(&repository, "first")
        }),
    );
    let refused = third.ended();
    assert_eq!(refused.how, Ending::Failed);
    assert!(refused.words.contains("agent/first"), "{}", refused.words);

    // A run that is taken up again finds its working copy as it left it.
    assert!(first.run.close());
    assert_eq!(first.ended().how, Ending::Finished);
    let again = Coding::start_in(
        host.workspaces(),
        None,
        Some(WorkingCopy::of(&repository, "first")),
    );
    assert!(again.run.message("where do you work"));
    again.written.wait_for("says in ");
    assert!(
        Path::new(&format!("{repository}.agents/first"))
            .join("NOTES")
            .exists()
    );

    // A folder that is no repository is said so.
    let nowhere = Coding::start_in(
        host.workspaces(),
        None,
        Some(WorkingCopy::of(
            &code.path().join("absent").display().to_string(),
            "lost",
        )),
    );
    let lost = nowhere.ended();
    assert_eq!(lost.how, Ending::Failed);
    assert!(
        lost.words.contains("is not a git repository"),
        "{}",
        lost.words
    );
}
