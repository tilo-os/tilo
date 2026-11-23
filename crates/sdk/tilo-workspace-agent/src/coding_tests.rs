//! The coding part of a sub-agent, through the verbs of the workspace's agents.

use super::coding::{CodingHarness, CodingProcess, NewCoding};
use super::sub_tests::{ADA, MAIN, THEN, id, kept, start_as, sub_of, team, with_main};
use super::{Coding, NewSubAgent, Stands, WorkspaceAgents, workspace_agents};
use tilo_resource::{Invoke, Model as _, Replica};

/// A sub-agent whose work a coding harness does in `repository`.
fn coder(name: &str, coding: NewCoding) -> Invoke<WorkspaceAgents> {
    start_as(NewSubAgent {
        name: name.to_owned(),
        job: "Daily use fell since Tuesday. Find out why.".to_owned(),
        role: Some("Coder".to_owned()),
        coding: Some(coding),
        ..NewSubAgent::default()
    })
}

fn in_folder(repository: &str) -> NewCoding {
    NewCoding {
        repository: repository.to_owned(),
        ..NewCoding::default()
    }
}

/// Ada's agent with a coding agent and a sub-agent that is Tilo's own loop under it.
fn with_coder() -> Replica<WorkspaceAgents> {
    let mut agents = with_main().as_agent_for(MAIN, ADA);
    agents
        .apply(coder("Coder", in_folder("code/app/")))
        .unwrap();
    agents
        .apply(start_as(NewSubAgent {
            name: "Researcher".to_owned(),
            job: "Find the Q3 board deck.".to_owned(),
            ..NewSubAgent::default()
        }))
        .unwrap();
    agents
}

/// A runner's report on `agent`'s work, with what `named` sets on it.
fn report(
    agent: &str,
    named: impl FnOnce(&mut workspace_agents::SubAgentCoding),
) -> Invoke<WorkspaceAgents> {
    let mut report = workspace_agents::SubAgentCoding {
        agent: id(agent),
        process: None,
        session: None,
        spent_micros: None,
        harness: None,
    };
    named(&mut report);
    WorkspaceAgents::SUB_AGENT_CODING.with(report)
}

#[test]
fn a_coding_agent_is_a_sub_agent_that_says_where_it_works() {
    let mut agents = with_coder();
    let state = agents.state().unwrap();
    let coder_sub = sub_of(&state, "coder");
    assert_eq!(
        coder_sub.coding,
        Some(Coding {
            repository: "code/app".to_owned(),
            directory: "code/app.agents/coder".to_owned(),
            branch: "agent/coder".to_owned(),
            ..Coding::default()
        }),
        "a working copy beside its repository and a branch named for it"
    );
    // Everything else of it is the sub-agent's: who started it, its role, how it stands.
    assert_eq!(
        (
            coder_sub.started_by.as_str(),
            coder_sub.role.as_str(),
            coder_sub.kind.as_str(),
            coder_sub.stands,
        ),
        (MAIN, "Coder", "subtask", Stands::Working)
    );
    assert_eq!(
        sub_of(&state, "researcher").coding,
        None,
        "a sub-agent started without a coding part is Tilo's own loop"
    );

    // Which harness, its branch and what the branch starts from are kept as given.
    agents
        .apply(coder(
            "Fixer",
            NewCoding {
                harness: Some("claude-code".to_owned()),
                repository: "/home/ada/app".to_owned(),
                branch: Some("fix/sign-up".to_owned()),
                base: Some("main".to_owned()),
            },
        ))
        .unwrap();
    let state = agents.state().unwrap();
    assert_eq!(
        sub_of(&state, "fixer").coding,
        Some(Coding {
            harness: "claude-code".to_owned(),
            repository: "/home/ada/app".to_owned(),
            directory: "/home/ada/app.agents/fixer".to_owned(),
            branch: "fix/sign-up".to_owned(),
            base: "main".to_owned(),
            ..Coding::default()
        })
    );

    // No folder, a folder that goes up, and more than a line start nothing.
    for (repository, why) in [
        ("  ", "names a folder"),
        ("code/../../etc", "goes up"),
        ("code\napp", "one line"),
    ] {
        let refused = agents
            .apply(coder("Astray", in_folder(repository)))
            .unwrap_err();
        assert!(
            refused.to_string().contains(why),
            "{repository:?}: {refused}"
        );
        assert!(
            agents.state().unwrap().get(&id("astray")).is_none(),
            "{repository:?}: nothing was started"
        );
    }
}

