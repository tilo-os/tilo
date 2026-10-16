//! Test-only fixture, signed-event observer and independent evidence inspector.
//! Never publishes an action directory or sends a document/action mutation.
use std::{
    error::Error,
    io::Write,
    path::Path,
    time::{Duration, Instant},
};
use tilo_automerge::ApplyIncoming;

const WRITER: &str = "dev.tilo.word-processor";
use serde_json::json;

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().collect();
    let result = match args.as_slice() {
        [_, cmd, out] if cmd == "fixture" => {
            #[expect(
                clippy::arithmetic_side_effects,
                clippy::cast_possible_truncation,
                reason = "x < 48 and y < 32, so x * 5, y * 7 and x * 13 + y * 17 cannot overflow u32; the first two fit in u8 and the third is reduced mod 256"
            )]
            let image = image::RgbaImage::from_fn(48, 32, |x, y| {
                image::Rgba([(x * 5) as u8, (y * 7) as u8, ((x * 13 + y * 17) % 256) as u8, 255])
            });
            image.save(out)?;
            json!({"width":48,"height":32,"hash":tilo_content::hash_bytes(&std::fs::read(out)?)})
        }
        [_, cmd, profile, source_path, resource, output] if cmd == "image" => {
            let hash = tilo_actions::content_hash(resource).ok_or("not canonical content resource")?;
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("not a BLAKE3 address".into());
            }
            // `read` verifies the bytes against the address that named them.
            let bytes = tilo_content::ContentStore::in_dir(Path::new(profile).join("blobs"))
                .read(hash)
                .ok_or("blob unavailable or hash mismatch")?;
            let source = image::open(source_path)?.to_rgba8();
            let edited = image::load_from_memory(&bytes)?.to_rgba8();
            if source.dimensions() != (48, 32) || edited.dimensions() != (32, 32) {
                return Err("unexpected source/result dimensions".into());
            }
            for y in 0..32 { for (x, source_x) in (0..32).zip(8..40) {
                if edited.get_pixel(x,y) != source.get_pixel(source_x,y) {
                    return Err(format!("centered crop pixel mismatch at {x},{y}").into());
                }
            }}
            std::fs::write(output, &bytes)?;
            json!({"hash":hash,"width":32,"height":32,"pixels_compared":1024,"oracle":"source(x+8,y), all RGBA channels"})
        }
        [_, cmd, root] if cmd == "observe" => {
            observe(Path::new(root))?;
            return Ok(());
        }
        [_, cmd, observer_dir, writer] if cmd == "wait-ready-exec" => {
            use std::os::unix::process::CommandExt;
            let room = std::env::var("TILO_WORKSPACE_ID")?;
            wait_ready(Path::new(observer_dir), &room, Duration::from_secs(20))?;
            return Err(std::process::Command::new(writer).exec().into());
        }
        [_, cmd, root, blob, automerge_out, json_out] if cmd == "document" => {
            let root = Path::new(root);
            let workspace: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("profile/workspace.json"))?)?;
            let workspace_id = workspace.pointer("/workspaces/0/room").and_then(serde_json::Value::as_str).ok_or("missing workspace room")?;
            let events: Vec<collab_types::SignedOp> = std::fs::read_to_string(root.join("signed-document-events.jsonl"))?
                .lines().map(serde_json::from_str).collect::<Result<_, _>>()?;
            let mut doc = reconstruct(&events, workspace_id)?;
            let model = tilo_doc::model::open(&doc, &automerge::ROOT)?;
            let materialized = tilo_doc::model::materialize(&doc, &model);
            let images: Vec<_> = materialized.blocks.iter().filter(|b| b.kind == "image").collect();
            let [block] = images.as_slice() else { return Err("expected exactly one reconstructed image block".into()); };
            for (name, expected) in [("blob", tilo_doc::AttrVal::Str(blob.clone())), ("width",tilo_doc::AttrVal::Int(32)), ("height",tilo_doc::AttrVal::Int(32))] {
                if block.attrs.get(name) != Some(&expected) { return Err(format!("reconstructed {name} mismatch").into()); }
            }
            std::fs::write(automerge_out, doc.save())?;
            let evidence = json!({"app":WRITER,"record":"","room":workspace_id,"image_block":block.id,"blob":blob,"width":32,"height":32,"signed_envelopes":events.len(),"verification":"independent signature and exact-route validation, Automerge reconstruction"});
            std::fs::write(json_out, serde_json::to_vec_pretty(&evidence)?)?;
            evidence
        }
        _ => return Err("usage: fixture PATH | image PROFILE SOURCE CONTENT_RESOURCE OUTPUT | observe ROOT | wait-ready-exec ROOT WRITER | document ROOT HASH AUTOMERGE JSON".into()),
    };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn wait_ready(root: &Path, workspace_id: &str, timeout: Duration) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or("observer readiness timeout overflows")?;
    loop {
        if let Ok(bytes) = std::fs::read(root.join("observer-ready.json")) {
            let ready: serde_json::Value = serde_json::from_slice(&bytes)?;
            if ready.get("room").and_then(serde_json::Value::as_str) != Some(workspace_id)
                || ready.get("app").and_then(serde_json::Value::as_str) != Some(WRITER)
                || ready.get("subscribed") != Some(&serde_json::Value::Bool(true))
            {
                return Err("observer readiness route mismatch".into());
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("observer readiness timeout".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn reconstruct(
    events: &[collab_types::SignedOp],
    room: &str,
) -> Result<automerge::AutoCommit, Box<dyn Error>> {
    if events.is_empty() {
        return Err("no signed Writer document events".into());
    }
    let mut doc = automerge::AutoCommit::new();
    for event in events {
        collab_types::verify_op(event)?;
        match collab_types::validate_document_payload_route(WRITER, room, "", event)? {
            collab_types::RoutedDocumentPayload::Cleartext(
                collab_types::DocumentMutation::Snapshot(data),
            ) => {
                doc.merge_incoming(&mut automerge::AutoCommit::load(&data)?)?;
            }
            collab_types::RoutedDocumentPayload::Cleartext(
                collab_types::DocumentMutation::Incremental(data),
            ) => {
                doc.load_incoming(&data)?;
            }
            _ => return Err("unexpected encrypted Writer evidence".into()),
        }
    }
    Ok(doc)
}

fn observe(root: &Path) -> Result<(), Box<dyn Error>> {
    let workspace_id = std::env::var("TILO_WORKSPACE_ID")?;
    let mut manager = ui::collaboration::CollaborationManager::for_route(
        WRITER,
        uuid::Uuid::parse_str(&workspace_id)?,
        None,
        Some("MCP proof observer".into()),
    )?;
    let mut events = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(root.join("signed-document-events.jsonl"))?;
    // for_route returns only after authenticated primary-route setup and normal
    // SDK subscriptions. The fresh marker gates the real Writer exec, not edits.
    let ready = json!({"app":WRITER,"room":workspace_id,"record":"","pid":std::process::id(),"subscribed":true});
    std::fs::write(
        root.join("observer-ready.tmp"),
        serde_json::to_vec_pretty(&ready)?,
    )?;
    std::fs::rename(
        root.join("observer-ready.tmp"),
        root.join("observer-ready.json"),
    )?;
    loop {
        if !manager.is_connected() {
            return Err("observer collaboration disconnected".into());
        }
        for event in manager.poll() {
            if let ui::collaboration::CollabEvent::Document {
                envelope,
                decrypted_payload,
                ..
            } = event
            {
                if decrypted_payload.is_some() {
                    return Err("unexpected encrypted document route".into());
                }
                collab_types::verify_op(&envelope)?;
                collab_types::validate_document_payload_route(
                    WRITER,
                    &workspace_id,
                    "",
                    &envelope,
                )?;
                serde_json::to_writer(&mut events, &envelope)?;
                writeln!(events)?;
                events.flush()?;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope() -> collab_types::SignedOp {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let mut doc = automerge::AutoCommit::new();
        tilo_doc::model::init(&mut doc, &automerge::ROOT);
        let mut op = collab_types::SignedOp::new_unsigned(
            collab_types::DocumentMutation::Snapshot(doc.save())
                .encode()
                .unwrap(),
            collab_types::document_artifact_id("dev.tilo.word-processor", "room", ""),
            collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
            collab_types::document_attestation_id("dev.tilo.word-processor", "room", ""),
            collab_types::OpKind::Change,
            std::time::SystemTime::now(),
            1,
        );
        collab_types::sign_op(&mut op, &key).unwrap();
        op
    }

    #[test]
    fn signed_reconstruction_and_readiness_check_the_route() {
        // Phase `signed_reconstruction_rejects_tampering_and_wrong_route`.
        {
            let op = envelope();
            let doc = reconstruct(std::slice::from_ref(&op), "room").unwrap();
            assert!(tilo_doc::model::open(&doc, &automerge::ROOT).is_ok());
            assert!(reconstruct(std::slice::from_ref(&op), "another-room").is_err());
            let mut altered = op;
            altered.payload.push(0);
            assert!(reconstruct(&[altered], "room").is_err());
        }
        // Phase `readiness_wait_is_bounded_and_checks_route`.
        {
            let dir = tempfile::tempdir().unwrap();
            assert!(wait_ready(dir.path(), "room", std::time::Duration::ZERO).is_err());
            std::fs::write(
                dir.path().join("observer-ready.json"),
                br#"{"room":"wrong","app":"dev.tilo.word-processor","subscribed":true}"#,
            )
            .unwrap();
            assert!(wait_ready(dir.path(), "room", std::time::Duration::ZERO).is_err());
            std::fs::write(
                dir.path().join("observer-ready.json"),
                br#"{"room":"room","app":"dev.tilo.word-processor","subscribed":true}"#,
            )
            .unwrap();
            assert!(wait_ready(dir.path(), "room", std::time::Duration::ZERO).is_ok());
        }
    }
}
