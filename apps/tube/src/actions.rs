//! Tube's exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! These are the collaborative, replicable operations on Tube's shared CRDT
//! state — likes, the watch queue, comment likes + replies — as distinct from
//! Tube's device-local presentation (fetched catalogues, playhead and route).
//! Watch history is intentionally absent: it is held by the separate
//! owner-local action/store in `history_store`, never in collaboration data.
//! An action mutates a structured sub-tree under the
//! document's `"tube"` map so it replicates + replays deterministically:
//!
//! ```text
//! root["tube"]
//! ├── "likes"           (Map)  video_id            -> bool
//! ├── "queue"           (Map)  video_id -> bool (membership/tombstone)
//! ├── "comment_likes"   (Map)  comment_id -> bool (membership/tombstone)
//! ├── "comment_replies" (Map)  comment_id -> (Map reply_id -> reply record)
//! └── "settings"        (Map)  durable shared preferences
//! ```
//!
//! Liking a video / comment is [`ExecutionLocus::ClientLocal`]; adding to the
//! shared queue and posting a reply are [`ExecutionLocus::ServerPreferred`]
//! (a host serializes the shared list / moderates the reply); they are refused
//! when no authoritative host is reachable.
//!
//! On-screen collaborative mutations use Tube's embedded [`ui::ActionHost`]
//! with the same encoded actions published here. View/effect operations are
//! decoded by the widget's resource-operation door, so Ctrl+K, agents and
//! replay do not need app-specific callbacks.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};

use tilo_actions::{
    Action, ActionContract, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc,
    EncodedAction, ExecutionLocus, ExecutionMode, OperationKind, ParamType, Resource, RetryPolicy,
};

use crate::action::SettingFlag;
use crate::model::{Route, TubeModel};

const MAX_RESOURCE_ENTITIES: usize = 100;
const MAX_RESOURCE_COMMENTS: usize = 50;
const MAX_RESOURCE_LABEL_CHARS: usize = 160;
const QUALITY_OPTIONS: [&str; 4] = ["1080p", "720p", "480p", "auto"];

/// Registry `type_id` for every Tube action.
pub const TUBE_ACTION_TYPE: &str = "tube.action";
pub const TUBE_OPERATION_TYPE: &str = "tube.operation";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TubeOperation {
    TogglePlayback,
    ToggleSidebar,
    Share {
        video_id: String,
    },
    Navigate {
        route: Route,
    },
    OpenVideo {
        video_id: String,
    },
    OpenChannel {
        channel_id: String,
    },
    OpenPlaylist {
        playlist_id: String,
    },
    /// Persisting media requires a host storage/download capability, which the
    /// current Tube backend intentionally does not claim to provide.
    Download {
        video_id: String,
    },
    PairDevice {
        invite: String,
    },
    SyncDevices,
    ApproveDevice {
        peer_id: String,
    },
    RevokeDevice {
        peer_id: String,
    },
    RecordHistory {
        video_id: String,
    },
    RemoveHistory {
        video_id: String,
    },
    ClearHistory,
}

pub fn operation_kind(operation: &TubeOperation) -> OperationKind {
    match operation {
        TubeOperation::Share { .. }
        | TubeOperation::PairDevice { .. }
        | TubeOperation::SyncDevices
        | TubeOperation::ApproveDevice { .. }
        | TubeOperation::RevokeDevice { .. }
        | TubeOperation::Download { .. }
        | TubeOperation::RecordHistory { .. }
        | TubeOperation::RemoveHistory { .. }
        | TubeOperation::ClearHistory => OperationKind::Effect,
        TubeOperation::TogglePlayback
        | TubeOperation::ToggleSidebar
        | TubeOperation::Navigate { .. }
        | TubeOperation::OpenVideo { .. }
        | TubeOperation::OpenChannel { .. }
        | TubeOperation::OpenPlaylist { .. } => OperationKind::View,
    }
}

/// Process-local device resources. Pairing invites are explicit `Secret`
/// parameters, so agents can drive the operation while hosts redact the value
/// from logs, history, and confirmation copy. Private key material is absent.
pub fn provide_device_resources(view: &crate::sync::SyncView) -> Resource {
    let unavailable = matches!(view.status, crate::sync::SyncStatus::Unavailable);
    let unavailable_reason = if view.summary.trim().is_empty() {
        crate::sync::capability().summary.to_string()
    } else {
        view.summary.clone()
    };
    let mut root = Resource::new(
        "tube.device-sync",
        "tube.device-sync",
        if unavailable {
            "Grayjay device sync (unavailable)"
        } else {
            "Grayjay device sync"
        },
    );
    // Runtime/built-in capability failure is availability, not merely
    // explanatory text. Keep stable operations in the resource tree so agents
    // and history can report *why* they cannot run instead of mistaking a
    // missing action for a missing integration.
    if unavailable {
        return unavailable_device_resource_actions(root, &unavailable_reason);
    }
    root = root.with_action(device_pair_action());

    let has_active_peer = view.peers.iter().any(crate::sync::PeerRecord::is_active);
    let mut root_actions = Vec::new();
    if has_active_peer {
        root_actions.push((
            "sync",
            "Sync approved devices now",
            TubeOperation::SyncDevices,
        ));
    }
    for (id, label, operation) in root_actions {
        if let Some(mut spec) = view_spec(id, label, &operation) {
            spec = spec.retry_policy(if id == "sync" {
                RetryPolicy::Idempotent
            } else {
                RetryPolicy::AtMostOnce
            });
            root = root.with_action(spec);
        }
    }
    for peer in &view.peers {
        if peer.revoked_at.is_some() {
            continue;
        }
        let mut resource = Resource::new(
            format!("tube.device:{}", peer.id),
            "tube.device",
            peer.name.clone(),
        );
        if peer.pending_approval {
            if let Some(spec) = view_spec(
                "approve",
                "Approve device",
                &TubeOperation::ApproveDevice {
                    peer_id: peer.id.clone(),
                },
            ) {
                resource =
                    resource.with_action(spec.privileged().retry_policy(RetryPolicy::Idempotent));
            }
        } else if let Some(spec) = view_spec(
            "revoke",
            "Revoke device access",
            &TubeOperation::RevokeDevice {
                peer_id: peer.id.clone(),
            },
        ) {
            resource =
                resource.with_action(spec.destructive().retry_policy(RetryPolicy::Idempotent));
        }
        root = root.with_child(resource);
    }
    root
}

fn device_pair_action() -> ActionSpec {
    ActionSpec::typed_operation(
        "pair",
        "Pair using invite",
        "Paste a Grayjay pairing invite",
        OperationKind::Effect,
        TUBE_OPERATION_TYPE,
        vec![
            ActionParam::required(
                "invite",
                ParamType::Secret,
                "Grayjay pairing invite; redacted from logs and history",
            )
            .with_schema(serde_json::json!({ "minLength": 1, "maxLength": 16384 })),
        ],
        |args| {
            let operation = TubeOperation::PairDevice {
                invite: args.text("invite").unwrap_or_default().to_string(),
            };
            EncodedAction {
                type_id: TUBE_OPERATION_TYPE.into(),
                payload: serde_json::to_vec(&operation).unwrap_or_default(),
            }
        },
    )
    .privileged()
    .retry_policy(RetryPolicy::AtMostOnce)
}

fn unavailable_device_resource_actions(mut root: Resource, reason: &str) -> Resource {
    root = root.with_action(device_pair_action().unavailable("grayjay_sync_unavailable", reason));
    if let Some(sync) = view_spec(
        "sync",
        "Sync approved devices now",
        &TubeOperation::SyncDevices,
    ) {
        root = root.with_action(
            sync.retry_policy(RetryPolicy::Idempotent)
                .unavailable("grayjay_sync_unavailable", reason),
        );
    }
    root
}