#[test]
fn only_its_own_runner_says_how_a_coding_agents_process_stands() {
    let mut agents = with_coder();
    let mut runner = agents.fork().as_agent_for("coder", ADA);
    runner.set_now_ms(THEN + 5);
    runner
        .apply(report("coder", |report| {
            report.harness = Some(CodingHarness {
                id: "claude-code".to_owned(),
                name: "Claude Code".to_owned(),
            });
            report.process = Some(CodingProcess {
                started_ms: THEN + 5,
                ..CodingProcess::default()
            });
        }))
        .unwrap();
    // Only what is named changes, and what was spent only grows.
    runner
        .apply(report("coder", |report| {
            report.session = Some("s-1".to_owned());
            report.spent_micros = Some(700);
        }))
        .unwrap();
    runner
        .apply(report("coder", |report| report.spent_micros = Some(300)))
        .unwrap();
    let state = runner.state().unwrap();
    let coding = sub_of(&state, "coder").coding.as_ref().expect("its part");
    assert_eq!(
        (
            coding.harness.as_str(),
            coding.harness_name.as_str(),
            coding.session.as_str(),
            coding.spent_micros,
        ),
        ("claude-code", "Claude Code", "s-1", 700)
    );

    // Its end: how, when, who stopped it and one line of why.
    runner
        .apply(report("coder", |report| {
            report.process = Some(CodingProcess {
                started_ms: THEN + 5,
                ended_ms: Some(THEN + 90),
                ended: "stopped".to_owned(),
                exit: Some(130),
                words: "\n  Its person stopped it.  \nIt had read two files.".to_owned(),
                stopped_by: "person:ada".to_owned(),
            });
        }))
        .unwrap();
    let state = runner.state().unwrap();
    let process = sub_of(&state, "coder")
        .coding
        .as_ref()
        .and_then(|coding| coding.process.as_ref())
        .expect("its process");
    assert_eq!(
        (
            process.ended.as_str(),
            process.ended_ms,
            process.exit,
            process.stopped_by.as_str(),
            process.words.as_str(),
        ),
        (
            "stopped",
            Some(THEN + 90),
            Some(130),
            "person:ada",
            "Its person stopped it."
        )
    );
    let refused = runner
        .apply(report("coder", |report| {
            report.process = Some(CodingProcess {
                ended: "crashed".to_owned(),
                ..CodingProcess::default()
            });
        }))
        .unwrap_err();
    assert!(
        refused.to_string().contains("a process ends as"),
        "{refused}"
    );
}

#[test]
fn nobody_else_says_how_a_coding_agents_work_stands() {
    let mut agents = with_coder();
    let spent = |agent: &str| report(agent, |report| report.spent_micros = Some(1));
    // Not its starter, not another sub-agent, not its person.
    for by in [MAIN, "researcher"] {
        let mut asking = agents.fork().as_agent_for(by, ADA);
        let refused = asking.apply(spent("coder")).unwrap_err();
        assert!(
            refused.to_string().contains("own runner"),
            "{by}: {refused}"
        );
    }
    let mut ada = agents.fork().as_author(ADA);
    let refused = ada.apply(spent("coder")).unwrap_err();
    assert!(refused.to_string().contains("by an agent"), "{refused}");
    // A sub-agent that is Tilo's own loop has no harness to speak of.
    let mut researcher = agents.fork().as_agent_for("researcher", ADA);
    let refused = researcher.apply(spent("researcher")).unwrap_err();
    assert!(refused.to_string().contains("no coding agent"), "{refused}");
    assert_eq!(
        agents.state().unwrap(),
        with_coder().state().unwrap(),
        "nothing of it was written"
    );
}

