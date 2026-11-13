use super::{Agent, AgentState, Bound, KIND, MAX_AGENTS, WorkspaceAgents, workspace_agents};
use tilo_resource::{Id, Model as _, NonBlank, Replica};

const ADA: &str = "ada";
const GRACE: &str = "grace";

fn id(text: &str) -> Id {
    Id::from_raw(text)
}

fn make(name: &str) -> tilo_resource::Invoke<WorkspaceAgents> {
    WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
        name: NonBlank::parse(name).expect("a name"),
        keeper: None,
        id: None,
        model: None,
        colour: None,
    })
}

/// An agent admitted before records were, under the id its grants carry and
/// kept by whoever admitted it.
fn restore(id: &str, keeper: &str) -> tilo_resource::Invoke<WorkspaceAgents> {
    WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
        name: NonBlank::parse(id).expect("a name"),
        keeper: Some(keeper.to_owned()),
        id: Some(id.to_owned()),
        model: None,
        colour: None,
    })
}

fn rename(agent: &str, name: &str) -> tilo_resource::Invoke<WorkspaceAgents> {
    WorkspaceAgents::RENAME_AGENT.with(workspace_agents::RenameAgent {
        agent: id(agent),
        name: NonBlank::parse(name).expect("a name"),
    })
}

fn set_model(agent: &str, model: Option<&str>) -> tilo_resource::Invoke<WorkspaceAgents> {
    WorkspaceAgents::SET_AGENT_MODEL.with(workspace_agents::SetAgentModel {
        agent: id(agent),
        model: model.map(str::to_owned),
    })
}

fn set_colour(agent: &str, colour: u8) -> tilo_resource::Invoke<WorkspaceAgents> {
    WorkspaceAgents::SET_AGENT_COLOUR.with(workspace_agents::SetAgentColour {
        agent: id(agent),
        colour,
    })
}

fn set_limit(agent: &str, limit: Option<u64>) -> tilo_resource::Invoke<WorkspaceAgents> {
    WorkspaceAgents::SET_AGENT_SPEND_LIMIT.with(workspace_agents::SetAgentSpendLimit {
        agent: id(agent),
        limit_micros: limit,
    })
}

fn charge(agent: &str, day: &str, cost_micros: u64) -> tilo_resource::Invoke<WorkspaceAgents> {
    WorkspaceAgents::RECORD_AGENT_SPEND.with(workspace_agents::RecordAgentSpend {
        agent: id(agent),
        day: day.to_owned(),
        cost_micros,
    })
}

fn set_bound(agent: &str, bound: Vec<Bound>) -> tilo_resource::Invoke<WorkspaceAgents> {
    WorkspaceAgents::SET_AGENT_BOUND.with(workspace_agents::SetAgentBound {
        agent: id(agent),
        bound,
    })
}

fn retire(agent: &str) -> tilo_resource::Invoke<WorkspaceAgents> {
    WorkspaceAgents::RETIRE_AGENT.with(workspace_agents::RetireAgent { agent: id(agent) })
}

fn as_person(person: &str) -> Replica<WorkspaceAgents> {
    Replica::<WorkspaceAgents>::new().as_author(person)
}

fn only(state: &WorkspaceAgents) -> &Agent {
    assert_eq!(state.agents.len(), 1, "one agent");
    &state.agents[0]
}

#[test]
fn an_agent_is_made_with_a_name_keeper_colour_and_when() {
    let mut record = as_person(ADA);
    record.set_now_ms(1_700_000_000_000);
    record.apply(make("Claude")).unwrap();
    let state = record.state().unwrap();
    let agent = only(&state);
    assert_eq!(agent.name, "Claude");
    assert_eq!(agent.keeper, ADA, "made by someone, it is theirs");
    assert_eq!(agent.created_at, 1_700_000_000_000);
    assert_eq!(
        agent.model, None,
        "its keeper's default until one is chosen"
    );
    assert!(agent.bound.is_empty(), "the default bound until one is set");
    assert_eq!(agent.state, AgentState::Active);
    assert_eq!(state.names(), ["Claude"]);
    assert_eq!(state.active().count(), 1);
    assert_eq!(
        state.by_name("Claude").map(|agent| agent.id.0.clone()),
        Some(agent.id.0.clone())
    );
}

