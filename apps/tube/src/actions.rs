//! Tube's exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! These are the collaborative, replicable operations on Tube's shared CRDT
//! state — likes, the watch queue, comment likes + replies — as distinct from
//! Tube's *presentation* (the JSON render-cache blob under `STATE_KEY`, the
//! playhead, the route). An action mutates a structured sub-tree under the
//! document's `"tube"` map so it replicates + replays deterministically:
//!
//! ```text
//! root["tube"]
//! ├── "likes"           (Map)  video_id            -> bool
//! ├── "queue"           (List) video_id, …
//! ├── "comment_likes"   (Map)  "<video_id>#<idx>"  -> bool
//! └── "comment_replies" (Map)  "<video_id>#<idx>"  -> (List) reply text, …
//! ```
//!
//! Liking a video / comment is [`ExecutionLocus::ClientLocal`]; adding to the
//! shared queue and posting a reply are [`ExecutionLocus::ServerPreferred`]
//! (a host serializes the shared list / moderates the reply), falling back to
//! local execution when no host is reachable.
//!
//! **Why no `ui::ActionHost`:** that wrapper hoists the *app-owned* dispatch path
//! — a `perform_action` that brackets an undo boundary, encodes a typed action,
//! and pushes it through a local transport. Tube has none of that. Its
//! collaborative `TubeAction`s are dispatched only through the shared Ctrl+K
//! `ui::ActionPalette`, which decodes each `ActionSpec` and applies it through
//! `qos_actions::LocalTransport` + the registry — already the framework path.
//! Tube's on-screen affordances mutate the in-memory model directly, so there is
//! no hand-rolled dispatch boilerplate for `ActionHost` to replace; embedding it
//! would only add an unused field.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};

use qos_actions::{
    Action, ActionContract, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, ExecutionMode, OperationKind, Resource, ResourceProvider, RetryPolicy,
};

use crate::model::{Route, TubeModel};

/// Registry `type_id` for every Tube action.
pub const TUBE_ACTION_TYPE: &str = "tube.action";
pub const TUBE_OPERATION_TYPE: &str = "tube.operation";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TubeOperation {
    TogglePlayback,
    Share { video_id: String },
}

pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<TubeOperation>, String> {
    if type_id != TUBE_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// Composite comment key: stable across the ordered `Video.comments` list.
fn comment_key(video_id: &str, index: usize) -> String {
    format!("{video_id}#{index}")
}

/// Every collaborative operation Tube exports. One serializable enum so a single
/// registry `type_id` covers the app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TubeAction {
    /// Set the local "liked" flag for a video.
    LikeVideo { video_id: String },
    /// Append a video to the shared watch queue.
    AddToQueue { video_id: String },
    /// Like a comment (addressed by its video + ordered index).
    LikeComment { video_id: String, index: usize },
    ReplyToComment {
        video_id: String,
        index: usize,
        text: String,
    },
}

/// The `root["tube"]` map, created on first write.
fn tube_map(doc: &mut Doc) -> Result<ObjId, ActionError> {
    match doc.get(&ROOT, "tube")? {
        Some((Value::Object(ObjType::Map), id)) => Ok(id),
        _ => Ok(doc.put_object(&ROOT, "tube", ObjType::Map)?),
    }
}

fn child_map(doc: &mut Doc, parent: &ObjId, key: &str) -> Result<ObjId, ActionError> {
    match doc.get(parent, key)? {
        Some((Value::Object(ObjType::Map), id)) => Ok(id),
        _ => Ok(doc.put_object(parent, key, ObjType::Map)?),
    }
}

fn child_list(doc: &mut Doc, parent: &ObjId, key: &str) -> Result<ObjId, ActionError> {
    match doc.get(parent, key)? {
        Some((Value::Object(ObjType::List), id)) => Ok(id),
        _ => Ok(doc.put_object(parent, key, ObjType::List)?),
    }
}

