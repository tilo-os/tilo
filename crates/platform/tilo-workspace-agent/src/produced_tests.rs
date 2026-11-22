//! What any sub-agent made, through the verbs of the workspace's agents.

use super::produced::MAX_PRODUCED;
use super::sub_tests::{ADA, MAIN, THEN, id, sub_of, team};
use super::{ProducedAt, WorkspaceAgents, workspace_agents};
use tilo_resource::{Invoke, NonBlank};

fn made(
    agent: &str,
    (kind, label): (&str, &str),
    url: Option<&str>,
    at: Option<ProducedAt>,
) -> Invoke<WorkspaceAgents> {
    WorkspaceAgents::SUB_AGENT_PRODUCED.with(workspace_agents::SubAgentProduced {
        agent: id(agent),
        kind: kind.to_owned(),
        label: NonBlank::parse(label).expect("a label"),
        url: url.map(str::to_owned),
        at,
    })
}

fn in_files(path: &str) -> ProducedAt {
    ProducedAt {
        app: "so.tilo.files".to_owned(),
        document: String::new(),
        resource: path.to_owned(),
    }
}

#[test]
fn what_a_sub_agent_made_is_kept_as_things_one_can_open() {
    // Every sub-agent lists what it made, not only one that codes.
    let mut agents = team();
    let mut other = agents.fork().as_agent_for("other", ADA);
    other.set_now_ms(THEN + 40);
    let pull = "https://git.example/app/pulls/7";
    other
        .apply(made(
            "other",
            ("pull-request", "Fix the sign-up form"),
            Some(pull),
            None,
        ))
        .unwrap();
    other
        .apply(made(
            "other",
            ("change", "The change"),
            None,
            Some(in_files("code/app.agents/other/change.diff")),
        ))
        .unwrap();
    let state = other.state().unwrap();
    let produced = &sub_of(&state, "other").produced;
    assert_eq!(produced.len(), 2);
    assert_eq!(
        (
            produced[0].kind.as_str(),
            produced[0].label.as_str(),
            produced[0].url.as_deref(),
            produced[0].at_ms,
        ),
        (
            "pull-request",
            "Fix the sign-up form",
            Some(pull),
            THEN + 40
        )
    );
    assert_eq!(
        produced[1].at,
        Some(in_files("code/app.agents/other/change.diff"))
    );
    assert_ne!(produced[0].id, produced[1].id);

    // The same thing said again replaces what was said of it.
    other
        .apply(made(
            "other",
            ("pull-request", "Fix the sign-up form (merged)"),
            Some(pull),
            None,
        ))
        .unwrap();
    let state = other.state().unwrap();
    let produced = &sub_of(&state, "other").produced;
    assert_eq!(
        produced
            .iter()
            .map(|made| made.label.as_str())
            .collect::<Vec<_>>(),
        ["The change", "Fix the sign-up form (merged)"]
    );

    // The oldest leave first.
    for n in 0..MAX_PRODUCED + 3 {
        let (label, url) = (format!("File {n}"), format!("https://files.example/{n}"));
        other
            .apply(made("other", ("file", &label), Some(&url), None))
            .unwrap();
    }
    let state = other.state().unwrap();
    let produced = &sub_of(&state, "other").produced;
    assert_eq!(produced.len(), MAX_PRODUCED);
    assert_eq!(
        (
            produced.first().map(|made| made.label.as_str()),
            produced.last().map(|made| made.label.as_str()),
        ),
        (Some("File 3"), Some("File 34"))
    );
}

#[test]
fn a_thing_made_is_said_by_its_sub_agent_alone_and_names_one_thing() {
    let mut agents = team();
    let mut other = agents.fork().as_agent_for("other", ADA);
    let place = || Some(in_files("notes/findings.md"));
    for (kind, url, at, why) in [
        ("screenshot", Some("https://a.example/x"), None, "no kind"),
        ("page", Some("http://a.example/x"), None, "https://"),
        ("page", Some("javascript:alert(1)"), None, "https://"),
        ("file", Some("https://a.example/x"), place(), "either"),
        ("file", None, None, "either"),
        (
            "file",
            None,
            Some(ProducedAt {
                resource: String::new(),
                ..in_files("x")
            }),
            "names its app",
        ),
    ] {
        let refused = other
            .apply(made("other", (kind, "A thing"), url, at))
            .unwrap_err();
        assert!(
            refused.to_string().contains(why),
            "{kind} {url:?}: {refused}"
        );
    }
    // Not its starter, not one above that, not a sub-agent beside it, not its person.
    for by in [MAIN, "lead"] {
        let mut asking = agents.fork().as_agent_for(by, ADA);
        let refused = asking
            .apply(made("other", ("file", "A file"), None, place()))
            .unwrap_err();
        assert!(
            refused.to_string().contains("own runner"),
            "{by}: {refused}"
        );
    }
    let mut ada = agents.fork().as_author(ADA);
    let refused = ada
        .apply(made("other", ("file", "A file"), None, place()))
        .unwrap_err();
    assert!(refused.to_string().contains("by an agent"), "{refused}");
    assert!(
        sub_of(&other.state().unwrap(), "other").produced.is_empty(),
        "nothing of it was written"
    );
}
