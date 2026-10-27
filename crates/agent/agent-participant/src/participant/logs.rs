//! The opt-in JSON-lines logs a run is traced and replayed from:
//! `TILO_AGENT_PLAN_LOG` (every plan) and `TILO_AGENT_ACTION_LOG` (every
//! invoked action). Neither holds a picture's bytes.

use tilo_actions::ActionOutput;
use tilo_actions::door::ActionAddress;
use tilo_browser_peer::DocumentRoute;

use super::running::{FollowUp, produced, step_brief};
use crate::planner::{Obligations, PlanRole, Step};

/// One plan as [`record_plan`] logs it.
pub(super) struct PlanLine<'a> {
    pub agent: &'a str,
    pub task: &'a str,
    pub text: &'a str,
    pub continues: bool,
    pub turn: Option<FollowUp>,
    /// For a repair, which it is and the failure it answers.
    pub repair: Option<serde_json::Value>,
    pub by_model: bool,
    pub steps: &'a Result<Vec<Step>, String>,
    pub declared: &'a Obligations,
    /// The checklist the plan was shown.
    pub shown: Option<serde_json::Value>,
    /// Who made it, in a coordinated task.
    pub role: Option<&'a PlanRole>,
}

/// Appends one JSON line per plan to `TILO_AGENT_PLAN_LOG`, when set: the
/// task, what was planned (its text, or the rest of it, with the follow-up
/// turn it is), and the steps or why there are none, so a task that ended
/// early can be traced to its plan. With them, the obligations the plan
/// declared (each step's in `steps_for`) and the checklist it was shown,
/// each item open, done or set aside; and for a repair, which it is and the
/// failure it answers. A plan of a coordinated task also names its role and
/// item, under the task id its role logs (an item's agent's with its own
/// suffix), and the items and notes it declared.
pub(super) fn record_plan(plan: &PlanLine<'_>) {
    let Some(path) = std::env::var_os("TILO_AGENT_PLAN_LOG") else {
        return;
    };
    append_line(&path, &plan_line(plan), "a plan");
}

/// The line [`record_plan`] appends for `plan`.
pub(super) fn plan_line(plan: &PlanLine<'_>) -> serde_json::Value {
    let PlanLine {
        agent,
        task,
        text,
        continues,
        turn,
        ref repair,
        by_model,
        steps,
        declared,
        ref shown,
        role,
    } = *plan;
    let serves: Option<Vec<Option<usize>>> = steps
        .as_ref()
        .ok()
        .filter(|steps| steps.iter().any(|step| step.serves.is_some()))
        .map(|steps| steps.iter().map(|step| step.serves).collect());
    let mut line = serde_json::json!({
        "at_unix_ms": tilo_clock::unix_now_ms(),
        "agent": agent,
        "task": task,
        "continues": continues,
        "turn": turn.map(|turn| format!("{turn:?}")),
        "by_model": by_model,
        "text": text.chars().take(4_000).collect::<String>(),
        "steps": steps.as_ref().ok().map(|steps| steps.iter().map(step_brief).collect::<Vec<_>>()),
        "error": steps.as_ref().err(),
        "obligations": (!declared.items.is_empty()).then_some(&declared.items),
        "steps_for": serves,
        "set_aside": (!declared.set_aside.is_empty()).then_some(&declared.set_aside),
        "checklist": shown,
        "repair": repair,
    });
    if let (Some(role), Some(fields)) = (role, line.as_object_mut()) {
        fields.insert("task".into(), role.task.clone().into());
        fields.insert("role".into(), role.role.into());
        fields.insert("item".into(), role.item.into());
        if !declared.plans.is_empty() {
            let plans: Vec<serde_json::Value> = declared
                .plans
                .iter()
                .map(|plan| {
                    serde_json::json!({
                        "apps": plan.apps,
                        "after": plan.after,
                        "wait": plan.wait.as_ref().map(|wait| serde_json::json!({
                            "app": wait.app, "contains": wait.contains, "seconds": wait.seconds,
                        })),
                    })
                })
                .collect();
            fields.insert("items".into(), plans.into());
        }
        if !declared.adjust.is_empty() {
            fields.insert("adjust".into(), serde_json::json!(declared.adjust));
        }
        if let Some(reply) = &declared.reply {
            fields.insert("reply".into(), reply.clone().into());
        }
    }
    line
}

