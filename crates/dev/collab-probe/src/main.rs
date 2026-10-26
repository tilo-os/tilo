//! `tilo-collab-probe` is a packaged app that checks collaboration through the
//! sandbox service descriptor supplied by the launcher (`TILO_SVC_COLLAB_FD`).
//! It does not connect to a socket path directly.
//!
//! ```text
//! tilo-collab-probe write:<text>      # author an edit, publish it
//! tilo-collab-probe read:<seconds>    # wait for one, print it
//! tilo-collab-probe routes:<app>,...  # what this launch may do on each app's route
//! ```
//!
//! The room comes from `TILO_WORKSPACE_ID`. `write` publishes a signed
//! Automerge change through the broker, and `read` loads the change into its
//! own document. Exit status is zero only when the requested operation
//! succeeds; failures identify the step that failed.
//!
//! `routes` asks, as the attested launch of whatever package the probe was
//! launched as (`TILO_PACKAGE_ID`), to join each named app's route and to
//! write on it, and prints one `probe: ROUTE` line per app with what the
//! broker and the identity service answered. It changes nothing it may not:
//! the write it offers another app's route is not a valid operation, so only
//! a broker that admits writes there would even look at it. A package whose
//! entrypoint is the probe and that the shell launches without arguments
//! reads the instruction from `probe-routes` in its payload
//! (`TILO_APP_MODULE`): how `docs/tilolang-e2e.md` checks a tilolang app's
//! route access as that app.

use std::process::ExitCode;
use tilo_automerge::ApplyIncoming;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue};
use collab_types::{CommitNamed, DocumentMutation, Ed25519PublicKey, OpKind, Signature, SignedOp};

/// Root key the probe reads and writes. One flat string is enough because the
/// probe proves delivery, not a document model.
const FIELD: &str = "text";
const APP_ID: &str = "so.tilo.collab-probe";
const RECORD_ID: &str = "";
/// The record kind the probe's one field edit is named as.
const RECORD_KIND: &str = "probe";

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  \
         tilo-collab-probe write:<text>\n  \
         tilo-collab-probe read:<timeout-seconds>\n\
         The room comes from TILO_WORKSPACE_ID."
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

    // The room is the collaboration address; without one the app cannot join.
    let channel = match std::env::var("TILO_WORKSPACE_ID") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            eprintln!(
                "probe: FAIL TILO_WORKSPACE_ID is unset, so this app has no \
                 room to collaborate in."
            );
            return ExitCode::from(2);
        }
    };
    println!("probe: room = {channel}");

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

fn certified_join(
    workspace_id: &str,
) -> Result<(Vec<u8>, runtime_identity::SigningIdentity), String> {
    let identity = runtime_identity::load()
        .map_err(|error| format!("could not reach the workspace identity service: {error}"))?;
    let certificate = identity
        .device_certificate()
        .ok_or_else(|| "workspace identity has no certified device".to_string())?;
    let proof = identity
        .sign_collaboration_join(workspace_id)
        .map_err(|error| format!("collaboration join was denied: {error}"))?;
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id: workspace_id.to_string(),
        device_certificate: certificate.to_vec(),
        proof: proof.to_vec(),
    }
    .encode()
    .map_err(|error| format!("could not encode the certified join: {error}"))?;
    Ok((presentation, identity))
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
) -> Result<
    (
        runtime_collab::ChannelHandle,
        runtime_identity::SigningIdentity,
    ),
    String,
> {
    let (presentation, identity) = certified_join(workspace_id)?;
    let broker = runtime_collab::try_connect()
        .map_err(|error| format!("could not reach the broker over the service fd: {error}"))?;
    println!("probe: connected to the broker");
    let channel = channel_key(workspace_id);
    let handle = broker
        .join_channel(&channel, &presentation)
        .await
        .map_err(|error| format!("join_channel({channel}) failed: {error}"))?;
    println!("probe: joined channel {channel}");
    Ok((handle, identity))
}

fn signed_incremental(
    identity: &runtime_identity::SigningIdentity,
    workspace_id: &str,
    bytes: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let payload = DocumentMutation::Incremental(bytes)
        .encode()
        .map_err(|error| format!("typed mutation would not encode: {error}"))?;
    let now = std::time::SystemTime::now();
    let seq = now
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
        });
    let mut op = SignedOp::new_unsigned(
        payload,
        collab_types::document_artifact_id(APP_ID, workspace_id, RECORD_ID),
        Ed25519PublicKey::from_bytes(identity.signing_public_key()),
        collab_types::document_attestation_id(APP_ID, workspace_id, RECORD_ID),
        OpKind::Change,
        now,
        seq,
    );
    let unsigned = postcard::to_stdvec(&op)
        .map_err(|error| format!("unsigned operation would not encode: {error}"))?;
    let signature = identity
        .sign_collaboration_op(workspace_id, APP_ID, RECORD_ID, &unsigned)
        .map_err(|error| format!("workspace policy denied the operation: {error}"))?;
    op.signature = Signature::from_bytes(signature);
    postcard::to_stdvec(&op).map_err(|error| format!("signed operation would not encode: {error}"))
}

