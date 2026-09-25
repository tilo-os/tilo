//! Test-only fixture, signed-event observer and independent evidence inspector.
//! Never publishes an action directory or sends a document/action mutation.
use std::{
    error::Error,
    io::Write,
    path::Path,
    time::{Duration, Instant},
};
use tilo_automerge::ApplyIncoming;

const WRITER: &str = "dev.qos.word-processor";
use serde_json::json;

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("fixture") if args.len() == 3 => {
            let image = image::RgbaImage::from_fn(48, 32, |x, y| {
                image::Rgba([(x * 5) as u8, (y * 7) as u8, ((x * 13 + y * 17) % 256) as u8, 255])
            });
            image.save(&args[2])?;
            json!({"width":48,"height":32,"hash":qdoc::blobstore::hash_bytes(&std::fs::read(&args[2])?)})
        }
        Some("image") if args.len() == 6 => {
            let hash = tilo_actions::content_hash(&args[4]).ok_or("not canonical content resource")?;
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("not a BLAKE3 address".into());
            }
            let bytes = std::fs::read(Path::new(&args[2]).join("blobs").join(&hash[..2]).join(hash))?;
            if qdoc::blobstore::hash_bytes(&bytes) != hash { return Err("blob hash mismatch".into()); }
            let source = image::open(&args[3])?.to_rgba8();
            let edited = image::load_from_memory(&bytes)?.to_rgba8();
            if source.dimensions() != (48, 32) || edited.dimensions() != (32, 32) {
                return Err("unexpected source/result dimensions".into());
            }
            for y in 0..32 { for x in 0..32 {
                if edited.get_pixel(x,y) != source.get_pixel(x+8,y) {
                    return Err(format!("centered crop pixel mismatch at {x},{y}").into());
                }
            }}
            std::fs::write(&args[5], &bytes)?;
            json!({"hash":hash,"width":32,"height":32,"pixels_compared":1024,"oracle":"source(x+8,y), all RGBA channels"})
        }
        Some("observe") if args.len() == 3 => {
            observe(Path::new(&args[2]))?;
            return Ok(());
        }
        Some("wait-ready-exec") if args.len() == 4 => {
            use std::os::unix::process::CommandExt;
            let room = std::env::var("TILO_WORKSPACE_ID")?;
            wait_ready(Path::new(&args[2]), &room, Duration::from_secs(20))?;
            return Err(std::process::Command::new(&args[3]).exec().into());
        }
        Some("document") if args.len() == 6 => {
            let root = Path::new(&args[2]);
            let workspace: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("profile/workspace.json"))?)?;
            let room = workspace["workspaces"][0]["room"].as_str().ok_or("missing workspace room")?;
            let events: Vec<collab_types::SignedOp> = std::fs::read_to_string(root.join("signed-document-events.jsonl"))?
                .lines().map(serde_json::from_str).collect::<Result<_, _>>()?;
            let mut doc = reconstruct(&events, room)?;
            let model = qdoc::model::open(&doc, &automerge::ROOT)?;
            let materialized = qdoc::model::materialize(&doc, &model);
            let images: Vec<_> = materialized.blocks.iter().filter(|b| b.kind == "image").collect();
            if images.len() != 1 { return Err("expected exactly one reconstructed image block".into()); }
            let block = images[0];
            for (name, expected) in [("blob", qdoc::AttrVal::Str(args[3].clone())), ("width",qdoc::AttrVal::Int(32)), ("height",qdoc::AttrVal::Int(32))] {
                if block.attrs.get(name) != Some(&expected) { return Err(format!("reconstructed {name} mismatch").into()); }
            }
            std::fs::write(&args[4], doc.save())?;
            let evidence = json!({"app":WRITER,"record":"","room":room,"image_block":block.id,"blob":args[3],"width":32,"height":32,"signed_envelopes":events.len(),"verification":"independent signature and exact-route validation, Automerge reconstruction"});
            std::fs::write(&args[5], serde_json::to_vec_pretty(&evidence)?)?;
            evidence
        }
        _ => return Err("usage: fixture PATH | image PROFILE SOURCE CONTENT_RESOURCE OUTPUT | observe ROOT | wait-ready-exec ROOT WRITER | document ROOT HASH AUTOMERGE JSON".into()),
    };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn wait_ready(root: &Path, room: &str, timeout: Duration) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(bytes) = std::fs::read(root.join("observer-ready.json")) {
            let ready: serde_json::Value = serde_json::from_slice(&bytes)?;
            if ready["room"] != room || ready["app"] != WRITER || ready["subscribed"] != true {
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
    let room = std::env::var("TILO_WORKSPACE_ID")?;
    let mut manager = ui::collaboration::CollaborationManager::for_route(
        WRITER,
        uuid::Uuid::parse_str(&room)?,
        None,
        Some("MCP proof observer".into()),
    )?;
    let mut events = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(root.join("signed-document-events.jsonl"))?;
    // for_route returns only after authenticated primary-route setup and normal
    // SDK subscriptions. The fresh marker gates the real Writer exec, not edits.
    let ready =
        json!({"app":WRITER,"room":room,"record":"","pid":std::process::id(),"subscribed":true});
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
                collab_types::validate_document_payload_route(WRITER, &room, "", &envelope)?;
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
        qdoc::model::init(&mut doc, &automerge::ROOT);
        let mut op = collab_types::SignedOp::new_unsigned(
            collab_types::DocumentMutation::Snapshot(doc.save())
                .encode()
                .unwrap(),
            collab_types::document_artifact_id("dev.qos.word-processor", "room", ""),
            collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
            collab_types::document_attestation_id("dev.qos.word-processor", "room", ""),
            collab_types::OpClass::Splice,
            std::time::SystemTime::now(),
            1,
        );
        collab_types::sign_op(&mut op, &key).unwrap();
        op
    }

    #[test]
    fn signed_reconstruction_rejects_tampering_and_wrong_route() {
        let op = envelope();
        let doc = reconstruct(&[op.clone()], "room").unwrap();
        assert!(qdoc::model::open(&doc, &automerge::ROOT).is_ok());
        assert!(reconstruct(&[op.clone()], "another-room").is_err());
        let mut altered = op;
        altered.payload.push(0);
        assert!(reconstruct(&[altered], "room").is_err());
    }

    #[test]
    fn readiness_wait_is_bounded_and_checks_route() {
        let dir = tempfile::tempdir().unwrap();
        assert!(wait_ready(dir.path(), "room", std::time::Duration::ZERO).is_err());
        std::fs::write(
            dir.path().join("observer-ready.json"),
            br#"{"room":"wrong","app":"dev.qos.word-processor","subscribed":true}"#,
        )
        .unwrap();
        assert!(wait_ready(dir.path(), "room", std::time::Duration::ZERO).is_err());
        std::fs::write(
            dir.path().join("observer-ready.json"),
            br#"{"room":"room","app":"dev.qos.word-processor","subscribed":true}"#,
        )
        .unwrap();
        assert!(wait_ready(dir.path(), "room", std::time::Duration::ZERO).is_ok());
    }
}
