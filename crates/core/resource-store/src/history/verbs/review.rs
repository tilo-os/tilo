//! A stateless, causal review for exact-item restoration. The full change
//! hashes bind both the target and the live document; a checkpoint name is
//! resolved once, never again when the caller restores. This is a write
//! precondition, not a grant or a substitute for action-API admission.

use super::HistoryEdit;
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tilo_actions::ActionSpec;

use crate::history::steps;

const MAX_HEADS: usize = 64;
const MAX_READ_BYTES: usize = 256 * 1024;

/// Copy the `review` object returned by `review-restore` unchanged into
/// `restore-reviewed`. There is no server-local preview session to lose on
/// restart, and no mutable name to resolve differently on replay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreReview {
    schema: u8,
    target_heads: Vec<String>,
    live_heads: Vec<String>,
    paths: Vec<String>,
}

fn heads(values: &[String]) -> Result<Vec<ChangeHash>, String> {
    if values.is_empty() || values.len() > MAX_HEADS {
        return Err(format!(
            "A review needs 1 to {MAX_HEADS} full causal heads."
        ));
    }
    let mut parsed = Vec::with_capacity(values.len());
    for value in values {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("A review head must be a full change hash.".into());
        }
        let hash = value
            .parse::<ChangeHash>()
            .map_err(|error| error.to_string())?;
        if parsed.contains(&hash) {
            return Err("A review cannot repeat a causal head.".into());
        }
        parsed.push(hash);
    }
    parsed.sort_unstable();
    Ok(parsed)
}

impl RestoreReview {
    pub fn apply(&self, doc: &mut AutoCommit) -> Result<(), String> {
        if self.schema != 1 {
            return Err("Unsupported restore review schema.".into());
        }
        if doc.pending_ops() != 0 {
            return Err("Finish pending edits before restoring a reviewed version.".into());
        }
        let target = heads(&self.target_heads)?;
        let live = heads(&self.live_heads)?;
        let mut current = doc.base_heads();
        current.sort_unstable();
        if live != current {
            return Err(
                "Stale restore review: the live document changed; compare and review again.".into(),
            );
        }
        steps::restore_paths(doc, &target, &self.paths).map(drop)
    }
}

/// The review a restoration is taken from, and its restore: both declared with the other history
/// verbs ([`HistoryViewAction::ReviewRestore`](super::HistoryViewAction) and
/// [`HistoryEdit::RestoreReviewed`]), answered here.
pub fn specs() -> Vec<ActionSpec> {
    ["review-restore"]
        .iter()
        .filter_map(|id| <super::HistoryViewAction as tilo_resource::Effect>::spec(id))
        .map(super::published)
        .map(|mut spec| {
            // The declaration types the paths as a list; a review says how many, and of what.
            let schema = paths_schema();
            spec.narrow_param("paths", |param| {
                *param = param.clone().with_schema(schema);
            });
            spec
        })
        .chain(
            ["restore-reviewed"]
                .iter()
                .filter_map(|id| <HistoryEdit as tilo_resource::Effect>::spec(id))
                .map(super::published),
        )
        .collect()
}

/// The schema of the paths a review selects.
fn paths_schema() -> Value {
    json!({
        "type": "array", "minItems": 1, "maxItems": steps::MAX_RESTORE_PATHS,
        "items": {"type": "string", "maxLength": steps::MAX_RESTORE_PATH_BYTES},
    })
}

impl tilo_resource::JsonSchema for RestoreReview {
    fn json_schema() -> Value {
        json!({
            "type": "object", "additionalProperties": false,
            "required": ["schema", "target_heads", "live_heads", "paths"],
            "properties": {
                "schema": {"type": "integer", "const": 1},
                "target_heads": {"type": "array", "minItems": 1, "maxItems": MAX_HEADS,
                    "items": {"type": "string", "pattern": "^[0-9a-fA-F]{64}$"}},
                "live_heads": {"type": "array", "minItems": 1, "maxItems": MAX_HEADS,
                    "items": {"type": "string", "pattern": "^[0-9a-fA-F]{64}$"}},
                "paths": paths_schema(),
            },
        })
    }
}

/// Bound historical reads rather than returning an incomplete document as if
/// it were complete. A caller can read a smaller typed item instead.
pub fn bounded(value: Value) -> Result<Value, String> {
    let bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_READ_BYTES {
        return Err("Historical content exceeds 256 KiB; read a smaller item.".into());
    }
    Ok(value)
}

pub fn answer(doc: &AutoCommit, version: &str, paths: &[String]) -> Result<Value, String> {
    if doc.pending_ops() != 0 {
        return Err("Finish pending edits before reviewing restoration.".into());
    }
    let target = steps::resolve(doc, version)?;
    let live = doc.base_heads();
    let review = RestoreReview {
        schema: 1,
        target_heads: target.iter().map(ToString::to_string).collect(),
        live_heads: live.iter().map(ToString::to_string).collect(),
        paths: paths.to_vec(),
    };
    // Validate every selection, including patch applicability, on a disposable
    // copy. The original transaction, actor, heads and history are untouched.
    review.apply(&mut doc.clone())?;
    let items: Vec<Value> = steps::changed_items(&mut doc.clone(), &target, &live)
        .into_iter()
        .filter(|item| paths.contains(&item.path))
        .map(|item| json!({"item": item.path, "change": item.change}))
        .collect();
    bounded(json!({"review": review, "items": items}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ROOT;

    tilo_test_env::scenario! {
        /// Reviewing a restoration before it is applied.
        fn a_restore_review_never_commits_pending_edits_and_bounds_what_it_reads;

        phase pending_edits_are_never_committed_by_review_or_restore;
        {
            let mut doc = AutoCommit::new();
            doc.put(ROOT, "title", "old").unwrap();
            let target = doc.commit().unwrap();
            doc.put(ROOT, "title", "new").unwrap();
            doc.commit();
            let response = answer(&doc, &target.to_string(), &["title".into()]).unwrap();
            let review: RestoreReview = serde_json::from_value(response["review"].clone()).unwrap();
            doc.put(ROOT, "draft", true).unwrap();
            let before = (doc.base_heads(), doc.pending_ops());
            assert!(answer(&doc, &target.to_string(), &["title".into()]).is_err());
            assert!(review.apply(&mut doc).is_err());
            assert_eq!((doc.base_heads(), doc.pending_ops()), before);
        }

        phase historical_read_limit_refuses_incomplete_content;
        {
            assert!(bounded(json!({"body": "x".repeat(MAX_READ_BYTES)})).is_err());
            assert!(bounded(json!({"body": "short"})).is_ok());
        }
    }
}
