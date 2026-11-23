//! A stand-in outside worker: answers `Start`, echoes performs, reports one
//! event on start, asks for a secret, writes a checkpoint, and exits on
//! `{"op":"crash"}`.

#![allow(
    clippy::unwrap_used,
    clippy::exit,
    clippy::indexing_slicing,
    reason = "a test stand-in: a broken pipe should end it loudly, exiting is how it crashes, and indexing a JSON value yields null rather than panicking"
)]

use std::io::{BufReader, Write as _};

use tilo_connector::framing::{read_frame, write_frame};
use tilo_connector::protocol::{Bytes, FromWorker, Outcome, PROTOCOL_VERSION, ToWorker};

fn main() {
    let mut input = BufReader::new(std::io::stdin().lock());
    let mut out = std::io::stdout().lock();
    while let Ok(Some(message)) = read_frame::<_, ToWorker>(&mut input) {
        match message {
            ToWorker::Start { checkpoint, .. } => {
                write_frame(
                    &mut out,
                    &FromWorker::Ready {
                        version: PROTOCOL_VERSION,
                    },
                )
                .unwrap();
                let payload = serde_json::json!({"checkpoint": checkpoint})
                    .as_object()
                    .cloned()
                    .unwrap();
                write_frame(
                    &mut out,
                    &FromWorker::Event {
                        source: "started".into(),
                        id: "boot".into(),
                        at_ms: 1,
                        payload,
                    },
                )
                .unwrap();
                write_frame(
                    &mut out,
                    &FromWorker::NeedSecret {
                        purpose: "bot-token".into(),
                        item: "default".into(),
                    },
                )
                .unwrap();
                write_frame(
                    &mut out,
                    &FromWorker::Checkpoint {
                        state: serde_json::json!({"seen": 3}),
                    },
                )
                .unwrap();
            }
            ToWorker::Secret { bytes, .. } => {
                let healthy = bytes.as_ref().is_some_and(|b| b.0 == b"token");
                write_frame(
                    &mut out,
                    &FromWorker::Status {
                        healthy,
                        message: "token checked".into(),
                    },
                )
                .unwrap();
            }
            ToWorker::Blob { .. } => {}
            ToWorker::Perform { request, op } => {
                if op["op"] == "crash" {
                    out.flush().unwrap();
                    std::process::exit(3);
                }
                if op["op"] == "download" {
                    write_frame(
                        &mut out,
                        &FromWorker::Blob {
                            bytes: Bytes(b"downloaded".to_vec()),
                        },
                    )
                    .unwrap();
                }
                let outcome = if op["op"] == "refuse" {
                    Outcome::Refused {
                        reason: "Missing Permissions".into(),
                    }
                } else {
                    Outcome::Done { result: op }
                };
                write_frame(&mut out, &FromWorker::Result { request, outcome }).unwrap();
            }
            ToWorker::Stop => break,
        }
    }
}
