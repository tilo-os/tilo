//! Explicit migration of reviewed archived CONTENT into a different durable owner.
//! This module grants no old identity, governance, effects or signature freshness.
use crate::{CommitReceipt, SessionOwner};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{fs::File, io::Read, path::Path};
use ui::automerge::{ActorId, AutoCommit, ObjType, ROOT, ReadDoc, transaction::Transactable};
use ui::history_mode::restore::{
    HistoryMapEntries, HistoryMapField, HistoryMapOverlay, HistoryMembership, HistoryRestoreNode,
    HistoryRestoreRoot, HistoryRestoreSchema, HistoryRootPattern, HistoryRootSelector,
    PreservedOnCreate, RestoreValueType, ScalarType, apply_prepared_content_restore,
    prepare_external_content_import,
};

const CHAT: &str = "so.tilo.chat";
const WRITER: &str = "so.tilo.word-processor";
const WRITER_RECORD: &str = "tab:0000000000000001";
const WRITER_KEYS: &[&str] = &[
    "blocks",
    "styles",
    "preserved",
    "meta",
    "blobs",
    "header",
    "footer",
    "notes",
    "comments",
    "changes",
];
const CHAT_KEYS: &[&str] = &[
    "workspaces",
    "categories",
    "channels",
    "messages",
    "drafts",
    "users",
];
const PROFILE_KEYS: &[&str] = &["name", "initials", "color_idx", "agent", "status"];
const fn map_root(key: &'static str, required: bool) -> HistoryRestoreRoot {
    HistoryRestoreRoot {
        key,
        required,
        expected_type: RestoreValueType::Object(ObjType::Map),
    }
}
const fn scalar_field(key: &'static str, ty: ScalarType, required: bool) -> HistoryMapField {
    HistoryMapField {
        key,
        required,
        node: HistoryRestoreNode::Whole {
            expected_type: RestoreValueType::Scalar(ty),
        },
    }
}
static PROFILES: HistoryMapOverlay = HistoryMapOverlay {
    fields: &[],
    entries: Some(&HistoryMapEntries {
        key_limit_bytes: 1024,
        membership: HistoryMembership::TargetAuthoritative,
        on_create: PreservedOnCreate::Omit,
        node: HistoryRestoreNode::MapOverlay(&HistoryMapOverlay {
            fields: &[
                scalar_field("name", ScalarType::String, true),
                scalar_field("initials", ScalarType::String, true),
                scalar_field("color_idx", ScalarType::Int, true),
                scalar_field("agent", ScalarType::Boolean, false),
            ],
            entries: None,
        }),
    }),
};
static WRITER_SCHEMA: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "session-recovery.writer-content",
    version: 1,
    roots: &[
        map_root("blocks", true),
        map_root("styles", true),
        map_root("preserved", true),
        map_root("meta", true),
        map_root("blobs", false),
        map_root("header", false),
        map_root("footer", false),
        map_root("notes", false),
        map_root("comments", false),
        map_root("changes", false),
    ],
    selectors: &[],
    preserved_roots: &["schema", "schema_version"],
    preserved: &[],
    validate: validate_writer,
    validate_result: validate_writer,
};
static CHAT_SCHEMA: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "session-recovery.chat-content",
    version: 1,
    roots: &[
        map_root("workspaces", true),
        map_root("categories", true),
        map_root("channels", true),
        map_root("messages", false),
        map_root("drafts", false),
    ],
    selectors: &[HistoryRootSelector {
        pattern: HistoryRootPattern::Exact("users"),
        required: true,
        node: HistoryRestoreNode::MapOverlay(&PROFILES),
    }],
    preserved_roots: &["seeded", "voice", "read"],
    preserved: &[],
    validate: validate_chat,
    validate_result: validate_chat,
};
fn allowed_roots(doc: &AutoCommit, selected: &[&str], operational: &[&str]) -> Result<(), String> {
    for key in doc.keys(ROOT) {
        if !key.starts_with("qos:")
            && !key.starts_with("tilo:")
            && !selected.contains(&key.as_str())
            && !operational.contains(&key.as_str())
        {
            return Err(format!("unsupported recovery root {key}"));
        }
    }
    Ok(())
}
fn validate_writer(doc: &AutoCommit) -> Result<(), String> {
    allowed_roots(doc, WRITER_KEYS, &["schema", "schema_version"])?;
    // Reuse the real read-only application model rather than a parallel qdoc reader.
    word_processor::WordProcessor::from_document(doc).map(|_| ())
}
fn validate_chat(doc: &AutoCommit) -> Result<(), String> {
    allowed_roots(doc, CHAT_KEYS, &["seeded", "voice", "read"])?;
    let users = doc
        .get(ROOT, "users")
        .map_err(|e| e.to_string())?
        .ok_or("missing Chat users")?
        .1;
    for id in doc.keys(&users) {
        let profile = doc
            .get(&users, &id)
            .map_err(|e| e.to_string())?
            .ok_or("missing Chat profile")?
            .1;
        for key in doc.keys(&profile) {
            if !PROFILE_KEYS.contains(&key.as_str()) {
                return Err(format!("unsupported Chat profile property {key}"));
            }
        }
    }
    if let Some((_, messages)) = doc.get(ROOT, "messages").map_err(|e| e.to_string())? {
        for channel in doc.keys(&messages) {
            let list = doc
                .get(&messages, &channel)
                .map_err(|e| e.to_string())?
                .ok_or("missing Chat message list")?
                .1;
            for index in 0..doc.length(&list) {
                let message = doc
                    .get(&list, index)
                    .map_err(|e| e.to_string())?
                    .ok_or("missing Chat message")?
                    .1;
                if doc
                    .get(&message, "work")
                    .map_err(|e| e.to_string())?
                    .is_some()
                {
                    return Err("Chat agent work requires a separate historical-provenance import; refusing to copy runtime work state".into());
                }
            }
        }
    }
    Ok(())
}
#[derive(Deserialize)]
struct Manifest {
    format: String,
    status: String,
    verification_mode: String,
    live_admission: bool,
    source_room: String,
    documents: Vec<ArchivedDocument>,
    operation_checks: Vec<OperationCheck>,
}
#[derive(Deserialize)]
struct ArchivedDocument {
    app_id: String,
    record_id: String,
    canonical_file: String,
    canonical_blake3: String,
    heads: Vec<String>,
    causally_complete: bool,
    canonical_record_schema: String,
}
#[derive(Deserialize)]
struct OperationCheck {
    signature: String,
    signer: String,
}
fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    ensure!(
        std::fs::symlink_metadata(path)?.is_file(),
        "archive input must be a regular file"
    );
    let file = File::open(path)?;
    ensure!(
        file.metadata()?.len() <= limit,
        "archive input exceeds recovery limit"
    );
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "archive input grew beyond recovery limit"
    );
    Ok(bytes)
}
/// Import a root-reviewed inspector export into an explicitly NEW, unused owner.
/// The expected digest must come from independent historical archive verification;
/// a manifest is not itself an authority certificate or a fresh admission proof.
/// Both applications commit together before this returns a durable receipt.
pub fn import_verified_archive(
    owner: &mut SessionOwner,
    archive: &Path,
    reviewed_manifest_blake3: &str,
) -> Result<CommitReceipt> {
    ensure!(
        owner.status().generation == 1,
        "content migration requires a freshly initialized owner before hosting or enrollment"
    );
    let manifest_bytes = read_bounded(&archive.join("manifest.json"), 1024 * 1024)?;
    ensure!(
        blake3::hash(&manifest_bytes).to_hex().as_str() == reviewed_manifest_blake3,
        "reviewed archive manifest digest mismatch"
    );
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    ensure!(
        manifest.format == "qos-offline-archive-inspection-v1"
            && manifest.status == "historical_content_verified"
            && manifest.verification_mode == "historical_content_only"
            && !manifest.live_admission,
        "archive is not a reviewed historical content export"
    );
    ensure!(
        manifest.source_room != owner.peer().room(),
        "migration requires a different destination room"
    );
    let new_signer = hex::encode(owner.peer().authority().device_public_key);
    ensure!(
        !manifest.operation_checks.is_empty()
            && manifest
                .operation_checks
                .iter()
                .all(|op| op.signature == "verified" && op.signer != new_signer),
        "migration requires verified historical content and a different native signing authority"
    );
    ensure!(
        manifest.documents.len() == 2,
        "recovery expects exactly the reviewed Chat and Writer records"
    );
    let mut candidate = owner.peer().clone();
    for (index, (app, record, schema)) in [
        (CHAT, "", &CHAT_SCHEMA),
        (WRITER, WRITER_RECORD, &WRITER_SCHEMA),
    ]
    .into_iter()
    .enumerate()
    {
        let entry = manifest
            .documents
            .iter()
            // Archives reviewed before the rename name the retired ids.
            .find(|entry| {
                ui::application::lookup_compatible_first_party_app_ids(&entry.app_id, app)
                    && entry.record_id == record
            })
            .context("reviewed canonical record missing")?;
        ensure!(
            entry.canonical_file == format!("document-{index}.automerge")
                && entry.causally_complete
                && entry.canonical_record_schema == "verified",
            "archive record is incomplete or names an unexpected file"
        );
        let bytes = read_bounded(&archive.join(&entry.canonical_file), 64 * 1024 * 1024)?;
        ensure!(
            blake3::hash(&bytes).to_hex().as_str() == entry.canonical_blake3,
            "archived canonical bytes changed after review"
        );
        let mut source = AutoCommit::load(&bytes)?;
        let source_heads = source.get_heads();
        let mut actual: Vec<_> = source_heads.iter().map(ToString::to_string).collect();
        actual.sort();
        let mut expected = entry.heads.clone();
        expected.sort();
        ensure!(
            !actual.is_empty() && actual == expected,
            "archived canonical heads changed after review"
        );
        let current = candidate
            .document(app, record)
            .context("fresh destination canonical record missing")?;
        let mut destination = AutoCommit::load(&current)?;
        destination.set_actor(ActorId::from(
            owner.identity.native.verifying_key().to_bytes().to_vec(),
        ));
        let destination_heads = destination.get_heads();
        let prepared = prepare_external_content_import(
            &destination,
            &source,
            schema,
            &destination_heads,
            &source_heads,
        )?;
        apply_prepared_content_restore(&mut destination, &prepared)?;
        if app == CHAT {
            // Historical authors remain display provenance; nobody is made currently online.
            let users = destination
                .get(ROOT, "users")?
                .context("imported Chat users missing")?
                .1;
            let ids: Vec<_> = destination.keys(&users).collect();
            for id in ids {
                let profile = destination
                    .get(&users, &id)?
                    .context("imported Chat profile missing")?
                    .1;
                destination.put(&profile, "status", "offline")?;
            }
        }
        destination.commit();
        // Re-run exact semantic comparison after the intentional presence reset.
        let imported_heads = destination.get_heads();
        let checked = prepare_external_content_import(
            &destination,
            &source,
            schema,
            &imported_heads,
            &source_heads,
        )?;
        ensure!(
            checked.target_projection() == prepared.target_projection(),
            "content source projection changed"
        );
        ensure!(
            apply_prepared_content_restore(&mut destination, &checked)?
                == ui::history_mode::restore::RestoreOutcome::Unchanged,
            "imported content differs from reviewed projection"
        );
        if imported_heads != destination_heads {
            let changes = destination.save_after(&destination_heads);
            candidate
                .publish_incremental(
                    app,
                    record,
                    changes,
                    &owner.identity.native,
                    web_time::SystemTime::now(),
                )
                .map_err(anyhow::Error::msg)?;
        }
    }
    owner.commit_admission(candidate, owner.directory().to_vec())
}
#[cfg(test)]
mod tests {
    use super::*;
    use ui::automerge::{
        ActorId, AutoCommit, ObjType, ROOT, ReadDoc,
        marks::{ExpandMark, Mark},
        transaction::Transactable,
    };
    use ui::history_mode::restore::*;