/// Decode an operation received from the resource/action host.
///
/// # Errors
///
/// Returns an error when a payload for Tube's operation type is not valid JSON.
pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<TubeOperation>, String> {
    if type_id != TUBE_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// Stable content identity for a comment. The upstream model has no comment
/// id, so hash all immutable source fields with an explicitly fixed FNV-1a
/// algorithm rather than addressing a row by its current list position.
fn comment_key(video_id: &str, comment: &crate::model::Comment) -> String {
    let mut hasher = fnv::FnvHasher::default();
    for field in [
        video_id,
        comment.author.as_str(),
        comment.initials.as_str(),
        comment.text.as_str(),
        comment.time_ago.as_str(),
    ] {
        std::hash::Hasher::write(&mut hasher, field.as_bytes());
        std::hash::Hasher::write_u8(&mut hasher, 0xff);
    }
    format!("{video_id}#{:016x}", std::hash::Hasher::finish(&hasher))
}

/// Deterministic reply entity identity. Equal text is intentionally idempotent
/// for one comment, which gives a retried prompted action the same address
/// rather than accidentally posting a duplicate. The reply action carries the
/// whole key, so replay never re-hashes under a changed implementation.
fn reply_key(comment_id: &str, text: &str) -> String {
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, comment_id.as_bytes());
    std::hash::Hasher::write_u8(&mut hasher, 0xff);
    std::hash::Hasher::write(&mut hasher, text.as_bytes());
    format!("reply-{:016x}", std::hash::Hasher::finish(&hasher))
}

/// Every collaborative operation Tube exports. One serializable enum so a single
/// registry `type_id` covers the app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TubeAction {
    /// Legacy receiver-dependent video-like toggle. Kept only so old
    /// envelopes decode; applying it is refused because it cannot replay
    /// deterministically.
    LikeVideo {
        video_id: String,
    },
    /// Set the state resolved by the authoring surface. Retains the legacy
    /// toggle variant above so recorded envelopes remain replayable.
    SetVideoLiked {
        video_id: String,
        liked: bool,
    },
    /// Set one queue membership explicitly. This is safe to replay, retry and
    /// merge: `false` is a tombstone, never a list deletion whose index can drift.
    SetQueueMembership {
        video_id: String,
        queued: bool,
    },
    /// Legacy shared-history envelope. Kept only so a replay can be rejected
    /// safely; owner history has a distinct local-only action type.
    SetHistoryEntry {
        owner_id: String,
        video_id: String,
        watched_at_ms: u64,
        present: bool,
        mutation_id: String,
    },
    /// Legacy shared-history envelope, refused before it can write a document.
    ClearHistory {
        owner_id: String,
        cleared_at_ms: u64,
        clear_id: String,
    },
    /// Legacy receiver-dependent subscription toggle. Kept only for decoding;
    /// new authors must use [`Self::SetSubscription`].
    ToggleSubscription {
        channel_id: String,
    },
    /// Set the state resolved by the authoring surface. Retains the legacy
    /// toggle variant above so recorded envelopes remain replayable.
    SetSubscription {
        channel_id: String,
        subscribed: bool,
    },
    SetSetting {
        setting: SettingFlag,
        enabled: bool,
    },
    /// Legacy receiver-dependent quality cycle. Kept only for decoding;
    /// applying it is refused because a peer may start at another quality.
    CyclePreferredQuality,
    /// Set the allowed preferred quality chosen by the authoring surface.
    SetPreferredQuality {
        quality: String,
    },
    /// Set one `SponsorBlock` category's handling mode. The target mode is
    /// embedded instead of inferred from the document so the visible control,
    /// a replay and an agent all make the same transition (including Filler,
    /// whose default is Off rather than Auto-skip).
    SetSponsorCategory {
        category: crate::model::Category,
        mode: crate::model::SkipMode,
    },
    /// Absolute comment-like membership. `liked: false` is retained as a
    /// tombstone rather than deleting an address that a concurrent client may
    /// still be updating.
    SetCommentLiked {
        comment_id: String,
        liked: bool,
    },
    /// Set one deterministic reply entity. Reply mutations are host-mediated
    /// because a host may apply moderation before making the value visible.
    SetCommentReply {
        comment_id: String,
        reply_id: String,
        text: String,
        active: bool,
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

impl Action for TubeAction {
    #[allow(
        clippy::too_many_lines,
        reason = "one explicit storage transition per durable Tube action keeps action replay auditable"
    )]
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        // These envelopes were emitted by older Tube versions. A receiver's
        // document state must never decide an authored transition, so retain
        // decoder compatibility but refuse them before even creating `tube`.
        match self {
            TubeAction::LikeVideo { .. }
            | TubeAction::ToggleSubscription { .. }
            | TubeAction::CyclePreferredQuality
            // History has a separate owner-local action type/store. Refusing
            // these legacy envelopes here is deliberate: an encoded public
            // Tube action must never recreate a history subtree in a shared
            // collaboration document.
            | TubeAction::SetHistoryEntry { .. }
            | TubeAction::ClearHistory { .. } => {
                return Err(ActionError::Refused(
                    "this Tube action is not accepted by the shared document".into(),
                ));
            }
            TubeAction::SetPreferredQuality { quality }
                if !QUALITY_OPTIONS.contains(&quality.as_str()) =>
            {
                return Err(ActionError::Refused(format!(
                    "unsupported preferred quality: {quality}"
                )));
            }
            _ => {}
        }
        let tube = tube_map(doc)?;
        match self {
            TubeAction::LikeVideo { .. }
            | TubeAction::ToggleSubscription { .. }
            | TubeAction::CyclePreferredQuality
            | TubeAction::SetHistoryEntry { .. }
            | TubeAction::ClearHistory { .. } => {
                return Err(ActionError::Refused(
                    "legacy Tube toggle/cycle actions cannot replay deterministically".into(),
                ));
            }
            TubeAction::SetVideoLiked { video_id, liked } => {
                let likes_map = child_map(doc, &tube, "likes")?;
                doc.put(&likes_map, video_id.as_str(), ScalarValue::Boolean(*liked))?;
            }
            TubeAction::SetQueueMembership { video_id, queued } => {
                let queue = child_map(doc, &tube, "queue")?;
                doc.put(&queue, video_id.as_str(), ScalarValue::Boolean(*queued))?;
            }
            TubeAction::SetSubscription {
                channel_id,
                subscribed,
            } => {
                let subscriptions = child_map(doc, &tube, "subscriptions")?;
                doc.put(
                    &subscriptions,
                    channel_id.as_str(),
                    ScalarValue::Boolean(*subscribed),
                )?;
            }
            TubeAction::SetSetting { setting, enabled } => {
                let settings = child_map(doc, &tube, "settings")?;
                let key = match setting {
                    SettingFlag::SponsorBlock => "sponsorblock",
                    SettingFlag::DeArrow => "dearrow",
                    SettingFlag::DeArrowTitles => "dearrow_titles",
                    SettingFlag::DeArrowThumbnails => "dearrow_thumbnails",
                    SettingFlag::ReturnDislike => "return_dislike",
                    SettingFlag::Autoplay => "autoplay",
                    SettingFlag::ShowComments => "show_comments",
                };
                doc.put(&settings, key, ScalarValue::Boolean(*enabled))?;
            }
            TubeAction::SetPreferredQuality { quality } => {
                let settings = child_map(doc, &tube, "settings")?;
                doc.put(
                    &settings,
                    "preferred_quality",
                    ScalarValue::Str(quality.as_str().into()),
                )?;
            }
            TubeAction::SetSponsorCategory { category, mode } => {
                apply_sponsor_category(doc, &tube, *category, *mode)?;
            }
            TubeAction::SetCommentLiked { comment_id, liked } => {
                let likes = child_map(doc, &tube, "comment_likes")?;
                doc.put(&likes, comment_id, ScalarValue::Boolean(*liked))?;
            }
            TubeAction::SetCommentReply {
                comment_id,
                reply_id,
                text,
                active,
            } => {
                let replies_map = child_map(doc, &tube, "comment_replies")?;
                let replies = child_map(doc, &replies_map, comment_id)?;
                // The value is a small record rather than a list item. Keeping
                // `active` beside the immutable text means a removal is an
                // addressable tombstone and concurrent reply entities merge.
                let reply = child_map(doc, &replies, reply_id)?;
                doc.put(&reply, "text", ScalarValue::Str(text.as_str().into()))?;
                doc.put(&reply, "active", ScalarValue::Boolean(*active))?;
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        match self {
            // Shared, host-serialized state.
            TubeAction::SetQueueMembership { .. } | TubeAction::SetCommentReply { .. } => {
                ExecutionLocus::ServerPreferred
            }
            // Client-local shared-document mutations. History variants above
            // are rejected by `apply` and cannot enter this document.
            TubeAction::LikeVideo { .. }
            | TubeAction::SetVideoLiked { .. }
            | TubeAction::SetHistoryEntry { .. }
            | TubeAction::ClearHistory { .. }
            | TubeAction::SetCommentLiked { .. }
            | TubeAction::ToggleSubscription { .. }
            | TubeAction::SetSubscription { .. }
            | TubeAction::SetSetting { .. }
            | TubeAction::CyclePreferredQuality
            | TubeAction::SetPreferredQuality { .. }
            | TubeAction::SetSponsorCategory { .. } => ExecutionLocus::ClientLocal,
        }
    }
}

