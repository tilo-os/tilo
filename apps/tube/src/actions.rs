//! Tube's exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! These are the collaborative, replicable operations on Tube's shared CRDT
//! state — likes, the watch queue, comment likes + replies — as distinct from
//! Tube's device-local presentation (fetched catalogues, playhead, route and
//! watch history). An action mutates a structured sub-tree under the
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

use qos_actions::{
    Action, ActionContract, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc,
    EncodedAction, ExecutionLocus, ExecutionMode, OperationKind, ParamType, Resource,
    ResourceProvider, RetryPolicy,
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
    Share { video_id: String },
    Navigate { route: Route },
    OpenVideo { video_id: String },
    OpenChannel { channel_id: String },
    OpenPlaylist { playlist_id: String },
    PairDevice { invite: String },
    SyncDevices,
    ApproveDevice { peer_id: String },
    RevokeDevice { peer_id: String },
}

pub fn operation_kind(operation: &TubeOperation) -> OperationKind {
    match operation {
        TubeOperation::Share { .. }
        | TubeOperation::PairDevice { .. }
        | TubeOperation::SyncDevices
        | TubeOperation::ApproveDevice { .. }
        | TubeOperation::RevokeDevice { .. } => OperationKind::Effect,
        TubeOperation::TogglePlayback
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

/// Composite comment key: stable across the ordered `Video.comments` list.
fn legacy_comment_key(video_id: &str, index: usize) -> String {
    format!("{video_id}#{index}")
}

/// Stable content identity for a comment. The upstream model has no comment
/// id, so hash all immutable source fields with an explicitly fixed FNV-1a
/// algorithm rather than addressing a row by its current list position.
fn comment_key(video_id: &str, comment: &crate::model::Comment) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for field in [
        video_id,
        comment.author.as_str(),
        comment.initials.as_str(),
        comment.text.as_str(),
        comment.time_ago.as_str(),
    ] {
        for byte in field.bytes().chain(std::iter::once(0xff)) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{video_id}#{hash:016x}")
}

/// Every collaborative operation Tube exports. One serializable enum so a single
/// registry `type_id` covers the app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TubeAction {
    /// Set the local "liked" flag for a video.
    LikeVideo {
        video_id: String,
    },
    /// Set the state resolved by the authoring surface. Retains the legacy
    /// toggle variant above so recorded envelopes remain replayable.
    SetVideoLiked {
        video_id: String,
        liked: bool,
    },
    /// Append a video to the shared watch queue.
    AddToQueue {
        video_id: String,
    },
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
    /// Advance the preferred playback quality through Tube's visible quality
    /// selector. Keeping this as one document action means the settings row,
    /// Ctrl+K and an agent all use the same deterministic transition.
    CyclePreferredQuality,
    /// Set one `SponsorBlock` category's handling mode. The target mode is
    /// embedded instead of inferred from the document so the visible control,
    /// a replay and an agent all make the same transition (including Filler,
    /// whose default is Off rather than Auto-skip).
    SetSponsorCategory {
        category: crate::model::Category,
        mode: crate::model::SkipMode,
    },
    /// Like a comment (addressed by its video + ordered index).
    LikeComment {
        video_id: String,
        index: usize,
        #[serde(default)]
        comment_id: String,
    },
    ReplyToComment {
        video_id: String,
        index: usize,
        #[serde(default)]
        comment_id: String,
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
                let likes_map = child_map(doc, &tube, "likes")?;
                let is_liked = doc
                    .get(&likes_map, video_id.as_str())?
                    .and_then(|(value, _)| value.to_bool())
                    .unwrap_or(false);
                doc.put(
                    &likes_map,
                    video_id.as_str(),
                    ScalarValue::Boolean(!is_liked),
                )?;
            }
            TubeAction::SetVideoLiked { video_id, liked } => {
                let likes_map = child_map(doc, &tube, "likes")?;
                doc.put(&likes_map, video_id.as_str(), ScalarValue::Boolean(*liked))?;
            }
            TubeAction::AddToQueue { video_id } => {
                let queue = child_list(doc, &tube, "queue")?;
                let len = doc.length(&queue);
                doc.insert(&queue, len, ScalarValue::Str(video_id.as_str().into()))?;
            }
            TubeAction::ToggleSubscription { channel_id } => {
                let subscriptions = child_map(doc, &tube, "subscriptions")?;
                let subscribed = doc
                    .get(&subscriptions, channel_id.as_str())?
                    .and_then(|(value, _)| value.to_bool())
                    .unwrap_or(false);
                doc.put(
                    &subscriptions,
                    channel_id.as_str(),
                    ScalarValue::Boolean(!subscribed),
                )?;
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
            TubeAction::CyclePreferredQuality => apply_cycle_preferred_quality(doc, &tube)?,
            TubeAction::SetSponsorCategory { category, mode } => {
                apply_sponsor_category(doc, &tube, *category, *mode)?;
            }
            TubeAction::LikeComment {
                video_id,
                index,
                comment_id,
            } => {
                let likes = child_map(doc, &tube, "comment_likes")?;
                let key = if comment_id.is_empty() {
                    legacy_comment_key(video_id, *index)
                } else {
                    comment_id.clone()
                };
                doc.put(&likes, key, ScalarValue::Boolean(true))?;
            }
            TubeAction::ReplyToComment {
                video_id,
                index,
                comment_id,
                text,
            } => {
                let replies_map = child_map(doc, &tube, "comment_replies")?;
                let key = if comment_id.is_empty() {
                    legacy_comment_key(video_id, *index)
                } else {
                    comment_id.clone()
                };
                let list = child_list(doc, &replies_map, &key)?;
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
            TubeAction::LikeVideo { .. }
            | TubeAction::SetVideoLiked { .. }
            | TubeAction::LikeComment { .. }
            | TubeAction::ToggleSubscription { .. }
            | TubeAction::SetSubscription { .. }
            | TubeAction::SetSetting { .. }
            | TubeAction::CyclePreferredQuality
            | TubeAction::SetSponsorCategory { .. } => ExecutionLocus::ClientLocal,
        }
    }
}

fn apply_cycle_preferred_quality(doc: &mut Doc, tube: &ObjId) -> Result<(), ActionError> {
    let settings = child_map(doc, tube, "settings")?;
    let current = doc
        .get(&settings, "preferred_quality")?
        .and_then(|(value, _)| value.to_str().map(str::to_owned))
        .unwrap_or_else(|| "1080p".to_string());
    let index = QUALITY_OPTIONS
        .iter()
        .position(|quality| *quality == current)
        .unwrap_or(0);
    let next = QUALITY_OPTIONS
        .get((index + 1) % QUALITY_OPTIONS.len())
        .copied()
        .unwrap_or("1080p");
    doc.put(
        &settings,
        "preferred_quality",
        ScalarValue::Str(next.into()),
    )?;
    Ok(())
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
                "Add to queue",
                &TubeAction::AddToQueue {
                    video_id: video_id.to_string(),
                },
            )
            .with_description("Append to the shared watch queue")
            .with_contract(ActionContract {
                retry: RetryPolicy::AtMostOnce,
                execution: ExecutionMode::Immediate,
                ..ActionContract::default()
            })
            .reversible(),
        );
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
        resource = add_comment_resources(resource, video_id, &video.comments);
    }
    Some(resource)
}

