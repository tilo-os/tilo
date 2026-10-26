//! Perform one queued op against Discord's REST API.
//!
//! Performs run concurrently. Calls in one rate-limit bucket still go one at
//! a time and in order (see [`crate::ratelimit`]), so ops in one channel
//! reach Discord in the order the host handed them over.
//!
//! Webhook sends go through a lane per webhook and thread. When a burst
//! has emptied the webhook's bucket, the sends waiting in its lane that
//! share an author (username and avatar) and carry no files are merged into
//! one Discord message, their texts joined with newlines up to Discord's
//! 2000 characters, and every merged op settles with that message's id. A
//! later edit of one of them replaces the merged message's text with its
//! own; that is the price of not falling ever further behind in a flood.

#![allow(
    clippy::indexing_slicing,
    reason = "serde_json::Value's string index answers Null for anything missing and never panics"
)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use discord_wire::op::{
    AnswerInteractionOp, ChannelKind, ChannelSpec, CreateWebhookOp, DeleteEmojiOp, DeleteOp,
    DiscordOp, DiscordResult, EditOp, EmojiSpec, FetchAssetOp, InAttachment, MemberOp,
    MemberRolesOp, OverwriteKind, OverwriteOp, ReactOp, RoleSpec, SendOp, StartThreadOp,
};
use serde_json::{Value, json};
use tilo_connector::protocol::{Bytes, FromWorker, Outcome};
use tilo_net::Method;
use tokio::sync::oneshot;
use url::Url;

use crate::Ctx;
use crate::egress::Purpose;
use crate::events::snowflake_ms;
use crate::rest::{Auth, Body, RestError, segment};
use crate::secrets::WEBHOOK;

/// Discord's message length limit.
pub const MAX_CONTENT_CHARS: usize = 2000;
/// How far back the duplicate check looks (discord-bridge.md §8).
pub const DUPLICATE_WINDOW: u32 = 50;
/// The name of webhooks the bridge creates.
pub const WEBHOOK_NAME: &str = "Tilo";
/// Discord's error for a second thread on one message.
const THREAD_EXISTS: u64 = 160_004;
/// Discord's error for a call the bot's permissions do not allow.
const MISSING_PERMISSIONS: u64 = 50_013;
/// Discord's error for a server whose emoji slots are all taken.
const MAX_EMOJIS: u64 = 30_008;
/// The largest picture Discord takes for a custom emoji.
pub const MAX_EMOJI_BYTES: usize = 256 * 1024;
/// What the bridge's page says when the bot may not upload emoji: the
/// permission its role needs, by the name Discord's settings show.
pub const EMOJI_PERMISSION_PROBLEM: &str =
    "the bot needs the Create Expressions permission in the Discord server";
/// What the bridge's page says when the server has no emoji slot left.
pub const EMOJI_FULL_PROBLEM: &str =
    "the Discord server has no free emoji slot, so newer ones stay in Tilo";

/// Why an op did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpError {
    /// Permanent: dead-letter it.
    Refused(String),
    /// Transient: the host retries.
    Unavailable(String),
}

impl From<RestError> for OpError {
    fn from(error: RestError) -> Self {
        match error {
            RestError::Refused { .. } => Self::Refused(error.to_string()),
            RestError::Unauthorized(message) | RestError::Unavailable(message) => {
                Self::Unavailable(message)
            }
        }
    }
}

type OpResult = Result<DiscordResult, OpError>;

enum Started {
    Send(oneshot::Receiver<OpResult>),
    Other(Box<DiscordOp>),
}

struct Waiting {
    op: SendOp,
    answer: oneshot::Sender<OpResult>,
}

#[derive(Default)]
struct Lane {
    queue: VecDeque<Waiting>,
    draining: bool,
}

pub struct Performer {
    ctx: Arc<Ctx>,
    /// Attachment bytes the host sent ahead of the op that names them.
    blobs: Mutex<HashMap<String, Vec<u8>>>,
    lanes: Mutex<HashMap<String, Lane>>,
}

impl Performer {
    #[must_use]
    pub fn new(ctx: Arc<Ctx>) -> Self {
        Self {
            ctx,
            blobs: Mutex::new(HashMap::new()),
            lanes: Mutex::new(HashMap::new()),
        }
    }