/// Nothing chosen, an agent wears what the one rule gives it: what it is, clear of the colour
/// of the person who keeps it, as the bar and every app draw it.
#[test]
fn an_agents_colour_follows_the_rule_every_surface_draws_it_by() {
    let mut record = as_person(ADA);
    record.apply(make("Claude")).unwrap();
    let agent = {
        let state = record.state().unwrap();
        state.agents[0].clone()
    };
    let hue = collab_types::presence::PresenceHue::of_participant(
        &format!("agent:{}", agent.id.0),
        true,
        Some(&agent.keeper),
    );
    assert_eq!(agent.colour, None, "nothing chosen, so the rule's");
    let keeper = collab_types::presence::PresenceHue::of_identity(&agent.keeper);
    assert_ne!(
        usize::from(agent.slot()),
        keeper.slot(),
        "never its keeper's colour"
    );
    assert_eq!(agent.slot(), u8::try_from(hue.slot()).unwrap());
    assert_eq!(agent.identity(), format!("agent:{}", agent.id.0));
}

#[test]
fn an_agent_is_made_in_the_colour_its_keeper_picks() {
    let mut record = as_person(ADA);
    record
        .apply(
            WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
                name: NonBlank::parse("Claude").expect("a name"),
                keeper: None,
                id: None,
                model: None,
                colour: Some(4),
            }),
        )
        .unwrap();
    assert_eq!(record.state().unwrap().agents[0].colour, Some(4));
    let beyond = record.apply(
        WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
            name: NonBlank::parse("Ada").expect("a name"),
            keeper: None,
            id: None,
            model: None,
            colour: Some(u8::try_from(collab_types::presence::PRESENCE_LEN).unwrap()),
        }),
    );
    let refused = beyond.unwrap_err();
    assert!(refused.to_string().contains("palette"), "{refused}");
}

#[test]
fn only_the_keeper_chooses_the_colour_it_is_drawn_in() {
    let mut record = as_person(ADA);
    record.apply(make("Claude")).unwrap();
    let agent = record.state().unwrap().agents[0].id.0.clone();
    let mut grace = record.fork().as_author(GRACE);
    let refused = grace.apply(set_colour(&agent, 2)).unwrap_err();
    assert!(refused.to_string().contains("keeper"), "{refused}");
    record.apply(set_colour(&agent, 2)).unwrap();
    assert_eq!(record.state().unwrap().agents[0].colour, Some(2));
    let beyond = u8::try_from(collab_types::presence::PRESENCE_LEN).unwrap();
    let refused = record.apply(set_colour(&agent, beyond)).unwrap_err();
    assert!(refused.to_string().contains("palette"), "{refused}");
}

#[test]
fn names_are_one_line_of_a_bounded_length() {
    let mut record = as_person(ADA);
    for name in [&"x".repeat(super::MAX_NAME + 1), "two\nlines"] {
        let refused = record.apply(make(name)).unwrap_err();
        assert!(refused.to_string().contains("name"), "{refused}");
    }
    record.apply(make(&"x".repeat(super::MAX_NAME))).unwrap();
    assert_eq!(record.state().unwrap().agents.len(), 1);
}

#[test]
fn only_the_keeper_chooses_the_model_it_plans_with() {
    let mut record = as_person(ADA);
    record.apply(make("Claude")).unwrap();
    let agent = record.state().unwrap().agents[0].id.0.clone();
    let mut grace = record.fork().as_author(GRACE);
    let refused = grace
        .apply(set_model(&agent, Some("openai/gpt")))
        .unwrap_err();
    assert!(refused.to_string().contains("keeper"), "{refused}");
    record
        .apply(set_model(&agent, Some("anthropic/claude")))
        .unwrap();
    assert_eq!(
        record.state().unwrap().agents[0].model.as_deref(),
        Some("anthropic/claude")
    );
    record.apply(set_model(&agent, None)).unwrap();
    assert_eq!(record.state().unwrap().agents[0].model, None);
    for model in ["gpt", "/model", "provider/"] {
        let refused = record.apply(set_model(&agent, Some(model))).unwrap_err();
        assert!(refused.to_string().contains("provider/model"), "{refused}");
    }
}

#[test]
fn a_keeper_named_the_way_grants_name_them_keeps_their_agent() {
    let mut record = as_person(GRACE);
    record
        .apply(restore("claude-code", &format!("person:{ADA}")))
        .unwrap();
    let agent = record.state().unwrap().agents[0].id.0.clone();
    let mut ada = record.fork().as_author(ADA);
    ada.apply(set_model(&agent, Some("anthropic/claude")))
        .unwrap();
    let mut by_grace = record.fork().as_author(GRACE);
    assert!(
        by_grace
            .apply(set_model(&agent, Some("openai/gpt")))
            .is_err()
    );
}