fn add_comment_resources(
    mut parent: Resource,
    video_id: &str,
    comments: &[crate::model::Comment],
) -> Resource {
    for (index, comment) in comments.iter().take(MAX_RESOURCE_COMMENTS).enumerate() {
        parent = parent.with_child(comment_resource(video_id, index, comment));
    }
    parent
}

fn comment_resource(video_id: &str, index: usize, comment: &crate::model::Comment) -> Resource {
    let comment_id = comment_key(video_id, comment);
    let reply_comment_id = comment_id.clone();
    let like_video_id = video_id.to_string();
    let reply_video_id = like_video_id.clone();
    Resource::new(
        format!("tube.comment:{comment_id}"),
        "tube.comment",
        resource_label(&comment_label(&comment.author, &comment.text)),
    )
    .with_action(
        immediate_spec(
            "like",
            "Like comment",
            &TubeAction::LikeComment {
                video_id: like_video_id,
                index,
                comment_id,
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
                video_id: reply_video_id.clone(),
                index,
                comment_id: reply_comment_id.clone(),
                text: text.to_string(),
            })
        },
    ))
}

/// Publish one stable app root with bounded, hierarchical library and current
/// view resources. Search/history/agents can therefore navigate Tube even when
/// the watch page is not open, without materialising an unbounded catalogue.
pub fn provide_resources(model: &TubeModel) -> Vec<Resource> {
    let mut app = Resource::new("tube.app", "tube.application", "Tube");
    // Current context is deliberately first: bounded palettes surface what the
    // user is looking at before the broader known library.
    if let Route::Watch(id) = &model.route
        && let Some(mut current) = video_resource(model, id, true)
    {
        current.id.0 = format!("tube.current:{id}");
        if let Some(play) = view_spec(
            "play-pause",
            "Play or pause",
            &TubeOperation::TogglePlayback,
        ) {
            current = current.with_action(play);
        }
        app = app.with_child(current);
    }
    app = app
        .with_child(navigation_resource())
        .with_child(library_resource(model))
        .with_child(settings_resource(model));
    vec![app]
}