    /// Keep the bytes of an attachment for the perform that follows.
    pub fn blob(&self, blake3_hex: &str, bytes: &Bytes) {
        if blake3::hash(&bytes.0).to_hex().as_str() != blake3_hex {
            return;
        }
        if let Ok(mut blobs) = self.blobs.lock() {
            blobs.insert(blake3_hex.to_owned(), bytes.0.clone());
        }
    }

    /// Perform `op` (a [`DiscordOp`] as JSON) and say how it went.
    ///
    /// A send joins its lane before this returns, so sends reach Discord in
    /// the order the host handed them over even though the returned futures
    /// run concurrently.
    pub fn perform(self: &Arc<Self>, op: Value) -> impl Future<Output = Outcome> + Send + 'static {
        let started = match serde_json::from_value::<DiscordOp>(op) {
            Err(error) => Err(OpError::Refused(format!("not a Discord op: {error}"))),
            Ok(op) if self.ctx.config.read_only && !matches!(op, DiscordOp::FetchAsset(_)) => {
                Err(OpError::Refused(
                    "the Discord worker runs read-only and changes nothing in Discord".into(),
                ))
            }
            Ok(op) if self.ctx.config.only_channel.is_some() && op.reaches_server() => {
                Err(OpError::Refused(
                    "a bridge of one channel changes nothing else in the server".into(),
                ))
            }
            Ok(op)
                if op
                    .channel()
                    .is_some_and(|channel| !self.ctx.in_scope(channel)) =>
            {
                Err(OpError::Refused(
                    "this bridge reaches only its one Discord channel".into(),
                ))
            }
            Ok(DiscordOp::Send(op)) => Ok(Started::Send(self.enqueue(op))),
            Ok(op) => Ok(Started::Other(Box::new(op))),
        };
        let performer = Arc::clone(self);
        async move {
            let answer = match started {
                Err(error) => Err(error),
                Ok(Started::Send(answered)) => answered
                    .await
                    .unwrap_or_else(|_| Err(OpError::Unavailable("the send was dropped".into()))),
                Ok(Started::Other(op)) => performer.run(*op).await,
            };
            match answer {
                Ok(result) => Outcome::Done {
                    result: serde_json::to_value(result).unwrap_or(Value::Null),
                },
                Err(OpError::Refused(reason)) => Outcome::Refused { reason },
                Err(OpError::Unavailable(reason)) => Outcome::Unavailable { reason },
            }
        }
    }

    async fn run(self: &Arc<Self>, op: DiscordOp) -> OpResult {
        match op {
            DiscordOp::Send(op) => self
                .enqueue(op)
                .await
                .unwrap_or_else(|_| Err(OpError::Unavailable("the send was dropped".into()))),
            DiscordOp::Edit(op) => self.edit(op).await,
            DiscordOp::Delete(op) => self.delete(op).await,
            DiscordOp::React(op) => self.react(op, true).await,
            DiscordOp::Unreact(op) => self.react(op, false).await,
            DiscordOp::StartThread(op) => self.start_thread(op).await,
            DiscordOp::CreateWebhook(op) => self.create_webhook(op).await,
            DiscordOp::ApplyChannel(spec) => self.apply_channel(spec).await,
            DiscordOp::DeleteChannel(op) => {
                let path = format!("/channels/{}", segment(&op.channel));
                self.gone_is_done(Method::DELETE, &path, None).await
            }
            DiscordOp::ApplyRole(spec) => self.apply_role(spec).await,
            DiscordOp::DeleteRole(op) => {
                let path = format!(
                    "/guilds/{}/roles/{}",
                    segment(&self.guild()?),
                    segment(&op.role)
                );
                self.gone_is_done(Method::DELETE, &path, None).await
            }
            DiscordOp::SetMemberRoles(op) => self.member_roles(op).await,
            DiscordOp::Kick(op) => {
                let path = format!(
                    "/guilds/{}/members/{}",
                    segment(&self.guild()?),
                    segment(&op.user)
                );
                self.gone_is_done(Method::DELETE, &path, reason(&op)).await
            }
            DiscordOp::Ban(op) => {
                let path = format!(
                    "/guilds/{}/bans/{}",
                    segment(&self.guild()?),
                    segment(&op.user)
                );
                self.call(Method::PUT, &path, Body::Json(json!({})), reason(&op))
                    .await
                    .map(|_| DiscordResult::Done)
            }
            DiscordOp::Unban(op) => {
                let path = format!(
                    "/guilds/{}/bans/{}",
                    segment(&self.guild()?),
                    segment(&op.user)
                );
                self.gone_is_done(Method::DELETE, &path, reason(&op)).await
            }
            DiscordOp::SetOverwrite(op) => self.overwrite(op).await,
            DiscordOp::FetchAsset(op) => self.fetch_asset(op).await,
            DiscordOp::AnswerInteraction(op) => self.answer_interaction(op).await,
            DiscordOp::ApplyEmoji(spec) => self.apply_emoji(spec).await,
            DiscordOp::DeleteEmoji(op) => self.delete_emoji(op).await,
        }
    }

    /// Upload a workspace emoji, or rename the one the bridge uploaded for
    /// it (uploading it again when it was deleted in Discord meanwhile).
    /// What Discord refuses for the server's sake, a permission the bot
    /// lacks or no free slot, is the result's problem, said on the bridge's
    /// page, rather than a failure: the bridge carries on without it.
    async fn apply_emoji(&self, spec: EmojiSpec) -> OpResult {
        let guild = self.guild()?;
        if let Some(id) = &spec.id {
            let path = format!("/guilds/{}/emojis/{}", segment(&guild), segment(id));
            let renamed = self
                .ctx
                .rest
                .call(
                    Method::PATCH,
                    &path,
                    &[],
                    Body::Json(json!({"name": spec.name})),
                    Auth::Bot,
                    None,
                )
                .await;
            match renamed {
                Ok(_) => {
                    return Ok(DiscordResult::Emoji {
                        emoji_id: id.clone(),
                        tilo: spec.tilo,
                        problem: None,
                    });
                }
                // Deleted in Discord meanwhile: uploaded again below.
                Err(error) if error.is_unknown() => {}
                Err(error) => return emoji_refusal(error, spec.tilo),
            }
        }
        let picture = spec.picture.as_deref().unwrap_or_default();
        let bytes = self
            .blobs
            .lock()
            .ok()
            .and_then(|mut blobs| blobs.remove(picture))
            .ok_or_else(|| OpError::Unavailable("the emoji's picture has not arrived".into()))?;
        if bytes.len() > MAX_EMOJI_BYTES {
            return Ok(DiscordResult::Emoji {
                emoji_id: String::new(),
                tilo: spec.tilo,
                problem: Some(format!(
                    "Discord takes pictures of at most 256 KB, and :{}:'s is larger",
                    spec.name
                )),
            });
        }
        let image = format!(
            "data:{};base64,{}",
            picture_type(&bytes),
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        );
        let path = format!("/guilds/{}/emojis", segment(&guild));
        let uploaded = self
            .ctx
            .rest
            .call(
                Method::POST,
                &path,
                &[],
                Body::Json(json!({"name": spec.name, "image": image, "roles": []})),
                Auth::Bot,
                None,
            )
            .await;
        match uploaded {
            Ok(emoji) => Ok(DiscordResult::Emoji {
                emoji_id: emoji["id"].as_str().unwrap_or_default().to_owned(),
                tilo: spec.tilo,
                problem: None,
            }),
            Err(error) => emoji_refusal(error, spec.tilo),
        }
    }

    async fn delete_emoji(&self, op: DeleteEmojiOp) -> OpResult {
        let path = format!(
            "/guilds/{}/emojis/{}",
            segment(&self.guild()?),
            segment(&op.emoji)
        );
        let deleted = self
            .ctx
            .rest
            .call(Method::DELETE, &path, &[], Body::Empty, Auth::Bot, None)
            .await;
        match deleted {
            Ok(_) => {}
            Err(error) if error.is_unknown() => {}
            Err(error) => return emoji_refusal(error, op.tilo),
        }
        Ok(DiscordResult::Emoji {
            emoji_id: String::new(),
            tilo: op.tilo,
            problem: None,
        })
    }

    fn guild(&self) -> Result<String, OpError> {
        self.ctx
            .guild()
            .ok_or_else(|| OpError::Unavailable("not connected to a Discord server yet".into()))
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Body,
        reason: Option<&str>,
    ) -> Result<Value, OpError> {
        Ok(self
            .ctx
            .rest
            .call(method, path, &[], body, Auth::Bot, reason)
            .await?)
    }

    /// A delete whose target is already gone has done its job: the op runs
    /// at least once, so a repeat must not dead-letter.
    async fn gone_is_done(&self, method: Method, path: &str, reason: Option<&str>) -> OpResult {
        match self
            .ctx
            .rest
            .call(method, path, &[], Body::Empty, Auth::Bot, reason)
            .await
        {
            Ok(_) => Ok(DiscordResult::Done),
            Err(error) if error.is_unknown() => Ok(DiscordResult::Done),
            Err(error) => Err(error.into()),
        }
    }

    async fn webhook_token(&self, channel: &str) -> Result<String, OpError> {
        self.ctx
            .secrets
            .get(WEBHOOK, channel)
            .await
            .map(|token| token.as_str().to_owned())
            .ok_or_else(|| {
                OpError::Refused(format!(
                    "the host holds no webhook token for channel {channel}"
                ))
            })
    }

    /// Queue a send in its lane (webhook and thread) and start the lane's
    /// drain if it is idle.
    fn enqueue(self: &Arc<Self>, op: SendOp) -> oneshot::Receiver<OpResult> {
        let key = format!(
            "{}:{}",
            op.webhook,
            op.thread.as_deref().unwrap_or_default()
        );
        let (answer, answered) = oneshot::channel();
        let Ok(mut lanes) = self.lanes.lock() else {
            return answered;
        };
        let lane = lanes.entry(key.clone()).or_default();
        lane.queue.push_back(Waiting { op, answer });
        let start = !std::mem::replace(&mut lane.draining, true);
        drop(lanes);
        if start {
            let performer = Arc::clone(self);
            tokio::spawn(async move { performer.drain(key).await });
        }
        answered
    }

    /// The next batch of a lane: its first send, plus the sends behind it
    /// that may share its message when the webhook's bucket is empty.
    fn batch(&self, key: &str) -> Vec<Waiting> {
        let Ok(mut lanes) = self.lanes.lock() else {
            return Vec::new();
        };
        let Some(lane) = lanes.get_mut(key) else {
            return Vec::new();
        };
        let Some(first) = lane.queue.pop_front() else {
            lanes.remove(key);
            return Vec::new();
        };
        // The token is held once the lane's first send went out; before
        // that nothing has been sent, so nothing can be flooded.
        let flooded = !lane.queue.is_empty()
            && self
                .ctx
                .secrets
                .cached(WEBHOOK, &first.op.channel)
                .is_some_and(|token| {
                    let path = format!(
                        "/webhooks/{}/{}",
                        segment(&first.op.webhook),
                        segment(&token)
                    );
                    self.ctx.rest.busy(&Method::POST, &path)
                });
        let mut chars = first.op.content.chars().count();
        let mut batch = vec![first];
        while flooded && let Some(next) = lane.queue.front() {
            let head = &batch[0].op;
            let joined = chars
                .saturating_add(1)
                .saturating_add(next.op.content.chars().count());
            let same = next.op.channel == head.channel
                && next.op.username == head.username
                && next.op.avatar_url == head.avatar_url
                && next.op.attachments.is_empty()
                && head.attachments.is_empty();
            if !same || joined > MAX_CONTENT_CHARS {
                break;
            }
            chars = joined;
            if let Some(next) = lane.queue.pop_front() {
                batch.push(next);
            }
        }
        batch
    }

    async fn drain(self: Arc<Self>, key: String) {
        loop {
            let batch = self.batch(&key);
            if batch.is_empty() {
                return;
            }
            let ops: Vec<SendOp> = batch.iter().map(|w| w.op.clone()).collect();
            let results = self.send_batch(ops).await;
            for (waiting, result) in batch.into_iter().zip(results) {
                let _ = waiting.answer.send(result);
            }
        }
    }

    /// Send `ops` as one message, after the duplicate check: an op whose
    /// text Discord already shows from this webhook since the op was queued
    /// (a send that went through before the host could settle it) settles
    /// with that message instead of being sent again.
    async fn send_batch(&self, ops: Vec<SendOp>) -> Vec<OpResult> {
        let Some(head) = ops.first().cloned() else {
            return Vec::new();
        };
        let fail = |error: OpError| ops.iter().map(|_| Err(error.clone())).collect::<Vec<_>>();
        let token = match self.webhook_token(&head.channel).await {
            Ok(token) => token,
            Err(error) => return fail(error),
        };
        let recent = match self.recent_from_webhook(&head).await {
            Ok(recent) => recent,
            Err(error) => return fail(error),
        };
        let mut results: Vec<Option<OpResult>> = ops
            .iter()
            .map(|op| {
                recent
                    .iter()
                    .find(|(id, content)| {
                        snowflake_ms(id) >= op.created_ms && shows(content, &op.content)
                    })
                    .map(|(id, _)| {
                        Ok(DiscordResult::Message {
                            message_id: id.clone(),
                            tilo: op.tilo.clone(),
                        })
                    })
            })
            .collect();
        let pending: Vec<&SendOp> = ops
            .iter()
            .zip(&results)
            .filter(|(_, done)| done.is_none())
            .map(|(op, _)| op)
            .collect();
        if let Some(first) = pending.first() {
            let content = pending
                .iter()
                .map(|op| op.content.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let sent = self.execute(first, &content, &token).await;
            for (op, slot) in ops.iter().zip(results.iter_mut()) {
                if slot.is_none() {
                    *slot = Some(sent.clone().map(|message_id| DiscordResult::Message {
                        message_id,
                        tilo: op.tilo.clone(),
                    }));
                }
            }
        }
        results
            .into_iter()
            .map(|r| r.unwrap_or_else(|| Err(OpError::Unavailable("not sent".into()))))
            .collect()
    }

    /// `(id, content)` of the webhook's messages among the channel's last
    /// [`DUPLICATE_WINDOW`].
    async fn recent_from_webhook(&self, op: &SendOp) -> Result<Vec<(String, String)>, OpError> {
        let channel = op.thread.as_deref().unwrap_or(&op.channel);
        let answer = self
            .ctx
            .rest
            .call(
                Method::GET,
                &format!("/channels/{}/messages", segment(channel)),
                &[("limit", DUPLICATE_WINDOW.to_string())],
                Body::Empty,
                Auth::Bot,
                None,
            )
            .await?;
        Ok(answer
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| m["webhook_id"] == op.webhook.as_str())
            .map(|m| {
                (
                    m["id"].as_str().unwrap_or_default().to_owned(),
                    m["content"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .collect())
    }

    /// Execute the webhook; the new message's id.
    async fn execute(&self, op: &SendOp, content: &str, token: &str) -> Result<String, OpError> {
        let mut payload = json!({
            "content": content,
            "username": op.username,
            // Pings only for users written as <@id>: never @everyone,
            // @here or a role, whatever the Tilo text says.
            "allowed_mentions": {"parse": ["users"]},
        });
        if let Some(avatar) = &op.avatar_url {
            payload["avatar_url"] = json!(avatar);
        }
        let body = if op.attachments.is_empty() {
            Body::Json(payload)
        } else {
            let mut files = Vec::new();
            {
                let mut blobs = self
                    .blobs
                    .lock()
                    .map_err(|_| OpError::Unavailable("worker state poisoned".into()))?;
                for attachment in &op.attachments {
                    let bytes = blobs.remove(&attachment.blob).ok_or_else(|| {
                        OpError::Unavailable(format!(
                            "the bytes of {} did not arrive",
                            attachment.name
                        ))
                    })?;
                    files.push((attachment.name.clone(), bytes));
                }
                drop(blobs);
            }
            payload["attachments"] = Value::Array(
                op.attachments
                    .iter()
                    .enumerate()
                    .map(|(index, a)| json!({"id": index, "filename": a.name}))
                    .collect(),
            );
            Body::Multipart { payload, files }
        };
        let mut query = vec![("wait", "true".to_owned())];
        if let Some(thread) = &op.thread {
            query.push(("thread_id", thread.clone()));
        }
        let path = format!("/webhooks/{}/{}", segment(&op.webhook), segment(token));
        let answer = self
            .ctx
            .rest
            .call(Method::POST, &path, &query, body, Auth::InPath, None)
            .await?;
        answer["id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| OpError::Unavailable("Discord did not say which message it made".into()))
    }

    async fn edit(&self, op: EditOp) -> OpResult {
        let token = self.webhook_token(&op.channel).await?;
        let path = format!(
            "/webhooks/{}/{}/messages/{}",
            segment(&op.webhook),
            segment(&token),
            segment(&op.message)
        );
        let query: Vec<(&str, String)> =
            op.thread.iter().map(|t| ("thread_id", t.clone())).collect();
        self.ctx
            .rest
            .call(
                Method::PATCH,
                &path,
                &query,
                Body::Json(
                    json!({"content": op.content, "allowed_mentions": {"parse": ["users"]}}),
                ),
                Auth::InPath,
                None,
            )
            .await?;
        Ok(DiscordResult::Done)
    }

    async fn delete(&self, op: DeleteOp) -> OpResult {
        let answer = if let Some(webhook) = &op.webhook {
            let token = self.webhook_token(&op.channel).await?;
            let path = format!(
                "/webhooks/{}/{}/messages/{}",
                segment(webhook),
                segment(&token),
                segment(&op.message)
            );
            let query: Vec<(&str, String)> =
                op.thread.iter().map(|t| ("thread_id", t.clone())).collect();
            self.ctx
                .rest
                .call(
                    Method::DELETE,
                    &path,
                    &query,
                    Body::Empty,
                    Auth::InPath,
                    None,
                )
                .await
        } else {
            let channel = op.thread.as_deref().unwrap_or(&op.channel);
            let path = format!(
                "/channels/{}/messages/{}",
                segment(channel),
                segment(&op.message)
            );
            self.ctx
                .rest
                .call(Method::DELETE, &path, &[], Body::Empty, Auth::Bot, None)
                .await
        };
        match answer {
            Ok(_) => Ok(DiscordResult::Done),
            Err(error) if error.is_unknown() => Ok(DiscordResult::Done),
            Err(error) => Err(error.into()),
        }
    }

    async fn react(&self, op: ReactOp, add: bool) -> OpResult {
        let path = format!(
            "/channels/{}/messages/{}/reactions/{}/@me",
            segment(&op.channel),
            segment(&op.message),
            segment(&op.emoji)
        );
        if add {
            self.call(Method::PUT, &path, Body::Empty, None).await?;
            Ok(DiscordResult::Done)
        } else {
            self.gone_is_done(Method::DELETE, &path, None).await
        }
    }

    async fn start_thread(&self, op: StartThreadOp) -> OpResult {
        let path = format!(
            "/channels/{}/messages/{}/threads",
            segment(&op.channel),
            segment(&op.message)
        );
        let name: String = op.name.chars().take(100).collect();
        let name = if name.trim().is_empty() {
            "Thread".to_owned()
        } else {
            name
        };
        let answer = self
            .ctx
            .rest
            .call(
                Method::POST,
                &path,
                &[],
                Body::Json(json!({"name": name, "auto_archive_duration": 10_080})),
                Auth::Bot,
                None,
            )
            .await;
        let thread_id = match answer {
            Ok(thread) => thread["id"].as_str().unwrap_or_default().to_owned(),
            // A thread started from a message has the message's id, so a
            // repeated op finds the thread it made the first time.
            Err(error) if error.code() == Some(THREAD_EXISTS) => op.message.clone(),
            Err(error) => return Err(error.into()),
        };
        // Its own ops may follow before the gateway reports it.
        self.ctx
            .threads_in([(thread_id.clone(), op.channel.clone())]);
        Ok(DiscordResult::Thread {
            thread_id,
            tilo: op.tilo,
        })
    }

    /// Create the channel's webhook, or adopt the one the bridge made
    /// before (an op may run twice). Its token goes to the host's secret
    /// store, never into the result.
    async fn create_webhook(&self, op: CreateWebhookOp) -> OpResult {
        let path = format!("/channels/{}/webhooks", segment(&op.channel));
        let existing = self.call(Method::GET, &path, Body::Empty, None).await?;
        let adopt = existing.as_array().into_iter().flatten().find(|w| {
            // Discord shows a token only for webhooks this bot made.
            w["name"] == WEBHOOK_NAME && w["token"].is_string()
        });
        let webhook = match adopt {
            Some(webhook) => webhook.clone(),
            None => {
                self.call(
                    Method::POST,
                    &path,
                    Body::Json(json!({"name": WEBHOOK_NAME})),
                    None,
                )
                .await?
            }
        };
        let (Some(id), Some(token)) = (webhook["id"].as_str(), webhook["token"].as_str()) else {
            return Err(OpError::Unavailable(
                "Discord returned a webhook without a token".into(),
            ));
        };
        self.ctx.secrets.store(WEBHOOK, &op.channel, token);
        Ok(DiscordResult::Webhook {
            webhook_id: id.to_owned(),
            tilo: op.tilo,
        })
    }

    async fn apply_channel(&self, spec: ChannelSpec) -> OpResult {
        let mut body = json!({
            "name": spec.name,
            "parent_id": spec.parent,
            "position": spec.position,
        });
        if spec.kind == ChannelKind::Text {
            body["topic"] = json!(spec.topic);
        }
        let answer = if let Some(id) = &spec.id {
            let path = format!("/channels/{}", segment(id));
            self.call(Method::PATCH, &path, Body::Json(body), None)
                .await?
        } else {
            body["type"] = json!(spec.kind.discord_type());
            let path = format!("/guilds/{}/channels", segment(&self.guild()?));
            self.call(Method::POST, &path, Body::Json(body), None)
                .await?
        };
        Ok(DiscordResult::Channel {
            channel_id: answer["id"]
                .as_str()
                .map(str::to_owned)
                .or(spec.id)
                .unwrap_or_default(),
            tilo: spec.tilo,
        })
    }

    async fn apply_role(&self, spec: RoleSpec) -> OpResult {
        let guild = self.guild()?;
        let mut body = json!({"permissions": spec.permissions, "color": spec.color});
        let answer = if let Some(id) = &spec.id {
            // `@everyone` keeps its name; Discord refuses to rename it.
            if *id != guild {
                body["name"] = json!(spec.name);
            }
            let path = format!("/guilds/{}/roles/{}", segment(&guild), segment(id));
            self.call(Method::PATCH, &path, Body::Json(body), None)
                .await?
        } else {
            body["name"] = json!(spec.name);
            let path = format!("/guilds/{}/roles", segment(&guild));
            self.call(Method::POST, &path, Body::Json(body), None)
                .await?
        };
        Ok(DiscordResult::Role {
            role_id: answer["id"]
                .as_str()
                .map(str::to_owned)
                .or(spec.id)
                .unwrap_or_default(),
            tilo: spec.tilo,
        })
    }

    async fn member_roles(&self, op: MemberRolesOp) -> OpResult {
        let guild = self.guild()?;
        for (method, roles) in [(Method::PUT, &op.add), (Method::DELETE, &op.remove)] {
            for role in roles {
                let path = format!(
                    "/guilds/{}/members/{}/roles/{}",
                    segment(&guild),
                    segment(&op.user),
                    segment(role)
                );
                self.call(method.clone(), &path, Body::Empty, None).await?;
            }
        }
        Ok(DiscordResult::Done)
    }

    async fn overwrite(&self, op: OverwriteOp) -> OpResult {
        let path = format!(
            "/channels/{}/permissions/{}",
            segment(&op.channel),
            segment(&op.target)
        );
        let kind = match op.target_kind {
            OverwriteKind::Role => 0,
            OverwriteKind::Member => 1,
        };
        self.call(
            Method::PUT,
            &path,
            Body::Json(json!({"type": kind, "allow": op.allow, "deny": op.deny})),
            None,
        )
        .await?;
        Ok(DiscordResult::Done)
    }

    /// Download a CDN file (an avatar, an attachment) up to `max_bytes` and
    /// hand its bytes to the host, which stores them before it reads the
    /// result naming them.
    /// Edit the reply to a slash command to what came of it, its button
    /// gone.
    async fn answer_interaction(&self, op: AnswerInteractionOp) -> OpResult {
        let Some(kept) = self.ctx.take_interaction(&op.interaction) else {
            return Err(OpError::Refused(
                "the reply was answered already, or Discord no longer lets it be edited".into(),
            ));
        };
        crate::gateway::edit_reply(&self.ctx.rest, &kept, &op.content)
            .await
            .map_err(|error| OpError::Unavailable(format!("editing the reply: {error:?}")))?;
        Ok(DiscordResult::Done)
    }

    async fn fetch_asset(&self, op: FetchAssetOp) -> OpResult {
        // A path is on the CDN this worker talks to: the bridge names a
        // linked account's avatar without knowing where Discord serves it.
        let url = if op.url.starts_with("avatars/") && !op.url.contains("..") {
            let base = self.ctx.config.cdn_base.as_str().trim_end_matches('/');
            Url::parse(&format!("{base}/{}", op.url))
        } else {
            Url::parse(&op.url)
        }
        .map_err(|_| OpError::Refused("not a URL".into()))?;
        if self.ctx.config.egress.check(&url, Purpose::Cdn).is_err() {
            return Err(OpError::Refused("only Discord's CDN is fetched".into()));
        }
        let limit = usize::try_from(op.max_bytes).unwrap_or(usize::MAX);
        let response = self
            .ctx
            .rest
            .http()
            .get(url.as_str())
            .send()
            .await
            .map_err(|_| OpError::Unavailable("could not reach Discord's CDN".into()))?;
        let status = response.status().as_u16();
        if status == 404 || status == 403 {
            return Err(OpError::Refused(format!(
                "the CDN answered {status}; the file is gone"
            )));
        }
        if !(200..300).contains(&status) {
            return Err(OpError::Unavailable(format!("the CDN answered {status}")));
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_owned();
        let bytes = response
            .bytes_limited(limit)
            .await
            .map_err(|error| match error {
                tilo_net::NetError::TooLarge { .. } => {
                    OpError::Refused(format!("the file is larger than {} bytes", op.max_bytes))
                }
                _ => OpError::Unavailable("the download from Discord's CDN failed".into()),
            })?;
        let blob = blake3::hash(&bytes).to_hex().to_string();
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if op.inline {
            // A picture a record keeps as it is: only an image type a
            // profile may carry.
            let kind = content_type.split(';').next().unwrap_or_default().trim();
            if !matches!(kind, "image/png" | "image/jpeg" | "image/webp") {
                return Err(OpError::Refused(format!("not a picture: {kind}")));
            }
            use base64::Engine as _;
            let data_url = format!(
                "data:{kind};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&bytes)
            );
            return Ok(DiscordResult::Asset {
                blob,
                size,
                content_type,
                attachment: None,
                data_url: Some(data_url),
            });
        }
        let _ = self.ctx.out.send(FromWorker::Blob {
            bytes: Bytes(bytes.to_vec()),
        });
        let attachment = op.attachment.map(|attachment| InAttachment {
            blob: blob.clone(),
            size,
            ..attachment
        });
        Ok(DiscordResult::Asset {
            blob,
            size,
            content_type,
            attachment,
            data_url: None,
        })
    }
}

fn reason(op: &MemberOp) -> Option<&str> {
    Some(op.reason.as_str()).filter(|r| !r.is_empty())
}

/// What became of an emoji op Discord refused. A permission the bot lacks,
/// a full server or a picture Discord would not take is the result's
/// problem, which the bridge's page says; Discord out of reach is retried.
fn emoji_refusal(error: RestError, tilo: String) -> OpResult {
    let problem = match (&error, error.code()) {
        (_, Some(MISSING_PERMISSIONS)) | (RestError::Refused { status: 403, .. }, _) => {
            EMOJI_PERMISSION_PROBLEM.to_owned()
        }
        (_, Some(MAX_EMOJIS)) => EMOJI_FULL_PROBLEM.to_owned(),
        (RestError::Refused { message, .. }, _) => {
            let said: String = message.chars().take(200).collect();
            format!("Discord did not take :{tilo}: ({said})")
        }
        _ => return Err(error.into()),
    };
    Ok(DiscordResult::Emoji {
        emoji_id: String::new(),
        tilo,
        problem: Some(problem),
    })
}

/// The media type of an emoji's picture, by its first bytes: Discord takes
/// PNG, JPEG, GIF and WebP.
fn picture_type(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        "image/webp"
    } else {
        "image/png"
    }
}

/// Whether a webhook message's `shown` text is `content`, alone or as one
/// of the texts merged into it.
fn shows(shown: &str, content: &str) -> bool {
    shown == content
        || shown.starts_with(&format!("{content}\n"))
        || shown.ends_with(&format!("\n{content}"))
        || shown.contains(&format!("\n{content}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merged_texts_count_as_shown() {
        assert!(shows("a", "a"));
        assert!(shows("a\nb\nc", "a"));
        assert!(shows("a\nb\nc", "b"));
        assert!(shows("a\nb\nc", "c"));
        assert!(!shows("ab", "a"));
        assert!(!shows("a\nbc", "b"));
    }
}