impl Action for TubeAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let tube = tube_map(doc)?;
        match self {
            TubeAction::LikeVideo { video_id } => {
                let likes = child_map(doc, &tube, "likes")?;
                let liked = doc
                    .get(&likes, video_id.as_str())?
                    .and_then(|(value, _)| value.to_bool())
                    .unwrap_or(false);
                doc.put(&likes, video_id.as_str(), ScalarValue::Boolean(!liked))?;
            }
            TubeAction::AddToQueue { video_id } => {
                let queue = child_list(doc, &tube, "queue")?;
                let len = doc.length(&queue);
                doc.insert(&queue, len, ScalarValue::Str(video_id.as_str().into()))?;
            }
            TubeAction::LikeComment { video_id, index } => {
                let likes = child_map(doc, &tube, "comment_likes")?;
                doc.put(
                    &likes,
                    comment_key(video_id, *index).as_str(),
                    ScalarValue::Boolean(true),
                )?;
            }
            TubeAction::ReplyToComment {
                video_id,
                index,
                text,
            } => {
                let replies_map = child_map(doc, &tube, "comment_replies")?;
                let list = child_list(doc, &replies_map, &comment_key(video_id, *index))?;
                let len = doc.length(&list);
                doc.insert(&list, len, ScalarValue::Str(text.as_str().into()))?;
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        match self {
            // Shared, host-serialized state.
            TubeAction::AddToQueue { .. } | TubeAction::ReplyToComment { .. } => {
                ExecutionLocus::ServerPreferred
            }
            // Personal, replicable state.
            TubeAction::LikeVideo { .. } | TubeAction::LikeComment { .. } => {
                ExecutionLocus::ClientLocal
            }
        }
    }
}

/// Register Tube's action decoder so the palette (and remote replay) can
/// reconstruct a [`TubeAction`] from the wire.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<TubeAction>(TUBE_ACTION_TYPE);
}

/// Encode a Tube action; used by the resource specs (and their prompted
/// builders). Serialization of this enum cannot fail — every variant holds
/// only `String`/`usize` fields — so the panic is unreachable; kept explicit
/// rather than swallowing a would-be bug. `ActionSpec::prompted`'s `build`
/// closure must return `EncodedAction` directly (not `Result`), so there is
/// no error path to propagate to instead.
fn encode(action: &TubeAction) -> EncodedAction {
    #[allow(clippy::expect_used)]
    let encoded =
        EncodedAction::encode(TUBE_ACTION_TYPE, action).expect("TubeAction is always serializable");
    encoded
}

/// `ActionSpec::immediate` for a [`TubeAction`]. Every variant holds only
/// `String`/`usize` fields, so encoding cannot fail — see [`encode`].
fn immediate_spec(id: &str, label: &str, action: &TubeAction) -> ActionSpec {
    #[allow(clippy::expect_used)]
    let spec = ActionSpec::immediate(id, label, TUBE_ACTION_TYPE, action)
        .expect("TubeAction always serializes");
    spec
}

/// One-line comment preview for the palette label.
fn comment_label(author: &str, text: &str) -> String {
    let mut t: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > 48 {
        t = t.chars().take(45).collect::<String>() + "…";
    }
    if author.is_empty() {
        t
    } else {
        format!("{author}: {t}")
    }
}

