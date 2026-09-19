//! Measurement only: calls production app actions and consumes existing UI timing hooks.
use super::*;
use std::path::Path;

fn boundaries(root: &Path) -> Result<serde_json::Value> {
    let mut result = serde_json::Map::new();
    for app in [PHOTOS, WRITER] {
        let mut counts = serde_json::Map::new();
        for suffix in ["frames.jsonl", "log"] {
            let data = std::fs::read(root.join(format!("{app}.{suffix}")))?;
            counts.insert(
                suffix.into(),
                json!(data.iter().filter(|&&b| b == b'\n').count()),
            );
        }
        result.insert(app.into(), counts.into());
    }
    Ok(result.into())
}

pub(super) fn run(
    session: &Session,
    children: &mut Children,
    root: &Path,
    hash: &str,
) -> Result<()> {
    let actions = session
        .broker
        .app_action_broker()
        .ok_or("missing action broker")?;
    let warmup = 10;
    let iterations = 50;
    emit(json!({"event":"benchmark_config", "warmup_per_app":warmup,
        "samples_per_app":iterations, "fixture_width":1280,"fixture_height":720,
        "pace_ms":100,"tail_ms":500,"debug_assertions":cfg!(debug_assertions),
        "profile_window_front":true,
        "profile_resources":true,
        "cpu_worker_correlation":true,
        "workload":"Photos rotates right; Word Processor appends the original photo to a growing document",
        "clock":"std::time::Instant", "latency_boundary":"before broker.start through completion receiver"}));
    for phase in ["warmup", "measure"] {
        std::thread::sleep(Duration::from_millis(500));
        emit(json!({"event":"phase_start", "phase":phase,"boundaries":boundaries(root)?}));
        let count = if phase == "warmup" {
            warmup
        } else {
            iterations
        };
        for iteration in 0..count {
            for (app, action, input) in [
                (PHOTOS, "rotate-right", None),
                (WRITER, "place-image", Some(hash)),
            ] {
                children.live()?;
                // Resolve the current resource surface outside the timed action.
                let discovery = Instant::now();
                let (surface, operation) = wait_action(session, children, app, action)?;
                let discovery_ns = discovery.elapsed().as_nanos();
                let start = Instant::now();
                let job = actions.start(
                    surface.peer_id,
                    &operation.resource,
                    action,
                    input,
                    &session.actor,
                )?;
                let accepted_ns = start.elapsed().as_nanos();
                let completion = job.completion.recv_timeout(Duration::from_secs(30))?;
                let elapsed_ns = start.elapsed().as_nanos();
                let succeeded = matches!(
                    actions.status(job.id),
                    Some(tilo_actions::InvocationUpdate::Succeeded { .. })
                );
                emit(
                    json!({"event":"action_sample","phase":phase,"iteration":iteration,
                    "app":app,"action":action,"invocation":job.id,"discovery_ns":discovery_ns,
                    "accepted_ns":accepted_ns,"elapsed_ns":elapsed_ns,"succeeded":succeeded,
                    "completion_code":completion.code}),
                );
                if !succeeded {
                    return Err(format!("benchmark action failed: {app}/{action}").into());
                }
                // Pacing is excluded from action latency. Frame windows include it.
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        std::thread::sleep(Duration::from_millis(500));
        children.live()?;
        emit(json!({"event":"phase_end","phase":phase,"boundaries":boundaries(root)?}));
    }
    emit(
        json!({"event":"benchmark_passed","app_pids":children.0.iter().map(Child::id).collect::<Vec<_>>()}),
    );
    Ok(())
}
