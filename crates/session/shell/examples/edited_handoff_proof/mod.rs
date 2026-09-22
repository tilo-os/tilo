//! Opt-in real typed SDK dispatch; no model and no private app hooks.
mod pixels;
use super::*;
use std::path::Path;
use tilo_actions::InvocationUpdate;
use ui::collaboration::{CollabRequest, CollaborationManager};

const CALLER: &str = "dev.qos.edited-handoff-proof";
const READER: &str = "edited-proof-reader";

// The helper inherits exactly the same SDK descriptors as a launched app.
// Files carry only local test instructions/results, never caller authority.
pub(super) fn helper(root: PathBuf, room: String, denied: bool) -> Result<()> {
    let connection = if denied {
        CollaborationManager::for_agent_route(CALLER, room.parse()?, None, READER)
    } else {
        CollaborationManager::for_route(CALLER, room.parse()?, None, None)
    };
    let mut manager = connection.map_err(|e| format!("SDK connection unavailable: {e:?}"))?;
    for sequence in 0..32_u64 {
        let path = root.join(format!("request-{sequence}.json"));
        let deadline = Instant::now();
        while !path.exists() {
            manager.poll();
            if deadline.elapsed() > Duration::from_secs(90) {
                return Err("helper request timeout".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let request: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
        manager.send(CollabRequest::InvokePublishedAction {
            request_id: sequence,
            peer_id: request["peer"].as_u64().ok_or("missing peer")?,
            resource: request["resource"]
                .as_str()
                .ok_or("missing resource")?
                .into(),
            action: request["action"].as_str().ok_or("missing action")?.into(),
            arguments: request["arguments"]
                .as_object()
                .ok_or("missing arguments")?
                .clone(),
            confirmation: json!({}),
        });
        let deadline = Instant::now();
        let result = loop {
            if deadline.elapsed() > Duration::from_secs(35) {
                return Err("typed invocation timeout".into());
            }
            let mut finished = None;
            for event in manager.poll() {
                if let CollabEvent::PublishedInvocationResult { request_id, result } = event {
                    if request_id != sequence {
                        continue;
                    }
                    match result {
                        Err(error) => finished = Some(json!({"error":error})),
                        Ok((id, update)) if update.is_terminal() => {
                            finished = Some(json!({"id":id,"update":update}))
                        }
                        Ok((id, _)) => {
                            manager.send(CollabRequest::PublishedInvocationStatus {
                                request_id: sequence,
                                id,
                            });
                        }
                    }
                }
            }
            if let Some(result) = finished {
                break result;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let temporary = root.join("response.tmp");
        std::fs::write(&temporary, serde_json::to_vec(&result)?)?;
        std::fs::rename(temporary, root.join(format!("response-{sequence}.json")))?;
    }
    Ok(())
}

struct Caller {
    sequence: u64,
}
impl Caller {
    fn invoke(
        &mut self,
        root: &Path,
        children: &mut Children,
        surface: &PublishedSurface,
        action: &PublishedAction,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let request = json!({"peer":surface.peer_id,"resource":action.resource,"action":action.action,"arguments":arguments});
        let temporary = root.join("request.tmp");
        std::fs::write(&temporary, serde_json::to_vec(&request)?)?;
        std::fs::rename(
            temporary,
            root.join(format!("request-{}.json", self.sequence)),
        )?;
        let response = root.join(format!("response-{}.json", self.sequence));
        self.sequence += 1;
        let deadline = Instant::now();
        while !response.exists() {
            children.live()?;
            if deadline.elapsed() > Duration::from_secs(40) {
                return Err("caller response timeout".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let result = serde_json::from_slice(&std::fs::read(response)?)?;
        emit(json!({"event":"typed_invocation_result","request":request,"result":result}));
        Ok(result)
    }
}
fn succeeded(value: &serde_json::Value) -> Result<tilo_actions::ActionOutput> {
    match serde_json::from_value::<InvocationUpdate>(value["update"].clone())? {
        InvocationUpdate::Succeeded { output, .. } => Ok(output),
        other => Err(format!("invocation did not succeed: {other:?}").into()),
    }
}

pub(super) fn run(root: PathBuf, photos: PathBuf, writer: PathBuf) -> Result<()> {
    // The isolated fixture grants only Read to a delegated, certified agent.
    // The normal Session host still owns admission and invocation policy.
    let state_path = root.join("workspace.json");
    if state_path.exists() {
        return Err("edited proof requires a fresh isolated profile".into());
    }
    let mut tree = shell::shell::state::WorkspaceTree::bootstrap_default();
    let workspace = &mut tree.workspaces[0];
    workspace.grant_capability(shell::shell::state::CapOverride::membership(
        shell::shell::state::Subject::agent(READER),
        workspace.room.to_string(),
        shell::shell::state::Verb::Read,
    ));
    shell::shell::persist::save_tree_to(&tree, &state_path)?;
    let session = Session::start(&root)?;
    let captured = Arc::new(Mutex::new(Vec::new()));
    let received = captured.clone();
    let _subscription = session.broker.subscribe_rooms(
        vec![session.room.clone()],
        Box::new(move |event, route| {
            if route.app_id == WRITER {
                if let CollabEvent::Document { envelope, .. } = event {
                    received
                        .lock()
                        .unwrap()
                        .push((route.clone(), envelope.clone()));
                }
            }
        }),
    );
    let fixture = root.join("source.png");
    image::RgbaImage::from_fn(48, 32, |x, y| {
        image::Rgba([
            (x * 5) as u8,
            (y * 7) as u8,
            ((x * 3 + y * 11) % 256) as u8,
            255,
        ])
    })
    .save(&fixture)?;
    let mut children = Children(Vec::new());
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
        children.0.push(session.launch(id, command)?);
    }
    let app_pids: Vec<_> = children.0.iter().map(Child::id).collect();
    let (photo_surface, rotate) = wait_action(&session, &mut children, PHOTOS, "rotate-right")?;
    let (writer_surface, place) = wait_action(&session, &mut children, WRITER, "place-image")?;
    if photo_surface.peer_id == writer_surface.peer_id {
        return Err("apps share a connection".into());
    }
    if rotate.operation_kind != tilo_actions::OperationKind::Document {
        return Err("rotation is not Document".into());
    }
    let source_hash = rotate
        .resource
        .strip_prefix("photos.image:")
        .ok_or("missing photo content address")?
        .to_owned();
    let source_bytes = qdoc::blobstore::get(&source_hash).ok_or("source unavailable")?;
    if blake3::hash(&source_bytes).to_hex().as_str() != source_hash {
        return Err("source hash mismatch".into());
    }
    std::fs::write(root.join("source-content.png"), &source_bytes)?;
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("--edited-helper").arg(&root).arg(&session.room);
    let log = File::create(root.join("typed-caller.log"))?;
    command
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    children.0.push(session.launch(CALLER, command)?);
    let mut caller = Caller { sequence: 0 };
    let denied_root = root.join("denied");
    std::fs::create_dir(&denied_root)?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--edited-helper")
        .arg(&denied_root)
        .arg(&session.room)
        .arg("--denied");
    let log = File::create(root.join("denied-caller.log"))?;
    command
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    children.0.push(session.launch(CALLER, command)?);
    let mut denied_caller = Caller { sequence: 0 };
    let (denied_surface, denied_export) =
        wait_action(&session, &mut children, PHOTOS, "export-edited-content")?;
    for (surface, action, arguments) in [
        (&denied_surface, &denied_export, json!({})),
        (&writer_surface, &place, json!({"value": source_hash})),
    ] {
        let result =
            denied_caller.invoke(&denied_root, &mut children, surface, action, arguments)?;
        if !result["error"]
            .as_str()
            .is_some_and(|error| error.contains("not allowed to invoke"))
        {
            return Err(
                format!("expected permission denial for {}: {result}", action.action).into(),
            );
        }
        emit(
            json!({"event":"typed_permission_denied", "principal":format!("agent:{READER}"), "action":action.action,"result":result}),
        );
    }
    // Genuine missing bytes through the same production consumer dispatch.
    let missing = "0".repeat(64);
    if qdoc::blobstore::get(&missing).is_some() {
        return Err("missing-byte fixture unexpectedly exists".into());
    }
    let unavailable = caller.invoke(
        &root,
        &mut children,
        &writer_surface,
        &place,
        json!({"value":missing}),
    )?;
    if (unavailable.get("error").is_none() && unavailable["update"]["state"] != "failed")
        || !unavailable
            .to_string()
            .contains("not available on this machine")
    {
        return Err(format!("expected unavailable-content rejection, got: {unavailable}").into());
    }
    emit(json!({"event":"unavailable_consumer_content_rejected","result":unavailable}));
    succeeded(&caller.invoke(&root, &mut children, &photo_surface, &rotate, json!({}))?)?;
    // Completion follows apply; wait for the refreshed action surface (reset
    // is advertised only after the edit exists) before invoking the Effect.
    wait_action(&session, &mut children, PHOTOS, "reset")?;
    let (export_surface, export) =
        wait_action(&session, &mut children, PHOTOS, "export-edited-content")?;
    if export.resource != rotate.resource
        || export.operation_kind != tilo_actions::OperationKind::Effect
        || export.contract.result
            != (tilo_actions::ResultSpec::Resource {
                resource_kind: tilo_actions::CONTENT_RESOURCE_KIND.into(),
            })
    {
        return Err("edited export contract mismatch".into());
    }
    let live_result = caller.invoke(&root, &mut children, &export_surface, &export, json!({}))?;
    let output = succeeded(&live_result)?;
    if output.resources.len() != 1 {
        return Err("producer must return exactly one content resource".into());
    }
    let hash = tilo_actions::content_hash(&output.resources[0]).ok_or("wrong resource kind")?;
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || hash == source_hash
    {
        return Err("noncanonical or original result hash".into());
    }
    let bytes = qdoc::blobstore::get(hash).ok_or("returned bytes unavailable")?;
    if blake3::hash(&bytes).to_hex().as_str() != hash || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("stored PNG/hash mismatch".into());
    }
    let source = image::load_from_memory(&source_bytes)?.into_rgba8();
    let edited = image::load_from_memory(&bytes)?.into_rgba8();
    pixels::verify_clockwise_rgba(
        source.width(),
        source.height(),
        source.as_raw(),
        edited.width(),
        edited.height(),
        edited.as_raw(),
    )?;
    std::fs::write(root.join("edited.png"), &bytes)?;
    std::fs::write(
        root.join("live-producer-result.json"),
        serde_json::to_vec_pretty(&live_result)?,
    )?;
    succeeded(&caller.invoke(
        &root,
        &mut children,
        &writer_surface,
        &place,
        json!({"value":hash}),
    )?)?;
    // Inspect the actual broker-persisted workspace, not a local reduction of
    // the live output. The producer invocation links the two observations.
    let persisted = shell::shell::persist::load_tree_from(&root.join("workspace.json"))?;
    let workspace = persisted
        .workspaces
        .iter()
        .find(|workspace| workspace.room.to_string() == session.room)
        .ok_or("persisted proof workspace missing")?;
    let producer_update: InvocationUpdate = serde_json::from_value(live_result["update"].clone())?;
    let matching: Vec<_> = workspace
        .effect_receipts
        .iter()
        .filter(|receipt| receipt.invocation == producer_update.invocation())
        .collect();
    if matching.len() != 1 {
        return Err("expected exactly one persisted producer receipt".into());
    }
    let receipt = serde_json::to_value(matching[0])?;
    if matching[0].status != tilo_actions::EffectReceiptStatus::Succeeded
        || matching[0].actor != session.actor
        || receipt.to_string().contains(hash)
        || receipt.get("output").is_some()
        || receipt.get("resources").is_some()
    {
        return Err("persisted receipt status/actor/redaction mismatch".into());
    }
    std::fs::write(
        root.join("persisted-producer-receipt.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    emit(json!({"event":"persisted_producer_receipt_verified",
        "invocation":producer_update.invocation(), "workspace":session.room,
        "status":"succeeded", "live_output_hash_absent":true}));
    let deadline = Instant::now();
    while deadline.elapsed() < Duration::from_secs(15) {
        children.live()?;
        let events: Vec<_> = captured
            .lock()
            .map_err(|_| "capture lock poisoned")?
            .iter()
            .filter(|(route, _)| *route == writer_surface.route)
            .map(|(_, op)| op.clone())
            .collect();
        let mut doc = automerge::AutoCommit::new();
        for op in &events {
            collab_types::verify_op(op)?;
            let route = &writer_surface.route;
            match collab_types::validate_document_payload_route(
                &route.app_id,
                &route.cluster_id,
                &route.record_id,
                op,
            )? {
                collab_types::RoutedDocumentPayload::Cleartext(
                    collab_types::DocumentMutation::Snapshot(data),
                ) => {
                    doc.merge(&mut automerge::AutoCommit::load(&data)?)?;
                }
                collab_types::RoutedDocumentPayload::Cleartext(
                    collab_types::DocumentMutation::Incremental(data),
                ) => {
                    doc.load_incremental(&data)?;
                }
                _ => return Err("unexpected encrypted proof document".into()),
            }
        }
        if let Ok(model) = qdoc::model::open(&doc, &automerge::ROOT) {
            let images: Vec<_> = qdoc::model::materialize(&doc, &model)
                .blocks
                .into_iter()
                .filter(|block| block.kind == "image")
                .collect();
            if images.len() > 1 {
                return Err("unexpected extra Writer image blocks".into());
            }
            if let Some(block) = images
                .first()
                .filter(|block| block.attrs.get("blob") == Some(&qdoc::AttrVal::Str(hash.into())))
            {
                if block.attrs.get("width") != Some(&qdoc::AttrVal::Int(i64::from(edited.width())))
                    || block.attrs.get("height")
                        != Some(&qdoc::AttrVal::Int(i64::from(edited.height())))
                {
                    return Err("Writer dimensions mismatch".into());
                }
                std::fs::write(root.join("received.automerge"), doc.save())?;
                std::fs::write(
                    root.join("signed-document-events.json"),
                    serde_json::to_vec_pretty(&events)?,
                )?;
                let evidence = json!({
                    "event": "edited_cross_app_proof_passed",
                    "os": std::env::consts::OS,
                    "runtime_pid": std::process::id(), "app_pids": app_pids,
                    "source_hash": source_hash, "edited_hash": hash,
                    "source_width": source.width(), "source_height": source.height(),
                    "width": edited.width(), "height": edited.height(),
                    "expected_snapshot": {"rotate_quarters": 1, "crop": "identity"},
                    "export_contract": export.contract,
                    "pixel_oracle": "independent clockwise RGBA mapping",
                    "writer_route": {"app": writer_surface.route.app_id,
                        "workspace": writer_surface.route.cluster_id,
                        "record": writer_surface.route.record_id},
                    "image_block": block.id, "signed_document_events": events.len(),
                    "both_apps_alive": true,
                    "dispatch": "SDK InvokePublishedAction typed arguments",
                    "live_result_is_durable_receipt": false,
                    "persisted_receipt_output_redaction_verified": true,
                    "headless_parity_proven": false, "permission_denials_proven": true
                });
                std::fs::write(
                    root.join("edited-proof.json"),
                    serde_json::to_vec_pretty(&evidence)?,
                )?;
                emit(evidence);
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err("Writer signed exact-route document missing edited image".into())
}
