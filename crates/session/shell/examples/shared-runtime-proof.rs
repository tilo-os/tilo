//! Live macOS/Linux proof: two unmodified apps and one production service host.
//! Run with isolated `XDG_CONFIG_HOME`, `TILO_IDENTITY_ROOT`, `XDG_DATA_HOME` and
//! `XDG_CACHE_HOME`. See the proof runner script.
use serde_json::json;
use shell::{
    runtime_host::Session,
    shell::collab_broker_capnp::{PublishedAction, PublishedSurface},
};
use std::{
    error::Error,
    fs::File,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tilo_automerge::ApplyIncoming;
use ui::collaboration::CollabEvent;
mod edited_handoff_proof;
#[path = "runtime_benchmark/mod.rs"]
mod runtime_benchmark;

const PHOTOS: &str = "dev.tilo.photos";
const WRITER: &str = "dev.tilo.word-processor";
type Result<T> = std::result::Result<T, Box<dyn Error>>;

struct Children(Vec<Child>);
impl Drop for Children {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
impl Children {
    fn live(&mut self) -> Result<()> {
        for child in &mut self.0 {
            if let Some(status) = child.try_wait()? {
                return Err(format!("app {} exited: {status}", child.id()).into());
            }
        }
        Ok(())
    }
}

fn emit(value: &serde_json::Value) {
    println!("{value}");
}

/// Invoke through the session's action host, as the fixed session actor
/// confined to the surface's workspace — the path every caller takes.
fn start_action(
    actions: &shell::shell::collab_broker::AppActionBroker,
    actor: &str,
    surface: &PublishedSurface,
    resource: &str,
    action: &str,
    input: Option<&str>,
) -> Result<tilo_actions::InvocationId> {
    use tilo_actions::remote::{
        ActionHost as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
    };
    let invocation = tilo_actions::InvocationId::new();
    let receipt = shell::shell::remote_actions::BrokerActionHost::new(actions.clone()).invoke(
        &Caller::local_person(actor).in_room(&surface.route.cluster_id),
        InvokeRequest {
            on_behalf_of: None,
            invocation,
            target: RemoteActionTarget {
                provider: surface.peer_id.to_string(),
                address: tilo_actions::ActionAddress::new(resource, action),
            },
            argument: input.map_or(InvokeArgument::None, |text| {
                InvokeArgument::Text(text.to_string())
            }),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        },
    );
    // This thread does not drive the broker; waiting here blocks nothing.
    let receipt = receipt
        .wait_timeout(Duration::from_secs(30))
        .map_err(|_| "the action host did not admit the action in time")?
        .ok_or("the action host dropped the request")??;
    if let ReceiptStatus::Rejected { error } = receipt.status {
        return Err(error.into());
    }
    Ok(invocation)
}

/// Observe an invocation until it is terminal.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the shell fixture owns long-lived permits by design and lives for the whole run"
)]
fn wait_terminal(
    actions: &shell::shell::collab_broker::AppActionBroker,
    actor: &str,
    surface: &PublishedSurface,
    invocation: tilo_actions::InvocationId,
    timeout: Duration,
) -> Result<tilo_actions::InvocationUpdate> {
    use tilo_actions::remote::{ActionHost as _, Caller};
    let host = shell::shell::remote_actions::BrokerActionHost::new(actions.clone());
    let caller = Caller::local_person(actor).in_room(&surface.route.cluster_id);
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Some(state) = host.observe(&caller, &invocation)
            && state.is_terminal()
        {
            return Ok(state.update);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err("the invocation did not finish in time".into())
}

fn wait_action(
    session: &Session,
    children: &mut Children,
    app: &str,
    action: &str,
) -> Result<(PublishedSurface, PublishedAction)> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(45) {
        children.live()?;
        for surface in session.broker.published_action_surfaces() {
            if surface.live
                && surface.app_id == app
                && let Some(found) = surface
                    .actions
                    .iter()
                    .find(|entry| entry.action == action)
                    .cloned()
            {
                return Ok((surface, found));
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!("timed out waiting for {app}/{action}").into())
}

#[allow(
    clippy::too_many_lines,
    reason = "the proof is one linear scripted scenario whose emitted events are read in order"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the shell fixture owns long-lived permits by design and lives for the whole run"
)]
fn main() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .try_init();
    let mut args = std::env::args_os().skip(1);
    let first = args.next().ok_or("expected profile path")?;
    if first == "--edited-helper" {
        return edited_handoff_proof::helper(
            &PathBuf::from(args.next().ok_or("expected helper root")?),
            &args.next().ok_or("expected room")?.to_string_lossy(),
            args.next().is_some_and(|arg| arg == "--denied"),
        );
    }
    let root = PathBuf::from(first);
    let photos = PathBuf::from(args.next().ok_or("expected Photos binary")?);
    let writer = PathBuf::from(args.next().ok_or("expected Word Processor binary")?);
    let mode = args.next();
    if mode.as_deref() == Some(std::ffi::OsStr::new("--edited-handoff")) {
        return edited_handoff_proof::run(&root, photos, writer);
    }
    let benchmark = mode.is_some_and(|arg| arg == "--benchmark");
    let runtime_start = Instant::now();
    let session = Session::start(&root)?;
    emit(&json!({"event":"runtime_ready", "elapsed_ns":runtime_start.elapsed().as_nanos()}));
    emit(
        &json!({"event":"runtime_started", "pid":std::process::id(), "os":std::env::consts::OS, "workspace":session.room, "socket":session.broker.capnp_socket(), "implementation":"shell::shell::collab_broker::spawn_native"}),
    );
    let captured = Arc::new(Mutex::new(Vec::new()));
    let received = captured.clone();
    let subscription = session.broker.subscribe_rooms(
        vec![session.room.clone()],
        Box::new(move |event, route| {
            if route.app_id == WRITER
                && let runtime_collab::event::CollabEvent::Document { envelope, .. } = event
                && let Ok(mut events) = received.lock()
            {
                events.push(envelope.clone());
            }
        }),
    );
    let fixture = root.join("source.png");
    let (width, height) = if benchmark { (1280, 720) } else { (48, 32) };
    #[expect(
        clippy::cast_possible_truncation,
        reason = "both channels are reduced mod 256 before the cast"
    )]
    image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 120])
    })
    .save(&fixture)?;
    let mut children = Children(Vec::new());
    let mut launched = Vec::new();
    for (id, executable, extra) in [
        (
            PHOTOS,
            photos,
            vec!["--open".into(), fixture.into_os_string()],
        ),
        (WRITER, writer, vec![]),
    ] {
        let log = File::create(root.join(format!("{id}.log")))?;
        let mut command = Command::new(executable);
        command
            .args(extra)
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        if benchmark {
            command
                .env("TILO_PROFILE_OUT", root.join(format!("{id}.frames.jsonl")))
                .env("TILO_UI_PRESENT", "1")
                .env("TILO_PROFILE_WINDOW_FRONT", "1")
                .env("TILO_PROFILE_RESOURCES", "1")
                .env(
                    "TILO_PROFILE_WINDOW_POSITION",
                    if id == PHOTOS { "20,60" } else { "840,60" },
                );
        }
        launched.push(Instant::now());
        let child = session.launch(id, command)?;
        emit(
            &json!({"event":"app_started", "app":id, "pid":child.id(), "runtime_pid":std::process::id(), "workspace":session.room}),
        );
        children.0.push(child);
    }
    let [photos_launched, writer_launched] = launched.as_slice() else {
        return Err("expected exactly two launched apps".into());
    };
    let (photo_surface, photo_action) =
        wait_action(&session, &mut children, PHOTOS, "rotate-right")?;
    emit(
        &json!({"event":"action_surface_ready", "app":PHOTOS, "elapsed_ns":photos_launched.elapsed().as_nanos(), "poll_resolution_ms":50}),
    );
    let (writer_surface, writer_action) =
        wait_action(&session, &mut children, WRITER, "place-image")?;
    emit(
        &json!({"event":"action_surface_ready", "app":WRITER, "elapsed_ns":writer_launched.elapsed().as_nanos(), "poll_resolution_ms":50, "observed_after_photos":true}),
    );
    if photo_surface.peer_id == writer_surface.peer_id {
        return Err("apps unexpectedly share one connection".into());
    }
    let hash = photo_action
        .resource
        .strip_prefix("photos.image:")
        .ok_or("Photos did not publish a content address")?;
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid photo content hash".into());
    }
    let bytes = session
        .read_media(PHOTOS, hash)?
        .ok_or("published photo bytes unavailable")?;
    if blake3::hash(&bytes).to_hex().as_str() != hash {
        return Err("content hash mismatch".into());
    }
    emit(
        &json!({"event":"source_discovered", "app":PHOTOS, "peer":photo_surface.peer_id, "resource":photo_action.resource, "content_hash":hash, "bytes":bytes.len()}),
    );
    let actions = session
        .broker
        .app_action_broker()
        .ok_or("missing action broker")?;
    // Prove the same broker denies a caller with no workspace authority.
    if start_action(
        &actions,
        "ungranted-proof-caller",
        &writer_surface,
        &writer_action.resource,
        "place-image",
        Some(hash),
    )
    .is_ok()
    {
        return Err("ungranted caller unexpectedly invoked the app".into());
    }
    emit(&json!({"event":"unauthorized_invocation_denied"}));
    let invocation = start_action(
        &actions,
        &session.actor,
        &writer_surface,
        &writer_action.resource,
        "place-image",
        Some(hash),
    )?;
    emit(
        &json!({"event":"handoff_started", "invocation":invocation.0.to_string(), "source_peer":photo_surface.peer_id, "target_peer":writer_surface.peer_id, "target_app":WRITER, "resource":writer_action.resource, "action":"place-image", "content_hash":hash}),
    );
    let update = wait_terminal(
        &actions,
        &session.actor,
        &writer_surface,
        invocation,
        Duration::from_secs(30),
    )?;
    emit(
        &json!({"event":"invocation_completed", "invocation":invocation.0.to_string(), "status":update}),
    );
    if !matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }) {
        return Err("insertion did not succeed".into());
    }
    // Reconstruct the app's signed document stream, not a locally fabricated
    // result or a success string, and inspect the actual inserted image block.
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(15) {
        children.live()?;
        let events = captured
            .lock()
            .map_err(|_| "capture lock poisoned")?
            .clone();
        let mut document = automerge::AutoCommit::new();
        for envelope in &events {
            collab_types::verify_op(envelope)?;
            match collab_types::DocumentMutation::decode(&envelope.payload)? {
                collab_types::DocumentMutation::Snapshot(data) => {
                    let mut snapshot = automerge::AutoCommit::load(&data)?;
                    document.merge_incoming(&mut snapshot)?;
                }
                collab_types::DocumentMutation::Incremental(data) => {
                    document.load_incoming(&data)?;
                }
            }
        }
        if let Ok(model) = tilo_doc::model::open(&document, &automerge::ROOT)
            && let Some(block) = tilo_doc::model::materialize(&document, &model)
                .blocks
                .into_iter()
                .find(|block| {
                    block.kind == "image"
                        && block.attrs.get("blob") == Some(&tilo_doc::AttrVal::Str(hash.into()))
                })
        {
            std::fs::write(root.join("received.automerge"), document.save())?;
            std::fs::write(
                root.join("signed-document-events.json"),
                serde_json::to_vec_pretty(&events)?,
            )?;
            emit(
                &json!({"event":"cross_app_proof_passed", "runtime_pid":std::process::id(), "app_pids":children.0.iter().map(Child::id).collect::<Vec<_>>(), "workspace":session.room, "source_app":PHOTOS, "target_app":WRITER, "content_hash":hash, "image_block":block.id, "signed_document_events":events.len(), "both_apps_alive":true}),
            );
            if benchmark {
                drop(subscription);
                runtime_benchmark::run(&session, &mut children, &root, hash)?;
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("app reported success but its document stream did not contain the image".into())
}
