//! A guest's live plane over the HTTPS stream fallback, the path every
//! browser takes when the relay is reached through an HTTP ingress (no UDP,
//! so no WebTransport): open with the session's live grant, read the
//! server's frame stream, send presence frames.
//!
//! Frames are sealed in form only: the relay reads a sealed frame's clear
//! kind, sequence and record and never its content, so random ciphertext of
//! a real frame's size costs it exactly what a cursor costs.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use http_body_util::BodyExt as _;
use tilo_relay_protocol::live::{LiveKind, LiveRecord, SealedLiveFrame};

use crate::http::Plain;
use crate::stats::Stats;

const OPEN: &str = "/peer/v3/live/fallback/open";
const RESUME: &str = "/peer/v3/live/fallback/resume";
const SEND: &str = "/peer/v3/live/fallback/send";
const SESSION: &str = "x-tilo-live-session";
const HELLO: u8 = 0x01;
const PRESENCE: u8 = 0x03;
const END: u8 = 0x07;
const END_ROTATE: u8 = 0;
const SEND_PRESENCE: u8 = 0x11;
/// A sealed pointer-and-focus presence frame's ciphertext, as pages send.
const SEALED_PAYLOAD: usize = 96;

pub struct Live {
    client: Arc<Plain>,
    session: String,
    record: [u8; 32],
    seq: u64,
}

fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len().saturating_add(5));
    out.push(kind);
    out.extend_from_slice(
        &u32::try_from(payload.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    out.extend_from_slice(payload);
    out
}

/// Split complete frames off the front of `buffer`.
fn frames(buffer: &mut Vec<u8>) -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    while let Some(header) = buffer.get(..5) {
        let kind = header.first().copied().unwrap_or_default();
        let mut size = [0u8; 4];
        size.copy_from_slice(header.get(1..5).unwrap_or(&[0; 4]));
        let end = (u32::from_be_bytes(size) as usize).saturating_add(5);
        let Some(payload) = buffer.get(5..end) else {
            break;
        };
        out.push((kind, payload.to_vec()));
        buffer.drain(..end);
    }
    out
}

impl Live {
    /// Open the fallback session with `grant` and start reading its stream.
    pub async fn open(client: Arc<Plain>, grant: &[u8], stats: Arc<Stats>) -> Result<Self, String> {
        let (code, mut body) = client.open(OPEN, &[], grant.to_vec()).await?;
        if code != 200 {
            return Err(format!("live open: status {code}"));
        }
        let mut buffer = Vec::new();
        let session = loop {
            let Some(chunk) = body.frame().await else {
                return Err("live open: stream ended before hello".into());
            };
            let chunk = chunk.map_err(|error| error.to_string())?;
            if let Ok(data) = chunk.into_data() {
                buffer.extend_from_slice(&data);
            }
            if let Some((kind, payload)) = frames(&mut buffer).into_iter().next() {
                if kind != HELLO {
                    return Err(format!("live open: first frame {kind}"));
                }
                break hex::encode(payload.get(..16).unwrap_or_default());
            }
        };
        tokio::spawn(read(client.clone(), session.clone(), body, buffer, stats));
        Ok(Self {
            client,
            session,
            record: LiveRecord {
                app: "so.tilo.desktop".into(),
                record: "shared".into(),
            }
            .digest(),
            seq: 0,
        })
    }

    /// Publish one presence frame.
    pub async fn presence(&mut self) -> Result<(), String> {
        self.seq = self.seq.saturating_add(1);
        let sealed = SealedLiveFrame {
            kind: LiveKind::Presence,
            seq: self.seq,
            record: self.record,
            channel: [7; 16],
            epoch: 1,
            payload: (0..SEALED_PAYLOAD).map(|_| rand::random::<u8>()).collect(),
        }
        .encode()
        .map_err(|error| error.to_string())?;
        let (status, _) = self
            .client
            .post_with(
                SEND,
                &[(SESSION, &self.session)],
                frame(SEND_PRESENCE, &sealed),
            )
            .await?;
        if status == 204 || status == 200 {
            Ok(())
        } else {
            Err(format!("live send: status {status}"))
        }
    }
}

/// Count what the stream delivers, resuming on rotation, until it ends.
async fn read(
    client: Arc<Plain>,
    session: String,
    mut body: hyper::body::Incoming,
    mut buffer: Vec<u8>,
    stats: Arc<Stats>,
) {
    loop {
        let rotate = loop {
            let Some(Ok(chunk)) = body.frame().await else {
                break false;
            };
            if let Ok(data) = chunk.into_data() {
                client
                    .received
                    .fetch_add(data.len() as u64, Ordering::Relaxed);
                buffer.extend_from_slice(&data);
            }
            let mut ended = None;
            for (kind, payload) in frames(&mut buffer) {
                match kind {
                    PRESENCE => {
                        stats.presence_rx.fetch_add(1, Ordering::Relaxed);
                    }
                    END => ended = Some(payload.first().copied() == Some(END_ROTATE)),
                    _ => {}
                }
            }
            if let Some(rotate) = ended {
                break rotate;
            }
        };
        if !rotate {
            stats.live_ended.fetch_add(1, Ordering::Relaxed);
            return;
        }
        buffer.clear();
        if let Ok((200, next)) = client
            .open(RESUME, &[(SESSION, &session)], Vec::new())
            .await
        {
            body = next;
        } else {
            stats.live_ended.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
}
