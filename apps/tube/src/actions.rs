//! Tube's document actions, its effects, and the library it publishes.
//!
//! [`TubeAction`] is every change to the shared library: likes, the watch
//! queue, subscriptions, and comment likes and replies.
//! It writes a structured sub-tree under the document's `"tube"` map, keyed
//! by stable ids so concurrent edits merge and replays converge:
//!
//! ```text
//! root["tube"]
//! ├── "likes"           (Map)  video_id   -> bool
//! ├── "queue"           (Map)  video_id   -> bool (membership/tombstone)
//! ├── "subscriptions"   (Map)  channel_id -> bool
//! ├── "comment_likes"   (Map)  comment_id -> bool (membership/tombstone)
//! └── "comment_replies" (Map)  comment_id -> (Map reply_id -> {text, active})
//! ```
//!
//! [`TubeEffect`] is everything Tube does that changes no document:
//! navigating, playing, sharing, the person's own watch history and playback
//! preferences, and Grayjay device sync. Fetched catalogue rows and paired devices are the surface's
//! live rows (`crate::rows`); they offer effects, and the edits on them are
//! the declarations here, named by the row's id.

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};

use tilo_actions::{
    Action, ActionError, ActionParam, ActionSpec, Doc, ExecutionLocus, ParamType, Resource,
};
use ui::tilo_resource::{self, ActionDecl, Effect};

use crate::action::SettingFlag;
use crate::model::{Category, Route, Settings, SkipMode};
use crate::resource::{Tube, TubeShared};

const MAX_RESOURCE_ENTITIES: usize = 100;
const MAX_RESOURCE_LABEL_CHARS: usize = 160;
pub(crate) const QUALITY_OPTIONS: [&str; 4] = ["1080p", "720p", "480p", "auto"];

/// Registry `type_id` for every Tube action.
pub const TUBE_ACTION_TYPE: &str = "tube.action";

/// The binding of one fully specified effect, for a control: the same
/// published effect the palette and agents invoke.
pub fn effect_binding(effect: &TubeEffect) -> Option<ui::tilo_actions::ActionBinding<()>> {
    use ui::tilo_resource::{Effect as _, Resource as _};
    let spec = TubeEffect::specs()
        .into_iter()
        .find(|spec| spec.id == effect.id())?;
    let app = crate::resource::Tube::KIND;
    let root = ui::tilo_actions::Resource::new(app, app, crate::resource::Tube::LABEL)
        .with_action(spec.clone());
    ui::tilo_actions::ActionBinding::static_json(&root, &spec, effect.arguments()).ok()
}

/// What Tube does that changes no document, invoked from its own controls,
/// the palette, an agent or a script alike. A live row offers the effects
/// bound to its entity (open *this* video).
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "tube.effect")]
pub enum TubeEffect {
    /// Show a screen.
    #[effect(view)]
    ShowScreen {
        #[param("home | trending | subscriptions | queue | history | settings")]
        screen: String,
    },
    /// Show or hide navigation.
    #[effect(view)]
    ToggleSidebar,
    /// Open video.
    #[effect(permission = "read")]
    OpenVideo {
        #[param("The video's id")]
        video: String,
    },
    /// Open creator.
    #[effect(permission = "read")]
    OpenCreator {
        #[param("The creator's channel id")]
        creator: String,
    },
    /// Open playlist.
    #[effect(permission = "read")]
    OpenPlaylist {
        #[param("The playlist's id")]
        playlist: String,
    },
    /// Play or pause.
    #[effect(permission = "read")]
    TogglePlayback,
    /// Copy share link.
    #[effect(permission = "read")]
    Share {
        #[param("The video's id")]
        video: String,
    },
    /// Download video.
    #[effect(device_only)]
    Download {
        #[param("The video's id")]
        video: String,
    },
    /// Mark watched.
    ///
    /// Adds the video to your own watch history, which follows you to your
    /// other devices and never reaches collaborators.
    #[effect(permission = "read")]
    MarkWatched {
        #[param("The video's id")]
        video: String,
    },
    /// Remove from history.
    #[effect(permission = "read")]
    ForgetWatched {
        #[param("The video's id")]
        video: String,
    },
    /// Clear watch history.
    #[effect(destructive, permission = "read")]
    ClearHistory,
    /// Pair using invite.
    ///
    /// Pairs a Grayjay device, which then reads and writes your
    /// subscriptions, history and settings.
    #[effect(destructive, device_only)]
    PairDevice {
        #[param(secret, "Grayjay pairing invite; redacted from logs and history")]
        invite: String,
    },
    /// Sync approved devices now.
    #[effect(device_only)]
    SyncDevices,
    /// Approve device.
    ///
    /// Lets a device that asked to pair read and write your subscriptions,
    /// history and settings.
    #[effect(destructive, device_only)]
    ApproveDevice {
        #[param("The device's id")]
        device: String,
    },
    /// Revoke device access.
    #[effect(destructive, device_only)]
    RevokeDevice {
        #[param("The device's id")]
        device: String,
    },
    /// Set a playback preference.
    ///
    /// Your own choice, which follows you to your other devices and never
    /// reaches collaborators.
    #[effect(permission = "read")]
    SetPreference {
        #[param(
            "The preference",
            choices = [
                "autoplay",
                "comments",
                "sponsorblock",
                "dearrow",
                "dearrow-titles",
                "dearrow-thumbnails",
                "dislikes"
            ]
        )]
        preference: String,
        #[param("true to turn it on, false to turn it off")]
        enabled: bool,
    },
    /// Set preferred quality.
    ///
    /// Your own choice, which follows you to your other devices.
    #[effect(permission = "read")]
    SetPreferredQuality {
        #[param("The quality", choices = ["1080p", "720p", "480p", "auto"])]
        quality: String,
    },
    /// Set segment handling.
    ///
    /// How playback treats one category of marked segments: skip it, show it
    /// with a skip button, or ignore it. Your own choice, which follows you to
    /// your other devices.
    #[effect(permission = "read")]
    SetSegmentHandling {
        #[param(
            "The SponsorBlock category",
            choices = [
                "sponsor",
                "selfpromo",
                "interaction",
                "intro",
                "outro",
                "preview",
                "music_offtopic",
                "filler"
            ]
        )]
        category: String,
        #[param("How it is handled", choices = ["skip", "show", "disabled"])]
        mode: String,
    },
}