fn apply_sponsor_category(
    doc: &mut Doc,
    tube: &ObjId,
    category: crate::model::Category,
    mode: crate::model::SkipMode,
) -> Result<(), ActionError> {
    let settings = child_map(doc, tube, "settings")?;
    let categories = child_map(doc, &settings, "sponsor_categories")?;
    let stored = match mode {
        crate::model::SkipMode::Skip => "skip",
        crate::model::SkipMode::Show => "show",
        crate::model::SkipMode::Disabled => "disabled",
    };
    doc.put(
        &categories,
        category.api_key(),
        ScalarValue::Str(stored.into()),
    )?;
    Ok(())
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
    #[allow(
        clippy::expect_used,
        reason = "TubeAction holds only String and usize fields, so it always serializes"
    )]
    let encoded =
        EncodedAction::encode(TUBE_ACTION_TYPE, action).expect("TubeAction is always serializable");
    encoded
}

/// `ActionSpec::immediate` for a [`TubeAction`]. Every variant holds only
/// `String`/`usize` fields, so encoding cannot fail — see [`encode`].
fn immediate_spec(id: &str, label: &str, action: &TubeAction) -> ActionSpec {
    #[allow(
        clippy::expect_used,
        reason = "TubeAction holds only String and usize fields, so it always serializes"
    )]
    let spec = ActionSpec::immediate(id, label, TUBE_ACTION_TYPE, action)
        .expect("TubeAction always serializes");
    spec
}

