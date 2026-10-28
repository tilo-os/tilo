//! The gateway: hello, heartbeats, identify, resume and replay.

use std::sync::Arc;

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_websockets::{CloseCode, Message};

use crate::{BOT_USER, GUILD, Outgoing, Session, Shared, State};

/// Short, so a test sees several heartbeats without waiting long.
const HEARTBEAT_INTERVAL_MS: u64 = 2_000;

pub async fn serve(shared: Arc<Shared>, listener: tokio::net::TcpListener) {
    while let Ok((socket, _)) = listener.accept().await {
        tokio::spawn(connection(Arc::clone(&shared), socket));
    }
}

async fn connection(shared: Arc<Shared>, socket: tokio::net::TcpStream) {
    let Ok((_, mut ws)) = tokio_websockets::ServerBuilder::new().accept(socket).await else {
        return;
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    shared.lock().gateway.live = Some(tx.clone());
    let hello = json!({"op": 10, "d": {"heartbeat_interval": HEARTBEAT_INTERVAL_MS}});
    if ws.send(Message::text(hello.to_string())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            frame = ws.next() => {
                let Some(Ok(frame)) = frame else { break };
                if frame.is_close() {
                    break;
                }
                if let Some(text) = frame.as_text() {
                    let parsed: Value = serde_json::from_str(text).unwrap_or(Value::Null);
                    handle(&mut shared.lock(), &tx, &parsed);
                }
            }
            outgoing = rx.recv() => match outgoing {
                Some(Outgoing::Text(text)) => {
                    if ws.send(Message::text(text)).await.is_err() {
                        break;
                    }
                }
                Some(Outgoing::Close(code, reason)) => {
                    let _ = ws.send(Message::close(CloseCode::try_from(code).ok(), &reason)).await;
                    let _ = ws.close().await;
                    break;
                }
                None => break,
            },
        }
    }
    let mut state = shared.lock();
    if state
        .gateway
        .live
        .as_ref()
        .is_some_and(|live| live.same_channel(&tx))
    {
        state.gateway.live = None;
    }
}

fn handle(state: &mut State, tx: &mpsc::UnboundedSender<Outgoing>, frame: &Value) {
    let data = &frame["d"];
    match frame["op"].as_u64() {
        Some(1) => {
            let _ = tx.send(Outgoing::Text(json!({"op": 11}).to_string()));
        }
        Some(2) => {
            state.gateway.identifies = state.gateway.identifies.saturating_add(1);
            if data["token"] != state.token.as_str() {
                let _ = tx.send(Outgoing::Close(4004, "Authentication failed.".into()));
                return;
            }
            state.gateway.intents = data["intents"].as_u64();
            state.gateway.sessions_started = state.gateway.sessions_started.saturating_add(1);
            state.gateway.session = Some(Session {
                id: format!("session-{}", state.gateway.sessions_started),
                ..Session::default()
            });
            state.gateway.live = Some(tx.clone());
            let ready = json!({
                "v": 10,
                "user": {"id": BOT_USER, "username": "tilo-bridge", "bot": true},
                "guilds": [{"id": GUILD, "unavailable": true}],
                "session_id": format!("session-{}", state.gateway.sessions_started),
                "resume_gateway_url": state.gateway_url,
                "application": {"id": BOT_USER, "flags": 0},
            });
            state.dispatch("READY", ready);
            let guild = state.guild_json();
            state.dispatch("GUILD_CREATE", guild);
        }
        Some(6) => {
            state.gateway.resumes = state.gateway.resumes.saturating_add(1);
            if data["token"] != state.token.as_str() {
                let _ = tx.send(Outgoing::Close(4004, "Authentication failed.".into()));
                return;
            }
            let refuse = std::mem::take(&mut state.gateway.refuse_next_resume);
            let known = state
                .gateway
                .session
                .as_ref()
                .is_some_and(|s| data["session_id"] == s.id.as_str());
            if refuse || !known {
                state.gateway.session = None;
                let _ = tx.send(Outgoing::Text(json!({"op": 9, "d": false}).to_string()));
                return;
            }
            state.gateway.live = Some(tx.clone());
            let after = data["seq"].as_u64().unwrap_or(0);
            if let Some(session) = &state.gateway.session {
                for (_, frame) in session.log.iter().filter(|(seq, _)| *seq > after) {
                    let _ = tx.send(Outgoing::Text(frame.clone()));
                }
            }
            state.dispatch("RESUMED", json!({}));
        }
        Some(4) if data["guild_id"] == GUILD => {
            state.bot_voice_state(data["channel_id"].as_str());
        }
        _ => {}
    }
}
