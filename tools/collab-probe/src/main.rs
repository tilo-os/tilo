//! `tilo-collab-probe` is a packaged app that checks collaboration through the sandbox service
//! descriptor the launcher supplies (`TILO_SVC_COLLAB_FD`), never a socket path.
//!
//! ```text
//! tilo-collab-probe write:<text>      # author an edit, write it
//! tilo-collab-probe read:<seconds>    # wait for one, print it
//! tilo-collab-probe routes:<app>,...  # what this launch may do on each app's route
//! ```
//!
//! The workspace comes from `TILO_WORKSPACE_ID`. `write` hands the device's store an Automerge
//! change of the probe's record, and `read` asks the store for the record's changes, as it opens and
//! each time the store says the record changed, loading them into its own document. Exit status is
//! zero only when the requested operation succeeds.
//!
//! `routes` asks, as the attested launch of the package the probe was launched as
//! (`TILO_PACKAGE_ID`), to join each named app's route and to write on it, and prints one
//! `probe: ROUTE` line per app with what the broker and the store answered. The write it offers
//! another app's route is no valid change, so only a store that admits writes there would look at it. A package whose entrypoint is the probe and that the shell launches without
//! arguments reads the instruction from `probe-routes` in its payload (`TILO_APP_MODULE`).

use collab_types::store::wire::{Bytes, Command, Reply};
use std::process::ExitCode;
use std::sync::Arc;
use tilo_automerge::ApplyIncoming;
use tilo_identity_client::BrokerSigningIdentity;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue};
use collab_types::CommitNamed;

/// The key the probe reads and writes: one flat string is enough, since the probe proves delivery
/// and not a document model.
const FIELD: &str = "text";
const APP_ID: &str = "so.tilo.collab-probe";
/// The record kind the probe's one field edit is named as.
const RECORD_KIND: &str = "probe";

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  \
         tilo-collab-probe write:<text>\n  \
         tilo-collab-probe read:<timeout-seconds>\n\
         The workspace comes from TILO_WORKSPACE_ID."
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty()
        && let Some(routes) = std::env::var_os("TILO_APP_MODULE")
            .map(|payload| std::path::PathBuf::from(payload).join("probe-routes"))
            .and_then(|file| std::fs::read_to_string(file).ok())
    {
        args.push(format!(
            "routes:{}",
            routes.split_whitespace().collect::<Vec<_>>().join(",")
        ));
    }
    let Some(instruction) = args.first() else {
        return usage();
    };
    let Some((mode, arg)) = instruction.split_once(':') else {
        return usage();
    };
    let (mode, arg) = (mode.to_string(), arg.to_string());

    // The workspace is the collaboration address; without one the app cannot join.
    let channel = match std::env::var("TILO_WORKSPACE_ID") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            eprintln!(
                "probe: FAIL TILO_WORKSPACE_ID is unset, so this app has no \
                 workspace to collaborate in."
            );
            return ExitCode::from(2);
        }
    };
    println!("probe: workspace = {channel}");

    // Report the descriptor before using it. `service_fd` returns None when
    // the variable is absent or names a closed descriptor.
    if let Some(fd) = runtime_portal::service_fd("collab") {
        println!("probe: collab service fd = {fd}");
    } else {
        eprintln!(
            "probe: FAIL no usable collab service fd. TILO_SVC_COLLAB_FD={:?}. \
             The launcher did not put a live descriptor on that number, so \
             this app cannot collaborate at all.",
            std::env::var("TILO_SVC_COLLAB_FD").ok(),
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
            "routes" => routes(&channel, arg.split(',').filter(|app| !app.is_empty())).await,
            _ => usage(),
        }
    })
}

fn channel_key(workspace_id: &str) -> String {
    format!("{workspace_id}#app:{APP_ID}")
}

fn workspace_identity() -> Result<Arc<BrokerSigningIdentity>, String> {
    tilo_identity_client::load()
        .map_err(|error| format!("could not reach the workspace identity service: {error}"))
}