#[test]
fn a_bound_narrows_and_never_allows_an_outside_effect_unasked() {
    let mut record = as_person(ADA);
    record.apply(make("Claude")).unwrap();
    let agent = record.state().unwrap().agents[0].id.0.clone();
    let rule = |node: &str, kinds: &[&str], answer: &str| Bound {
        node: node.to_owned(),
        kinds: kinds.iter().map(|kind| (*kind).to_owned()).collect(),
        answer: answer.to_owned(),
    };
    record
        .apply(set_bound(
            &agent,
            vec![
                rule("room:ws/app:so.tilo.chat", &["read", "undoable"], "allow"),
                rule(
                    "room:ws/app:so.tilo.file-explorer",
                    &["read", "outside"],
                    "ask",
                ),
            ],
        ))
        .unwrap();
    let state = record.state().unwrap();
    let rules = state.agents[0].bound_rules().expect("a bound of its own");
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].answer, tilo_action_policy::Answer::Allow);
    assert_eq!(
        rules[1].kinds,
        tilo_action_policy::KindSet::READ.union(tilo_action_policy::KindSet::OUTSIDE)
    );

    for bad in [
        rule("room:ws", &["outside"], "allow"),
        rule("room:ws", &["write"], "allow"),
        rule("room:ws", &[], "allow"),
        rule("room:ws", &["read"], "maybe"),
    ] {
        let refused = record
            .apply(set_bound(&agent, vec![bad.clone()]))
            .unwrap_err();
        assert!(!refused.to_string().is_empty());
    }
    record.apply(set_bound(&agent, Vec::new())).unwrap();
    assert_eq!(
        record.state().unwrap().agents[0].bound_rules(),
        None,
        "an empty bound follows the default"
    );
}

#[test]
fn retiring_keeps_the_agent_and_happens_once() {
    let mut record = as_person(ADA);
    record.apply(make("Claude")).unwrap();
    let agent = record.state().unwrap().agents[0].id.0.clone();
    record.apply(retire(&agent)).unwrap();
    let state = record.state().unwrap();
    assert_eq!(state.agents.len(), 1, "its name and its conversations stay");
    assert_eq!(state.agents[0].state, AgentState::Retired);
    assert_eq!(state.active().count(), 0);
    assert_eq!(state.names(), ["Claude"]);
    let refused = record.apply(retire(&agent)).unwrap_err();
    assert!(refused.to_string().contains("already"), "{refused}");
}

#[test]
fn an_agent_is_found_by_its_name_with_the_reference_that_opens_it() {
    let mut record = as_person(ADA);
    record.apply(make("Claude")).unwrap();
    record.apply(make("Reviewer")).unwrap();
    let state = record.state().unwrap();
    let found = <WorkspaceAgents as tilo_resource::Resource>::search(&state);
    assert_eq!(found.len(), 2, "one entry per agent");
    for agent in &state.agents {
        let entry = found
            .iter()
            .find(|entry| entry.title == agent.name)
            .expect("each agent is found by its name");
        assert_eq!(
            entry.anchor.as_deref(),
            Some(format!("{KIND}/agents/{}", agent.id.0).as_str())
        );
    }
}

#[test]
fn a_given_id_is_kept_and_told_apart() {
    let mut record = as_person(ADA);
    record.apply(restore("claude-code", ADA)).unwrap();
    let state = record.state().unwrap();
    assert_eq!(
        state
            .get(&id("claude-code"))
            .map(|agent| agent.name.as_str()),
        Some("claude-code")
    );
    let refused = record.apply(restore("claude-code", ADA)).unwrap_err();
    assert!(refused.to_string().contains("already"), "{refused}");
}

#[test]
fn changing_an_agent_that_is_gone_is_refused() {
    let mut record = as_person(ADA);
    record.apply(make("Claude")).unwrap();
    for invoke in [
        rename("missing", "Other"),
        set_model("missing", Some("anthropic/claude")),
        set_bound("missing", Vec::new()),
        retire("missing"),
    ] {
        let refused = record.apply(invoke).unwrap_err();
        assert!(refused.to_string().contains("no agent"), "{refused}");
    }
}

#[test]
fn a_workspace_keeps_at_most_its_limit() {
    let mut record = as_person(ADA);
    for n in 0..MAX_AGENTS {
        record.apply(make(&format!("agent {n}"))).unwrap();
    }
    let refused = record.apply(make("one more")).unwrap_err();
    assert!(refused.to_string().contains("at most"), "{refused}");
}