    static CONTENT: HistoryRestoreSchema = HistoryRestoreSchema {
        id: "recovery-test",
        version: 1,
        roots: &[HistoryRestoreRoot {
            key: "content",
            required: true,
            expected_type: RestoreValueType::Object(ObjType::Map),
        }],
        selectors: &[],
        preserved_roots: &[],
        preserved: &[],
        validate: |_| Ok(()),
        validate_result: |_| Ok(()),
    };
    fn sample(actor: u8, text: &str) -> AutoCommit {
        let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![actor]));
        let map = doc.put_object(ROOT, "content", ObjType::Map).unwrap();
        let txt = doc.put_object(&map, "body", ObjType::Text).unwrap();
        doc.splice_text(&txt, 0, 0, text).unwrap();
        doc.mark(&txt, Mark::new("bold".into(), true, 0, 2), ExpandMark::None)
            .unwrap();
        let list = doc.put_object(&map, "ordered", ObjType::List).unwrap();
        doc.insert(&list, 0, 42_i64).unwrap();
        doc.put(ROOT, "qos:authority", format!("authority-{actor}"))
            .unwrap();
        doc.commit();
        doc
    }
    // Catches history merging, actor adoption, mark flattening and framework overwrite.
    #[test]
    fn external_copy_preserves_rich_values_without_source_history_or_authority() {
        let mut source = sample(1, "rich text");
        let mut dest = sample(2, "new");
        let source_heads = source.get_heads();
        let dest_heads = dest.get_heads();
        let actor = dest.get_actor().clone();
        let protected = format!("{:?}", dest.get(ROOT, "qos:authority").unwrap());
        let prepared =
            prepare_external_content_import(&dest, &source, &CONTENT, &dest_heads, &source_heads)
                .unwrap();
        assert_eq!(
            apply_prepared_content_restore(&mut dest, &prepared).unwrap(),
            RestoreOutcome::Changed
        );
        assert_eq!(dest.get_actor(), &actor);
        assert_eq!(
            format!("{:?}", dest.get(ROOT, "qos:authority").unwrap()),
            protected
        );
        dest.commit();
        let content = dest.get(ROOT, "content").unwrap().unwrap().1;
        let text = dest.get(&content, "body").unwrap().unwrap().1;
        assert_eq!(dest.text(&text).unwrap(), "rich text");
        assert_eq!(
            dest.marks(&text).unwrap(),
            vec![Mark::new("bold".into(), true, 0, 2)]
        );
        assert!(
            source_heads
                .iter()
                .all(|head| dest.get_change_by_hash(head).is_none())
        );
        let current_heads = dest.get_heads();
        let compare = prepare_external_content_import(
            &dest,
            &source,
            &CONTENT,
            &current_heads,
            &source_heads,
        )
        .unwrap();
        assert_eq!(
            apply_prepared_content_restore(&mut dest, &compare).unwrap(),
            RestoreOutcome::Unchanged
        );
    }
    // Catches accepting a reviewed frontier after either independent input changed.
    #[test]
    fn external_copy_rejects_stale_source_and_destination() {
        let mut source = sample(1, "old");
        let mut dest = sample(2, "new");
        let sh = source.get_heads();
        let dh = dest.get_heads();
        source.put(ROOT, "other", true).unwrap();
        source.commit();
        assert!(matches!(
            prepare_external_content_import(&dest, &source, &CONTENT, &dh, &sh),
            Err(RestoreError::StaleReview)
        ));
        let sh = source.get_heads();
        dest.put(ROOT, "other", true).unwrap();
        dest.commit();
        assert!(matches!(
            prepare_external_content_import(&dest, &source, &CONTENT, &dh, &sh),
            Err(RestoreError::StaleReview)
        ));
    }
    // Catches importing stale approval bytes or an already-used destination.
    #[test]
    fn archive_digest_mismatch_is_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
        let archive = dir.path().join("archive");
        std::fs::create_dir(&archive).unwrap();
        std::fs::write(archive.join("manifest.json"), b"{}").unwrap();
        let before = owner.status();
        assert!(import_verified_archive(&mut owner, &archive, &"0".repeat(64)).is_err());
        assert_eq!(owner.status(), before);
    }
    fn archive_fixture(owner: &SessionOwner, archive: &Path, unknown_writer_root: bool) -> String {
        std::fs::create_dir(archive).unwrap();
        let mut documents = vec![];
        for (index, (app, record)) in [
            ("so.tilo.chat", ""),
            ("so.tilo.word-processor", "tab:0000000000000001"),
        ]
        .iter()
        .enumerate()
        {
            let mut doc = AutoCommit::load(&owner.peer().document(app, record).unwrap()).unwrap();
            if *app == "so.tilo.chat" {
                chat::doc::ChatDoc::push_message_with_id(
                    &mut doc,
                    "c_general",
                    "historical-message",
                    chat::doc::NewMessage {
                        author: "native".into(),
                        text: "saved message".into(),
                        ..Default::default()
                    },
                );
                chat::doc::ChatDoc::set_draft(&mut doc, "c_general", "shared draft");
                // Content import must not adopt either generation of framework metadata.
                doc.put(ROOT, "qos:schema_version", 17_u64).unwrap();
                doc.put(ROOT, "tilo:schema_version", 19_u64).unwrap();
            } else if unknown_writer_root {
                doc.put(ROOT, "unreviewed_content", "must not disappear")
                    .unwrap();
            }
            doc.commit();
            let heads: Vec<_> = doc.get_heads().iter().map(ToString::to_string).collect();
            let bytes = doc.save();
            let file = format!("document-{index}.automerge");
            std::fs::write(archive.join(&file), &bytes).unwrap();
            // Reviewed archives predate the rename and name the retired ids.
            let archived_app = app.replacen("so.tilo.", "dev.qos.", 1);
            documents.push(serde_json::json!({"app_id":archived_app,"record_id":record,"canonical_file":file,"canonical_blake3":blake3::hash(&bytes).to_hex().to_string(),"heads":heads,"canonical_record_schema":"verified","causally_complete":true}));
        }
        let manifest = serde_json::to_vec(&serde_json::json!({"format":"qos-offline-archive-inspection-v1","status":"historical_content_verified","verification_mode":"historical_content_only","live_admission":false,"source_room":owner.peer().room(),"documents":documents,"operation_checks":[{"signature":"verified","signer":hex::encode(owner.peer().authority().device_public_key)}]})).unwrap();
        std::fs::write(archive.join("manifest.json"), &manifest).unwrap();
        blake3::hash(&manifest).to_hex().to_string()
    }
    // Catches lossy text-only recovery, old actor/history adoption and non-durable import.
    #[test]
    fn archived_chat_messages_drafts_and_profiles_are_new_owned_content() {
        let dir = tempfile::tempdir().unwrap();
        let old = SessionOwner::initialize(&dir.path().join("old")).unwrap();
        let archive = dir.path().join("archive");
        let digest = archive_fixture(&old, &archive, false);
        let mut new = SessionOwner::initialize(&dir.path().join("new")).unwrap();
        let authority = new.peer().authority().clone();
        let mut initial =
            AutoCommit::load(&new.peer().document("so.tilo.chat", "").unwrap()).unwrap();
        let initial_heads = initial.get_heads();
        let metadata = initial.get_all(ROOT, "qos:record").unwrap();
        let schema_metadata = ["qos:schema_version", "tilo:schema_version"]
            .map(|key| (key, initial.get_all(ROOT, key).unwrap()));
        import_verified_archive(&mut new, &archive, &digest).unwrap();
        assert_eq!(new.peer().authority(), &authority);
        assert_ne!(new.peer().room(), old.peer().room());
        let mut doc = AutoCommit::load(&new.peer().document("so.tilo.chat", "").unwrap()).unwrap();
        assert_eq!(doc.get_all(ROOT, "qos:record").unwrap(), metadata);
        for (key, expected) in schema_metadata {
            assert_eq!(doc.get_all(ROOT, key).unwrap(), expected);
        }
        let changes = doc.get_changes(&initial_heads);
        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes[0].actor_id().to_bytes(),
            &authority.device_public_key
        );
        assert_eq!(
            chat::doc::ChatDoc::list_messages(&doc, "c_general")[0].text,
            "saved message"
        );
        assert_eq!(
            chat::doc::ChatDoc::draft_text(&doc, "c_general"),
            "shared draft"
        );
        assert_eq!(
            chat::doc::ChatDoc::list_users(&doc)[0].status,
            chat::model::Presence::Offline
        );
        let before = new.status();
        drop(new);
        assert_eq!(
            SessionOwner::open(&dir.path().join("new"))
                .unwrap()
                .status(),
            before
        );
    }
    // Catches partial Chat import before unsupported Writer content is detected.
    #[test]
    fn unsupported_archive_content_and_same_identity_never_mutate_owner() {
        let dir = tempfile::tempdir().unwrap();
        let old = SessionOwner::initialize(&dir.path().join("old")).unwrap();
        let archive = dir.path().join("archive");
        let digest = archive_fixture(&old, &archive, true);
        let mut new = SessionOwner::initialize(&dir.path().join("new")).unwrap();
        let before = new.status();
        assert!(import_verified_archive(&mut new, &archive, &digest).is_err());
        assert_eq!(new.status(), before);
        let archive = dir.path().join("same-identity");
        let digest = archive_fixture(&new, &archive, false);
        assert!(import_verified_archive(&mut new, &archive, &digest).is_err());
        assert_eq!(new.status(), before);
        // A different room string alone cannot launder the same signing authority.
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(archive.join("manifest.json")).unwrap()).unwrap();
        manifest["source_room"] = serde_json::json!(uuid::Uuid::new_v4().to_string());
        let bytes = serde_json::to_vec(&manifest).unwrap();
        std::fs::write(archive.join("manifest.json"), &bytes).unwrap();
        assert!(
            import_verified_archive(
                &mut new,
                &archive,
                &blake3::hash(&bytes).to_hex().to_string()
            )
            .is_err()
        );
        assert_eq!(new.status(), before);
    }
    // Explicit opt-in: reads the reviewed archive, writes only an isolated temporary owner.
    #[test]
    #[ignore = "requires QOS_REVIEWED_ARCHIVE and QOS_REVIEWED_MANIFEST_BLAKE3"]
    fn reviewed_archive_roundtrip_in_temporary_new_owner() {
        let archive = std::env::var("QOS_REVIEWED_ARCHIVE").unwrap();
        let digest = std::env::var("QOS_REVIEWED_MANIFEST_BLAKE3").unwrap();
        eprintln!(
            "archive manifest BLAKE3: {}",
            blake3::hash(&std::fs::read(Path::new(&archive).join("manifest.json")).unwrap())
                .to_hex()
        );
        let preserved_archive: Vec<_> = [
            "manifest.json",
            "document-0.automerge",
            "document-1.automerge",
        ]
        .into_iter()
        .map(|name| (name, std::fs::read(Path::new(&archive).join(name)).unwrap()))
        .collect();
        let dir = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&dir.path().join("new-owner")).unwrap();
        let authority = owner.peer().authority().clone();
        let room = owner.peer().room().to_owned();
        import_verified_archive(&mut owner, Path::new(&archive), &digest).unwrap();
        assert_eq!(owner.peer().authority(), &authority);
        assert_eq!(owner.peer().room(), room);
        assert_ne!(room, "f378f3d5-b73b-420b-a3f0-8e74098b1b37");
        let chat = AutoCommit::load(&owner.peer().document("so.tilo.chat", "").unwrap()).unwrap();
        assert_eq!(chat::doc::ChatDoc::list_channels(&chat).len(), 2);
        let channels = chat::doc::ChatDoc::list_channels(&chat);
        assert_eq!(
            channels
                .iter()
                .map(|c| chat::doc::ChatDoc::list_messages(&chat, &c.id).len())
                .sum::<usize>(),
            2
        );
        let writer = AutoCommit::load(
            &owner
                .peer()
                .document("so.tilo.word-processor", "tab:0000000000000001")
                .unwrap(),
        )
        .unwrap();
        let styles = writer.get(ROOT, "styles").unwrap().unwrap().1;
        assert_eq!(writer.length(&styles), 6);
        let chars = |key: &str| {
            let map = writer.get(ROOT, key).unwrap().unwrap().1;
            writer
                .keys(&map)
                .map(|id| {
                    let block = writer.get(&map, &id).unwrap().unwrap().1;
                    writer
                        .get(&block, "text")
                        .unwrap()
                        .map(|(_, text)| writer.text(&text).unwrap().chars().count())
                        .unwrap_or(0)
                })
                .sum::<usize>()
        };
        assert_eq!(chars("blocks"), 3);
        assert_eq!(chars("header"), 2);
        for (name, bytes) in preserved_archive {
            assert_eq!(
                std::fs::read(Path::new(&archive).join(name)).unwrap(),
                bytes
            );
        }
        let before = owner.status();
        drop(owner);
        assert_eq!(
            SessionOwner::open(&dir.path().join("new-owner"))
                .unwrap()
                .status(),
            before
        );
    }
}