impl TubeEffect {
    pub(crate) fn screen(name: &str) -> Option<Route> {
        Some(match name {
            "home" => Route::Home,
            "trending" => Route::Trending,
            "subscriptions" => Route::Subscriptions,
            "queue" => Route::Queue,
            "history" => Route::History,
            "settings" => Route::Settings,
            _ => return None,
        })
    }

    /// The preferences `settings` becomes under this effect, or why it
    /// cannot; `None` for an effect that sets no preference.
    pub(crate) fn preferences(&self, settings: &Settings) -> Option<Result<Settings, String>> {
        let mut changed = settings.clone();
        match self {
            TubeEffect::SetPreference {
                preference,
                enabled,
            } => {
                let Some(flag) = SettingFlag::from_id(preference) else {
                    return Some(Err(format!("there is no preference `{preference}`")));
                };
                flag.set(&mut changed, *enabled);
            }
            TubeEffect::SetPreferredQuality { quality } => {
                if !QUALITY_OPTIONS.contains(&quality.as_str()) {
                    return Some(Err(format!("unsupported preferred quality: {quality}")));
                }
                changed.preferred_quality.clone_from(quality);
            }
            TubeEffect::SetSegmentHandling { category, mode } => {
                let Some(category) = Category::from_api_key(category) else {
                    return Some(Err(format!(
                        "there is no SponsorBlock category `{category}`"
                    )));
                };
                let Some(mode) = SkipMode::from_key(mode) else {
                    return Some(Err(format!("there is no segment handling `{mode}`")));
                };
                changed
                    .categories
                    .insert(category.api_key().to_owned(), mode);
            }
            _ => return None,
        }
        Some(Ok(changed))
    }

    /// The label a control invoking this effect is announced with.
    pub(crate) fn label(&self) -> &'static str {
        let id = self.id();
        Self::declarations()
            .into_iter()
            .find(|decl| decl.id == id)
            .map_or(id, |decl| decl.label)
    }
}

/// Stable content identity for a comment. The upstream model has no comment
/// id, so hash all immutable source fields with an explicitly fixed FNV-1a
/// algorithm rather than addressing a row by its current list position.
pub(crate) fn comment_key(video_id: &str, comment: &crate::model::Comment) -> String {
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
/// for one comment, which gives a retried reply the same address rather than
/// posting a duplicate. The reply action carries the whole key, so replay
/// never re-hashes under a changed implementation.
pub(crate) fn reply_key(comment_id: &str, text: &str) -> String {
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, comment_id.as_bytes());
    std::hash::Hasher::write_u8(&mut hasher, 0xff);
    std::hash::Hasher::write(&mut hasher, text.as_bytes());
    format!("reply-{:016x}", std::hash::Hasher::finish(&hasher))
}