#[test]
fn stored_agents_read_back_as_they_were() {
    let mut record = as_person(ADA);
    record
        .apply(restore("claude-code", &format!("person:{GRACE}")))
        .unwrap();
    record.apply(make("Reviewer")).unwrap();
    let state = record.state().unwrap();
    let kept_by_grace = state
        .agents
        .iter()
        .find(|agent| agent.keeper == format!("person:{GRACE}"))
        .map(|agent| agent.id.0.clone())
        .expect("the restored agent");
    let reviewer = state
        .agents
        .iter()
        .find(|agent| agent.name == "Reviewer")
        .map(|agent| agent.id.0.clone())
        .expect("the made agent");
    record
        .apply(set_model(&reviewer, Some("anthropic/claude")))
        .unwrap();
    record
        .apply(set_bound(
            &reviewer,
            vec![Bound {
                node: "room:ws/app:so.tilo.chat".to_owned(),
                kinds: vec!["read".to_owned(), "undoable".to_owned()],
                answer: "allow".to_owned(),
            }],
        ))
        .unwrap();
    record.apply(retire(&kept_by_grace)).unwrap();
    let before = record.state().unwrap();
    let loaded = Replica::<WorkspaceAgents>::load(&record.save()).unwrap();
    let after = loaded.state().unwrap();
    assert_eq!(after, before);
    assert!(after.valid().is_ok());
}

#[test]
fn the_same_agent_made_on_two_devices_shares_its_record() {
    let mut ada = as_person(ADA);
    ada.apply(make("Claude")).unwrap();
    let mut grace = ada.fork().as_author(GRACE);
    ada.apply(rename(
        &ada.state().unwrap().agents[0].id.0.clone(),
        "Claude Code",
    ))
    .unwrap();
    grace.apply(make("Reviewer")).unwrap();
    ada.sync_with(&mut grace).unwrap();
    let (a, b) = (ada.state().unwrap(), grace.state().unwrap());
    assert_eq!(a, b);
    let mut names = a.names();
    names.sort_unstable();
    assert_eq!(names, ["Claude Code", "Reviewer"]);
    assert!(a.valid().is_ok());
}

#[test]
fn only_the_keeper_sets_the_most_it_may_spend_in_a_day() {
    let mut record = as_person(ADA);
    record.apply(make("Claude")).unwrap();
    let agent = record.state().unwrap().agents[0].id.0.clone();
    let mut grace = record.fork().as_author(GRACE);
    let refused = grace.apply(set_limit(&agent, Some(5_000_000))).unwrap_err();
    assert!(refused.to_string().contains("keeper"), "{refused}");
    record.apply(set_limit(&agent, Some(5_000_000))).unwrap();
    let state = record.state().unwrap();
    assert_eq!(state.agents[0].spend_limit_micros, Some(5_000_000));
    assert!(
        state.agents[0].within_budget("2026-01-01"),
        "nothing spent yet"
    );
    let refused = record
        .apply(set_limit(&agent, Some(super::MAX_SPEND_LIMIT_MICROS + 1)))
        .unwrap_err();
    assert!(refused.to_string().contains("limit"), "{refused}");
    record.apply(set_limit(&agent, None)).unwrap();
    assert_eq!(record.state().unwrap().agents[0].spend_limit_micros, None);
}

#[test]
fn a_days_spend_adds_up_and_starts_again_the_next_day() {
    let mut record = as_person(ADA);
    record.apply(make("Claude")).unwrap();
    let agent = record.state().unwrap().agents[0].id.0.clone();
    record
        .apply(set_limit(&agent, Some(5_000_000)))
        .expect("its keeper sets a limit");
    record
        .apply(charge(&agent, "2026-01-01", 200_000))
        .expect("a first charge");
    record
        .apply(charge(&agent, "2026-01-01", 220_000))
        .expect("a second charge");
    let state = record.state().unwrap();
    assert_eq!(state.agents[0].spent_on("2026-01-01"), 420_000);
    assert_eq!(
        state.agents[0].spent_on("2026-01-02"),
        0,
        "a day no charge counted"
    );
    assert!(state.agents[0].within_budget("2026-01-01"));
    // An earlier day is over: what is charged to it changes nothing.
    record
        .apply(charge(&agent, "2025-12-31", 1_000_000))
        .unwrap();
    assert_eq!(
        record.state().unwrap().agents[0].spent_on("2026-01-01"),
        420_000
    );
    // A later day starts the counter again.
    record.apply(charge(&agent, "2026-01-02", 100_000)).unwrap();
    let state = record.state().unwrap();
    assert_eq!(state.agents[0].spent_on("2026-01-02"), 100_000);
    assert_eq!(state.agents[0].spent_on("2026-01-01"), 0);
    // Past its keeper's limit it may not spend more.
    record
        .apply(charge(&agent, "2026-01-02", 5_000_000))
        .unwrap();
    assert!(!record.state().unwrap().agents[0].within_budget("2026-01-02"));
}