/// Build the resource tree Tube currently has open: on the watch page, the
/// playing video (like / add-to-queue) with its comments as nested resources
/// (like / reply). Off the watch page, nothing is "open", so no rows.
pub fn provide_resources(model: &TubeModel) -> Vec<Resource> {
    let Route::Watch(video_id) = &model.route else {
        return Vec::new();
    };
    let Some(video) = model.video(video_id) else {
        return Vec::new();
    };

    let mut root = Resource::new(
        format!("tube.video:{video_id}"),
        "tube.video",
        video.title.clone(),
    );

    root = root.with_action(
        immediate_spec(
            "like",
            "Like video",
            &TubeAction::LikeVideo {
                video_id: video_id.clone(),
            },
        )
        .with_description("Toggle the like on this video")
        .reversible(),
    );
    for (id, label, kind, operation) in [
        (
            "play-pause",
            "Play or pause",
            OperationKind::View,
            TubeOperation::TogglePlayback,
        ),
        (
            "share",
            "Copy share link",
            OperationKind::Effect,
            TubeOperation::Share {
                video_id: video_id.clone(),
            },
        ),
    ] {
        if let Ok(spec) = ActionSpec::operation(id, label, kind, TUBE_OPERATION_TYPE, &operation) {
            let spec = if id == "share" {
                spec.with_contract(ActionContract {
                    retry: RetryPolicy::Idempotent,
                    tags: vec!["clipboard".into(), "sharing".into()],
                    ..ActionContract::default()
                })
            } else {
                spec
            };
            root = root.with_action(spec);
        }
    }
    root = root.with_action(
        immediate_spec(
            "queue",
            "Add to queue",
            &TubeAction::AddToQueue {
                video_id: video_id.clone(),
            },
        )
        .with_description("Append to the shared watch queue")
        .with_contract(ActionContract {
            retry: RetryPolicy::RetryWithInvocationId,
            execution: ExecutionMode::Immediate,
            ..ActionContract::default()
        })
        .reversible(),
    );

    for (index, comment) in video.comments.iter().enumerate() {
        let vid = video_id.clone();
        let vid2 = video_id.clone();
        let child = Resource::new(
            format!("tube.comment:{}", comment_key(video_id, index)),
            "tube.comment",
            comment_label(&comment.author, &comment.text),
        )
        .with_action(
            immediate_spec(
                "like",
                "Like comment",
                &TubeAction::LikeComment {
                    video_id: vid.clone(),
                    index,
                },
            )
            .reversible(),
        )
        .with_action(ActionSpec::prompted(
            "reply",
            "Reply to comment",
            "Reply",
            "your reply…",
            ExecutionLocus::ServerPreferred,
            move |text: &str| {
                encode(&TubeAction::ReplyToComment {
                    video_id: vid2.clone(),
                    index,
                    text: text.to_string(),
                })
            },
        ));
        root = root.with_child(child);
    }

    vec![root]
}

impl ResourceProvider for crate::app::TubeApp {
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(self.model_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::TubeApp;
    use crate::model::Route;
    use qos_actions::{ActionRegistry, LocalTransport};
    use ui::widget::Widget;

    /// A `TubeApp` sitting on the watch page of a sample video that has comments.
    fn watching() -> (TubeApp, String, usize) {
        let mut model = crate::sample::model();
        let v = model
            .videos
            .iter()
            .find(|v| !v.comments.is_empty())
            .expect("sample has a video with comments")
            .clone();
        let vid = v.id.clone();
        let n_comments = v.comments.len();
        model.route = Route::Watch(vid.clone());
        (TubeApp::new().with_model_for_test(model), vid, n_comments)
    }

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    /// Ctrl+K on tube lists the open video + its comment resources and their
    /// actions. Drives the real resource-collection hook (`Widget::resources`)
    /// and the real `ui::ActionPalette`.
    #[test]
    fn ctrl_k_lists_open_video_and_comment_resources() {
        let (app, _vid, n_comments) = watching();
        let doc = AutoCommit::new();

        // The hook `ui::App::collect_resources` calls each frame.
        let resources = Widget::resources(&app, &doc);
        assert_eq!(resources.len(), 1, "one open video resource");
        let video = &resources[0];
        assert_eq!(video.kind, "tube.video");
        let video_actions: Vec<&str> = video.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(video_actions.contains(&"like"), "video exports like");
        assert!(
            video_actions.contains(&"queue"),
            "video exports add-to-queue"
        );
        assert!(video_actions.contains(&"play-pause"));
        assert!(video_actions.contains(&"share"));
        assert_eq!(
            video
                .actions
                .iter()
                .find(|a| a.id == "queue")
                .unwrap()
                .contract
                .retry,
            RetryPolicy::RetryWithInvocationId
        );

        assert_eq!(
            video.children.len(),
            n_comments,
            "each comment is a child resource"
        );
        let c0 = &video.children[0];
        assert_eq!(c0.kind, "tube.comment");
        let comment_actions: Vec<&str> = c0.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(comment_actions.contains(&"like"), "comment exports like");
        assert!(comment_actions.contains(&"reply"), "comment exports reply");

        // The real Ctrl+K palette, populated from the open resources.
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Like video")),
            "palette lists the video's like action: {labels:?}"
        );
        assert!(labels.iter().any(|l| l.contains("Add to queue")));
        assert!(
            labels.iter().any(|l| l.contains("Reply to comment")),
            "palette lists a comment reply: {labels:?}"
        );
    }