/// Every change to Tube's shared library. Each carries the state it sets,
/// never a toggle, so a replay or a collaborator's replica ends where the
/// author did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TubeAction {
    SetVideoLiked {
        video_id: String,
        liked: bool,
    },
    /// Set one queue membership explicitly. `false` is a tombstone, never a
    /// list deletion whose index can drift.
    SetQueueMembership {
        video_id: String,
        queued: bool,
    },
    SetSubscription {
        channel_id: String,
        subscribed: bool,
    },
    /// `liked: false` is kept as a tombstone rather than deleting an address
    /// a concurrent client may still be updating.
    SetCommentLiked {
        comment_id: String,
        liked: bool,
    },
    /// Set one reply entity; `active: false` retracts it.
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
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let tube = tube_map(doc)?;
        match self {
            TubeAction::SetVideoLiked { video_id, liked } => {
                let likes = child_map(doc, &tube, "likes")?;
                doc.put(&likes, video_id.as_str(), ScalarValue::Boolean(*liked))?;
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
                // A small record rather than a list item: `active` beside the
                // immutable text makes a retraction an addressable tombstone
                // and concurrent replies merge.
                let reply = child_map(doc, &replies, reply_id)?;
                doc.put(&reply, "text", ScalarValue::Str(text.as_str().into()))?;
                doc.put(&reply, "active", ScalarValue::Boolean(*active))?;
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        match self {
            // Shared, host-serialized state: a host orders the queue and may
            // moderate a reply before it is visible.
            TubeAction::SetQueueMembership { .. } | TubeAction::SetCommentReply { .. } => {
                ExecutionLocus::ServerPreferred
            }
            TubeAction::SetVideoLiked { .. }
            | TubeAction::SetCommentLiked { .. }
            | TubeAction::SetSubscription { .. } => ExecutionLocus::ClientLocal,
        }
    }
}

fn text_arg(args: &tilo_actions::ActionArgs, name: &str) -> Result<String, String> {
    args.text(name)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("missing {name}"))
}

fn flag_arg(args: &tilo_actions::ActionArgs, name: &str) -> Result<bool, String> {
    args.boolean(name).ok_or_else(|| format!("missing {name}"))
}

/// The edits offered by name, on any video, creator or comment: the ones a
/// live row names by its id (`tube.video:ID` is video `ID`).
#[allow(
    clippy::too_many_lines,
    reason = "one declaration per edit an entity row names, each with its parameters"
)]
pub(crate) fn declarations() -> Vec<ActionDecl<Tube>> {
    let id =
        |name: &'static str, what: &'static str| ActionParam::required(name, ParamType::Text, what);
    let flag = |name: &'static str, what: &'static str| {
        ActionParam::required(name, ParamType::Boolean, what)
    };
    vec![
        ActionDecl::new(
            Tube::SET_VIDEO_LIKED.id,
            "Like video",
            vec![
                id("video", "The video's id"),
                flag("liked", "true to like, false to unlike"),
            ],
            |args, _| {
                Ok(TubeAction::SetVideoLiked {
                    video_id: text_arg(args, "video")?,
                    liked: flag_arg(args, "liked")?,
                })
            },
        )
        .describe("Set whether you like a video."),
        ActionDecl::new(
            Tube::SET_QUEUED.id,
            "Add to queue",
            vec![
                id("video", "The video's id"),
                flag("queued", "true to add, false to remove"),
            ],
            |args, _| {
                Ok(TubeAction::SetQueueMembership {
                    video_id: text_arg(args, "video")?,
                    queued: flag_arg(args, "queued")?,
                })
            },
        )
        .describe("Set a video's membership in the shared watch queue."),
        ActionDecl::new(
            Tube::SET_SUBSCRIBED.id,
            "Subscribe",
            vec![
                id(
                    "creator",
                    "The creator's channel id, or a subscription's channel URL",
                ),
                flag("subscribed", "true to subscribe, false to unsubscribe"),
            ],
            |args, _| {
                Ok(TubeAction::SetSubscription {
                    channel_id: text_arg(args, "creator")?,
                    subscribed: flag_arg(args, "subscribed")?,
                })
            },
        )
        .describe("Subscribe to a creator or unsubscribe."),
        ActionDecl::new(
            Tube::SET_COMMENT_LIKED.id,
            "Like comment",
            vec![
                id(
                    "comment",
                    "The comment's id: its row id after `tube.comment:`",
                ),
                flag("liked", "true to like, false to unlike"),
            ],
            |args, _| {
                Ok(TubeAction::SetCommentLiked {
                    comment_id: text_arg(args, "comment")?,
                    liked: flag_arg(args, "liked")?,
                })
            },
        )
        .describe("Set whether you like a comment."),
        ActionDecl::new(
            Tube::REPLY_TO_COMMENT.id,
            "Reply to comment",
            vec![
                id(
                    "comment",
                    "The comment's id: its row id after `tube.comment:`",
                ),
                ActionParam::required("text", ParamType::Text, "The reply"),
            ],
            |args, _| {
                let comment_id = text_arg(args, "comment")?;
                let text = text_arg(args, "text")?;
                Ok(TubeAction::SetCommentReply {
                    reply_id: reply_key(&comment_id, &text),
                    comment_id,
                    text,
                    active: true,
                })
            },
        )
        .describe("Reply to a comment, visible to everyone sharing this library."),
    ]
}

