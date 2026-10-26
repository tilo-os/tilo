//! The REST routes and the CDN.

use std::convert::Infallible;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper::{Request, Response};
use serde_json::{Value, json};

use crate::{BOT_USER, GUILD, Scripted, Shared, State, Webhook, time};

/// One REST request the fake received.
#[derive(Clone, Debug, PartialEq)]
pub struct Recorded {
    pub method: String,
    /// The path after `/api/v10`, percent-decoded per segment, or the
    /// `/cdn/...` path for CDN fetches.
    pub path: String,
    pub query: Vec<(String, String)>,
    /// Whether it carried `Authorization: Bot <the current token>`.
    pub bot_auth: bool,
    /// `X-Audit-Log-Reason`, decoded.
    pub audit_reason: Option<String>,
    /// The JSON body, or a multipart request's `payload_json`.
    pub json: Value,
    /// A multipart request's files: `(filename, bytes)`.
    pub files: Vec<(String, Vec<u8>)>,
}

impl Recorded {
    #[must_use]
    pub fn query(&self, key: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

pub async fn serve(shared: Arc<Shared>, listener: tokio::net::TcpListener) {
    while let Ok((socket, _)) = listener.accept().await {
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |request| {
                let shared = Arc::clone(&shared);
                async move { Ok::<_, Infallible>(handle(&shared, request).await) }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(hyper_util::rt::TokioIo::new(socket), service)
                .await;
        });
    }
}

struct Answer {
    status: u16,
    body: Vec<u8>,
    content_type: String,
    headers: Vec<(&'static str, String)>,
}

impl Answer {
    fn json(status: u16, value: &Value) -> Self {
        Self {
            status,
            body: if status == 204 {
                Vec::new()
            } else {
                value.to_string().into_bytes()
            },
            content_type: "application/json".into(),
            headers: vec![
                ("x-ratelimit-limit", "5".into()),
                ("x-ratelimit-remaining", "4".into()),
                ("x-ratelimit-reset-after", "0.001".into()),
                ("x-ratelimit-bucket", "fake-bucket".into()),
            ],
        }
    }

    fn error(status: u16, code: u32, message: &str) -> Self {
        Self::json(status, &json!({"message": message, "code": code}))
    }

    fn unauthorized() -> Self {
        Self::error(401, 0, "401: Unauthorized")
    }