#[test]
fn only_the_keeper_counts_what_an_agent_spends() {
    let mut record = as_person(ADA);
    record.apply(make("Claude")).unwrap();
    let agent = record.state().unwrap().agents[0].id.0.clone();
    let mut grace = record.fork().as_author(GRACE);
    let refused = grace
        .apply(charge(&agent, "2026-01-01", 1_000))
        .unwrap_err();
    assert!(refused.to_string().contains("keeper"), "{refused}");
    let mut itself = record.fork().as_author(&format!("agent:{agent}"));
    let refused = itself
        .apply(charge(&agent, "2026-01-01", 1_000))
        .unwrap_err();
    assert!(
        refused.to_string().contains("keeper"),
        "an agent never counts its own spending: {refused}"
    );
    for day in ["today", "2026-1-1", "2026-01-01T00", ""] {
        let refused = record.apply(charge(&agent, day, 1_000)).unwrap_err();
        assert!(refused.to_string().contains("YYYY-MM-DD"), "{refused}");
    }
}

#[test]
fn a_record_of_before_limits_reads_without_one() {
    let old = serde_json::json!({
        "agents": [{
            "id": "claude",
            "name": "Claude",
            "colour": 2,
            "keeper": "ada",
            "model": "anthropic/sonnet",
            "bound": [],
            "state": "active",
            "created_at": 1
        }]
    });
    let read = WorkspaceAgents::from_json(&old).expect("a record of before limits reads");
    assert_eq!(read.agents[0].spend_limit_micros, None);
    assert_eq!(read.agents[0].spent, None);
    assert!(read.valid().is_ok());
    assert!(read.agents[0].within_budget("2026-01-01"));
}

/// The record's layout as the bytes devices keep it in: the field order,
/// the names and the enums are the format, and this build must write them
/// back exactly.
#[test]
fn the_record_layout_is_the_bytes_devices_keep() {
    let state = golden_state();
    tilo_files::native::migration::assert_golden_bytes(
        "chat.agent v1",
        GOLDEN.as_bytes(),
        |bytes| {
            let value: serde_json::Value =
                serde_json::from_slice(bytes).expect("the kept bytes are JSON");
            let read = WorkspaceAgents::from_json(&value).expect("the kept record reads");
            assert_eq!(read, state, "the kept bytes hold this record");
        },
        || serde_json::to_vec_pretty(&state.to_json()).expect("the record writes"),
    );
}

/// The bytes `chat.agent` v1 was published with.
const GOLDEN: &str = r#"{
  "agents": [
    {
      "id": "claude",
      "name": "Claude",
      "colour": 2,
      "keeper": "ada",
      "model": "anthropic/sonnet",
      "bound": [
        {
          "node": "room:ws/app:so.tilo.notes",
          "kinds": [
            "read",
            "undoable"
          ],
          "answer": "allow"
        }
      ],
      "spend_limit_micros": null,
      "spent": null,
      "state": "active",
      "created_at": 1700000000000
    }
  ]
}"#;

/// One agent as the format keeps it: a keeper's model choice and a bound.
fn golden_state() -> WorkspaceAgents {
    let mut record = as_person(ADA);
    record.set_now_ms(1_700_000_000_000);
    record
        .apply(
            WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
                name: NonBlank::parse("Claude").expect("a name"),
                keeper: Some(ADA.to_owned()),
                id: Some("claude".to_owned()),
                model: Some("anthropic/sonnet".to_owned()),
                colour: Some(2),
            }),
        )
        .unwrap();
    record
        .apply(set_bound(
            "claude",
            vec![Bound {
                node: "room:ws/app:so.tilo.notes".to_owned(),
                kinds: vec!["read".to_owned(), "undoable".to_owned()],
                answer: "allow".to_owned(),
            }],
        ))
        .unwrap();
    record.state().unwrap()
}