fn next_quality(current: &str) -> String {
    let index = QUALITY_OPTIONS
        .iter()
        .position(|quality| *quality == current)
        .unwrap_or(0);
    QUALITY_OPTIONS
        .get((index + 1) % QUALITY_OPTIONS.len())
        .copied()
        .unwrap_or("1080p")
        .to_string()
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

/// Resource labels originate in remote metadata and are shown in global
/// surfaces. Keep them single-line and bounded before publishing, so a hostile
/// title/creator/query cannot dominate the palette or force unbounded text
/// shaping work. Resource actions retain the untouched canonical ids.
fn resource_label(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    for _ in 0..MAX_RESOURCE_LABEL_CHARS {
        let Some(ch) = chars.next() else {
            return out;
        };
        out.push(if ch.is_control() { ' ' } else { ch });
    }
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

fn view_spec(id: &str, label: &str, operation: &TubeOperation) -> Option<ActionSpec> {
    ActionSpec::operation(
        id,
        label,
        operation_kind(operation),
        TUBE_OPERATION_TYPE,
        operation,
    )
    .ok()
}

fn video_resource(model: &TubeModel, video_id: &str, include_comments: bool) -> Option<Resource> {
    let video = model.video(video_id)?;
    let mut resource = Resource::new(
        format!("tube.video:{video_id}"),
        "tube.video",
        resource_label(&video.title),
    );
    if let Some(open) = view_spec(
        "open",
        "Open video",
        &TubeOperation::OpenVideo {
            video_id: video_id.to_string(),
        },
    ) {
        resource = resource.with_action(open);
    }
    resource = resource
        .with_action(
            immediate_spec(
                "like",
                "Like video",
                &TubeAction::SetVideoLiked {
                    video_id: video_id.to_string(),
                    liked: !model.is_liked(video_id),
                },
            )
            .with_description("Set the like state for this video")
            .reversible(),
        )
        .with_action(
            immediate_spec(
                "queue",
                if model.is_queued(video_id) {
                    "Remove from queue"
                } else {
                    "Add to queue"
                },
                &TubeAction::SetQueueMembership {
                    video_id: video_id.to_string(),
                    queued: !model.is_queued(video_id),
                },
            )
            .with_description("Set membership in the shared watch queue")
            .with_contract(ActionContract {
                retry: RetryPolicy::AtMostOnce,
                execution: ExecutionMode::Immediate,
                ..ActionContract::default()
            })
            .reversible(),
        );
    if let Some(mark_watched) = view_spec(
        "mark-watched",
        "Mark watched",
        &TubeOperation::RecordHistory {
            video_id: video_id.to_string(),
        },
    ) {
        resource =
            resource.with_action(mark_watched.with_description(
                "Add this video to owner-local, device-synchronised watch history",
            ));
    }
    if let Some(download) = view_spec(
        "download",
        "Download video",
        &TubeOperation::Download {
            video_id: video_id.to_string(),
        },
    ) {
        // Deliberately publish the blocked action instead of a decorative
        // download button. Search and automation can discover the stable
        // address and receive the concrete capability reason.
        resource = resource.with_action(download.unavailable(
            "download_storage_unavailable",
            "Tube has no host-granted download storage capability",
        ));
    }
    if let Some(share) = view_spec(
        "share",
        "Copy share link",
        &TubeOperation::Share {
            video_id: video_id.to_string(),
        },
    ) {
        resource = resource.with_action(share.with_contract(ActionContract {
            retry: RetryPolicy::Idempotent,
            tags: vec!["clipboard".into(), "sharing".into()],
            ..ActionContract::default()
        }));
    }
    if include_comments {
        resource = add_comment_resources(resource, model, video_id, &video.comments);
    }
    Some(resource)
}

fn add_comment_resources(
    mut parent: Resource,
    model: &TubeModel,
    video_id: &str,
    comments: &[crate::model::Comment],
) -> Resource {
    for (index, comment) in comments.iter().take(MAX_RESOURCE_COMMENTS).enumerate() {
        parent = parent.with_child(comment_resource(model, video_id, index, comment));
    }
    parent
}

fn comment_resource(
    model: &TubeModel,
    video_id: &str,
    _index: usize,
    comment: &crate::model::Comment,
) -> Resource {
    let comment_id = comment_key(video_id, comment);
    let reply_comment_id = comment_id.clone();
    let liked = model
        .comment_likes
        .get(&comment_id)
        .copied()
        .unwrap_or(false);
    Resource::new(
        format!("tube.comment:{comment_id}"),
        "tube.comment",
        resource_label(&comment_label(&comment.author, &comment.text)),
    )
    .with_action(
        immediate_spec(
            "like",
            if liked {
                "Unlike comment"
            } else {
                "Like comment"
            },
            &TubeAction::SetCommentLiked {
                comment_id,
                liked: !liked,
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
            let reply_id = reply_key(&reply_comment_id, text);
            encode(&TubeAction::SetCommentReply {
                comment_id: reply_comment_id.clone(),
                reply_id,
                text: text.to_string(),
                active: true,
            })
        },
    ))
}

/// Shared-document door for [`tilo_resource::publish`]: navigation, queue,
/// subscriptions, downloads placeholder and settings. Fetched catalogues,
/// the open watch page, history and device sync stay on the widget when hosted.
pub(crate) fn document_children(shared: &crate::resource::TubeShared) -> Vec<Resource> {
    let model = shared.as_model_projection();
    vec![
        navigation_resource(),
        Resource::new("tube.library", "tube.library", "Known library")
            .with_child(subscriptions_resource(&model))
            .with_child(queue_resource(&model))
            .with_child(Resource::new(
                "tube.downloads",
                "tube.collection",
                "Downloads (none available)",
            )),
        settings_resource(&model),
    ]
}

/// The open watch page, addressed as `tube.current:{id}`. Live presentation
/// and the on-screen play/share controls share this one resource.
pub(crate) fn current_watch_resource(model: &TubeModel) -> Option<Resource> {
    let Route::Watch(id) = &model.route else {
        return None;
    };
    let mut current = video_resource(model, id, true)?;
    current.id.0 = format!("tube.current:{id}");
    if let Some(play) = view_spec(
        "play-pause",
        "Play or pause",
        &TubeOperation::TogglePlayback,
    ) {
        current = current.with_action(play);
    }
    Some(current)
}

/// Live presentation beside the framework root: current watch context, fetched
/// catalogues, owner-local history/search hits, and Grayjay device sync.
pub(crate) fn live_presentation_resources(
    model: &TubeModel,
    device: &crate::sync::SyncView,
) -> Vec<Resource> {
    let mut extras = Vec::new();
    if let Some(current) = current_watch_resource(model) {
        extras.push(current);
    }
    for nested in library_resource(model).children {
        if matches!(
            nested.id.0.as_str(),
            "tube.videos"
                | "tube.creators"
                | "tube.playlists"
                | "tube.history"
                | "tube.search-results"
        ) {
            extras.push(nested);
        }
    }
    extras.push(provide_device_resources(device));
    extras
}

/// Publish one stable app root with bounded, hierarchical library and current
/// view resources. Search/history/agents can therefore navigate Tube even when
/// the watch page is not open, without materialising an unbounded catalogue.
pub fn provide_resources(model: &TubeModel) -> Vec<Resource> {
    let shared = crate::resource::TubeShared::from_model(model);
    let mut app = ui::tilo_resource::publish::<crate::resource::Tube>("tube.app", "Tube", &shared);
    if let Some(current) = current_watch_resource(model) {
        app.children.insert(0, current);
    }
    if let Some(library) = app
        .children
        .iter_mut()
        .find(|child| child.id.0 == "tube.library")
    {
        *library = library_resource(model);
    } else {
        app.children.push(library_resource(model));
    }
    vec![app]
}

fn navigation_resource() -> Resource {
    let mut navigation = Resource::new("tube.navigation", "tube.navigation", "Navigate");
    for (id, label, route) in [
        ("home", "Open Home", Route::Home),
        ("trending", "Open Trending", Route::Trending),
        ("subscriptions", "Open subscriptions", Route::Subscriptions),
        ("queue", "Open shared watch queue", Route::Queue),
        ("history", "Open watch history", Route::History),
        ("settings", "Open settings", Route::Settings),
    ] {
        if let Some(spec) = view_spec(id, label, &TubeOperation::Navigate { route }) {
            navigation = navigation.with_action(spec);
        }
    }
    if let Some(spec) = view_spec(
        "toggle-sidebar",
        "Show or hide navigation",
        &TubeOperation::ToggleSidebar,
    ) {
        navigation = navigation.with_action(spec);
    }

    navigation
}

/// Return the exact navigation binding used by Tube's visible rail.  Keeping
/// this beside `navigation_resource` means a redesign cannot accidentally
/// leave a pretty custom control on a private callback path while Ctrl+K and
/// agents use a different operation.
pub fn navigation_binding(route: &Route) -> Option<tilo_actions::ActionBinding<()>> {
    let action_id = match route {
        Route::Home => "home",
        Route::Trending => "trending",
        Route::Subscriptions => "subscriptions",
        Route::Queue => "queue",
        Route::History => "history",
        Route::Settings => "settings",
        // Entity routes require an id and are therefore reached by their
        // corresponding entity resource rather than a generic rail button.
        Route::Watch(_) | Route::Channel(_) | Route::Playlist(_) | Route::Search(_) => {
            return None;
        }
    };
    tilo_actions::ActionBinding::unit_for(&navigation_resource(), action_id).ok()
}

/// Binding for the visible navigation-drawer toggle. The rail state is local
/// presentation state, but the verb remains an addressable view action so
/// keyboard users, assistive technology, Ctrl+K, and agents use one route.
pub fn sidebar_binding() -> Option<tilo_actions::ActionBinding<()>> {
    tilo_actions::ActionBinding::unit_for(&navigation_resource(), "toggle-sidebar").ok()
}

/// Resolve the canonical queue-membership action used by a visible video
/// control. It stays an action binding rather than a local UI mutation: queue
/// writes are host-serialised shared-document changes, so an unavailable host
/// can never make the app pretend a video was saved.
pub fn queue_binding(model: &TubeModel, video_id: &str) -> Option<tilo_actions::ActionBinding<()>> {
    let resource = video_resource(model, video_id, false)?;
    tilo_actions::ActionBinding::unit_for(&resource, "queue").ok()
}

fn library_resource(model: &TubeModel) -> Resource {
    let mut library = Resource::new("tube.library", "tube.library", "Known library");
    // Collections are deliberately separate resources, rather than merely
    // presentation filters over the catalogue.  That gives Ctrl+K, history,
    // collaboration and agents stable scopes to address without having to
    // infer membership from the currently rendered route.
    let mut videos = Resource::new("tube.videos", "tube.collection", "Videos");
    for video in model.videos.iter().take(MAX_RESOURCE_ENTITIES) {
        // The watched video's comments are published once, under the current
        // context resource; repeating them here would give each comment two
        // resources with the same stable address.
        if let Some(resource) = video_resource(model, &video.id, false) {
            videos = videos.with_child(resource);
        }
    }
    let mut creators = Resource::new("tube.creators", "tube.collection", "Creators");
    for channel in model.channels.iter().take(MAX_RESOURCE_ENTITIES) {
        let mut resource = Resource::new(
            format!("tube.channel:{}", channel.id),
            "tube.channel",
            resource_label(&channel.name),
        );
        if let Some(open) = view_spec(
            "open",
            "Open creator",
            &TubeOperation::OpenChannel {
                channel_id: channel.id.clone(),
            },
        ) {
            resource = resource.with_action(open);
        }
        resource = resource.with_action(
            immediate_spec(
                "subscribe",
                "Subscribe or unsubscribe",
                &TubeAction::SetSubscription {
                    channel_id: channel.id.clone(),
                    subscribed: !model.is_subscribed(&channel.id),
                },
            )
            .reversible(),
        );
        creators = creators.with_child(resource);
    }
    let mut playlists = Resource::new("tube.playlists", "tube.collection", "Playlists");
    for playlist in model.playlists.iter().take(MAX_RESOURCE_ENTITIES) {
        let mut resource = Resource::new(
            format!("tube.playlist:{}", playlist.id),
            "tube.playlist",
            resource_label(&playlist.title),
        );
        if let Some(open) = view_spec(
            "open",
            "Open playlist",
            &TubeOperation::OpenPlaylist {
                playlist_id: playlist.id.clone(),
            },
        ) {
            resource = resource.with_action(open);
        }
        playlists = playlists.with_child(resource);
    }
    library = library
        .with_child(videos)
        .with_child(creators)
        .with_child(playlists)
        .with_child(subscriptions_resource(model))
        .with_child(queue_resource(model))
        .with_child(history_resource(model))
        .with_child(search_results_resource(model))
        // Downloads are a first-class, canonical collection even when it is
        // empty. Tube does not claim to have downloaded media until a host
        // grants a storage/download capability, so this is intentionally an
        // honest empty collection rather than fabricated completed jobs.
        .with_child(Resource::new(
            "tube.downloads",
            "tube.collection",
            "Downloads (none available)",
        ));
    library
}

/// The shared queue is a set of stable video entities, not an ordered CRDT
/// list. A queue entry therefore has a deterministic address and a reversible
/// tombstone action even when concurrent clients add/remove it at once.
fn queue_resource(model: &TubeModel) -> Resource {
    let mut queue = Resource::new("tube.queue", "tube.collection", "Shared watch queue");
    for video_id in model.queue.iter().take(MAX_RESOURCE_ENTITIES) {
        let label = model.video(video_id).map_or_else(
            || resource_label(&format!("Queued video {video_id}")),
            |video| resource_label(&video.title),
        );
        let mut entry = Resource::new(format!("tube.queue:{video_id}"), "tube.queue-entry", label)
            .with_action(
                immediate_spec(
                    "remove",
                    "Remove from queue",
                    &TubeAction::SetQueueMembership {
                        video_id: video_id.clone(),
                        queued: false,
                    },
                )
                .reversible(),
            );
        if model.video(video_id).is_some()
            && let Some(open) = view_spec(
                "open",
                "Open video",
                &TubeOperation::OpenVideo {
                    video_id: video_id.clone(),
                },
            )
        {
            entry = entry.with_action(open);
        }
        queue = queue.with_child(entry);
    }
    queue
}

/// The user's durable subscriptions, projected separately from the discovered
/// creator catalogue. A subscription can survive its cached channel record, so
/// retain an entry (and a deterministic unsubscribe action) even if metadata
/// has not been fetched yet.
fn subscriptions_resource(model: &TubeModel) -> Resource {
    let mut subscriptions = Resource::new("tube.subscriptions", "tube.collection", "Subscriptions");
    for channel_id in model.subscriptions.iter().take(MAX_RESOURCE_ENTITIES) {
        let label = model.channel(channel_id).map_or_else(
            || resource_label(&format!("Subscribed creator {channel_id}")),
            |channel| resource_label(&channel.name),
        );
        let mut entry = Resource::new(
            format!("tube.subscription:{channel_id}"),
            "tube.subscription",
            label,
        )
        .with_action(
            immediate_spec(
                "unsubscribe",
                "Unsubscribe",
                &TubeAction::SetSubscription {
                    channel_id: channel_id.clone(),
                    subscribed: false,
                },
            )
            .reversible(),
        );
        if model.channel(channel_id).is_some()
            && let Some(open) = view_spec(
                "open",
                "Open creator",
                &TubeOperation::OpenChannel {
                    channel_id: channel_id.clone(),
                },
            )
        {
            entry = entry.with_action(open);
        }
        subscriptions = subscriptions.with_child(entry);
    }
    subscriptions
}

/// Ordered private-per-profile history. Entries are keyed membership resources
/// instead of aliases of `tube.video:*`, preserving both video identity and a
/// stable history address for the owner's devices. This collection is not a
/// collaborator activity feed.
fn history_resource(model: &TubeModel) -> Resource {
    let mut history = Resource::new("tube.history", "tube.collection", "Watch history");
    if let Some(clear) = view_spec("clear", "Clear watch history", &TubeOperation::ClearHistory) {
        history = history.with_action(
            clear
                .with_description("Clear owner-local, device-synchronised watch history")
                .destructive(),
        );
    }
    for video_id in model.history.iter().take(MAX_RESOURCE_ENTITIES) {
        let label = model.video(video_id).map_or_else(
            || resource_label(&format!("Watched video {video_id}")),
            |video| resource_label(&video.title),
        );
        let mut entry = Resource::new(
            format!("tube.history:{video_id}"),
            "tube.history-entry",
            label,
        );
        if let Some(open) = view_spec(
            "open",
            "Open video",
            &TubeOperation::OpenVideo {
                video_id: video_id.clone(),
            },
        ) {
            entry = entry.with_action(open);
        }
        if let Some(remove) = view_spec(
            "remove",
            "Remove from history",
            &TubeOperation::RemoveHistory {
                video_id: video_id.clone(),
            },
        ) {
            entry = entry.with_action(remove);
        }
        history = history.with_child(entry);
    }
    history
}

/// The exact ordered result set for the active completed search. Keeping this
/// separate from the general library means an agent can act on "the third
/// search result" without relying on local substring matching or screen state.
fn search_results_resource(model: &TubeModel) -> Resource {
    let query = model.search_hits_query.trim();
    let label = if query.is_empty() {
        "Search results".to_string()
    } else {
        resource_label(&format!("Search results: {query}"))
    };
    let mut results = Resource::new("tube.search-results", "tube.collection", label);
    if query.is_empty() {
        return results;
    }
    for video_id in model.search_hits.iter().take(MAX_RESOURCE_ENTITIES) {
        let Some(video) = model.video(video_id) else {
            continue;
        };
        let mut entry = Resource::new(
            format!("tube.search-result:video:{video_id}"),
            "tube.search-result",
            resource_label(&video.title),
        );
        if let Some(open) = view_spec(
            "open",
            "Open video",
            &TubeOperation::OpenVideo {
                video_id: video_id.clone(),
            },
        ) {
            entry = entry.with_action(open);
        }
        results = results.with_child(entry);
    }
    for playlist_id in model
        .search_playlist_hits
        .iter()
        .take(MAX_RESOURCE_ENTITIES)
    {
        let Some(playlist) = model.playlist(playlist_id) else {
            continue;
        };
        let mut entry = Resource::new(
            format!("tube.search-result:playlist:{playlist_id}"),
            "tube.search-result",
            resource_label(&playlist.title),
        );
        if let Some(open) = view_spec(
            "open",
            "Open playlist",
            &TubeOperation::OpenPlaylist {
                playlist_id: playlist_id.clone(),
            },
        ) {
            entry = entry.with_action(open);
        }
        results = results.with_child(entry);
    }
    for channel_id in model.search_channel_hits.iter().take(MAX_RESOURCE_ENTITIES) {
        let Some(channel) = model.channel(channel_id) else {
            continue;
        };
        let mut entry = Resource::new(
            format!("tube.search-result:channel:{channel_id}"),
            "tube.search-result",
            resource_label(&channel.name),
        );
        if let Some(open) = view_spec(
            "open",
            "Open creator",
            &TubeOperation::OpenChannel {
                channel_id: channel_id.clone(),
            },
        ) {
            entry = entry.with_action(open);
        }
        results = results.with_child(entry);
    }
    results
}

fn settings_resource(model: &TubeModel) -> Resource {
    let mut settings = Resource::new("tube.settings", "tube.settings", "Playback settings");
    for (id, label, setting, enabled) in [
        (
            "autoplay",
            "Toggle autoplay",
            SettingFlag::Autoplay,
            model.settings.autoplay,
        ),
        (
            "comments",
            "Toggle comments",
            SettingFlag::ShowComments,
            model.settings.show_comments,
        ),
        (
            "sponsorblock",
            "Toggle SponsorBlock",
            SettingFlag::SponsorBlock,
            model.settings.sponsorblock_enabled,
        ),
        (
            "dearrow",
            "Toggle DeArrow",
            SettingFlag::DeArrow,
            model.settings.dearrow_enabled,
        ),
        (
            "dislikes",
            "Toggle dislike counts",
            SettingFlag::ReturnDislike,
            model.settings.return_dislike_enabled,
        ),
    ] {
        settings = settings.with_action(
            immediate_spec(
                id,
                label,
                &TubeAction::SetSetting {
                    setting,
                    enabled: !enabled,
                },
            )
            .reversible(),
        );
    }
    let quality = Resource::new(
        "tube.setting:preferred-quality",
        "tube.setting",
        format!("Preferred quality: {}", model.settings.preferred_quality),
    )
    .with_action(
        immediate_spec(
            "cycle",
            "Cycle preferred quality",
            &TubeAction::SetPreferredQuality {
                quality: next_quality(&model.settings.preferred_quality),
            },
        )
        .reversible(),
    );
    settings = settings.with_child(quality);
    for category in crate::model::Category::ALL {
        let stored = model
            .settings
            .categories
            .get(category.api_key())
            .copied()
            .unwrap_or(crate::model::SkipMode::Skip);
        let resource = Resource::new(
            format!("tube.setting:sponsor-category:{}", category.api_key()),
            "tube.setting",
            format!("SponsorBlock {}: {}", category.label(), stored.label()),
        )
        .with_action(
            immediate_spec(
                "cycle",
                format!("Cycle {} handling", category.label()).as_str(),
                &TubeAction::SetSponsorCategory {
                    category,
                    mode: stored.next(),
                },
            )
            .reversible(),
        );
        settings = settings.with_child(resource);
    }
    settings
}

// Inherent resource publication (was ResourceProvider)
impl crate::app::TubeApp {
    pub fn publish_resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        let mut resources = provide_resources(self.model_ref());
        if let Some(app) = resources.first_mut() {
            #[cfg(feature = "grayjay-sync")]
            let device = provide_device_resources(self.device_sync_view());
            #[cfg(not(feature = "grayjay-sync"))]
            let device = provide_device_resources(&crate::sync::SyncView::default());
            app.children.push(device);
        }
        resources
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::TubeApp;
    use crate::model::Route;
    use tilo_actions::{ActionRegistry, LocalTransport};
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

    #[test]
    fn resource_publish_owns_shared_door_and_live_extras_stay_separate() {
        let (app, vid, _) = watching();
        let mut shared = crate::resource::TubeShared::default();
        shared.queue.insert(vid.clone(), true);
        shared.settings = app.model_ref().settings.clone();
        let published =
            ui::tilo_resource::publish::<crate::resource::Tube>("tube.app", "Tube", &shared);
        assert!(
            published
                .children
                .iter()
                .any(|child| child.id.0 == "tube.navigation"),
            "navigation is on the resource door"
        );
        assert!(
            published
                .children
                .iter()
                .any(|child| child.id.0 == "tube.settings"),
            "settings are on the resource door"
        );
        let library = published
            .children
            .iter()
            .find(|child| child.id.0 == "tube.library")
            .expect("shared library collections");
        assert!(
            library
                .children
                .iter()
                .any(|child| child.id.0 == "tube.queue")
        );
        assert!(
            !library
                .children
                .iter()
                .any(|child| child.id.0 == "tube.videos"),
            "fetched catalogue must not duplicate under publish"
        );

        let extras =
            live_presentation_resources(app.model_ref(), &crate::sync::SyncView::default());
        assert!(
            extras
                .iter()
                .any(|resource| resource.id.0 == format!("tube.current:{vid}"))
        );
        assert!(extras.iter().any(|resource| resource.id.0 == "tube.videos"));
        assert!(
            extras
                .iter()
                .any(|resource| resource.id.0 == "tube.device-sync")
        );
        assert!(
            !extras
                .iter()
                .any(|resource| resource.id.0 == "tube.app" || resource.kind == "tube.application"),
            "hosted extras must not republish the application root"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn resource_projection_is_stable_bounded_and_canonical() {
        // Case `comment_resource_identity_survives_reordering`.
        {
            let mut model = crate::sample::model();
            let video = model
                .videos
                .iter_mut()
                .find(|video| video.comments.len() > 1)
                .expect("sample has multiple comments");
            let video_id = video.id.clone();
            model.route = Route::Watch(video_id.clone());
            let before = provide_resources(&model)[0]
                .children
                .iter()
                .map(|resource| resource.id.0.clone())
                .collect::<std::collections::HashSet<_>>();
            model
                .videos
                .iter_mut()
                .find(|video| video.id == video_id)
                .unwrap()
                .comments
                .swap(0, 1);
            let after = provide_resources(&model)[0]
                .children
                .iter()
                .map(|resource| resource.id.0.clone())
                .collect::<std::collections::HashSet<_>>();
            assert_eq!(
                before, after,
                "comment addresses must not be list positions"
            );
        }
        // Case `resource_projection_is_bounded_for_large_catalogues`.
        {
            let mut model = crate::sample::model();
            let seed = model.videos[0].clone();
            model.videos = (0..250)
                .map(|index| {
                    let mut video = seed.clone();
                    video.id = format!("video-{index}");
                    video.title = format!("Video {index}");
                    video
                })
                .collect();
            let resources = provide_resources(&model);
            let library = resources[0]
                .children
                .iter()
                .find(|resource| resource.id.0 == "tube.library")
                .expect("library resource");
            let projected_videos = library
                .children
                .iter()
                .find(|resource| resource.id.0 == "tube.videos")
                .expect("videos collection")
                .children
                .iter()
                .filter(|resource| resource.kind == "tube.video")
                .count();
            assert_eq!(projected_videos, MAX_RESOURCE_ENTITIES);
        }
        // Case `durable_views_and_search_are_canonical_bounded_collections`.
        {
            let mut model = crate::sample::model();
            let video = model.videos[0].id.clone();
            let channel = model.videos[0].channel_id.clone();
            model.subscriptions = vec![channel];
            model.queue = vec![video.clone()];
            model.history = vec![video.clone()];
            model.search_hits_query = "rust".into();
            model.search_hits = vec![video.clone()];

            let resources = provide_resources(&model);
            let library = resources[0]
                .children
                .iter()
                .find(|resource| resource.id.0 == "tube.library")
                .expect("library resource");
            let subscriptions = library
                .children
                .iter()
                .find(|resource| resource.id.0 == "tube.subscriptions")
                .expect("subscriptions collection");
            assert_eq!(subscriptions.children.len(), 1);
            assert!(
                subscriptions.children[0]
                    .actions
                    .iter()
                    .any(|action| action.id == "unsubscribe")
            );
            let history = library
                .children
                .iter()
                .find(|resource| resource.id.0 == "tube.history")
                .expect("history collection");
            assert_eq!(history.children[0].id.0, format!("tube.history:{video}"));
            let search = library
                .children
                .iter()
                .find(|resource| resource.id.0 == "tube.search-results")
                .expect("search-results collection");
            assert_eq!(
                search.children[0].id.0,
                format!("tube.search-result:video:{video}")
            );
            assert!(
                library
                    .children
                    .iter()
                    .any(|resource| resource.id.0 == "tube.downloads")
            );
            let queue = library
                .children
                .iter()
                .find(|resource| resource.id.0 == "tube.queue")
                .expect("shared queue collection");
            assert_eq!(queue.children[0].id.0, format!("tube.queue:{video}"));
            assert!(
                queue.children[0]
                    .actions
                    .iter()
                    .any(|action| action.id == "remove")
            );
            assert!(history.actions.iter().any(|action| action.id == "clear"));
            assert!(
                history.children[0]
                    .actions
                    .iter()
                    .any(|action| action.id == "remove")
            );
            let videos = library
                .children
                .iter()
                .find(|resource| resource.id.0 == "tube.videos")
                .expect("videos collection");
            assert!(
                videos.children[0]
                    .actions
                    .iter()
                    .any(|action| action.id == "mark-watched")
            );
        }
        // Case `resource_labels_bound_remote_metadata_without_changing_actions`.
        {
            let mut model = crate::sample::model();
            model.videos[0].title = format!("title\n{}", "x".repeat(MAX_RESOURCE_LABEL_CHARS + 20));
            let resources = provide_resources(&model);
            let resource = resources[0]
                .children
                .iter()
                .find(|resource| resource.id.0 == "tube.library")
                .and_then(|library| {
                    library
                        .children
                        .iter()
                        .find(|resource| resource.id.0 == "tube.videos")
                })
                .and_then(|videos| videos.children.first())
                .expect("first video resource");
            assert!(resource.label.chars().count() <= MAX_RESOURCE_LABEL_CHARS + 1);
            assert!(!resource.label.contains('\n'));
            assert!(resource.actions.iter().any(|action| action.id == "open"));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn tube_actions_write_absolute_keyed_records_and_refuse_legacy_forms() {
        // Case `legacy_shared_history_actions_are_refused`.
        {
            let mut doc = AutoCommit::new();
            let watch = TubeAction::SetHistoryEntry {
                owner_id: "user:alice".into(),
                video_id: "v1".into(),
                watched_at_ms: 10,
                present: true,
                mutation_id: "alice:watch:v1:10".into(),
            };
            assert!(watch.apply(&mut doc).is_err());
            assert!(doc.get(&ROOT, "tube").unwrap().is_none());
        }
        // Case `queue_membership_is_an_absolute_keyed_tombstone`.
        {
            let mut doc = AutoCommit::new();
            tilo_actions::Action::apply(
                &TubeAction::SetQueueMembership {
                    video_id: "video-1".into(),
                    queued: true,
                },
                &mut doc,
            )
            .expect("queue add applies");
            tilo_actions::Action::apply(
                &TubeAction::SetQueueMembership {
                    video_id: "video-1".into(),
                    queued: false,
                },
                &mut doc,
            )
            .expect("queue tombstone applies");
            let (Value::Object(ObjType::Map), tube) = doc.get(&ROOT, "tube").unwrap().unwrap()
            else {
                panic!("tube root exists");
            };
            let (Value::Object(ObjType::Map), queue) = doc.get(&tube, "queue").unwrap().unwrap()
            else {
                panic!("queue is a keyed map");
            };
            assert_eq!(
                doc.get(&queue, "video-1").unwrap().unwrap().0.as_bool(),
                Some(false)
            );
        }
        // Case `comment_mutations_use_keyed_absolute_records`.
        {
            let mut doc = AutoCommit::new();
            let comment_id = "video-1#comment".to_string();
            let reply_id = reply_key(&comment_id, "Useful explanation");
            tilo_actions::Action::apply(
                &TubeAction::SetCommentLiked {
                    comment_id: comment_id.clone(),
                    liked: false,
                },
                &mut doc,
            )
            .expect("comment-like tombstone applies");
            tilo_actions::Action::apply(
                &TubeAction::SetCommentReply {
                    comment_id: comment_id.clone(),
                    reply_id: reply_id.clone(),
                    text: "Useful explanation".into(),
                    active: false,
                },
                &mut doc,
            )
            .expect("reply tombstone applies");
            let (Value::Object(ObjType::Map), tube) = doc.get(&ROOT, "tube").unwrap().unwrap()
            else {
                panic!("tube root exists");
            };
            let (Value::Object(ObjType::Map), likes) =
                doc.get(&tube, "comment_likes").unwrap().unwrap()
            else {
                panic!("comment likes is a keyed map");
            };
            assert_eq!(
                doc.get(&likes, &comment_id).unwrap().unwrap().0.as_bool(),
                Some(false)
            );
            let (Value::Object(ObjType::Map), all_replies) =
                doc.get(&tube, "comment_replies").unwrap().unwrap()
            else {
                panic!("comment replies is a keyed map");
            };
            let (Value::Object(ObjType::Map), replies) =
                doc.get(&all_replies, &comment_id).unwrap().unwrap()
            else {
                panic!("per-comment replies is a keyed map");
            };
            let (Value::Object(ObjType::Map), reply) =
                doc.get(&replies, &reply_id).unwrap().unwrap()
            else {
                panic!("reply is a keyed record");
            };
            assert_eq!(
                doc.get(&reply, "active").unwrap().unwrap().0.as_bool(),
                Some(false)
            );
        }
        // Case `legacy_receiver_dependent_actions_are_decoded_but_refused`.
        {
            let mut doc = AutoCommit::new();
            for action in [
                TubeAction::LikeVideo {
                    video_id: "video-1".into(),
                },
                TubeAction::ToggleSubscription {
                    channel_id: "channel-1".into(),
                },
                TubeAction::CyclePreferredQuality,
            ] {
                assert!(matches!(
                    tilo_actions::Action::apply(&action, &mut doc),
                    Err(ActionError::Refused(_))
                ));
            }
            assert!(doc.get(&ROOT, "tube").unwrap().is_none());
        }
        // Case `absolute_quality_replays_identically_on_divergent_replicas`.
        {
            let mut left = AutoCommit::new();
            let mut right = AutoCommit::new();
            for (doc, initial) in [(&mut left, "1080p"), (&mut right, "480p")] {
                let tube = doc.put_object(&ROOT, "tube", ObjType::Map).unwrap();
                let settings = doc.put_object(&tube, "settings", ObjType::Map).unwrap();
                doc.put(&settings, "preferred_quality", initial).unwrap();
            }
            let action = TubeAction::SetPreferredQuality {
                quality: "720p".into(),
            };
            action.apply(&mut left).expect("left replay applies");
            action.apply(&mut right).expect("right replay applies");
            for doc in [&left, &right] {
                let (Value::Object(ObjType::Map), tube) = doc.get(&ROOT, "tube").unwrap().unwrap()
                else {
                    panic!("tube root");
                };
                let (Value::Object(ObjType::Map), settings) =
                    doc.get(&tube, "settings").unwrap().unwrap()
                else {
                    panic!("settings");
                };
                assert_eq!(
                    doc.get(&settings, "preferred_quality")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str(),
                    Some("720p")
                );
            }
            assert!(matches!(
                TubeAction::SetPreferredQuality {
                    quality: "4K magic".into()
                }
                .apply(&mut left),
                Err(ActionError::Refused(_))
            ));
        }
    }

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn palette_lists_and_dispatches_tube_resource_actions() {
        // Case `ctrl_k_lists_open_video_and_comment_resources`:
        // Ctrl+K on tube lists the open video + its comment resources and their
        // actions. Drives the real resource-collection hook (`Widget::resources`)
        // and the real `ui::ActionPalette`.
        {
            let (app, _vid, n_comments) = watching();
            let doc = AutoCommit::new();

            // The hook `ui::App::collect_resources` calls each frame.
            let resources = ui::resources::resources(&app, &doc);
            assert_eq!(resources.len(), 1, "one stable Tube application root");
            assert_eq!(resources[0].kind, "tube.application");
            let video = resources[0]
                .children
                .iter()
                .find(|resource| resource.id.0.starts_with("tube.current:"))
                .expect("the current video is projected beside the bounded library");
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
                RetryPolicy::AtMostOnce
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
        }
        // Case `executing_like_action_mutates_crdt`:
        // Executing the "Like video" action mutates the CRDT.
        {
            let (app, vid, _) = watching();
            let mut doc = AutoCommit::new();
            let resources = ui::resources::resources(&app, &doc);
            let mut palette = ui::ActionPalette::new();
            palette.load_open_resources(&resources);
            palette.open();

            assert_eq!(read_bool(&doc, "likes", &vid), None, "not liked yet");
            let mut tx = LocalTransport::new();
            let d = run_row(&mut palette, &mut doc, &registry(), &mut tx, "Like video");
            assert!(matches!(d, tilo_actions::Dispatched::AppliedLocal(_)));
            assert_eq!(
                read_bool(&doc, "likes", &vid),
                Some(true),
                "like written to CRDT"
            );
            assert_eq!(tx.replicated.len(), 1, "replicated once");
        }
        // Case `playback_setting_resources_write_the_same_document_state_as_visible_controls`.
        {
            let app = TubeApp::new();
            let mut doc = AutoCommit::new();
            let resources = ui::resources::resources(&app, &doc);
            let settings = resources[0]
                .children
                .iter()
                .find(|resource| resource.id.0 == "tube.settings")
                .expect("settings resource");
            assert!(settings.children.iter().any(|resource| {
                resource.id.0 == "tube.setting:preferred-quality"
                    && resource.actions.iter().any(|action| action.id == "cycle")
            }));
            assert!(settings.children.iter().any(|resource| {
                resource.id.0 == "tube.setting:sponsor-category:sponsor"
                    && resource.actions.iter().any(|action| action.id == "cycle")
            }));

            let mut palette = ui::ActionPalette::new();
            palette.load_open_resources(&resources);
            palette.open();
            palette.set_query("cycle preferred quality");
            assert!(
                palette
                    .result_labels()
                    .iter()
                    .any(|label| label.contains("Cycle preferred quality")),
                "settings action appears in Ctrl+K: {:?}",
                palette.result_labels()
            );
            let mut tx = LocalTransport::new();
            let quality = run_row(
                &mut palette,
                &mut doc,
                &registry(),
                &mut tx,
                "Cycle preferred quality",
            );
            assert!(matches!(quality, tilo_actions::Dispatched::AppliedLocal(_)));
            assert_eq!(
                read_str(&doc, "settings", "preferred_quality").as_deref(),
                Some("720p")
            );

            palette.open();
            palette.set_query("cycle sponsor handling");
            let category = run_row(
                &mut palette,
                &mut doc,
                &registry(),
                &mut tx,
                "Cycle Sponsor handling",
            );
            assert!(matches!(
                category,
                tilo_actions::Dispatched::AppliedLocal(_)
            ));
            let (Value::Object(ObjType::Map), tube) = doc.get(&ROOT, "tube").unwrap().unwrap()
            else {
                panic!("tube root exists");
            };
            let (Value::Object(ObjType::Map), settings) =
                doc.get(&tube, "settings").unwrap().unwrap()
            else {
                panic!("settings map exists");
            };
            let (Value::Object(ObjType::Map), categories) =
                doc.get(&settings, "sponsor_categories").unwrap().unwrap()
            else {
                panic!("category map exists");
            };
            assert_eq!(
                doc.get(&categories, "sponsor").unwrap().unwrap().0.as_str(),
                Some("show"),
            );
        }
        // Case `executing_add_to_queue_refuses_without_host`:
        // The host-authoritative "Add to queue" action fails closed when no host
        // transport is installed.
        {
            let (app, vid, _) = watching();
            let mut doc = AutoCommit::new();
            let resources = ui::resources::resources(&app, &doc);
            let mut palette = ui::ActionPalette::new();
            palette.load_open_resources(&resources);
            palette.open();

            let mut tx = LocalTransport::new();
            let idx = palette
                .result_labels()
                .iter()
                .position(|label| label.contains("Add to queue"))
                .expect("queue action is published");
            palette.set_cursor(idx);
            palette.activate_selected();
            let error = palette
                .take_dispatch(&mut doc, &registry(), &mut tx)
                .expect("the selected action attempted dispatch")
                .unwrap_err();
            assert!(matches!(error, tilo_actions::ActionError::Refused(_)));
            assert!(doc.get(&ROOT, "tube").unwrap().is_none());
            let _ = vid;
        }
    }

    /// The watched sample video, its creator and its first comment's stable id.
    fn watched_ids() -> (String, String, String) {
        let (app, vid, _) = watching();
        let video = app.model_ref().video(&vid).expect("watched sample video");
        let comment = comment_key(&vid, &video.comments[0]);
        (vid.clone(), video.channel_id.clone(), comment)
    }

    /// A sample video from the catalogue other than the watched one.
    fn other_video() -> String {
        let (app, vid, _) = watching();
        app.model_ref()
            .videos
            .iter()
            .find(|video| video.id != vid)
            .expect("sample has a second video")
            .id
            .clone()
    }

    /// A shared document written by dispatched actions: the watched video is
    /// liked and queued, its creator subscribed, its first comment liked and
    /// the preferred quality chosen.
    fn seeded_doc() -> AutoCommit {
        let (vid, channel, comment) = watched_ids();
        let mut doc = AutoCommit::new();
        for action in [
            TubeAction::SetVideoLiked {
                video_id: vid.clone(),
                liked: true,
            },
            TubeAction::SetQueueMembership {
                video_id: vid,
                queued: true,
            },
            TubeAction::SetSubscription {
                channel_id: channel,
                subscribed: true,
            },
            TubeAction::SetCommentLiked {
                comment_id: comment,
                liked: true,
            },
            TubeAction::SetPreferredQuality {
                quality: "720p".into(),
            },
        ] {
            tilo_actions::apply_local(&mut doc, &action, encode(&action)).expect("seed applies");
        }
        doc
    }

    /// The live app's resources once its `sync` has reconciled `doc`.
    fn synced_resources(doc: &AutoCommit) -> Vec<Resource> {
        let (mut app, _, _) = watching();
        app.sync(&mut doc.clone(), &ROOT);
        ui::resources::resources(&app, doc)
    }

    ui::action_contract_tests! {
        seed: seeded_doc(),
        registry: registry(),
        resources: |doc| synced_resources(doc),
        palette: ["Like video", "Remove from queue", "Reply to comment", "Cycle preferred quality"],
        replay: [
            like_video => |_| encode(&TubeAction::SetVideoLiked {
                video_id: other_video(),
                liked: true,
            }),
            unsubscribe => |_| encode(&TubeAction::SetSubscription {
                channel_id: watched_ids().1,
                subscribed: false,
            }),
        ],
        undo: [
            preferred_quality => |_| encode(&TubeAction::SetPreferredQuality {
                quality: "480p".into(),
            }),
            reply => |_| {
                let comment = watched_ids().2;
                encode(&TubeAction::SetCommentReply {
                    reply_id: reply_key(&comment, "Useful explanation"),
                    comment_id: comment,
                    text: "Useful explanation".into(),
                    active: true,
                })
            },
        ],
        encodes: [
            unlike_video => (&format!("tube.current:{}", watched_ids().0), "like", |_, _| {
                encode(&TubeAction::SetVideoLiked {
                    video_id: watched_ids().0,
                    liked: false,
                })
            }),
            dequeue => (&format!("tube.current:{}", watched_ids().0), "queue", |_, _| {
                encode(&TubeAction::SetQueueMembership {
                    video_id: watched_ids().0,
                    queued: false,
                })
            }),
            unsubscribe => (&format!("tube.subscription:{}", watched_ids().1), "unsubscribe", |_, _| {
                encode(&TubeAction::SetSubscription {
                    channel_id: watched_ids().1,
                    subscribed: false,
                })
            }),
            unlike_comment => (&format!("tube.comment:{}", watched_ids().2), "like", |_, _| {
                encode(&TubeAction::SetCommentLiked {
                    comment_id: watched_ids().2,
                    liked: false,
                })
            }),
            reply => (&format!("tube.comment:{}", watched_ids().2), "reply", arg: "Thanks", |_, _| {
                let comment = watched_ids().2;
                encode(&TubeAction::SetCommentReply {
                    reply_id: reply_key(&comment, "Thanks"),
                    comment_id: comment,
                    text: "Thanks".into(),
                    active: true,
                })
            }),
            cycle_quality => ("tube.setting:preferred-quality", "cycle", |_, _| {
                encode(&TubeAction::SetPreferredQuality {
                    quality: next_quality("720p"),
                })
            }),
        ],
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
        v.as_bool()
    }

    fn read_str(doc: &AutoCommit, map: &str, key: &str) -> Option<String> {
        let (Value::Object(ObjType::Map), tube) = doc.get(&ROOT, "tube").ok()?? else {
            return None;
        };
        let (Value::Object(ObjType::Map), sub) = doc.get(&tube, map).ok()?? else {
            return None;
        };
        doc.get(&sub, key)
            .ok()
            .flatten()
            .and_then(|(value, _)| value.as_str().map(str::to_owned))
    }

    /// Select a palette row by a substring of its label, then activate it (the
    /// same path Enter takes) and dispatch — returns the dispatch outcome.
    fn run_row(
        palette: &mut ui::ActionPalette,
        doc: &mut AutoCommit,
        registry: &ActionRegistry,
        transport: &mut dyn tilo_actions::ActionTransport,
        needle: &str,
    ) -> tilo_actions::Dispatched {
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

    #[test]
    fn unavailable_sync_backend_keeps_discoverable_unavailable_effects() {
        // Case `unavailable_sync_keeps_discoverable_unavailable_effects`.
        {
            let app = TubeApp::new();
            let doc = AutoCommit::new();
            let resources = ui::resources::resources(&app, &doc);
            let device = resources[0]
                .children
                .iter()
                .find(|resource| resource.id.0 == "tube.device-sync")
                .expect("sync capability is discoverable to agents in every build");
            assert!(device.label.contains("unavailable"));
            assert!(
                device
                    .actions
                    .iter()
                    .all(|action| !action.contract.availability.is_available())
            );
        }
        // Case `unavailable_device_backend_keeps_stable_unavailable_effects`.
        {
            let resource = provide_device_resources(&crate::sync::SyncView::default());
            let pair = resource
                .actions
                .iter()
                .find(|action| action.id == "pair")
                .expect("pair remains discoverable to agents");
            assert!(!pair.contract.availability.is_available());
            assert!(
                resource
                    .actions
                    .iter()
                    .any(|action| action.id == "sync"
                        && !action.contract.availability.is_available())
            );
            assert!(resource.children.is_empty());
        }
        // Case `device_lifecycle_resources_are_effects_with_confirmation`.
        #[cfg(feature = "grayjay-sync")]
        {
            let view = crate::sync::SyncView {
                status: crate::sync::SyncStatus::Ready,
                summary: "ready".into(),
                device_public_key: Some("our-public-key".into()),
                peers: vec![crate::sync::PeerRecord {
                    id: "peer-public-key".into(),
                    name: "Phone".into(),
                    descriptor: crate::sync::PeerDescriptor {
                        public_key: "peer-public-key".into(),
                        addresses: vec!["127.0.0.1".into()],
                        port: 12315,
                    },
                    approved_at: 1,
                    pending_approval: false,
                    revoked_at: None,
                    last_sync_at: None,
                    last_error: None,
                }],
            };
            let resource = provide_device_resources(&view);
            let pair = resource
                .actions
                .iter()
                .find(|action| action.id == "pair")
                .expect("pair is published");
            assert_eq!(pair.kind, OperationKind::Effect);
            assert!(!matches!(
                pair.contract.confirmation,
                tilo_actions::ConfirmationPolicy::None
            ));
            let revoke = resource.children[0]
                .actions
                .iter()
                .find(|action| action.id == "revoke")
                .expect("revoke is published");
            assert_eq!(revoke.kind, OperationKind::Effect);
            assert_eq!(revoke.contract.retry, RetryPolicy::Idempotent);
            assert!(!matches!(
                revoke.contract.confirmation,
                tilo_actions::ConfirmationPolicy::None
            ));
            assert_eq!(pair.params()[0].ty, ParamType::Secret);
        }
    }
}