    fn rate_limited(retry_after_ms: u64, global: bool) -> Self {
        let secs = format!("{}.{:03}", retry_after_ms / 1000, retry_after_ms % 1000);
        let mut answer = Self::json(
            429,
            &json!({
                "message": "You are being rate limited.",
                "retry_after": f64::from(u32::try_from(retry_after_ms).unwrap_or(u32::MAX)) / 1000.0,
                "global": global,
            }),
        );
        answer.headers = vec![
            ("retry-after", retry_after_ms.div_ceil(1000).to_string()),
            ("x-ratelimit-limit", "5".into()),
            ("x-ratelimit-remaining", "0".into()),
            ("x-ratelimit-reset-after", secs),
            ("x-ratelimit-bucket", "fake-bucket".into()),
            (
                "x-ratelimit-scope",
                if global { "global" } else { "user" }.into(),
            ),
        ];
        if global {
            answer.headers.push(("x-ratelimit-global", "true".into()));
        }
        answer
    }
}

async fn handle(shared: &Shared, request: Request<Incoming>) -> Response<Full<Bytes>> {
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();
    let query: Vec<(String, String)> = request
        .uri()
        .query()
        .map(|q| {
            q.split('&')
                .filter_map(|pair| pair.split_once('='))
                .map(|(k, v)| (decode(k), decode(v)))
                .collect()
        })
        .unwrap_or_default();
    let header = |name: &str| {
        request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let authorization = header("authorization");
    let content_type = header("content-type").unwrap_or_default();
    let audit_reason = header("x-audit-log-reason").map(|r| decode(&r));
    let body = request
        .into_body()
        .collect()
        .await
        .map(|b| b.to_bytes().to_vec())
        .unwrap_or_default();
    let (json, files) = parse_body(&content_type, &body);
    let answer = {
        let mut state = shared.lock();
        let bot_auth = authorization.as_deref() == Some(format!("Bot {}", state.token).as_str());
        let route_path = path.strip_prefix("/api/v10").map(str::to_owned);
        let recorded = Recorded {
            method: method.clone(),
            path: route_path.as_deref().map_or_else(
                || path.clone(),
                |p| p.split('/').map(decode).collect::<Vec<_>>().join("/"),
            ),
            query,
            bot_auth,
            audit_reason,
            json,
            files,
        };
        state.requests.push(recorded.clone());
        if let Some(cdn) = path.strip_prefix("/cdn/") {
            match state.cdn.get(cdn) {
                Some((content_type, bytes)) => Answer {
                    status: 200,
                    body: bytes.clone(),
                    content_type: content_type.clone(),
                    headers: Vec::new(),
                },
                None => Answer::error(404, 0, "Not Found"),
            }
        } else if route_path.is_none() {
            Answer::error(404, 0, "404: Not Found")
        } else if let Some(scripted) = take_script(&mut state, &method, &recorded.path) {
            match scripted {
                Scripted::RateLimited {
                    retry_after_ms,
                    global,
                } => Answer::rate_limited(retry_after_ms, global),
                Scripted::Status {
                    status,
                    code,
                    message,
                } => Answer::error(status, code, &message),
            }
        } else if let Some(index) = state
            .windows
            .iter()
            .position(|w| w.method == method && recorded.path.starts_with(&w.path_prefix))
        {
            windowed(&mut state, index, &recorded)
        } else {
            route(&mut state, &recorded)
        }
    };
    let mut response = Response::builder()
        .status(answer.status)
        .header("content-type", answer.content_type);
    for (name, value) in answer.headers {
        response = response.header(name, value);
    }
    response
        .body(Full::new(Bytes::from(answer.body)))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())))
}

fn windowed(state: &mut State, index: usize, recorded: &Recorded) -> Answer {
    let now = std::time::Instant::now();
    let Some(window) = state.windows.get_mut(index) else {
        return route(state, recorded);
    };
    if now.duration_since(window.started) >= window.length {
        window.started = now;
        window.used = 0;
    }
    let left = window
        .length
        .saturating_sub(now.duration_since(window.started));
    let left_ms = u64::try_from(left.as_millis()).unwrap_or(u64::MAX).max(1);
    if window.used >= window.limit {
        let mut answer = Answer::rate_limited(left_ms, false);
        set_header(
            &mut answer,
            "x-ratelimit-bucket",
            format!("fake-window-{index}"),
        );
        return answer;
    }
    window.used = window.used.saturating_add(1);
    let remaining = window.limit.saturating_sub(window.used);
    let limit = window.limit;
    let mut answer = route(state, recorded);
    set_header(&mut answer, "x-ratelimit-limit", limit.to_string());
    set_header(&mut answer, "x-ratelimit-remaining", remaining.to_string());
    set_header(
        &mut answer,
        "x-ratelimit-reset-after",
        format!("{}.{:03}", left_ms / 1000, left_ms % 1000),
    );
    set_header(
        &mut answer,
        "x-ratelimit-bucket",
        format!("fake-window-{index}"),
    );
    answer
}

fn set_header(answer: &mut Answer, name: &'static str, value: String) {
    answer.headers.retain(|(n, _)| *n != name);
    answer.headers.push((name, value));
}

fn take_script(state: &mut State, method: &str, path: &str) -> Option<Scripted> {
    let script = state
        .scripts
        .iter_mut()
        .find(|s| s.times > 0 && s.method == method && path.starts_with(&s.path_prefix))?;
    script.times = script.times.saturating_sub(1);
    let answer = script.answer.clone();
    state.scripts.retain(|s| s.times > 0);
    Some(answer)
}

fn decode(text: &str) -> String {
    percent_encoding::percent_decode_str(text)
        .decode_utf8_lossy()
        .into_owned()
}

