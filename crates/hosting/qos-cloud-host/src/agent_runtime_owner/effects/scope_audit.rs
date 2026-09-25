//! Process-local, bounded per-task audit and rate state for scoped grants.
//! Neither grants authority: an absent entry only means "no history yet".
use qos_agent_control_protocol::{RateLimiter, Refusal, ScopeBudgets};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

const MAX_TASKS: usize = 64;
const MAX_ENTRIES_PER_TASK: usize = 256;

/// One agent step decision, visible to the owner (MUST-18).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AuditEntry {
    pub(crate) app: String,
    pub(crate) action: String,
    /// `None` = allowed; otherwise the refusal reason.
    pub(crate) refused: Option<String>,
}

#[derive(Default)]
struct Tasks {
    audit: BTreeMap<uuid::Uuid, VecDeque<AuditEntry>>,
    rate: BTreeMap<uuid::Uuid, RateLimiter>,
    removal: BTreeMap<uuid::Uuid, Removal>,
}
/// Content removal of one task: its size at the first step, removed so far,
/// and the open step's baseline.
#[derive(Default)]
struct Removal {
    task_start: Option<u64>,
    removed: u64,
    open: Option<(u32, u64)>,
}
static TASKS: Mutex<Option<Tasks>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut Tasks) -> T) -> Option<T> {
    let mut guard = TASKS.lock().ok()?;
    Some(f(guard.get_or_insert_with(Tasks::default)))
}
fn bound<V>(map: &mut BTreeMap<uuid::Uuid, V>, keep: uuid::Uuid) {
    while map.len() > MAX_TASKS {
        let Some(evict) = map.keys().find(|k| **k != keep).copied() else {
            break;
        };
        map.remove(&evict);
    }
}

/// Durable per-host audit log of agent step decisions (MUST-18), next to the
/// Store. Owner-private host state; bounded by one rotation.
pub(crate) const AUDIT_FILE: &str = "agent-audit.jsonl";
const MAX_AUDIT_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Append one decision to the durable log. Best effort: a failed append is
/// logged and never changes the decision itself.
pub(crate) fn persist(
    root: &std::path::Path,
    invocation: uuid::Uuid,
    app: &str,
    action: &str,
    refused: Option<&str>,
    at_ms: u64,
) {
    use std::io::Write;
    let path = root.join(AUDIT_FILE);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_AUDIT_FILE_BYTES) {
        let _ = std::fs::rename(&path, root.join(format!("{AUDIT_FILE}.1")));
    }
    let line = serde_json::json!({
        "invocation": invocation.to_string(), "app": app, "action": action,
        "allowed": refused.is_none(), "reason": refused, "atMs": at_ms,
    });
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| writeln!(file, "{line}"));
    if let Err(error) = result {
        tracing::warn!(%error, "agent audit append failed");
    }
}
/// The owner's read of one task's durable decisions, oldest first (both
/// the rotated and the current log).
pub(crate) fn read_persisted(
    root: &std::path::Path,
    invocation: uuid::Uuid,
) -> Vec<serde_json::Value> {
    let wanted = invocation.to_string();
    [format!("{AUDIT_FILE}.1"), AUDIT_FILE.to_string()]
        .iter()
        .filter_map(|name| std::fs::read_to_string(root.join(name)).ok())
        .flat_map(|text| {
            text.lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .collect::<Vec<_>>()
        })
        .filter(|entry| entry["invocation"] == wanted.as_str())
        .collect()
}

pub(crate) fn record(invocation: uuid::Uuid, app: &str, action: &str, refused: Option<String>) {
    tracing::info!(
        target: "agent_audit",
        %invocation,
        app,
        action,
        allowed = refused.is_none(),
        reason = refused.as_deref().unwrap_or(""),
        "agent step authorization"
    );
    with(|tasks| {
        let entries = tasks.audit.entry(invocation).or_default();
        if entries.len() == MAX_ENTRIES_PER_TASK {
            entries.pop_front();
        }
        entries.push_back(AuditEntry {
            app: app.into(),
            action: action.into(),
            refused,
        });
        bound(&mut tasks.audit, invocation);
    });
}

