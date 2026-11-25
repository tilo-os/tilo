//! A person's request is answered while an agent fills the workspace, about as fast as in a quiet
//! one: the person's listing and filing wait at most for the one agent list already running, never
//! for the lists the agents sent before it.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::language::agent_fixture::act;
use crate::language::host;

/// What the agent brings in, in lists of `LIST`, from `STREAMS` workers at once.
const ISSUES: usize = 2_000;
const LIST: usize = 125;
const STREAMS: usize = 4;

fn listed_issues(first: usize, count: usize) -> Value {
    let titles: Vec<Value> = (first..first.saturating_add(count))
        .map(|at| json!(format!("Imported issue {at}")))
        .collect();
    json!({ "titles": titles })
}

fn median(times: &mut [Duration]) -> Duration {
    times.sort();
    times.get(times.len() / 2).copied().unwrap_or_default()
}

/// The agent's workers send their lists one after another, several at once; the person's listing
/// and filing go through meanwhile, each waiting at most for the one list running when they asked.
#[test]
fn a_persons_requests_go_before_an_agents_bulk_import() {
    host::run(|host| async move {
        let import = host.agent();
        let (first, failed) = act(&import, "so.tilo.tasks#new_tasks", listed_issues(0, LIST)).await;
        assert!(
            !failed,
            "the agent brings its tracker into the workspace the daemon serves: {first}"
        );

        // The person's asks: the tracker's listing and one issue of their own.
        let person = host.key("Ada's backend");
        let workspace = host.workspace().to_owned();
        let asks = || {
            let person = &person;
            let workspace = workspace.clone();
            async move {
                let started = Instant::now();
                let (status, answer) = person
                    .get(&format!(
                        "/v1/workspaces/{workspace}/resources/so.tilo.tasks"
                    ))
                    .await;
                let listing = started.elapsed();
                assert_eq!(status, 200, "{answer}");
                let started = Instant::now();
                let (status, written) = person
                    .post(
                        &format!(
                            "/v1/workspaces/{workspace}/resources/so.tilo.tasks/actions/new_task"
                        ),
                        &json!({"title": "The person's own issue"}),
                    )
                    .await;
                (listing, started.elapsed(), status, written)
            }
        };
        let (_, _, status, written) = asks().await;
        assert_eq!(status, 200, "the person files an issue: {written}");

        // Busy: the agent's workers send their lists, one after another each.
        let per_stream = ISSUES / STREAMS;
        let mut workers = Vec::new();
        for stream in 0..STREAMS {
            let agent = host.agent();
            workers.push(tokio::spawn(async move {
                let mut lists = Vec::new();
                for first in (stream * per_stream..(stream + 1) * per_stream).step_by(LIST) {
                    let started = Instant::now();
                    let (error, failed) = act(
                        &agent,
                        "so.tilo.tasks#new_tasks",
                        listed_issues(first, LIST),
                    )
                    .await;
                    assert!(!failed, "the agent's list is taken: {error}");
                    lists.push(started.elapsed());
                }
                lists
            }));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let (mut busy, mut agent_lists) = (Vec::new(), Vec::new());
        while !workers.iter().all(tokio::task::JoinHandle::is_finished) {
            let (listing, filing, status, answer) = asks().await;
            assert_eq!(status, 200, "{answer}");
            busy.push(listing + filing);
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        for worker in workers {
            agent_lists.extend(worker.await.unwrap());
        }
        assert!(
            busy.len() >= 3,
            "the person asked several times while the agent worked"
        );
        let typical_ask = median(&mut busy.clone());
        let typical_agent_list = median(&mut agent_lists.clone());
        assert!(
            typical_ask <= typical_agent_list,
            "the person's ask took {typical_ask:?} at the median while the agent's lists took \
             {typical_agent_list:?}: they waited behind the agent's work"
        );
    });
}