/// Remote metadata shown in global surfaces, kept single-line and bounded so a
/// hostile title cannot dominate the palette or force unbounded shaping.
pub(crate) fn resource_label(value: &str) -> String {
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

/// `ActionSpec::immediate` for a [`TubeAction`], which always serializes.
fn immediate_spec(id: &str, label: &str, action: &TubeAction) -> Option<ActionSpec> {
    ActionSpec::immediate(id, label, TUBE_ACTION_TYPE, action)
        .ok()
        .map(ActionSpec::reversible)
}

fn on(map: &std::collections::BTreeMap<String, bool>) -> impl Iterator<Item = &String> {
    map.iter().filter(|(_, on)| **on).map(|(id, _)| id)
}

fn with_specs(resource: Resource, specs: impl IntoIterator<Item = Option<ActionSpec>>) -> Resource {
    specs
        .into_iter()
        .flatten()
        .fold(resource, Resource::with_action)
}

/// The library the document holds, beneath the resource: the creators the
/// library subscribes to, the shared queue and replies to comments.
pub(crate) fn document_children(state: &TubeShared) -> Vec<Resource> {
    let mut subscriptions = Resource::new("tube.subscriptions", "tube.collection", "Subscriptions");
    for channel_id in on(&state.subscriptions).take(MAX_RESOURCE_ENTITIES) {
        subscriptions = subscriptions.with_child(with_specs(
            Resource::new(
                format!("tube.subscription:{channel_id}"),
                "tube.subscription",
                resource_label(&format!("Subscribed creator {channel_id}")),
            ),
            [
                immediate_spec(
                    "unsubscribe",
                    "Unsubscribe",
                    &TubeAction::SetSubscription {
                        channel_id: channel_id.clone(),
                        subscribed: false,
                    },
                ),
                Some(
                    TubeEffect::OpenCreator {
                        creator: channel_id.clone(),
                    }
                    .bound(),
                ),
            ],
        ));
    }
    let mut queue = Resource::new("tube.queue", "tube.collection", "Shared watch queue");
    for video_id in on(&state.queue).take(MAX_RESOURCE_ENTITIES) {
        queue = queue.with_child(with_specs(
            Resource::new(
                format!("tube.queue:{video_id}"),
                "tube.queue-entry",
                resource_label(&format!("Queued video {video_id}")),
            ),
            [
                immediate_spec(
                    "remove",
                    "Remove from queue",
                    &TubeAction::SetQueueMembership {
                        video_id: video_id.clone(),
                        queued: false,
                    },
                ),
                Some(
                    TubeEffect::OpenVideo {
                        video: video_id.clone(),
                    }
                    .bound(),
                ),
            ],
        ));
    }
    let mut replies = Resource::new("tube.replies", "tube.collection", "Replies to comments");
    for (comment_id, reply_id, reply) in state
        .comment_replies
        .iter()
        .flat_map(|(comment, replies)| {
            replies
                .iter()
                .map(move |(reply_id, reply)| (comment, reply_id, reply))
        })
        .filter(|(_, _, reply)| reply.active)
        .take(MAX_RESOURCE_ENTITIES)
    {
        replies = replies.with_child(with_specs(
            Resource::new(
                format!("tube.reply:{reply_id}"),
                "tube.reply",
                resource_label(&reply.text),
            ),
            [immediate_spec(
                "retract",
                "Retract reply",
                &TubeAction::SetCommentReply {
                    comment_id: comment_id.clone(),
                    reply_id: reply_id.clone(),
                    text: reply.text.clone(),
                    active: false,
                },
            )],
        ));
    }
    vec![
        Resource::new("tube.library", "tube.library", "Library")
            .with_child(subscriptions)
            .with_child(queue)
            .with_child(replies),
    ]
}

/// The quality after `current` in [`QUALITY_OPTIONS`], for a control that
/// cycles through them.
pub(crate) fn next_quality(current: &str) -> String {
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