/// Dial the broker over the launcher-supplied descriptor and join the probe's
/// app-scoped channel with the launcher's certified workspace identity.
#[allow(
    clippy::future_not_send,
    reason = "only ever driven inside the single-threaded `LocalSet` this binary sets up; \
              never spawned across threads"
)]
async fn join(
    workspace_id: &str,
) -> Result<(runtime_collab::RecordHandle, Arc<BrokerSigningIdentity>), String> {
    let identity = workspace_identity()?;
    let broker = runtime_collab::try_connect()
        .map_err(|error| format!("could not reach the broker over the service fd: {error}"))?;
    println!("probe: connected to the broker");
    let channel = channel_key(workspace_id);
    let handle = broker.record(&channel, "");
    handle
        .sealed()
        .await
        .map_err(|error| format!("opening {channel} failed: {error}"))?;
    println!("probe: joined channel {channel}");
    Ok((handle, identity))
}

/// Ask the device's store `command` about the probe's record, and read its answer.
#[allow(
    clippy::future_not_send,
    reason = "only ever driven inside the single-threaded `LocalSet` this binary sets up; \
              never spawned across threads"
)]
async fn ask(handle: &runtime_collab::RecordHandle, command: &Command) -> Result<Reply, String> {
    let bytes = serde_json::to_vec(command).map_err(|error| error.to_string())?;
    let reply = handle
        .resource_command(&bytes)
        .await
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&reply)
        .map_err(|error| format!("the store's answer does not read: {error}"))
}

/// The record's changes past `after`, joined from every page of one answer, and the heads they
/// bring the probe to.
#[allow(
    clippy::future_not_send,
    reason = "only ever driven inside the single-threaded `LocalSet` this binary sets up; \
              never spawned across threads"
)]
async fn changes_after(
    handle: &runtime_collab::RecordHandle,
    after: &[String],
) -> Result<(Vec<u8>, Vec<String>), String> {
    'answer: loop {
        let (mut changes, mut first, mut from) = (Vec::new(), None, 0);
        loop {
            let command = Command::RecordChanges {
                after: after.to_vec(),
                from,
                record: None,
            };
            let Reply::RecordChanges {
                changes: page,
                heads,
                next,
                ..
            } = ask(handle, &command).await?
            else {
                return Err("the store answered a read with no changes".to_owned());
            };
            match &first {
                // A newer answer began: read it from its start.
                Some(first) if *first != heads => continue 'answer,
                Some(_) => {}
                None => first = Some(heads),
            }
            changes.extend(page.0);
            match next {
                Some(next) => from = next,
                None => return Ok((changes, first.unwrap_or_default())),
            }
        }
    }
}

/// Load `changes` into `doc` and read the probe's field.
fn apply_changes(doc: &mut AutoCommit, changes: &[u8]) -> Result<Option<String>, String> {
    doc.load_incoming(changes)
        .map_err(|error| format!("the record's changes did not load: {error}"))?;
    doc.get(&ROOT, FIELD)
        .map(|value| value.map(|(value, _)| value.as_str().unwrap_or_default().to_string()))
        .map_err(|error| format!("document field could not be read: {error}"))
}

#[allow(
    clippy::future_not_send,
    reason = "only ever driven inside the single-threaded `LocalSet` this binary sets up; \
              never spawned across threads"
)]
async fn write(workspace_id: &str, text: &str) -> ExitCode {
    let (handle, identity) = match join(workspace_id).await {
        Ok(joined) => joined,
        Err(error) => {
            eprintln!("probe: FAIL {error}");
            return ExitCode::from(5);
        }
    };

    let mut doc = AutoCommit::new();
    if let Err(e) = doc.put(&ROOT, FIELD, ScalarValue::Str(text.into())) {
        eprintln!("probe: FAIL building the document: {e}");
        return ExitCode::from(6);
    }
    // Every change names the action it ran and the person whose certified
    // device's store signs it: here, an edit of the text.
    doc.commit_edit(RECORD_KIND, FIELD, &identity.metadata().group_public_key());
    let command = Command::RecordChange {
        changes: Bytes(doc.save_after(&[])),
        more: false,
        commit: None,
        record: None,
    };
    match ask(&handle, &command).await {
        Ok(Reply::Written) => {}
        Ok(other) => {
            eprintln!("probe: FAIL the store answered {other:?}");
            return ExitCode::from(7);
        }
        Err(error) => {
            eprintln!("probe: FAIL writing the change: {error}");
            return ExitCode::from(7);
        }
    }
    println!("probe: WROTE {text:?}");
    ExitCode::SUCCESS
}