#[test]
fn a_coding_agent_is_kept_as_the_fields_a_client_reads() {
    let mut runner = with_coder().as_agent_for("coder", ADA);
    runner.set_now_ms(THEN);
    runner
        .apply(report("coder", |report| {
            report.harness = Some(CodingHarness {
                id: "claude-code".to_owned(),
                name: "Claude Code".to_owned(),
            });
            report.session = Some("s-1".to_owned());
            report.spent_micros = Some(1_200);
            report.process = Some(CodingProcess {
                started_ms: THEN,
                ended_ms: Some(THEN + 60_000),
                ended: "finished".to_owned(),
                exit: Some(0),
                ..CodingProcess::default()
            });
        }))
        .unwrap();
    let state = runner.state().unwrap();
    let written = state.to_json();
    let sub = written["agents"]
        .as_array()
        .expect("the agents are a list")
        .iter()
        .find(|agent| agent["id"] == "coder")
        .expect("the coding agent is an item of the same collection")["sub"]
        .clone();
    assert_eq!(
        sub["coding"],
        serde_json::json!({
            "harness": "claude-code",
            "harness_name": "Claude Code",
            "repository": "code/app",
            "directory": "code/app.agents/coder",
            "branch": "agent/coder",
            "base": "",
            "session": "s-1",
            "process": {
                "started_ms": THEN,
                "ended_ms": THEN + 60_000,
                "ended": "finished",
                "exit": 0,
                "words": "",
                "stopped_by": "",
            },
            "spent_micros": 1_200,
        })
    );
    assert_eq!(
        WorkspaceAgents::from_json(&written).expect("what was written reads back"),
        state
    );

    // A record written before sub-agents had a coding part reads, with none.
    let mut before = team().state().unwrap().to_json();
    for agent in before["agents"].as_array_mut().expect("a list") {
        if let Some(sub) = agent["sub"].as_object_mut() {
            sub.remove("coding");
        }
    }
    let read = WorkspaceAgents::from_json(&before).expect("a record of before reads");
    assert_eq!(sub_of(&read, "lead").coding, None);
    assert!(read.valid().is_ok());

    // A process that ends in no known way is no record this crate wrote.
    let mut state = with_main().state().unwrap();
    let mut agent = kept("coder", MAIN, THEN, Stands::Working);
    agent.sub.as_mut().expect("a sub-agent").coding = Some(Coding {
        process: Some(CodingProcess {
            ended: "vanished".to_owned(),
            ..CodingProcess::default()
        }),
        ..Coding::default()
    });
    state.agents.push(agent);
    assert!(state.valid().is_err());
}

/// A stop says who gave it on the process that lives, and its end keeps that.
#[test]
fn a_stop_says_who_stopped_a_coding_agents_process() {
    let stop = || {
        WorkspaceAgents::STOP_SUB_AGENT.with(workspace_agents::StopSubAgent { agent: id("coder") })
    };
    let lives = |started_ms: i64| {
        report("coder", move |report| {
            report.process = Some(CodingProcess {
                started_ms,
                ..CodingProcess::default()
            });
        })
    };
    let stopped_by = |agents: &mut Replica<WorkspaceAgents>| {
        let state = agents.state().unwrap();
        let sub = sub_of(&state, "coder");
        let by = sub
            .coding
            .as_ref()
            .and_then(|coding| coding.process.as_ref())
            .map(|process| process.stopped_by.clone());
        (sub.stands, by)
    };
    // With no process there is nothing to say it on.
    let mut ada = with_coder().fork().as_author(ADA);
    ada.apply(stop()).unwrap();
    assert_eq!(stopped_by(&mut ada), (Stands::Stopped, None));

    // Its person stops it while its harness lives.
    let mut runner = with_coder().as_agent_for("coder", ADA);
    runner.apply(lives(THEN + 5)).unwrap();
    let mut ada = runner.fork().as_author(ADA);
    ada.apply(stop()).unwrap();
    assert_eq!(
        stopped_by(&mut ada),
        (Stands::Stopped, Some("person:ada".to_owned()))
    );
    // Its runner then says how the process ended, and who stopped it stays said.
    let mut runner = ada.as_agent_for("coder", ADA);
    runner
        .apply(report("coder", |report| {
            report.process = Some(CodingProcess {
                started_ms: THEN + 5,
                ended_ms: Some(THEN + 9),
                ended: "stopped".to_owned(),
                ..CodingProcess::default()
            });
        }))
        .unwrap();
    assert_eq!(
        stopped_by(&mut runner),
        (Stands::Stopped, Some("person:ada".to_owned()))
    );

    // The agent that steers it stops it as itself.
    let mut runner = with_coder().as_agent_for("coder", ADA);
    runner.apply(lives(THEN + 5)).unwrap();
    let mut main = runner.as_agent_for(MAIN, ADA);
    main.apply(stop()).unwrap();
    assert_eq!(
        stopped_by(&mut main),
        (Stands::Stopped, Some(format!("agent:{MAIN}")))
    );
}