/// The retained decisions of one task, oldest first.
#[allow(
    dead_code,
    reason = "owner read path is wired by the agents panel lane"
)]
pub(crate) fn entries(invocation: uuid::Uuid) -> Vec<AuditEntry> {
    with(|tasks| {
        tasks
            .audit
            .get(&invocation)
            .map(|e| e.iter().cloned().collect())
            .unwrap_or_default()
    })
    .unwrap_or_default()
}

/// Baseline of a new step (covered Writer text bytes before it executes).
pub(crate) fn note_step_start(invocation: uuid::Uuid, step: u32, bytes: u64) {
    with(|tasks| {
        let removal = tasks.removal.entry(invocation).or_default();
        removal.task_start.get_or_insert(bytes);
        removal.open = Some((step, bytes));
        bound(&mut tasks.removal, invocation);
    });
}
/// Close a step: `None` when it had no baseline (a repeat or v1 grants),
/// otherwise the content-removal verdict for the step and the task so far.
pub(crate) fn finish_step(
    invocation: uuid::Uuid,
    step: u32,
    after: u64,
    budgets: &ScopeBudgets,
) -> Option<Result<(), Refusal>> {
    with(|tasks| {
        let removal = tasks.removal.get_mut(&invocation)?;
        let (_, before) = removal.open.take_if(|(open, _)| *open == step)?;
        let removed = before.saturating_sub(after);
        let verdict = qos_agent_control_protocol::check_removal(
            budgets,
            removed,
            removal.removed,
            removal.task_start.unwrap_or(before),
        );
        removal.removed = removal.removed.saturating_add(removed);
        Some(verdict)
    })
    .flatten()
}

pub(crate) fn admit_rate(
    invocation: uuid::Uuid,
    budgets: &ScopeBudgets,
    now_ms: u64,
) -> Result<(), Refusal> {
    with(|tasks| {
        let limiter = tasks
            .rate
            .entry(invocation)
            .or_insert_with(|| RateLimiter::new(budgets, now_ms));
        let result = limiter.admit(budgets, now_ms);
        bound(&mut tasks.rate, invocation);
        result
    })
    .unwrap_or(Err(Refusal::Budget("rate state unavailable")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_and_rate_state_stay_bounded() {
        let first = uuid::Uuid::from_u128(0xa0d1);
        for i in 0..(MAX_ENTRIES_PER_TASK + 10) {
            record(
                first,
                "app",
                &format!("a{i}"),
                (i % 2 == 0).then(|| "no".into()),
            );
        }
        let kept = entries(first);
        assert_eq!(kept.len(), MAX_ENTRIES_PER_TASK);
        assert_eq!(
            kept.last().unwrap().action,
            format!("a{}", MAX_ENTRIES_PER_TASK + 9)
        );
        for i in 0..(MAX_TASKS as u128 + 5) {
            record(uuid::Uuid::from_u128(0xb000 + i), "app", "x", None);
        }
        assert!(with(|t| t.audit.len()).unwrap() <= MAX_TASKS);

        let budgets = ScopeBudgets::default();
        let task = uuid::Uuid::from_u128(0xc0de);
        for _ in 0..budgets.invocation_burst {
            admit_rate(task, &budgets, 1_000).unwrap();
        }
        assert!(admit_rate(task, &budgets, 1_000).is_err());
        admit_rate(task, &budgets, 1_200).unwrap();

        // Durable audit: appended, read back per task, owner-private file.
        let dir = tempfile::tempdir().unwrap();
        persist(dir.path(), task, "app", "type-text", None, 1);
        persist(dir.path(), first, "app", "save", Some("not delegable"), 2);
        persist(dir.path(), task, "app", "erase-text", Some("budget"), 3);
        let kept = read_persisted(dir.path(), task);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0]["action"], "type-text");
        assert_eq!(kept[1]["allowed"], false);

        // Content removal: 10 KB document, a step removing all of it stops.
        let big = uuid::Uuid::from_u128(0xd0c);
        note_step_start(big, 0, 10_240);
        assert_eq!(finish_step(big, 0, 10_100, &budgets), Some(Ok(())));
        note_step_start(big, 1, 10_100);
        assert!(matches!(finish_step(big, 1, 0, &budgets), Some(Err(_))));
        // No baseline (repeat commit or another step) gives no verdict.
        assert_eq!(finish_step(big, 1, 0, &budgets), None);
        note_step_start(big, 2, 50);
        assert_eq!(finish_step(big, 3, 0, &budgets), None);
    }
}