#[allow(
    clippy::future_not_send,
    reason = "only ever driven inside the single-threaded `LocalSet` this binary sets up; \
              never spawned across threads"
)]
async fn read(workspace_id: &str, timeout_secs: u64) -> ExitCode {
    let (handle, _) = match join(workspace_id).await {
        Ok(joined) => joined,
        Err(error) => {
            eprintln!("probe: FAIL {error}");
            return ExitCode::from(5);
        }
    };
    let notices = match handle.subscribe_context().await {
        Ok(notices) => notices,
        Err(e) => {
            eprintln!("probe: FAIL subscribe: {e}");
            return ExitCode::from(8);
        }
    };
    println!("probe: subscribed; waiting up to {timeout_secs}s");

    let deadline = std::time::Duration::from_secs(timeout_secs);
    let mut doc = AutoCommit::new();
    let found = tokio::time::timeout(deadline, async {
        let mut after = Vec::new();
        loop {
            // What the store holds past what this probe has, now and after each change.
            match changes_after(&handle, &after).await {
                Ok((changes, heads)) => {
                    after = heads;
                    match apply_changes(&mut doc, &changes) {
                        Ok(Some(text)) => return Some(text),
                        Ok(None) => {}
                        Err(error) => {
                            eprintln!("probe: the record's changes did not apply: {error}");
                        }
                    }
                }
                Err(error) => eprintln!("probe: the record could not be read: {error}"),
            }
            loop {
                match notices.read().await {
                    Ok(context) if context.record_changed => break,
                    Ok(_) => {}
                    Err(_) => return None,
                }
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
            eprintln!("probe: FAIL the record's notices closed before anything arrived");
            ExitCode::from(9)
        }
        Err(_) => {
            eprintln!(
                "probe: FAIL nothing arrived in {timeout_secs}s. Either no peer \
                 wrote, or what they wrote never reached this device."
            );
            ExitCode::from(10)
        }
    }
}

/// What the attested launch this probe runs as may do on each of `apps`'
/// routes in `workspace_id`: one `probe: ROUTE <app> ...` line per app.
#[allow(
    clippy::future_not_send,
    reason = "only ever driven inside the single-threaded `LocalSet` this binary sets up; \
              never spawned across threads"
)]
async fn routes<'a>(workspace_id: &str, apps: impl Iterator<Item = &'a str>) -> ExitCode {
    let launched_as = std::env::var("TILO_PACKAGE_ID").unwrap_or_default();
    println!("probe: launched as {launched_as:?}");
    let broker = match runtime_collab::try_connect() {
        Ok(broker) => broker,
        Err(error) => {
            eprintln!("probe: FAIL could not reach the broker over the service fd: {error}");
            return ExitCode::from(5);
        }
    };
    for app in apps {
        let channel = format!("{workspace_id}#app:{app}");
        let handle = broker.record(&channel, "");
        if let Err(error) = handle.sealed().await {
            println!("probe: ROUTE {app} JOIN-REFUSED {error}");
            continue;
        }
        // Bytes that are no change: what the store answers is whether this launch may change
        // that route's record at all, and it never takes them.
        let command = Command::RecordChange {
            changes: Bytes(b"probe".to_vec()),
            more: false,
            commit: None,
            record: None,
        };
        let write = match ask(&handle, &command).await {
            Ok(Reply::Written) => "WRITE-ADMITTED".to_owned(),
            Ok(other) => format!("WRITE-ANSWERED {other:?}"),
            Err(error) => format!("WRITE-REFUSED {error}"),
        };
        println!("probe: ROUTE {app} JOINED; {write}");
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The record's changes, as the store joins them, bring the probe the text.
    #[test]
    fn the_probe_reads_the_text_from_the_records_changes() {
        let mut author = AutoCommit::new();
        author.put(&ROOT, FIELD, "hello").unwrap();
        author.commit_edit(RECORD_KIND, FIELD, &[1; 32]);
        let mut doc = AutoCommit::new();
        assert_eq!(apply_changes(&mut doc, &[]).unwrap(), None);
        let changes = author.save_after(&[]);
        assert_eq!(
            apply_changes(&mut doc, &changes).unwrap().as_deref(),
            Some("hello")
        );
        assert!(apply_changes(&mut doc, b"probe").is_err());
    }
}
