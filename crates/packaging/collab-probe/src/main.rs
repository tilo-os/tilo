//! `qos-collab-probe` — a packaged app that collaborates, and says out
//! loud whether it could.
//!
//! It exists because the interesting question about qOS collaboration
//! is not "does the broker fan out" — the unit tests answer that — but
//! "does an edit made by a **sandboxed** process reach another
//! machine". Answering that needs a real process, launched by the real
//! launcher, into a real bwrap sandbox, reaching the broker the only
//! way a sandboxed app can: the descriptor the launcher hands it as
//! `QOS_SVC_COLLAB_FD`. Nothing here dials a path or reads an env
//! socket; if the handoff is broken the probe cannot work, which is
//! the point.
//!
//! ```text
//! qos-collab-probe write <channel> <text>     # author an edit, publish it
//! qos-collab-probe read  <channel> <seconds>  # wait for one, print it
//! ```
//!
//! The payload is a genuine Automerge document, not a byte marker:
//! `write` builds one, puts `text` at the root, and hands the change
//! bytes to `sendUnsignedIncremental` — the same broker method the UI
//! toolkit calls on every keystroke. `read` loads what arrives into its
//! own empty document and prints the value it finds. So a line of
//! output from `read` means a CRDT edit authored in one sandbox
//! converged in another.
//!
//! Exit status is the verdict: 0 only when the thing actually
//! happened. Every failure names which step failed, because the whole
//! family of bugs this probe is aimed at is invisible — "collaboration
//! is off", "no peer connected" and "nobody typed" all render as
//! silence.

use std::process::ExitCode;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ReadDoc, ROOT, ScalarValue};

/// Root key the probe reads and writes. One flat string: the probe is
/// proving delivery, not modelling a document.
const FIELD: &str = "text";

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  \
         qos-collab-probe write <channel-id> <text>\n  \
         qos-collab-probe read <channel-id> <timeout-seconds>"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mode, channel, arg) = match args.as_slice() {
        [m, c, a] => (m.as_str(), c.clone(), a.clone()),
        _ => return usage(),
    };

    // The fd is the whole subject of the exercise, so report it before
    // trying to use it. `service_fd` returns None both when the env var
    // is absent and when it names something that is not open — the two
    // shapes the handoff has failed in.
    match runtime_portal::service_fd("collab") {
        Some(fd) => println!("probe: collab service fd = {fd}"),
        None => {
            eprintln!(
                "probe: FAIL no usable collab service fd. QOS_SVC_COLLAB_FD={:?}. \
                 The launcher did not put a live descriptor on that number, so \
                 this app cannot collaborate at all.",
                std::env::var("QOS_SVC_COLLAB_FD").ok(),
            );
            return ExitCode::from(3);
        }
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
        match mode {
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
/// channel. `channel` is the broker's routing key verbatim — the room
/// uuid, or `<room>#record:<id>` for one document's stream.
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
    // A fresh document's `save` is its complete change set, which is
    // what a peer starting from nothing needs in order to apply it.
    let bytes = doc.save();

    if let Err(e) = handle.send_unsigned_incremental(&bytes).await {
        eprintln!("probe: FAIL send_unsigned_incremental: {e}");
        return ExitCode::from(7);
    }
    println!("probe: WROTE {} bytes: {text:?}", bytes.len());
    // The broker's publish is fire-and-forget onto an actor queue, and
    // this process is about to exit. Give the frame a moment to leave
    // rather than racing teardown against it.
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
                // Not a document chunk we can apply — say so and keep
                // waiting rather than reporting a delivery failure for
                // what is really a decode failure.
                eprintln!("probe: received {} bytes that did not load", event.op.len());
                continue;
            }
            match doc.get(&ROOT, FIELD) {
                Ok(Some((value, _))) => return Some(value.to_str().unwrap_or_default().to_string()),
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
