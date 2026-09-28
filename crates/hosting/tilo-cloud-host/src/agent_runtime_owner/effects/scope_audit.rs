//! The durable owner audit of agent step decisions, and the process-local,
//! bounded per-task rate and content-removal state for scoped grants. None of
//! it grants authority: an absent entry only means "no history yet".
use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;
use std::sync::Mutex;
use tilo_agent_control_protocol::{RateLimiter, Refusal, ScopeBudgets};

const MAX_TASKS: usize = 64;

#[derive(Default)]
struct Tasks {
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

/// Durable per-host audit log of agent step decisions, next to the
/// Store. Owner-private host state; bounded by one rotation.
pub(crate) const AUDIT_FILE: &str = "agent-audit.jsonl";
const MAX_AUDIT_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// One persisted step decision, as the owner's audit read returns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AuditRecord {
    /// Position among the decisions of the same read's tasks.
    pub(crate) sequence: u64,
    pub(crate) app: String,
    pub(crate) action: String,
    /// `None` = allowed; otherwise the refusal reason.
    pub(crate) refused: Option<String>,
    pub(crate) at_ms: u64,
}

/// Append one decision to the durable log and the operator log. Best
/// effort: a failed append is logged and never changes the decision itself.
pub(crate) fn persist(
    root: &std::path::Path,
    invocation: uuid::Uuid,
    app: &str,
    action: &str,
    refused: Option<&str>,
    at_ms: u64,
) {
    use std::io::Write;
    tracing::info!(
        target: "agent_audit",
        %invocation,
        app,
        action,
        allowed = refused.is_none(),
        reason = refused.unwrap_or(""),
        "agent step authorization"
    );
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
/// One page of the durable decisions of `invocations`, oldest first (the
/// rotated log, then the current one), starting at `offset`, and whether
/// more follow. The logs are bounded by one rotation, so a read scans at most
/// about twice [`MAX_AUDIT_FILE_BYTES`] and returns at most `limit` records.
pub(crate) fn read_page(
    root: &std::path::Path,
    invocations: &BTreeSet<String>,
    offset: usize,
    limit: usize,
) -> (Vec<AuditRecord>, bool) {
    let mut page = Vec::new();
    let mut matched = 0usize;
    for name in [format!("{AUDIT_FILE}.1"), AUDIT_FILE.to_string()] {
        let Ok(file) = std::fs::File::open(root.join(name)) else {
            continue;
        };
        for line in std::io::BufReader::new(file).lines() {
            let Ok(line) = line else { break };
            let Ok(entry) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if !entry["invocation"]
                .as_str()
                .is_some_and(|invocation| invocations.contains(invocation))
            {
                continue;
            }
            let sequence = matched;
            matched += 1;
            if sequence < offset {
                continue;
            }
            if page.len() == limit {
                return (page, true);
            }
            page.push(AuditRecord {
                sequence: sequence as u64,
                app: entry["app"].as_str().unwrap_or_default().to_owned(),
                action: entry["action"].as_str().unwrap_or_default().to_owned(),
                refused: (entry["allowed"] != true).then(|| {
                    entry["reason"]
                        .as_str()
                        .filter(|reason| !reason.is_empty())
                        .unwrap_or("refused")
                        .to_owned()
                }),
                at_ms: entry["atMs"].as_u64().unwrap_or_default(),
            });
        }
    }
    (page, false)
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
        let verdict = tilo_agent_control_protocol::check_removal(
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
        let only = |id: uuid::Uuid| BTreeSet::from([id.to_string()]);
        let (kept, more) = read_page(dir.path(), &only(task), 0, 10);
        assert!(!more);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].action, "type-text");
        assert_eq!(kept[0].refused, None);
        assert_eq!(kept[1].refused.as_deref(), Some("budget"));
        assert_eq!(kept[1].sequence, 1);
        // Pages are bounded and continue from their offset.
        let (page, more) = read_page(dir.path(), &only(task), 0, 1);
        assert!(more && page.len() == 1 && page[0].action == "type-text");
        let (page, more) = read_page(dir.path(), &only(task), 1, 1);
        assert!(!more && page.len() == 1 && page[0].action == "erase-text");
        assert_eq!(read_page(dir.path(), &only(first), 0, 10).0.len(), 1);
        assert!(read_page(dir.path(), &BTreeSet::new(), 0, 10).0.is_empty());

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