fn apply_signed_document(
    doc: &mut AutoCommit,
    workspace_id: &str,
    bytes: &[u8],
) -> Result<Option<String>, String> {
    let envelope: SignedOp =
        postcard::from_bytes(bytes).map_err(|error| format!("invalid signed envelope: {error}"))?;
    collab_types::verify_op(&envelope)
        .map_err(|error| format!("invalid operation signature: {error}"))?;
    if envelope.artifact != collab_types::document_artifact_id(APP_ID, workspace_id, RECORD_ID)
        || envelope.attestation_id
            != collab_types::document_attestation_id(APP_ID, workspace_id, RECORD_ID)
    {
        return Err("signed operation belongs to another document route".to_string());
    }
    let mutation = DocumentMutation::decode(&envelope.payload)
        .map_err(|error| format!("untyped document operation: {error}"))?;
    match mutation {
        DocumentMutation::Incremental(bytes) => doc
            .load_incoming(&bytes)
            .map(|_| ())
            .map_err(|error| format!("incremental document bytes did not load: {error}"))?,
        DocumentMutation::Snapshot(bytes) => {
            let mut incoming = AutoCommit::load(&bytes)
                .map_err(|error| format!("document snapshot did not load: {error}"))?;
            doc.merge_incoming(&mut incoming)
                .map_err(|error| format!("document snapshot did not merge: {error}"))?;
        }
    }
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
    // Every change names the action it ran: here, an edit of the text.
    doc.commit_edit(RECORD_KIND, FIELD);
    let bytes = doc.save_incremental();
    let signed = match signed_incremental(&identity, workspace_id, bytes) {
        Ok(signed) => signed,
        Err(error) => {
            eprintln!("probe: FAIL {error}");
            return ExitCode::from(7);
        }
    };

    if let Err(e) = handle.send(&signed).await {
        eprintln!("probe: FAIL sending signed operation: {e}");
        return ExitCode::from(7);
    }
    println!("probe: WROTE {} signed bytes: {text:?}", signed.len());
    // The broker's publish is fire-and-forget onto an actor queue and
    // this process is about to exit — give the frame a moment to leave rather than racing teardown.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    ExitCode::SUCCESS
}

#[allow(
    clippy::future_not_send,
    reason = "only ever driven inside the single-threaded `LocalSet` this binary sets up; \
              never spawned across threads"
)]
async fn read(workspace_id: &str, timeout_secs: u64) -> ExitCode {
    let (handle, _identity) = match join(workspace_id).await {
        Ok(joined) => joined,
        Err(error) => {
            eprintln!("probe: FAIL {error}");
            return ExitCode::from(5);
        }
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
            match apply_signed_document(&mut doc, workspace_id, &event.op) {
                Ok(Some(text)) => return Some(text),
                Ok(None) => {}
                Err(error) => {
                    eprintln!("probe: received an operation that was not applicable: {error}");
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
    let (presentation, identity) = match certified_join(workspace_id) {
        Ok(joined) => joined,
        Err(error) => {
            eprintln!("probe: FAIL {error}");
            return ExitCode::from(5);
        }
    };
    let broker = match runtime_collab::try_connect() {
        Ok(broker) => broker,
        Err(error) => {
            eprintln!("probe: FAIL could not reach the broker over the service fd: {error}");
            return ExitCode::from(5);
        }
    };
    for app in apps {
        let channel = format!("{workspace_id}#app:{app}");
        let handle = match broker.join_channel(&channel, &presentation).await {
            Ok(handle) => handle,
            Err(error) => {
                println!("probe: ROUTE {app} JOIN-REFUSED {error}");
                continue;
            }
        };
        // The identity service lends the device signature only to changes of
        // the launch's own route.
        let signing = match identity.sign_collaboration_op(workspace_id, app, "", b"probe") {
            Ok(_) => "SIGN-LENT".to_owned(),
            Err(error) => format!("SIGN-REFUSED {error}"),
        };
        let write = match handle.send(b"probe: not an operation").await {
            Ok(()) => "WRITE-ADMITTED".to_owned(),
            Err(error) => format!("WRITE-REFUSED {error}"),
        };
        println!("probe: ROUTE {app} JOINED; {write}; {signing}");
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed_change(workspace_id: &str, text: &str) -> Vec<u8> {
        let mut doc = AutoCommit::new();
        doc.put(&ROOT, FIELD, text).unwrap();
        doc.commit_edit(RECORD_KIND, FIELD);
        let payload = DocumentMutation::Incremental(doc.save_incremental())
            .encode()
            .unwrap();
        let (key, author) = collab_types::generate_keypair();
        let mut op = SignedOp::new_unsigned(
            payload,
            collab_types::document_artifact_id(APP_ID, workspace_id, RECORD_ID),
            author,
            collab_types::document_attestation_id(APP_ID, workspace_id, RECORD_ID),
            OpKind::Change,
            std::time::SystemTime::now(),
            1,
        );
        collab_types::sign_op(&mut op, &key).unwrap();
        postcard::to_stdvec(&op).unwrap()
    }

    /// A typed, signed, incremental change applies; a valid operation for another route is rejected.
    #[test]
    fn the_probe_applies_only_signed_changes_for_its_route() {
        // Phase: applies_a_typed_signed_incremental_change.
        {
            let mut doc = AutoCommit::new();
            let text = apply_signed_document(
                &mut doc,
                "workspace-a",
                &signed_change("workspace-a", "hello"),
            )
            .unwrap();
            assert_eq!(text.as_deref(), Some("hello"));
        }

        // Phase: rejects_a_valid_operation_for_another_route.
        {
            let mut doc = AutoCommit::new();
            let error = apply_signed_document(
                &mut doc,
                "workspace-b",
                &signed_change("workspace-a", "hello"),
            )
            .unwrap_err();
            assert!(error.contains("another document route"));
        }
    }
}
