//! `qos-collab-probe` — a packaged app that collaborates, and says out
//! loud whether it could.
//!
//! Exists because the interesting question is not "does the broker fan
//! out" (unit tests cover that) but "does an edit from a **sandboxed**
//! process reach another machine" — which needs a real launcher, a
//! real bwrap sandbox, and the descriptor the launcher hands a
//! sandboxed app as `QOS_SVC_COLLAB_FD`; nothing here dials a path or
//! reads an env socket, so a broken handoff means the probe can't work.
//!
//! ```text
//! qos-collab-probe write:<text>      # author an edit, publish it
//! qos-collab-probe read:<seconds>    # wait for one, print it
//! ```
//!
//! The room comes from `QOS_WORKSPACE_ID` (as for every qOS app); the
//! payload is a genuine Automerge document — `write` publishes real
//! change bytes via the same `sendUnsignedIncremental` the UI toolkit
//! uses on every keystroke, and `read` loads what arrives into its own
//! document. A line from `read` means a real CRDT edit converged.
//!
//! Exit status is the verdict (0 only on real success); every failure
//! names the step, since "collaboration is off" / "no peer connected"
//! / "nobody typed" would otherwise all look like silence.

use std::process::ExitCode;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue};

/// Root key the probe reads and writes — one flat string, since the probe proves delivery, not a document model.
const FIELD: &str = "text";

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  \
         qos-collab-probe write:<text>\n  \
         qos-collab-probe read:<timeout-seconds>\n\
         The room comes from QOS_WORKSPACE_ID."
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(instruction) = args.first() else {
        return usage();
    };
    let Some((mode, arg)) = instruction.split_once(':') else {
        return usage();
    };
    let (mode, arg) = (mode.to_string(), arg.to_string());

    // The room is the collaboration address; an app that can't name one
    // isn't in a room — refuse rather than inventing a channel id that reaches nobody.
    let channel = match std::env::var("QOS_WORKSPACE_ID") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            eprintln!(
                "probe: FAIL QOS_WORKSPACE_ID is unset, so this app has no \
                 room to collaborate in."
            );
            return ExitCode::from(2);
        }
    };
    println!("probe: room = {channel}");

    // The fd is the whole subject of the exercise, so report it before
    // using it. `service_fd` returns None both when the env var is absent
    // and when it names something not open — the two shapes the handoff can fail in.
    if let Some(fd) = runtime_portal::service_fd("collab") {
        println!("probe: collab service fd = {fd}")
    } else {
        eprintln!(
            "probe: FAIL no usable collab service fd. QOS_SVC_COLLAB_FD={:?}. \
             The launcher did not put a live descriptor on that number, so \
             this app cannot collaborate at all.",
            std::env::var("QOS_SVC_COLLAB_FD").ok(),
        );
        return ExitCode::from(3);
    }

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("probe: FAIL tokio runtime: {e}");
            return ExitCode::from(4);
        }
    };
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        match mode.as_str() {
            "write" => write(&channel, &arg).await,
            "read" => {
                let secs: u64 = arg.parse().unwrap_or(30);
                read(&channel, secs).await
            }
            _ => usage(),
        }
    })
}

/// Dial the broker over the launcher-supplied descriptor and join one
/// channel; `channel` is the broker's routing key verbatim (room uuid, or `<room>#record:<id>`).
async fn join(
    channel: &str,
) -> Result<runtime_collab::ChannelHandle, runtime_portal::NotAvailable> {
    let broker = runtime_collab::try_connect().map_err(|e| {
        eprintln!("probe: FAIL could not reach the broker over the service fd: {e}");
        e
    })?;
    println!("probe: connected to the broker");
    let handle = broker.join_channel(channel, &[]).await.map_err(|e| {
        eprintln!("probe: FAIL join_channel({channel}): {e}");
        e
    })?;
    println!("probe: joined channel {channel}");
    Ok(handle)
}

async fn write(channel: &str, text: &str) -> ExitCode {
    let Ok(handle) = join(channel).await else {
        return ExitCode::from(5);
    };

    let mut doc = AutoCommit::new();
    if let Err(e) = doc.put(&ROOT, FIELD, ScalarValue::Str(text.into())) {
        eprintln!("probe: FAIL building the document: {e}");
        return ExitCode::from(6);
    }
    // A fresh document's save is its complete change set, which is what a peer starting from nothing needs to apply it.
    let bytes = doc.save();

    if let Err(e) = handle.send_unsigned_incremental(&bytes).await {
        eprintln!("probe: FAIL send_unsigned_incremental: {e}");
        return ExitCode::from(7);
    }
    println!("probe: WROTE {} bytes: {text:?}", bytes.len());
    // The broker's publish is fire-and-forget onto an actor queue and
    // this process is about to exit — give the frame a moment to leave rather than racing teardown.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    ExitCode::SUCCESS
}

async fn read(channel: &str, timeout_secs: u64) -> ExitCode {
    let Ok(handle) = join(channel).await else {
        return ExitCode::from(5);
    };
    let stream = match handle.subscribe().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("probe: FAIL subscribe: {e}");
            return ExitCode::from(8);
        }
    };
    println!("probe: subscribed; waiting up to {timeout_secs}s");

    let deadline = std::time::Duration::from_secs(timeout_secs);
    let mut doc = AutoCommit::new();
    let found = tokio::time::timeout(deadline, async {
        loop {
            let Ok(event) = stream.read().await else {
                return None;
            };
            if doc.load_incremental(&event.op).is_err() {
                // Not a document chunk we can apply — say so and keep waiting rather
                // than reporting a delivery failure for what's really a decode failure.
                eprintln!("probe: received {} bytes that did not load", event.op.len());
                continue;
            }
            match doc.get(&ROOT, FIELD) {
                Ok(Some((value, _))) => {
                    return Some(value.to_str().unwrap_or_default().to_string());
                }
                _ => continue,
            }
        }
    })
    .await;

    match found {
        Ok(Some(text)) => {
            println!("probe: READ {text:?}");
            ExitCode::SUCCESS
        }
        Ok(None) => {
            eprintln!("probe: FAIL the op stream closed before anything arrived");
            ExitCode::from(9)
        }
        Err(_) => {
            eprintln!(
                "probe: FAIL nothing arrived in {timeout_secs}s. Either no peer \
                 published, or what they published never left their machine."
            );
            ExitCode::from(10)
        }
    }
}