fn navigation_resource() -> Resource {
    let mut navigation = Resource::new("tube.navigation", "tube.navigation", "Navigate");
    for (id, label, route) in [
        ("home", "Open Home", Route::Home),
        ("trending", "Open Trending", Route::Trending),
        ("subscriptions", "Open subscriptions", Route::Subscriptions),
        ("history", "Open watch history", Route::History),
        ("settings", "Open settings", Route::Settings),
    ] {
        if let Some(spec) = view_spec(id, label, &TubeOperation::Navigate { route }) {
            navigation = navigation.with_action(spec);
        }
    }

    navigation
}

fn library_resource(model: &TubeModel) -> Resource {
    let mut library = Resource::new("tube.library", "tube.library", "Known library");
    // Collections are deliberately separate resources, rather than merely
    // presentation filters over the catalogue.  That gives Ctrl+K, history,
    // collaboration and agents stable scopes to address without having to
    // infer membership from the currently rendered route.
    let mut videos = Resource::new("tube.videos", "tube.collection", "Videos");
    for video in model.videos.iter().take(MAX_RESOURCE_ENTITIES) {
        if let Some(resource) = video_resource(
            model,
            &video.id,
            matches!(&model.route, Route::Watch(id) if id == &video.id),
        ) {
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

/// Ordered device-local watch history. Entries are membership resources rather
/// than aliases of `tube.video:*`, preserving both the original video identity
/// and the user's chronological history identity for local automation clients.
fn history_resource(model: &TubeModel) -> Resource {
    let mut history = Resource::new("tube.history", "tube.collection", "Watch history");
    for video_id in model.history.iter().take(MAX_RESOURCE_ENTITIES) {
        let Some(video) = model.video(video_id) else {
            // Do not invent a title for an evicted/unknown video. The durable
            // id remains useful to clients, and opening it will fail closed.
            continue;
        };
        let mut entry = Resource::new(
            format!("tube.history:{video_id}"),
            "tube.history-entry",
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
            &TubeAction::CyclePreferredQuality,
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

impl ResourceProvider for crate::app::TubeApp {
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
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

    #[test]
    fn comment_resource_identity_survives_reordering() {
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

    #[test]
    fn resource_projection_is_bounded_for_large_catalogues() {
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

    #[test]
    fn durable_views_and_search_are_canonical_bounded_collections() {
        let mut model = crate::sample::model();
        let video = model.videos[0].id.clone();
        let channel = model.videos[0].channel_id.clone();
        model.subscriptions = vec![channel];
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
    }

    #[test]
    fn resource_labels_bound_remote_metadata_without_changing_actions() {
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
            .and_then(|(value, _)| value.to_str().map(str::to_owned))
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

    #[test]
    fn playback_setting_resources_write_the_same_document_state_as_visible_controls() {
        let app = TubeApp::new();
        let mut doc = AutoCommit::new();
        let resources = Widget::resources(&app, &doc);
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
        assert!(matches!(quality, qos_actions::Dispatched::AppliedLocal(_)));
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
        assert!(matches!(category, qos_actions::Dispatched::AppliedLocal(_)));
        let (Value::Object(ObjType::Map), tube) = doc.get(&ROOT, "tube").unwrap().unwrap() else {
            panic!("tube root exists");
        };
        let (Value::Object(ObjType::Map), settings) = doc.get(&tube, "settings").unwrap().unwrap()
        else {
            panic!("settings map exists");
        };
        let (Value::Object(ObjType::Map), categories) =
            doc.get(&settings, "sponsor_categories").unwrap().unwrap()
        else {
            panic!("category map exists");
        };
        assert_eq!(
            doc.get(&categories, "sponsor").unwrap().unwrap().0.to_str(),
            Some("show"),
        );
    }

    #[test]
    fn unavailable_sync_keeps_discoverable_unavailable_effects() {
        let app = TubeApp::new();
        let doc = AutoCommit::new();
        let resources = Widget::resources(&app, &doc);
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

    /// The host-authoritative "Add to queue" action fails closed when no host
    /// transport is installed.
    #[test]
    fn executing_add_to_queue_refuses_without_host() {
        let (app, vid, _) = watching();
        let mut doc = AutoCommit::new();
        let resources = Widget::resources(&app, &doc);
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
        assert!(matches!(error, qos_actions::ActionError::Refused(_)));
        assert!(doc.get(&ROOT, "tube").unwrap().is_none());
        let _ = vid;
    }

    #[cfg(feature = "grayjay-sync")]
    #[test]
    fn device_lifecycle_resources_are_effects_with_confirmation() {
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
            qos_actions::ConfirmationPolicy::None
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
            qos_actions::ConfirmationPolicy::None
        ));
        assert_eq!(pair.params()[0].ty, ParamType::Secret);
    }

    #[test]
    fn unavailable_device_backend_keeps_stable_unavailable_effects() {
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
                .any(|action| action.id == "sync" && !action.contract.availability.is_available())
        );
        assert!(resource.children.is_empty());
    }
}
