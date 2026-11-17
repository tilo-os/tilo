//! What the coding agents' tests are built on, here and in the tests of whatever joins coding
//! agents to a workspace: this computer as a workspace's machine, with the real launcher, and a
//! scripted stand-in for Claude Code as the harness started in it.
//!
//! The machine stands for both kinds a server has. [`Local::new`] is the machine of a server
//! that starts its workspaces' programs itself. [`Local::placed`] is the machine on another
//! computer: what is started in it is asked of the dispatcher of a member device that takes the
//! workspace's work, as a server whose machines are placed asks it, and every frame crosses
//! by that member's actions both ways.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture failing is its test failing"
)]

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_actions::remote::environment_programs::ServeProgram;
use tilo_actions::remote::{ActionProvider, Caller};
use tilo_lock::MutexRecover as _;
use tilo_test_env::{DiesWithTheTest as _, SpawnUnderEnvLock as _};
use ush::environment_commands::EnvironmentCommands;
use ush::environment_programs::Served;
use ush::served_runs::ServeRun;
use ush::served_runs::wire::{self, FromRun as FromProcess, ToRun as ToProcess};
use ush::served_runs::{Heard, Over};

use super::claude_code::ClaudeCode;
use super::harness::{Harness, Harnesses, MODEL, ModelProtocol, Program, Reads, Start, TILO};
use super::run::Machine;
use super::runner::Machines;
use super::working_copy::{self, WorkingCopy};
use crate::workspaces::person_model::Interaction;

