//! A stateless, causal review for exact-item restoration. The full change
//! hashes bind both the target and the live document; a checkpoint name is
//! resolved once, never again when the caller restores. This is a write
//! precondition, not a grant or a substitute for action-door admission.

use super::{HistoryEdit, HistoryView, VIEW_TYPE, edit, invalid};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tilo_actions::{ActionKind, ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType};
use ui_core::automerge::transaction::Transactable;
use ui_core::automerge::{AutoCommit, ChangeHash};
use ui_core::history_source::steps;

const MAX_HEADS: usize = 64;
const MAX_READ_BYTES: usize = 256 * 1024;

/// Copy the `review` object returned by `review-restore` unchanged into
/// `restore-reviewed`. There is no server-local preview session to lose on
/// restart, and no mutable name to resolve differently on replay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RestoreReview {
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
    pub(super) fn apply(&self, doc: &mut AutoCommit) -> Result<(), String> {
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

pub(super) fn specs() -> Vec<ActionSpec> {
    let paths_schema = json!({
        "type": "array", "minItems": 1, "maxItems": steps::MAX_RESTORE_PATHS,
        "items": {"type": "string", "maxLength": steps::MAX_RESTORE_PATH_BYTES},
    });
    let mut review = ActionSpec::typed_operation(
        "review-restore",
        "Review restoration",
        "Review restoration",
        OperationKind::View,
        VIEW_TYPE,
        vec![
            ActionParam::required("version", ParamType::Text, "A change id, causal frontier, or checkpoint"),
            ActionParam::required("paths", ParamType::Array, "Exact differing paths from diff; a root selects its entire value")
                .with_schema(paths_schema.clone()),
        ],
        |args| {
            let built = (|| {
                let version = super::text_arg(args, "version")?;
                let paths = args.array("paths").ok_or("paths are required")?
                    .iter().map(|value| value.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>().ok_or("paths must be strings")?;
                EncodedAction::encode_operation(VIEW_TYPE, &HistoryView::ReviewRestore { version, paths })
                    .map_err(|error| error.to_string())
            })();
            built.unwrap_or_else(|reason: String| invalid(&reason))
        },
    )
    .of_kind(ActionKind::Read)
    .with_description("Review exact-item restoration without writing. Returns pinned target and live heads; copy review into restore-reviewed. Any later edit requires a new review.")
    .reads_document();
    review.type_mut().contract.result = tilo_actions::ResultSpec::Json {
        schema: json!({"type": "object"}),
    };
    let restore = edit(
        "restore-reviewed",
        "Restore reviewed items",
        "Restore precisely the reviewed paths as a new undoable change, only if the live causal frontier still matches. Empty selection never restores everything.",
        vec![
            ActionParam::required(
                "review",
                ParamType::Object,
                "The review object from review-restore",
            )
            .with_schema(json!({
                "type": "object", "additionalProperties": false,
                "required": ["schema", "target_heads", "live_heads", "paths"],
                "properties": {
                    "schema": {"type": "integer", "const": 1},
                    "target_heads": {"type": "array", "minItems": 1, "maxItems": MAX_HEADS,
                        "items": {"type": "string", "pattern": "^[0-9a-fA-F]{64}$"}},
                    "live_heads": {"type": "array", "minItems": 1, "maxItems": MAX_HEADS,
                        "items": {"type": "string", "pattern": "^[0-9a-fA-F]{64}$"}},
                    "paths": paths_schema,
                },
            })),
        ],
        |args| {
            let review = args.get("review").ok_or("review is required")?;
            let review =
                serde_json::from_value(review.to_json()).map_err(|error| error.to_string())?;
            Ok(HistoryEdit::RestoreReviewed { review })
        },
    );
    vec![review, restore]
}

/// Bound historical reads rather than returning an incomplete document as if
/// it were complete. A caller can read a smaller typed item instead.
pub(crate) fn bounded(value: Value) -> Result<Value, String> {
    let bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_READ_BYTES {
        return Err("Historical content exceeds 256 KiB; read a smaller item.".into());
    }
    Ok(value)
}

pub(super) fn answer(doc: &AutoCommit, version: &str, paths: &[String]) -> Result<Value, String> {
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
    use ui_core::automerge::ROOT;

    #[test]
    fn pending_edits_are_never_committed_by_review_or_restore() {
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

    #[derive(tilo_resource::Model, Default, Debug, Clone, PartialEq)]
    pub struct ReplayBoard {
        pub title: String,
    }

    #[tilo_resource::resource(kind = "test.review-replay")]
    impl ReplayBoard {}

    #[test]
    fn reviewed_restore_replays_identically_beside_concurrent_work_and_duplicate_delivery() {
        use super::super::{EDIT_TYPE, ResourceHistoryEdit};
        use ui_core::automerge::{ActorId, ReadDoc};
        let mut origin = AutoCommit::new();
        origin.put(ROOT, "title", "old").unwrap();
        let target = origin.commit().unwrap();
        origin.put(ROOT, "title", "new").unwrap();
        origin.commit();
        let response = answer(&origin, &target.to_string(), &["title".into()]).unwrap();
        let review: RestoreReview = serde_json::from_value(response["review"].clone()).unwrap();
        let edit = HistoryEdit::RestoreReviewed { review };
        let encoded = EncodedAction {
            type_id: EDIT_TYPE.into(),
            payload: serde_json::to_vec(&edit).unwrap(),
        };
        let mut registry = tilo_actions::ActionRegistry::new();
        registry.register::<ResourceHistoryEdit<ReplayBoard>>(EDIT_TYPE);
        let action = registry.decode(&encoded).unwrap();
        let mut peer = origin.fork();
        peer.set_actor(ActorId::from(b"peer".as_slice()));
        peer.put(ROOT, "unrelated", "keep").unwrap();
        let unrelated = peer.commit().unwrap();
        let mut transport = tilo_actions::LocalTransport::new();
        tilo_actions::dispatch_with_context(
            &mut origin,
            action.as_ref(),
            encoded,
            &mut transport,
            tilo_actions::ActionContext::for_resource("board", "test.review-replay", "Board"),
        )
        .unwrap();
        let restored = origin.get_heads()[0];
        tilo_actions::replay(&mut peer, &transport.replicated[0], &registry).unwrap();
        assert!(peer.get_heads().contains(&restored));
        assert!(peer.get_heads().contains(&unrelated));
        assert_eq!(
            peer.get(ROOT, "title").unwrap().unwrap().0.as_str(),
            Some("old")
        );
        assert_eq!(
            peer.get(ROOT, "unrelated").unwrap().unwrap().0.as_str(),
            Some("keep")
        );
        assert_eq!(
            tilo_actions::verify_change(&mut peer, restored, &registry).unwrap(),
            tilo_actions::Verified::Reproduced
        );
        let before = peer.get_heads();
        tilo_actions::replay(&mut peer, &transport.replicated[0], &registry).unwrap();
        assert_eq!(peer.get_heads(), before);
    }

    #[test]
    fn historical_read_limit_refuses_incomplete_content() {
        assert!(bounded(json!({"body": "x".repeat(MAX_READ_BYTES)})).is_err());
        assert!(bounded(json!({"body": "short"})).is_ok());
    }
}