/// Appends to `TILO_AGENT_PLAN_LOG`, when set, the end of a task that kept
/// a checklist or had a failed step: its outcome, each obligation as it
/// ended, how many repairs it took and why its last failure ended it. In a
/// coordinated task, an item's agent's end and the coordinator's carry
/// their role.
pub(super) fn record_ended(
    agent: &str,
    task: &str,
    (outcome, role): (&Result<String, String>, Option<&PlanRole>),
    (checklist, repairs): (&serde_json::Value, &serde_json::Value),
) {
    let Some(path) = std::env::var_os("TILO_AGENT_PLAN_LOG") else {
        return;
    };
    let mut line = serde_json::json!({
        "at_unix_ms": tilo_clock::unix_now_ms(),
        "agent": agent,
        "task": task,
        "ended": match outcome {
            Ok(done) => done,
            Err(error) => error,
        },
        "ok": outcome.is_ok(),
        "checklist": checklist,
        "repairs": repairs,
    });
    if let (Some(role), Some(fields)) = (role, line.as_object_mut()) {
        fields.insert("task".into(), role.task.clone().into());
        fields.insert("role".into(), role.role.into());
        fields.insert("item".into(), role.item.into());
    }
    append_line(&path, &line, "a task's end");
}

/// One write per line, as the usage log does: agents append at once.
fn append_line(path: &std::ffi::OsStr, line: &serde_json::Value, what: &str) {
    use std::io::Write as _;
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(format!("{line}\n").as_bytes()));
    if let Err(error) = written {
        tracing::warn!(%error, "cannot record {what}");
    }
}

/// Appends one JSON line per invoked action to `TILO_AGENT_ACTION_LOG`, when
/// set: who did what to which record, with its arguments, when, and how it
/// went. A run can be replayed from it, every record rebuilt step by step.
pub(super) fn record_action(
    agent: &str,
    route: &DocumentRoute,
    address: &ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
    started_ms: u64,
    outcome: &Result<Option<ActionOutput>, String>,
) {
    use std::io::Write as _;
    let Some(path) = std::env::var_os("TILO_AGENT_ACTION_LOG") else {
        return;
    };
    let line = serde_json::json!({
        "started_unix_ms": started_ms,
        "ended_unix_ms": tilo_clock::unix_now_ms(),
        "agent": agent,
        "app": route.app_id,
        "record": route.record,
        "resource": address.resource,
        "action": address.action,
        "arguments": arguments,
        "ok": outcome.is_ok(),
        "error": outcome.as_ref().err(),
        "produced": outcome.as_ref().ok().and_then(Option::as_ref).map(produced).unwrap_or_default(),
        // The resources it answered with: a replay maps the ids it made (a
        // new sheet's) onto the ones the replay makes.
        "resources": outcome
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .map(|output| serde_json::to_value(&output.resources).unwrap_or_default()),
        "value": outcome.as_ref().ok().and_then(Option::as_ref).and_then(|output| output.value.clone()),
    });
    // One write per line, as the usage log does: agents append at once.
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(format!("{line}\n").as_bytes()));
    if let Err(error) = written {
        tracing::warn!(%error, "cannot record an action");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plan of a coordinated task is logged with its role and item under
    /// its role's task id, with the items and notes it declared; an
    /// uncoordinated plan's line is as it was.
    #[test]
    fn a_coordinated_plan_s_line_names_its_role_and_item() {
        let steps: Result<Vec<Step>, String> = Ok(Vec::new());
        let declared = Obligations {
            items: vec!["post Alpha".into()],
            plans: vec![tilo_agent::tools::ItemPlan {
                apps: vec!["chat".into()],
                after: Vec::new(),
                wait: None,
            }],
            adjust: vec![(1, "say Alfa".into())],
            reply: Some("Listed.".into()),
            ..Obligations::default()
        };
        let plan = |role| PlanLine {
            agent: "ana",
            task: "t1",
            text: "post Alpha",
            continues: false,
            turn: None,
            repair: None,
            by_model: true,
            steps: &steps,
            declared: &declared,
            shown: None,
            role,
        };
        let uncoordinated = plan_line(&plan(None));
        assert_eq!(uncoordinated["task"], "t1");
        for key in ["role", "item", "items", "adjust", "reply"] {
            assert!(uncoordinated.get(key).is_none(), "{key}: {uncoordinated}");
        }
        let role = PlanRole {
            role: "item",
            item: Some(1),
            task: "t1/item-1".into(),
        };
        let line = plan_line(&plan(Some(&role)));
        assert_eq!(line["task"], "t1/item-1");
        assert_eq!(line["role"], "item");
        assert_eq!(line["item"], 1);
        assert_eq!(line["items"][0]["apps"], serde_json::json!(["chat"]));
        assert_eq!(line["adjust"], serde_json::json!([[1, "say Alfa"]]));
        assert_eq!(line["reply"], "Listed.");
    }
}