/// A stand-in for Claude Code. For every message it is given it asks its
/// model once and says the answer as Claude Code's lines; given `list your
/// tilo tools` it asks Tilo's MCP server instead, given `ask tilo to read
/// PATH` or `ask tilo to do PATH with ARGUMENTS end` it calls that tool of
/// the server and says what it answered, and given `wait for the word` it
/// works until a file named `go` stands in its folder, having made one named
/// `waiting`.
pub const STAND_IN: &str = r#"#!/bin/bash
trap '' PIPE
trap 'exit 130' INT
session="stand-in-$$"
text_first='"text":"([^"\\]*)","type":"text_delta"'
type_first='"type":"text_delta","text":"([^"\\]*)"'
message_of='"message":"([^"\\]*)"'
echo "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"$session\"}"
post() {
  local rest=${1#http://} answer
  local hostport=${rest%%/*} path=/${rest#*/}
  exec 3<>"/dev/tcp/${hostport%:*}/${hostport#*:}"
  printf 'POST %s HTTP/1.1\r\nHost: endpoint\r\nAuthorization: Bearer %s\r\nContent-Type: application/json\r\nContent-Length: %s\r\n\r\n%s' "$path" "$ANTHROPIC_AUTH_TOKEN" "${#2}" "$2" >&3 2>/dev/null
  answer=$(cat <&3 2>/dev/null)
  exec 3<&- 3>&-
  answer=${answer//$'\r'/}
  status=${answer%%$'\n'*}
  body=${answer#*$'\n\n'}
}
says() {
  echo "{\"type\":\"assistant\",\"session_id\":\"$session\",\"parent_tool_use_id\":null,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"$1\"}]}}"
}
while IFS= read -r line; do
  # The message object of the line, whichever of its two fields the line wrote first.
  message=${line#*\"message\":}
  case $message in
    *',"type":"user"}') message=${message%,\"type\":\"user\"\}} ;;
    *) message=${message%\}} ;;
  esac
  case $line in
    *"where do you work"*)
      says "in $PWD on $(git branch --show-current 2>/dev/null)"
      echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"$session\",\"result\":\"said\"}"
      ;;
    *"where is your model"*)
      says "my model is at $ANTHROPIC_BASE_URL"
      echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"$session\",\"result\":\"said\"}"
      ;;
    *"ask the model at "*)
      [[ $line =~ model\ at\ (http://[0-9.:]+) ]]
      post "${BASH_REMATCH[1]}/v1/messages" '{"model":"stand-in-main","max_tokens":8,"messages":[{"role":"user","content":"hello"}]}'
      says "from there came [$status]"
      echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"$session\",\"result\":\"asked\"}"
      ;;
    *"list your tilo tools"*)
      post "$TILO_MCP" '{"jsonrpc":"2.0","method":"notifications/initialized"}'
      accepted=$status
      post "$TILO_MCP" '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
      flat=${body//\"/}
      flat=${flat//\\/}
      flat=${flat//$'\n'/ }
      says "a note was $accepted and tilo answered $flat"
      echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"$session\",\"result\":\"listed\"}"
      ;;
    *"wait for the word"*)
      : > waiting
      while [ ! -e go ]; do sleep 0.05; done
      rm -f waiting go
      says "the word came"
      echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"$session\",\"result\":\"the word came\"}"
      ;;
    *"ask tilo to "*)
      if [[ $line =~ ask\ tilo\ to\ do\ ([^\ ]+)\ with\ (.*)\ end ]]; then
        args=${BASH_REMATCH[2]//\\\\/$'\x01'}
        args=${args//\\\"/\"}
        args=${args//$'\x01'/\\}
        call="{\"name\":\"tilo_do\",\"arguments\":{\"path\":\"${BASH_REMATCH[1]}\",\"args\":$args}}"
      else
        [[ $line =~ ask\ tilo\ to\ read\ ([A-Za-z0-9/._#-]+) ]]
        call="{\"name\":\"tilo_read\",\"arguments\":{\"path\":\"${BASH_REMATCH[1]}\"}}"
      fi
      post "$TILO_MCP" "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":$call}"
      flat=${body//\"/}
      flat=${flat//\\/}
      flat=${flat//$'\n'/ }
      says "tilo answered $flat"
      echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"$session\",\"result\":\"asked tilo\"}"
      ;;
    *)
      echo "{\"type\":\"assistant\",\"session_id\":\"$session\",\"parent_tool_use_id\":null,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Bash\",\"input\":{\"command\":\"git log --since=monday\"}}]}}"
      post "$ANTHROPIC_BASE_URL/v1/messages" "{\"model\":\"stand-in-main\",\"max_tokens\":64,\"stream\":true,\"messages\":[$message]}"
      if [[ $status == *" 200 "* ]] && { [[ $body =~ $text_first ]] || [[ $body =~ $type_first ]]; }; then
        says "${BASH_REMATCH[1]}"
        echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"$session\",\"result\":\"${BASH_REMATCH[1]}\"}"
      else
        [[ $body =~ $message_of ]]
        echo "{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"session_id\":\"$session\",\"result\":\"${BASH_REMATCH[1]}\"}"
      fi
      ;;
  esac
done
"#;

/// The stand-in as a harness: Claude Code's own ways of taking a message and
/// of printing, and a program of its own.
pub struct StandIn {
    pub script: PathBuf,
    /// The working copy it is started in, when it gets one of its own.
    pub copy: Option<WorkingCopy>,
    /// What it was started for, each time.
    pub starts: Arc<Mutex<Vec<Start>>>,
}

impl Harness for StandIn {
    fn id(&self) -> &'static str {
        "stand-in"
    }

    fn name(&self) -> &'static str {
        "Stand-in"
    }

    fn model_protocol(&self) -> ModelProtocol {
        ModelProtocol::AnthropicMessages
    }

    fn program(&self, start: &Start) -> Program {
        self.starts.lock_recover().push(start.clone());
        let program = vec![self.script.display().to_string()];
        Program {
            argv: self.copy.as_ref().map_or_else(
                || program.clone(),
                |copy| working_copy::within(copy, &program),
            ),
            env: vec![
                ("ANTHROPIC_BASE_URL".to_owned(), wire::placeholder(MODEL)),
                (
                    "ANTHROPIC_AUTH_TOKEN".to_owned(),
                    "a-word-not-a-key".to_owned(),
                ),
                (
                    "TILO_MCP".to_owned(),
                    format!("{}/mcp", wire::placeholder(TILO)),
                ),
            ],
            endpoints: vec![MODEL.to_owned(), TILO.to_owned()],
        }
    }

    fn message(&self, text: &str) -> Vec<u8> {
        ClaudeCode.message(text)
    }

    fn reads(&self) -> Box<dyn Reads> {
        ClaudeCode.reads()
    }

    fn kind_of(&self, model: &str) -> Interaction {
        ClaudeCode.kind_of(model)
    }
}

/// This computer as a workspace's machine: the real launcher, started as the
/// guest agent starts a program, and everything that went to and from it.
pub struct TestMachine {
    child: Mutex<Child>,
    input: Mutex<Option<ChildStdin>>,
    heard: Mutex<Receiver<Heard>>,
    launcher: u32,
    /// Every frame the host sent into the machine.
    told: Mutex<Vec<ToProcess>>,
    /// Every frame the machine sent.
    said: Arc<Mutex<Vec<FromProcess>>>,
    cut: AtomicBool,
}

impl TestMachine {
    pub fn start(
        argv: &[String],
        env: &[(String, String)],
        endpoints: &[String],
        directory: &Path,
        stop_grace: Duration,
    ) -> Arc<Self> {
        let endpoints: Vec<&str> = endpoints.iter().map(String::as_str).collect();
        let mut command = Command::new(tilo_test_env::sibling_binary(
            "legacy-agent",
            "tilo-legacy-agent",
        ));
        command
            .args(wire::arguments(&endpoints, stop_grace, argv))
            .envs(env.iter().cloned())
            .current_dir(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .dies_with_the_test();
        let mut child = command.spawn_locked().unwrap();
        let mut frames = child.stdout.take().unwrap();
        let mut beside = child.stderr.take().unwrap();
        let (hearing, heard) = mpsc::channel();
        let said = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&said);
        std::thread::spawn(move || {
            while let Ok(Some(frame)) = wire::recv::<_, FromProcess>(&mut frames) {
                recorded.lock_recover().push(frame.clone());
                if hearing.send(Heard::Frame(frame)).is_err() {
                    return;
                }
            }
            let mut words = String::new();
            let _ = beside.read_to_string(&mut words);
            let _ = hearing.send(Heard::Over(Over {
                exit: 0,
                refusal: None,
                words,
            }));
        });
        Arc::new(Self {
            launcher: child.id(),
            input: Mutex::new(child.stdin.take()),
            child: Mutex::new(child),
            heard: Mutex::new(heard),
            told: Mutex::default(),
            said,
            cut: AtomicBool::new(false),
        })
    }

    /// The harness's process, once the launcher said it started.
    pub fn harness(&self) -> u32 {
        tilo_test_env::wait_for("the harness to start", || {
            self.said
                .lock_recover()
                .iter()
                .find_map(|frame| match frame {
                    FromProcess::Started { pid } => Some(*pid),
                    _ => None,
                })
        })
    }
}

impl Machine for TestMachine {
    fn tell(&self, frame: &ToProcess) -> bool {
        self.told.lock_recover().push(frame.clone());
        let mut input = self.input.lock_recover();
        let told = input
            .as_mut()
            .is_some_and(|input| wire::send(input, frame).is_ok() && input.flush().is_ok());
        if !told {
            *input = None;
        }
        drop(input);
        told
    }

    fn hear(&self, wait: Duration) -> Option<Heard> {
        let heard = self.heard.lock_recover();
        let next = heard.recv_timeout(wait);
        drop(heard);
        match next {
            Ok(heard) => Some(heard),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(Heard::Over(Over::default())),
        }
    }

    fn cut(&self) {
        self.cut.store(true, Ordering::SeqCst);
        *self.input.lock_recover() = None;
        let _ = self.child.lock_recover().kill();
    }
}

impl Drop for TestMachine {
    fn drop(&mut self) {
        let mut child = self.child.lock_recover();
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl TestMachine {
    /// The launcher's process.
    pub fn launcher(&self) -> u32 {
        self.launcher
    }

    /// Every frame sent into the machine, in order.
    pub fn told(&self) -> Vec<ToProcess> {
        self.told.lock_recover().clone()
    }

    /// Every frame the machine sent, in order.
    pub fn said(&self) -> Vec<FromProcess> {
        self.said.lock_recover().clone()
    }

    /// Everything that went into the machine, as text: each frame as it is, and what the
    /// frames that carry bytes carried.
    pub fn went_in(&self) -> Vec<String> {
        let told = self.told();
        let mut texts = vec![format!("{told:?}")];
        for frame in &told {
            if let ToProcess::AnswerBody { bytes, .. } | ToProcess::In(bytes) = frame {
                texts.push(String::from_utf8_lossy(bytes).into_owned());
            }
        }
        texts
    }

    /// Everything the machine's programs printed and asked, as text.
    pub fn came_out(&self) -> Vec<String> {
        self.said()
            .iter()
            .filter_map(|frame| match frame {
                FromProcess::Out(bytes)
                | FromProcess::Err(bytes)
                | FromProcess::AskedBody { bytes, .. } => {
                    Some(String::from_utf8_lossy(bytes).into_owned())
                }
                _ => None,
            })
            .collect()
    }

    /// What was handed to the harness's input, each message as its harness takes it, in
    /// order.
    pub fn handed(&self) -> Vec<String> {
        self.told()
            .iter()
            .filter_map(|frame| match frame {
                ToProcess::In(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
                _ => None,
            })
            .collect()
    }

    /// How many callers that were not of its own processes the machine turned away at an
    /// endpoint.
    pub fn turned_away(&self) -> usize {
        self.said()
            .iter()
            .filter(|frame| matches!(frame, FromProcess::Turned { .. }))
            .count()
    }

    /// The status of each answer the machine's endpoints were given, in order.
    pub fn answered(&self) -> Vec<u16> {
        self.told()
            .iter()
            .filter_map(|frame| match frame {
                ToProcess::Answer { status, .. } => Some(*status),
                _ => None,
            })
            .collect()
    }
}

/// The operator whose device the member of [`Local::placed`] is, and whose server asks it.
const OPERATOR: [u8; 32] = [7; 32];
/// That server's own device.
const SERVER: [u8; 32] = [1; 32];

/// A machine started here, as the member that started it holds it.
struct OnThisComputer(Arc<TestMachine>);

impl Served for OnThisComputer {
    fn tell(&self, frame: &ToProcess) -> bool {
        Machine::tell(self.0.as_ref(), frame)
    }

    fn hear(&self, wait: Duration) -> Option<Heard> {
        Machine::hear(self.0.as_ref(), wait)
    }

    fn cut(&self) {
        Machine::cut(self.0.as_ref());
    }
}

/// This computer as every workspace's machine, and what was started in it.
pub struct Local {
    files: tempfile::TempDir,
    started: Arc<Mutex<Vec<ServeRun>>>,
    machines: Arc<Mutex<Vec<Arc<TestMachine>>>>,
    /// The member device that starts what is started here, when this computer stands for
    /// one that holds the machines of a server elsewhere.
    member: Option<Arc<EnvironmentCommands>>,
}

impl Local {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            files: tempfile::tempdir().unwrap(),
            started: Arc::default(),
            machines: Arc::default(),
            member: None,
        })
    }

    /// This computer as the one that holds every workspace's machine for a server elsewhere.
    /// What is started here is asked of a member's own dispatcher, as a server whose machines
    /// are placed asks it, and held there for the server: every frame to the program and from
    /// it crosses by the member's actions.
    pub fn placed() -> Arc<Self> {
        let files = tempfile::tempdir().unwrap();
        let started: Arc<Mutex<Vec<_>>> = Arc::default();
        let machines: Arc<Mutex<Vec<Arc<TestMachine>>>> = Arc::default();
        let (root, starts, kept) = (
            files.path().to_path_buf(),
            Arc::clone(&started),
            Arc::clone(&machines),
        );
        let shells = ush::workspace_shell::WorkspaceShells::new(files.path().join(".member"), None);
        let member = EnvironmentCommands::new(shells, OPERATOR).starting_programs_by(Box::new(
            move |_: &str,
                  _: &ush::workspace_shell::Payer,
                  start: ush::environment_programs::Start|
                  -> std::io::Result<Box<dyn Served>> {
                let machine = TestMachine::start(
                    &start.argv,
                    &start.env,
                    &start.endpoints,
                    &root.join(&start.directory),
                    start.stop_grace,
                );
                starts.lock_recover().push(start);
                kept.lock_recover().push(Arc::clone(&machine));
                Ok(Box::new(OnThisComputer(machine)) as Box<dyn Served>)
            },
        ));
        Arc::new(Self {
            files,
            started,
            machines,
            member: Some(Arc::new(member)),
        })
    }

    /// The folder every workspace's files are here.
    pub fn files(&self) -> &Path {
        self.files.path()
    }

    /// How many programs were started here.
    pub fn started(&self) -> usize {
        self.started.lock_recover().len()
    }

    /// What each program was started with, as text: its arguments and its variables.
    pub fn started_with(&self) -> Vec<String> {
        self.started
            .lock_recover()
            .iter()
            .map(|start| format!("{:?} {:?}", start.argv, start.env))
            .collect()
    }

    /// The machines started here, oldest first.
    pub fn machines(&self) -> Vec<Arc<TestMachine>> {
        self.machines.lock_recover().clone()
    }

    /// The stand-in as the one harness started here, and what it is started for, each time.
    pub fn stand_in(&self) -> (Harnesses, Arc<Mutex<Vec<Start>>>) {
        let script = tilo_test_env::script(self.files.path().join("claude"), STAND_IN).unwrap();
        let starts = Arc::new(Mutex::new(Vec::new()));
        let harness = Arc::new(StandIn {
            script: script.path().to_path_buf(),
            copy: None,
            starts: Arc::clone(&starts),
        });
        (Harnesses::default().with(harness), starts)
    }

    /// Make `name` among the files a git repository with one commit.
    pub fn repository(&self, name: &str) {
        let at = self.files.path().join(name);
        std::fs::create_dir_all(&at).unwrap();
        git(&at, &["init", "--quiet"]);
        std::fs::write(at.join("README"), "a repository\n").unwrap();
        git(&at, &["add", "README"]);
        git(&at, &["commit", "--quiet", "-m", "start"]);
    }
}

impl Machines for Local {
    fn serve(&self, workspace: &str, start: ServeRun) -> Result<Arc<dyn Machine>, String> {
        if let Some(member) = &self.member {
            let member: Arc<dyn ActionProvider + Send + Sync> = Arc::clone(member) as _;
            let asked = ServeProgram {
                argv: start.argv,
                env: start.env,
                directory: start.directory.to_string_lossy().into_owned(),
                endpoints: start.endpoints,
                stop_grace_ms: u64::try_from(start.stop_grace.as_millis()).unwrap(),
                agent_program: start.agent_program,
                account: "the-account-of-the-test".into(),
                size: "small".into(),
            };
            let server = Caller::authenticated(OPERATOR, SERVER).in_workspace(workspace);
            return crate::placed_programs::serve(member, server, &asked)
                .map(|carried| carried as Arc<dyn Machine>);
        }
        let machine = TestMachine::start(
            &start.argv,
            &start.env,
            &start.endpoints,
            &self.files.path().join(&start.directory),
            start.stop_grace,
        );
        self.started.lock_recover().push(start);
        self.machines.lock_recover().push(Arc::clone(&machine));
        Ok(machine as Arc<dyn Machine>)
    }
}

/// Everything of process `process` a program in the machine could read of
/// it: its variables and its arguments.
pub fn seen_of(process: u32) -> String {
    let read = |name: &str| {
        String::from_utf8_lossy(&std::fs::read(format!("/proc/{process}/{name}")).unwrap())
            .into_owned()
    };
    format!("{}\n{}", read("environ"), read("cmdline"))
}

/// Run `git` with `arguments` in `directory`, as a person at a terminal would.
pub fn git(directory: &Path, arguments: &[&str]) {
    let done = Command::new("git")
        .args(["-c", "user.name=Ada", "-c", "user.email=ada@example.org"])
        .args([
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(arguments)
        .current_dir(directory)
        .output_locked()
        .unwrap();
    assert!(
        done.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&done.stderr)
    );
}
