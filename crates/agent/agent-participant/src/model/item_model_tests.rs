//! The item model of coordinated tasks
//! ([`crate::profile::DECOMPOSE_ITEM_MODEL_ENV`]): its configuration, read
//! once at startup, and the routing of an item's agent's turns to it while
//! the coordinator's and whole tasks' turns stay with the agent's own model.

use super::tests::{endpoint, open_notes, plan_answer};
use super::*;
use crate::planner::{PlanRole, Role};
use crate::profile::{
    DECOMPOSE_ITEM_API_KEY_ENV, DECOMPOSE_ITEM_API_KEY_FILE_ENV, DECOMPOSE_ITEM_MODEL_ENV,
    DECOMPOSE_ITEM_URL_ENV,
};

/// A model that plans nothing and says which it is.
struct Named(&'static str);

impl PlanModel for Named {
    fn choose(&mut self, _: &str, _: &Catalogue, _: usize) -> Result<Answer, String> {
        Err(self.0.to_owned())
    }
}

/// Which model planned `role`'s turn.
fn asked(planner: &mut ModelPlanner, role: Role) -> String {
    planner
        .plan(
            "post Alpha ready",
            &PlanContext {
                role,
                ..PlanContext::default()
            },
        )
        .unwrap_err()
}

fn coordinator() -> Role {
    Role::Coordinator {
        lists: true,
        note: String::new(),
    }
}

fn item() -> Role {
    Role::Item {
        note: String::new(),
    }
}

/// An item's agent plans with the item model; the coordinator's turns and
/// a whole task with the agent's own.
#[test]
fn an_items_turn_goes_to_the_item_model() {
    let mut planner =
        ModelPlanner::new(Box::new(Named("own"))).with_item_model(Box::new(Named("items")));
    assert_eq!(asked(&mut planner, coordinator()), "own");
    assert_eq!(
        asked(
            &mut planner,
            Role::Coordinator {
                lists: false,
                note: "the check".into()
            }
        ),
        "own"
    );
    assert_eq!(asked(&mut planner, item()), "items");
    assert_eq!(asked(&mut planner, Role::Whole), "own");
}

/// With no item model every turn plans with the agent's own, as before.
#[test]
fn without_an_item_model_every_turn_goes_to_the_agents_own() {
    let mut planner = ModelPlanner::new(Box::new(Named("own")));
    for role in [coordinator(), item(), Role::Whole] {
        assert_eq!(asked(&mut planner, role), "own");
    }
    assert_eq!(
        ModelChoice::Scripted.with_items_from(&|_| None),
        Ok(ModelChoice::Scripted)
    );
    let blank = |name: &str| (name == DECOMPOSE_ITEM_MODEL_ENV).then(|| " ".to_owned());
    assert_eq!(
        ModelChoice::Scripted.with_items_from(&blank),
        Ok(ModelChoice::Scripted)
    );
}

/// The item model's provider is told by its name unless its endpoint is
/// named; its key is its own, and the vault a run keeps keys in keeps it.
#[test]
fn the_item_model_is_read_with_its_own_key_and_provider() {
    let key = tempfile::NamedTempFile::new().unwrap();
    let path = key.path().to_str().unwrap().to_owned();
    let told = |model: &'static str, url: Option<&'static str>| {
        let path = path.clone();
        move |name: &str| match name {
            DECOMPOSE_ITEM_MODEL_ENV => Some(model.to_owned()),
            DECOMPOSE_ITEM_URL_ENV => url.map(str::to_owned),
            DECOMPOSE_ITEM_API_KEY_FILE_ENV => Some(path.clone()),
            _ => None,
        }
    };
    let own = ModelChoice::parse(
        "openai:https://api.openai.com/v1",
        Some("gpt-6.1-terra"),
        ApiKey::File("/run/keys/openai".into()),
    )
    .unwrap();
    let routed = own
        .clone()
        .with_items_from(&told("claude-haiku-5-5", None))
        .unwrap();
    let haiku = ModelChoice::Anthropic {
        base_url: "https://api.anthropic.com".into(),
        model: "claude-haiku-5-5".into(),
        api_key: ApiKey::File(key.path().into()),
        vault: None,
    };
    assert_eq!(
        routed,
        ModelChoice::Decomposed {
            coordinator: Box::new(own.clone()),
            items: Box::new(haiku),
        }
    );
    assert!(
        routed.describe().contains("gpt-6.1-terra") && routed.describe().contains("claude-haiku"),
        "{}",
        routed.describe()
    );
    let kept = routed.keeping_keys_in("/tmp/bench-vault".into());
    let ModelChoice::Decomposed { coordinator, items } = &kept else {
        panic!("{kept:?}");
    };
    for choice in [coordinator.as_ref(), items.as_ref()] {
        assert!(
            matches!(choice, ModelChoice::OpenAi { vault: Some(root), .. }
                | ModelChoice::Anthropic { vault: Some(root), .. } if root.ends_with("bench-vault")),
            "{choice:?}"
        );
    }

    let luna = own.clone().with_items_from(&told("gpt-6-luna", None));
    assert!(
        matches!(&luna, Ok(ModelChoice::Decomposed { items, .. })
            if matches!(items.as_ref(), ModelChoice::OpenAi { base_url, model, .. }
                if base_url == "https://api.openai.com/v1" && model == "gpt-6-luna")),
        "{luna:?}"
    );
    let local = own.with_items_from(&told("qwen", Some("openai:http://127.0.0.1:1/v1")));
    assert!(
        matches!(&local, Ok(ModelChoice::Decomposed { items, .. })
            if matches!(items.as_ref(), ModelChoice::OpenAi { base_url, .. }
                if base_url == "http://127.0.0.1:1/v1")),
        "{local:?}"
    );
}

/// An item model with no key, or whose key file is not there, stops the
/// host at startup, saying which setting is missing and never the key.
#[test]
fn an_item_model_without_its_key_fails_at_startup() {
    let own = || ModelChoice::Scripted;
    let keyless = |name: &str| (name == DECOMPOSE_ITEM_MODEL_ENV).then(|| "gpt-6-luna".to_owned());
    let error = own().with_items_from(&keyless).unwrap_err();
    assert!(error.contains(DECOMPOSE_ITEM_API_KEY_FILE_ENV), "{error}");

    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("anthropic.key");
    let missing = missing.to_str().unwrap().to_owned();
    let no_file = |name: &str| match name {
        DECOMPOSE_ITEM_MODEL_ENV => Some("claude-haiku-5-5".to_owned()),
        DECOMPOSE_ITEM_API_KEY_FILE_ENV => Some(missing.clone()),
        _ => None,
    };
    let error = own().with_items_from(&no_file).unwrap_err();
    assert!(
        error.contains(DECOMPOSE_ITEM_API_KEY_FILE_ENV) && error.contains("missing"),
        "{error}"
    );

    let bad_url = |name: &str| match name {
        DECOMPOSE_ITEM_MODEL_ENV => Some("gpt-6-luna".to_owned()),
        DECOMPOSE_ITEM_URL_ENV => Some("ftp://x".to_owned()),
        DECOMPOSE_ITEM_API_KEY_ENV => Some("sk-item-secret".to_owned()),
        _ => None,
    };
    let error = own().with_items_from(&bad_url).unwrap_err();
    assert!(!error.contains("sk-item-secret"), "{error}");
    let scripted = |name: &str| match name {
        DECOMPOSE_ITEM_MODEL_ENV => Some("gpt-6-luna".to_owned()),
        DECOMPOSE_ITEM_URL_ENV => Some("scripted".to_owned()),
        DECOMPOSE_ITEM_API_KEY_ENV => Some("sk-item-secret".to_owned()),
        _ => None,
    };
    assert!(own().with_items_from(&scripted).is_err());
}

/// The host's planner, made from the process's settings, asks the
/// coordinator's turn of the agent's own endpoint and the item's of the
/// item model's, and each call's usage line names the model it went to
/// beside its role. It runs in a process of its own so the settings and
/// the usage log are no other test's.
#[test]
fn the_host_planner_routes_items_and_logs_each_model() {
    const COORDINATOR_ENDPOINT: &str = "TILO_ITEM_MODEL_TEST_ENDPOINT";
    let notes = || PlanContext {
        apps: vec!["so.tilo.notes".into()],
        ..PlanContext::default()
    };
    if let Ok(url) = std::env::var(COORDINATOR_ENDPOINT) {
        let choice = ModelChoice::parse(
            &format!("openai:{url}"),
            Some("coordinator-x"),
            ApiKey::None,
        )
        .unwrap()
        .with_items_from(&|name| std::env::var(name).ok())
        .unwrap();
        let mut planner = choice.planner();
        let role = |role: &'static str, item: Option<usize>| PlanRole {
            role,
            item,
            task: "t1".into(),
        };
        let listed = as_role(Some(role("coordinator", None)), || {
            planner.plan(
                "open notes",
                &PlanContext {
                    role: coordinator(),
                    ..notes()
                },
            )
        });
        assert!(listed.is_ok(), "{listed:?}");
        let done = as_role(Some(role("item", Some(1))), || {
            planner.plan(
                "open notes",
                &PlanContext {
                    role: item(),
                    ..notes()
                },
            )
        });
        assert!(done.is_ok(), "{done:?}");
        return;
    }
    let offer = open_notes().offers[0].id();
    let (own_url, to_own) = endpoint(vec![plan_answer("c1", &offer)]);
    let (item_url, to_items) = endpoint(vec![plan_answer("i1", &offer)]);
    let dir = tempfile::tempdir().unwrap();
    let usage = dir.path().join("usage.jsonl");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "model::item_model_tests::the_host_planner_routes_items_and_logs_each_model",
        ])
        .env(COORDINATOR_ENDPOINT, own_url)
        .env(DECOMPOSE_ITEM_MODEL_ENV, "item-y")
        .env(DECOMPOSE_ITEM_URL_ENV, format!("openai:{item_url}"))
        .env("TILO_AGENT_USAGE_LOG", &usage)
        .env_remove(DECOMPOSE_ITEM_API_KEY_ENV)
        .env_remove(DECOMPOSE_ITEM_API_KEY_FILE_ENV)
        .env_remove(crate::profile::HEDGE_ENV)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    // The child ran the test, not a filter that matched nothing.
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "{output:?}"
    );
    let model_of = |body: &str| {
        serde_json::from_str::<serde_json::Value>(body).unwrap()["model"]
            .as_str()
            .map(str::to_owned)
    };
    let (_, own_body) = to_own.recv_timeout(tilo_test_env::hang_bound()).unwrap();
    assert_eq!(model_of(&own_body).as_deref(), Some("coordinator-x"));
    let (_, item_body) = to_items.recv_timeout(tilo_test_env::hang_bound()).unwrap();
    assert_eq!(model_of(&item_body).as_deref(), Some("item-y"));
    assert_eq!(to_own.try_iter().count() + to_items.try_iter().count(), 0);

    let lines: Vec<serde_json::Value> = std::fs::read_to_string(&usage)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let calls: Vec<(String, String, Option<u64>)> = lines
        .iter()
        .map(|line| {
            (
                line["model"].as_str().unwrap_or_default().to_owned(),
                line["role"].as_str().unwrap_or_default().to_owned(),
                line["item"].as_u64(),
            )
        })
        .collect();
    assert_eq!(
        calls,
        [
            ("coordinator-x".into(), "coordinator".into(), None),
            ("item-y".into(), "item".into(), Some(1)),
        ]
    );
}
