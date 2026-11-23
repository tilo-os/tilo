//! Coding agents: a sub-agent whose work a coding harness does on a repository of this computer,
//! driven as its person and its starter drive it, through the workspace's actions. The harness
//! is a stand-in for Claude Code that speaks its protocol, asks its model at its run's endpoint
//! and Tilo's MCP server at its own, and works in the working copy it was started in.
use super::*;
use std::path::Path;
use tilo_workspace_service::agents::coding::{ClaudeCode, Coders};
use ui::tilo_workspace_agent::NewCoding;

/// A stand-in for Claude Code. It says its session at once, takes `--resume`, and for each
/// message: `wait for the word` makes a file named `waiting` in its folder and works until one
/// named `go` stands there; `where do you work` says its folder, its branch and the session it
/// took up; `list your tilo tools` asks Tilo's MCP server; anything else is asked of its model,
/// whose words it says.
const STAND_IN: &str = r#"trap '' PIPE
session="stand-in-$$" resumed="" mcp=""
while [ $# -gt 0 ]; do
  case $1 in
    --resume) resumed=$2 session=$2; shift ;;
    --mcp-config) [[ $2 =~ \"url\":\"([^\"]+)\" ]] && mcp=${BASH_REMATCH[1]}; shift ;;
  esac
  shift
done
echo "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"$session\"}"
post() {
  local rest=${1#http://}
  local hostport=${rest%%/*} path=/${rest#*/}
  exec 3<>"/dev/tcp/${hostport%:*}/${hostport#*:}"
  printf 'POST %s HTTP/1.1\r\nHost: endpoint\r\nContent-Type: application/json\r\nContent-Length: %s\r\n\r\n%s' "$path" "${#2}" "$2" >&3
  answer=$(cat <&3)
  exec 3<&- 3>&-
  answer=${answer//$'\r'/}
  body=${answer#*$'\n\n'}
}
says() {
  echo "{\"type\":\"assistant\",\"session_id\":\"$session\",\"parent_tool_use_id\":null,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"$1\"}]}}"
}
finished() {
  echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"$session\",\"result\":\"$1\"}"
}
while IFS= read -r line; do
  [[ $line =~ \"text\":\"([^\"]*)\" ]]
  text=${BASH_REMATCH[1]}
  case $text in
    *"wait for the word"*)
      : > waiting
      while [ ! -e go ]; do sleep 0.05; done
      rm -f waiting go
      says "the word came"
      finished "the word came"
      ;;
    *"where do you work"*)
      says "in $PWD on $(git branch --show-current), resumed [$resumed]"
      finished "said"
      ;;
    *"list your tilo tools"*)
      post "$mcp" '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"stand-in","version":"1"}}}'
      post "$mcp" '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
      if [[ $body == *tilo_read* ]]; then says "tilo lists tilo_read"; else says "tilo lists no tools"; fi
      finished "listed"
      ;;
    *)
      post "$ANTHROPIC_BASE_URL/v1/messages" "{\"model\":\"stand-in\",\"max_tokens\":64,\"messages\":[{\"role\":\"user\",\"content\":\"$text\"}]}"
      [[ $body =~ \"text\":\"([^\"]*)\" ]]
      says "${BASH_REMATCH[1]}"
      finished "asked"
      ;;
  esac
done
"#;

/// A git repository with one commit on `main`, in `folder`.
fn repository(folder: &Path) {
    let git = |arguments: &[&str]| {
        let ran = std::process::Command::new("git")
            .arg("-C")
            .arg(folder)
            .args(["-c", "user.name=Ada", "-c", "user.email=ada@example.com"])
            .args(arguments)
            .output()
            .expect("git runs");
        assert!(ran.status.success(), "git {arguments:?}: {ran:?}");
    };
    git(&["init", "--quiet", "--initial-branch=main"]);
    std::fs::write(folder.join("form.html"), "<form></form>\n").unwrap();
    git(&["add", "form.html"]);
    git(&["commit", "--quiet", "-m", "The sign-up form"]);
}

/// Wait until `path` stands.
async fn stands_at(path: &Path) {
    until(&format!("{} stands", path.display()), || path.exists()).await;
}

/// The coding part of `agent`'s item.
async fn coding_of(
    workspaces: &Workspaces,
    workspace: &str,
    agent: &str,
) -> ui::tilo_workspace_agent::Coding {
    roster(workspaces, workspace)
        .await
        .get(&Id::from_raw(agent))
        .and_then(|agent| agent.sub.clone())
        .and_then(|sub| sub.coding)
        .expect("a coding agent")
}

/// The person's own agent starts a coding agent on a repository of the computer that works it.
/// Its job alone starts nothing: it says it begins when its person says so. Their words start
/// its harness in a working copy and on a branch of its own, with its job first; what they write
/// while it works reaches the same process; their stop ends it, and says who stopped it; and
/// their next words take its conversation up again where it stood.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_coding_agent_starts_on_its_persons_word_hears_them_while_it_works_stops_and_resumes() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let ada = themselves(&root, &browser, &workspace);
    let own = chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent("fixer")]);
    let folder = tempfile::tempdir().unwrap();
    let repo = folder.path().join("app");
    std::fs::create_dir_all(&repo).unwrap();
    repository(&repo);
    let copy = folder.path().join("app.agents/fixer");

    let asked = Arc::new(Mutex::new(Vec::new()));
    let harness = ClaudeCode::at(
        ["bash", "-c", STAND_IN, "claude"]
            .map(str::to_owned)
            .to_vec(),
    );
    let agents = worked(
        &served,
        &workspace,
        plays(&asked, |request| {
            if said_in(request).contains("sign-up form") {
                says("The form has no label.")
            } else {
                says("Noted.")
            }
        }),
    )
    .await
    .with_coding(Coders::here().with(Arc::new(harness)));

    on_roster(
        &workspaces,
        &workspace,
        (Some(SCOUT), person.as_str()),
        WorkspaceAgents::START_SUB_AGENT.with(workspace_agents::StartSubAgent {
            sub_agent: NewSubAgent {
                name: "Fixer".to_owned(),
                job: "Fix the sign-up form.".to_owned(),
                role: Some("Coder".to_owned()),
                coding: Some(NewCoding {
                    repository: repo.display().to_string(),
                    ..NewCoding::default()
                }),
                ..NewSubAgent::default()
            },
        }),
    )
    .await
    .expect("the person's own agent starts a coding agent");
    let coding = coding_of(&workspaces, &workspace, "fixer").await;
    assert_eq!(
        (coding.directory.as_str(), coding.branch.as_str()),
        (copy.display().to_string().as_str(), "agent/fixer"),
        "a working copy beside its repository, on a branch of its own"
    );

    // Its job is written into its conversation, and alone starts nothing.
    agents.look(&workspace).await.unwrap();
    let job = caused(&workspaces, &workspace, (own.as_str(), "job"), 1).await;
    agents.look(&workspace).await.unwrap();
    let waits = reply_of(&workspaces, &workspace, &job[0].0, "fixer").await;
    assert!(
        waits.text.contains("I begin when you say so"),
        "{}",
        waits.text
    );
    assert!(!copy.exists(), "no working copy, no process");

    // Its person's words start it, its job first, in its own working copy.
    let first = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (own.as_str(), "Please wait for the word, then go on."),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    stands_at(&copy.join("waiting")).await;
    stands_as(&workspaces, &workspace, "fixer", Stands::Working).await;
    let started = coding_of(&workspaces, &workspace, "fixer").await;
    assert_eq!(
        (started.harness.as_str(), started.harness_name.as_str()),
        ("claude-code", "Claude Code")
    );
    assert!(started.session.starts_with("stand-in-"), "{started:?}");
    assert!(
        started
            .process
            .as_ref()
            .is_some_and(|process| process.ended.is_empty()),
        "its process lives: {started:?}"
    );

    // What its person writes while it works reaches the same process, after what it does.
    let more = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (
            own.as_str(),
            "Also: where do you work, and list your tilo tools.",
        ),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    std::fs::write(copy.join("go"), "").unwrap();
    let answered = reply_of(&workspaces, &workspace, &more, "fixer").await;
    let said = answered.text.clone();
    assert!(
        said.contains("The form has no label."),
        "its job, through its model: {said}"
    );
    assert!(said.contains("the word came"), "{said}");
    assert!(
        said.contains(&format!("in {} on agent/fixer, resumed []", copy.display())),
        "{said}"
    );
    let went_on = reply_of(&workspaces, &workspace, &first, "fixer").await;
    assert!(
        !went_on.text.contains("the word came"),
        "one turn, which went on with the newer words: {}",
        went_on.text
    );
    assert!(
        asked
            .lock()
            .unwrap()
            .iter()
            .any(|request| said_in(request).contains("Fix the sign-up form.")),
        "its model is the workspace's, asked at its run's endpoint"
    );
    stands_as(&workspaces, &workspace, "fixer", Stands::Done).await;
    let done = coding_of(&workspaces, &workspace, "fixer").await;
    let process = done.process.clone().expect("its process");
    assert_eq!(
        (process.ended.as_str(), process.exit),
        ("finished", Some(0)),
        "{done:?}"
    );
    assert_eq!(done.session, started.session, "one conversation");

    // Tilo's MCP server answers its harness as its turn.
    let tools = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (own.as_str(), "list your tilo tools"),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    let listed = reply_of(&workspaces, &workspace, &tools, "fixer").await;
    assert_eq!(listed.text, "tilo lists tilo_read");

    // Its person stops it while it works: its process ends, and its item says who stopped it.
    let stopped = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (own.as_str(), "wait for the word"),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    stands_at(&copy.join("waiting")).await;
    on_roster(
        &workspaces,
        &workspace,
        (None, person.as_str()),
        stop("fixer"),
    )
    .await
    .expect("its person stops it");
    agents.look(&workspace).await.unwrap();
    let cut = reply_of(&workspaces, &workspace, &stopped, "fixer").await;
    assert!(!cut.text.contains("the word came"), "{}", cut.text);
    let deadline = tilo_test_env::hang_deadline();
    let ended = loop {
        let now = coding_of(&workspaces, &workspace, "fixer").await;
        if now
            .process
            .as_ref()
            .is_some_and(|process| process.ended == "stopped")
        {
            break now;
        }
        assert!(
            Instant::now() < deadline,
            "its process never ended: {now:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    let process = ended.process.expect("its process");
    assert_eq!(process.stopped_by, format!("person:{person}"));
    stands_as(&workspaces, &workspace, "fixer", Stands::Stopped).await;

    // Its person's next words take its conversation up where it stood.
    let again = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (own.as_str(), "where do you work now?"),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    let resumed = reply_of(&workspaces, &workspace, &again, "fixer").await;
    assert_eq!(
        resumed.text,
        format!(
            "in {} on agent/fixer, resumed [{}]",
            copy.display(),
            started.session
        )
    );
    page.peer.close();
}