    /// Read `root["tube"]["<map>"]["<key>"]` as a bool.
    fn read_bool(doc: &AutoCommit, map: &str, key: &str) -> Option<bool> {
        let (Value::Object(ObjType::Map), tube) = doc.get(&ROOT, "tube").ok()?? else {
            return None;
        };
        let (Value::Object(ObjType::Map), sub) = doc.get(&tube, map).ok()?? else {
            return None;
        };
        let (v, _) = doc.get(&sub, key).ok()??;
        v.to_bool()
    }

    /// Select a palette row by a substring of its label, then activate it (the
    /// same path Enter takes) and dispatch — returns the dispatch outcome.
    fn run_row(
        palette: &mut ui::ActionPalette,
        doc: &mut AutoCommit,
        registry: &ActionRegistry,
        transport: &mut dyn qos_actions::ActionTransport,
        needle: &str,
    ) -> qos_actions::Dispatched {
        let idx = palette
            .result_labels()
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no palette row matching {needle:?}"));
        palette.set_cursor(idx);
        palette.activate_selected();
        palette
            .take_dispatch(doc, registry, transport)
            .expect("a selection was pending")
            .expect("dispatch succeeded")
    }

    /// Executing the "Like video" action mutates the CRDT.
    #[test]
    fn executing_like_action_mutates_crdt() {
        let (app, vid, _) = watching();
        let mut doc = AutoCommit::new();
        let resources = Widget::resources(&app, &doc);
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();

        assert_eq!(read_bool(&doc, "likes", &vid), None, "not liked yet");
        let mut tx = LocalTransport::new();
        let d = run_row(&mut palette, &mut doc, &registry(), &mut tx, "Like video");
        assert!(matches!(d, qos_actions::Dispatched::AppliedLocal(_)));
        assert_eq!(
            read_bool(&doc, "likes", &vid),
            Some(true),
            "like written to CRDT"
        );
        assert_eq!(tx.replicated.len(), 1, "replicated once");
    }

    /// The `ServerPreferred` "Add to queue" action, with a local (no-host)
    /// transport, falls back to a local apply and mutates the shared queue —
    /// proving the locus plumbing runs on the real app path.
    #[test]
    fn executing_add_to_queue_falls_back_local_and_mutates() {
        let (app, vid, _) = watching();
        let mut doc = AutoCommit::new();
        let resources = Widget::resources(&app, &doc);
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();

        let mut tx = LocalTransport::new();
        let d = run_row(&mut palette, &mut doc, &registry(), &mut tx, "Add to queue");
        assert!(
            matches!(d, qos_actions::Dispatched::AppliedLocalFallback(_)),
            "ServerPreferred fell back to local without a host"
        );
        // The queue list carries the video id.
        let (Value::Object(ObjType::Map), tube) = doc.get(&ROOT, "tube").unwrap().unwrap() else {
            panic!("tube map");
        };
        let (Value::Object(ObjType::List), queue) = doc.get(&tube, "queue").unwrap().unwrap()
        else {
            panic!("queue list");
        };
        assert_eq!(doc.length(&queue), 1);
        assert_eq!(
            doc.get(&queue, 0).unwrap().unwrap().0.to_str(),
            Some(vid.as_str())
        );
    }
}