#[allow(clippy::too_many_lines, reason = "one match arm per Discord route")]
fn route(state: &mut State, request: &Recorded) -> Answer {
    let segments: Vec<&str> = request.path.trim_start_matches('/').split('/').collect();
    let body = &request.json;
    let public = matches!(segments.first(), Some(&"webhooks" | &"interactions"));
    if !public && !request.bot_auth {
        return Answer::unauthorized();
    }
    match (request.method.as_str(), segments.as_slice()) {
        ("GET", ["gateway", "bot"]) => Answer::json(
            200,
            &json!({
                "url": state.gateway_url, "shards": 1,
                "session_start_limit": {"total": 1000, "remaining": 999, "reset_after": 0, "max_concurrency": 1},
            }),
        ),
        ("GET", ["users", "@me"]) => Answer::json(
            200,
            &json!({"id": BOT_USER, "username": "tilo-bridge", "bot": true}),
        ),
        ("POST", ["webhooks", id, token]) => execute_webhook(state, id, token, request),
        ("PATCH", ["webhooks", id, token, "messages", message]) => {
            if let Err(answer) = check_webhook(state, id, token) {
                return answer;
            }
            let Some(stored) = state
                .message_mut(message)
                .filter(|m| m["webhook_id"] == *id)
            else {
                return Answer::error(404, 10008, "Unknown Message");
            };
            if let Some(content) = body.get("content") {
                stored["content"] = content.clone();
            }
            stored["edited_timestamp"] = json!(time::rfc3339(time::now_ms()));
            let event = stored.clone();
            state.dispatch("MESSAGE_UPDATE", event.clone());
            Answer::json(200, &event)
        }
        ("DELETE", ["webhooks", id, token, "messages", message]) => {
            if let Err(answer) = check_webhook(state, id, token) {
                return answer;
            }
            if !state
                .messages
                .iter()
                .any(|m| m["id"] == *message && m["webhook_id"] == *id)
            {
                return Answer::error(404, 10008, "Unknown Message");
            }
            delete_message(state, message);
            Answer::json(204, &Value::Null)
        }
        ("GET", ["channels", channel, "messages"]) => list_messages(state, channel, request),
        ("DELETE", ["channels", _, "messages", message]) => {
            if !state.messages.iter().any(|m| m["id"] == *message) {
                return Answer::error(404, 10008, "Unknown Message");
            }
            delete_message(state, message);
            Answer::json(204, &Value::Null)
        }
        (
            "PUT",
            [
                "channels",
                channel,
                "messages",
                message,
                "reactions",
                emoji,
                "@me",
            ],
        ) => {
            if !state.messages.iter().any(|m| m["id"] == *message) {
                return Answer::error(404, 10008, "Unknown Message");
            }
            // A custom emoji is named `name:id`, and must be one the bot can
            // use: here, one of the guild's.
            if let Some((name, id)) = emoji.split_once(':')
                && !state
                    .emojis
                    .iter()
                    .any(|e| e["name"] == name && e["id"] == id)
            {
                return Answer::error(400, 10014, "Unknown Emoji");
            }
            let entry = ((*message).to_owned(), (*emoji).to_owned());
            if !state.bot_reactions.contains(&entry) {
                state.bot_reactions.push(entry);
                let emoji = state.emoji_json(emoji);
                state.dispatch(
                    "MESSAGE_REACTION_ADD",
                    json!({"user_id": BOT_USER, "channel_id": channel, "message_id": message, "guild_id": GUILD, "emoji": emoji}),
                );
            }
            Answer::json(204, &Value::Null)
        }
        (
            "DELETE",
            [
                "channels",
                channel,
                "messages",
                message,
                "reactions",
                emoji,
                "@me",
            ],
        ) => {
            let entry = ((*message).to_owned(), (*emoji).to_owned());
            if state.bot_reactions.contains(&entry) {
                state.bot_reactions.retain(|r| *r != entry);
                let emoji = state.emoji_json(emoji);
                state.dispatch(
                    "MESSAGE_REACTION_REMOVE",
                    json!({"user_id": BOT_USER, "channel_id": channel, "message_id": message, "guild_id": GUILD, "emoji": emoji}),
                );
            }
            Answer::json(204, &Value::Null)
        }
        ("POST", ["channels", channel, "messages", message, "threads"]) => {
            if state.channel(message).is_some() {
                return Answer::error(
                    400,
                    160_004,
                    "A thread has already been created for this message",
                );
            }
            if !state.messages.iter().any(|m| m["id"] == *message) {
                return Answer::error(404, 10008, "Unknown Message");
            }
            let thread = json!({"id": message, "type": 11, "name": body["name"], "parent_id": channel, "guild_id": GUILD, "position": 0});
            state.channels.push(thread.clone());
            state.dispatch("THREAD_CREATE", thread.clone());
            Answer::json(201, &thread)
        }
        ("GET", ["channels", channel, "webhooks"]) => {
            let hooks: Vec<Value> = state
                .webhooks
                .iter()
                .filter(|w| w.channel == *channel)
                .map(webhook_json)
                .collect();
            Answer::json(200, &Value::Array(hooks))
        }
        ("POST", ["channels", channel, "webhooks"]) => {
            if state.channel(channel).is_none() {
                return Answer::error(404, 10003, "Unknown Channel");
            }
            let id = state.snowflake();
            let webhook = Webhook {
                token: format!("whtoken-{id}"),
                id,
                channel: (*channel).to_owned(),
                name: body["name"].as_str().unwrap_or("Tilo").to_owned(),
            };
            let answer = webhook_json(&webhook);
            state.webhooks.push(webhook);
            Answer::json(200, &answer)
        }
        ("PUT", ["channels", channel, "voice-status"]) => {
            // Discord wants the bot in the channel (or Manage Channels).
            if state.voice.bot_channel.as_deref() != Some(*channel) {
                return Answer::error(403, 50013, "Missing Permissions");
            }
            let status = body["status"].as_str().map(str::to_owned);
            state.voice.statuses.insert((*channel).to_owned(), status);
            Answer::json(204, &Value::Null)
        }
        ("PATCH", ["guilds", GUILD, "members", "@me"]) => {
            state.voice.nick = body["nick"].as_str().map(str::to_owned);
            Answer::json(
                200,
                &json!({"nick": state.voice.nick, "roles": state.voice.bot_roles}),
            )
        }
        ("PATCH", ["channels", channel]) => {
            let Some(stored) = state.channel_mut(channel) else {
                return Answer::error(404, 10003, "Unknown Channel");
            };
            for field in ["name", "topic", "parent_id", "position"] {
                if let Some(value) = body.get(field) {
                    stored[field] = value.clone();
                }
            }
            let event = stored.clone();
            state.dispatch("CHANNEL_UPDATE", event.clone());
            Answer::json(200, &event)
        }
        ("DELETE", ["channels", channel]) => {
            let Some(index) = state.channels.iter().position(|c| c["id"] == *channel) else {
                return Answer::error(404, 10003, "Unknown Channel");
            };
            let removed = state.channels.remove(index);
            state.dispatch("CHANNEL_DELETE", removed.clone());
            Answer::json(200, &removed)
        }
        ("PUT", ["channels", channel, "permissions", target]) => {
            let Some(stored) = state.channel_mut(channel) else {
                return Answer::error(404, 10003, "Unknown Channel");
            };
            let overwrite = json!({"id": target, "type": body["type"], "allow": body["allow"], "deny": body["deny"]});
            let mut overwrites: Vec<Value> = stored["permission_overwrites"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            overwrites.retain(|o| o["id"] != *target);
            overwrites.push(overwrite);
            stored["permission_overwrites"] = Value::Array(overwrites);
            let event = stored.clone();
            state.dispatch("CHANNEL_UPDATE", event);
            Answer::json(204, &Value::Null)
        }
        ("GET", ["guilds", GUILD, "channels"]) => Answer::json(200, &json!(state.channels)),
        ("POST", ["guilds", GUILD, "channels"]) => {
            let id = state.snowflake();
            let channel = json!({
                "id": id, "type": body["type"], "name": body["name"], "topic": body.get("topic").cloned().unwrap_or(Value::Null),
                "parent_id": body.get("parent_id").cloned().unwrap_or(Value::Null),
                "position": body.get("position").cloned().unwrap_or_else(|| json!(0)),
                "permission_overwrites": [], "guild_id": GUILD,
            });
            state.channels.push(channel.clone());
            state.dispatch("CHANNEL_CREATE", channel.clone());
            Answer::json(201, &channel)
        }
        ("GET", ["guilds", GUILD, "roles"]) => Answer::json(200, &json!(state.roles)),
        ("POST", ["guilds", GUILD, "roles"]) => {
            let id = state.snowflake();
            let role = json!({
                "id": id, "name": body["name"], "permissions": body.get("permissions").cloned().unwrap_or_else(|| json!("0")),
                "color": body.get("color").cloned().unwrap_or_else(|| json!(0)), "position": state.roles.len(), "managed": false,
            });
            state.roles.push(role.clone());
            state.dispatch(
                "GUILD_ROLE_CREATE",
                json!({"guild_id": GUILD, "role": role}),
            );
            Answer::json(200, &role)
        }
        ("PATCH", ["guilds", GUILD, "roles", role]) => {
            let Some(stored) = state.roles.iter_mut().find(|r| r["id"] == *role) else {
                return Answer::error(404, 10011, "Unknown Role");
            };
            for field in ["name", "permissions", "color"] {
                if let Some(value) = body.get(field) {
                    stored[field] = value.clone();
                }
            }
            let event = stored.clone();
            state.dispatch(
                "GUILD_ROLE_UPDATE",
                json!({"guild_id": GUILD, "role": event}),
            );
            Answer::json(200, &event)
        }
        ("DELETE", ["guilds", GUILD, "roles", role]) => {
            if !state.roles.iter().any(|r| r["id"] == *role) {
                return Answer::error(404, 10011, "Unknown Role");
            }
            state.roles.retain(|r| r["id"] != *role);
            state.dispatch(
                "GUILD_ROLE_DELETE",
                json!({"guild_id": GUILD, "role_id": role}),
            );
            Answer::json(204, &Value::Null)
        }
        ("PUT" | "DELETE", ["guilds", GUILD, "members", user, "roles", role]) => {
            let roles = state.member_roles.entry((*user).to_owned()).or_default();
            roles.retain(|r| r != role);
            if request.method == "PUT" {
                roles.push((*role).to_owned());
            }
            let roles = roles.clone();
            state.dispatch(
                "GUILD_MEMBER_UPDATE",
                json!({"guild_id": GUILD, "user": {"id": user}, "roles": roles}),
            );
            Answer::json(204, &Value::Null)
        }
        ("DELETE", ["guilds", GUILD, "members", user]) => {
            state.member_roles.remove(*user);
            state.dispatch(
                "GUILD_MEMBER_REMOVE",
                json!({"guild_id": GUILD, "user": {"id": user}}),
            );
            Answer::json(204, &Value::Null)
        }
        ("PUT", ["guilds", GUILD, "bans", user]) => {
            if !state.bans.iter().any(|b| b == user) {
                state.bans.push((*user).to_owned());
                state.dispatch(
                    "GUILD_BAN_ADD",
                    json!({"guild_id": GUILD, "user": {"id": user}}),
                );
            }
            Answer::json(204, &Value::Null)
        }
        ("DELETE", ["guilds", GUILD, "bans", user]) => {
            if !state.bans.iter().any(|b| b == user) {
                return Answer::error(404, 10026, "Unknown Ban");
            }
            state.bans.retain(|b| b != user);
            state.dispatch(
                "GUILD_BAN_REMOVE",
                json!({"guild_id": GUILD, "user": {"id": user}}),
            );
            Answer::json(204, &Value::Null)
        }
        ("GET", ["guilds", GUILD, "audit-logs"]) => {
            let kind = request
                .query("action_type")
                .and_then(|k| k.parse::<u64>().ok());
            let limit = request
                .query("limit")
                .and_then(|l| l.parse::<usize>().ok())
                .unwrap_or(50);
            let entries: Vec<&Value> = state
                .audit
                .iter()
                .filter(|e| kind.is_none_or(|k| e["action_type"] == k))
                .take(limit)
                .collect();
            Answer::json(
                200,
                &json!({"audit_log_entries": entries, "users": [], "webhooks": []}),
            )
        }
        ("PUT", ["applications", BOT_USER, "guilds", GUILD, "commands"]) => {
            state.commands = body.clone();
            Answer::json(200, body)
        }
        ("POST", ["interactions", id, _token, "callback"]) => {
            state
                .interaction_replies
                .push(json!({"interaction": id, "reply": body}));
            Answer::json(204, &Value::Null)
        }
        _ => Answer::error(404, 0, "404: Not Found"),
    }
}

fn webhook_json(webhook: &Webhook) -> Value {
    json!({
        "id": webhook.id, "type": 1, "channel_id": webhook.channel, "guild_id": GUILD,
        "name": webhook.name, "token": webhook.token, "application_id": BOT_USER,
    })
}

fn check_webhook(state: &State, id: &str, token: &str) -> Result<Webhook, Answer> {
    let webhook = state
        .webhooks
        .iter()
        .find(|w| w.id == id)
        .ok_or_else(|| Answer::error(404, 10015, "Unknown Webhook"))?;
    if webhook.token != token {
        return Err(Answer::error(401, 50027, "Invalid Webhook Token"));
    }
    Ok(webhook.clone())
}

fn execute_webhook(state: &mut State, id: &str, token: &str, request: &Recorded) -> Answer {
    let webhook = match check_webhook(state, id, token) {
        Ok(webhook) => webhook,
        Err(answer) => return answer,
    };
    let body = &request.json;
    let mut channel = webhook.channel.clone();
    if let Some(thread) = request.query("thread_id") {
        let parent_ok = state
            .channel(thread)
            .is_some_and(|t| t["parent_id"] == webhook.channel.as_str());
        if !parent_ok {
            return Answer::error(400, 10003, "Unknown Channel");
        }
        thread.clone_into(&mut channel);
    }
    let content = body["content"].as_str().unwrap_or_default();
    if content.chars().count() > 2000 {
        return Answer::error(
            400,
            50035,
            "Invalid Form Body: content must be 2000 or fewer in length.",
        );
    }
    let message_id = state.snowflake();
    let attachments: Vec<Value> = request
        .files
        .iter()
        .map(|(name, bytes)| {
            let path = format!("attachments/{channel}/{message_id}/{name}");
            let url = format!("{}/cdn/{path}", state.rest_origin);
            state
                .cdn
                .insert(path, ("application/octet-stream".into(), bytes.clone()));
            json!({"id": message_id, "filename": name, "size": bytes.len(), "url": url, "proxy_url": url})
        })
        .collect();
    let message = json!({
        "id": message_id,
        "channel_id": channel,
        "guild_id": GUILD,
        "type": 0,
        "content": content,
        "author": {"id": webhook.id, "username": body["username"], "avatar": null, "bot": true},
        "webhook_id": webhook.id,
        "timestamp": time::rfc3339(time::now_ms()),
        "edited_timestamp": null,
        "attachments": attachments,
        "mentions": [],
    });
    state.messages.push(message.clone());
    state.dispatch("MESSAGE_CREATE", message.clone());
    if request.query("wait") == Some("true") {
        Answer::json(200, &message)
    } else {
        Answer::json(204, &Value::Null)
    }
}

fn delete_message(state: &mut State, id: &str) {
    let Some(index) = state.messages.iter().position(|m| m["id"] == id) else {
        return;
    };
    let removed = state.messages.remove(index);
    state.dispatch(
        "MESSAGE_DELETE",
        json!({"id": id, "channel_id": removed["channel_id"], "guild_id": GUILD}),
    );
}

/// Newest first, as Discord answers; with `after`, the `limit` messages
/// right after that id.
fn list_messages(state: &State, channel: &str, request: &Recorded) -> Answer {
    if state.channel(channel).is_none() {
        return Answer::error(404, 10003, "Unknown Channel");
    }
    let limit = request
        .query("limit")
        .and_then(|l| l.parse::<usize>().ok())
        .unwrap_or(50)
        .min(100);
    let id_of = |m: &Value| {
        m["id"]
            .as_str()
            .and_then(|i| i.parse::<u64>().ok())
            .unwrap_or(0)
    };
    let mut messages: Vec<&Value> = state
        .messages
        .iter()
        .filter(|m| m["channel_id"] == channel)
        .collect();
    messages.sort_by_key(|m| id_of(m));
    let selected: Vec<&Value> =
        if let Some(after) = request.query("after").and_then(|a| a.parse::<u64>().ok()) {
            let mut picked: Vec<&Value> = messages
                .into_iter()
                .filter(|m| id_of(m) > after)
                .take(limit)
                .collect();
            picked.reverse();
            picked
        } else {
            let before = request
                .query("before")
                .and_then(|b| b.parse::<u64>().ok())
                .unwrap_or(u64::MAX);
            messages
                .into_iter()
                .filter(|m| id_of(m) < before)
                .rev()
                .take(limit)
                .collect()
        };
    Answer::json(200, &json!(selected))
}

fn parse_body(content_type: &str, body: &[u8]) -> (Value, Vec<(String, Vec<u8>)>) {
    if let Some(boundary) = content_type
        .strip_prefix("multipart/form-data")
        .and_then(|rest| rest.split("boundary=").nth(1))
    {
        return parse_multipart(boundary.trim_matches('"'), body);
    }
    (
        serde_json::from_slice(body).unwrap_or(Value::Null),
        Vec::new(),
    )
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn parse_multipart(boundary: &str, body: &[u8]) -> (Value, Vec<(String, Vec<u8>)>) {
    let delimiter = format!("--{boundary}");
    let mut json = Value::Null;
    let mut files = Vec::new();
    let mut rest = body;
    while let Some(start) = find(rest, delimiter.as_bytes()) {
        let after = rest
            .get(start.saturating_add(delimiter.len())..)
            .unwrap_or_default();
        if after.starts_with(b"--") {
            break;
        }
        let part = after.strip_prefix(b"\r\n").unwrap_or(after);
        let Some(end) = find(part, delimiter.as_bytes()) else {
            break;
        };
        let whole = part.get(..end).unwrap_or_default();
        let whole = whole.strip_suffix(b"\r\n").unwrap_or(whole);
        rest = part.get(end..).unwrap_or_default();
        let Some(split) = find(whole, b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(whole.get(..split).unwrap_or_default()).into_owned();
        let content = whole.get(split.saturating_add(4)..).unwrap_or_default();
        let field = |key: &str| {
            head.split(';')
                .map(str::trim)
                .find_map(|p| p.strip_prefix(&format!("{key}=\"")))
                .and_then(|v| v.split('"').next())
                .map(str::to_owned)
        };
        match (field("name"), field("filename")) {
            (Some(name), None) if name == "payload_json" => {
                json = serde_json::from_slice(content).unwrap_or(Value::Null);
            }
            (_, Some(filename)) => files.push((filename, content.to_vec())),
            _ => {}
        }
    }
    (json, files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_payload_and_files_are_parsed() {
        let body = b"--XX\r\nContent-Disposition: form-data; name=\"payload_json\"\r\nContent-Type: application/json\r\n\r\n{\"content\":\"hi\"}\r\n--XX\r\nContent-Disposition: form-data; name=\"files[0]\"; filename=\"a.txt\"\r\n\r\nabc\r\n--XX--\r\n";
        let (json, files) = parse_body("multipart/form-data; boundary=XX", body);
        assert_eq!(json["content"], "hi");
        assert_eq!(files, vec![("a.txt".to_owned(), b"abc".to_vec())]);
    }
}
