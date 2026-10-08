//! Offline historical evidence inspection. Never a live admission or migration API.
#![allow(
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::needless_pass_by_value,
    reason = "offline diagnostic probe: it indexes JSON objects it builds itself and reads top to bottom"
)]
use anyhow::{Result, bail, ensure};
use automerge::{AutoCommit, ReadDoc, transaction::Transactable};
use collab_types::DocumentMutation;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tilo_peer_state::{NeighbourMsg, coverage, room};
use tilo_relay::{
    coedit::CoeditPayload,
    transport::{self, Envelope},
};
use tilo_user_identity::{
    DeviceCert, DirectoryUpdate, RevocationEntry, RevocationList, WorkspaceDirectory,
};

#[derive(Clone, Deserialize)]
struct Bootstrap {
    room: String,
    workspace_id: String,
    directory_author: DeviceCert,
    directory_update: DirectoryUpdate,
    certified_roster: Vec<DeviceCert>,
    records: Vec<tilo_peer_state::DocumentRow>,
}
#[derive(Clone, Deserialize)]
struct PublicEnrollment {
    root_public_key: [u8; 32],
    host_certificate: DeviceCert,
    revocations: Vec<RevocationEntry>,
    bootstrap: Bootstrap,
}
#[derive(Clone)]
struct Source {
    name: String,
    bytes: Vec<u8>,
}
struct Recovered {
    docs: BTreeMap<(String, String), AutoCommit>,
    checks: Vec<Value>,
}
fn decode_frame(bytes: &[u8]) -> Result<Envelope> {
    tokio::runtime::Builder::new_current_thread()
        .build()?
        .block_on(async {
            let mut reader = transport::FrameReader::new(bytes);
            let envelope = reader
                .read_next()
                .await?
                .ok_or_else(|| anyhow::anyhow!("empty archive frame"))?;
            ensure!(
                reader.read_next().await?.is_none(),
                "multiple frames in retained entry"
            );
            Ok(envelope)
        })
}
// Historical checks use each signed/recorded timestamp only for certificate and
// policy windows. They do not call StatePeer receive, mint authority, or establish
// live freshness. The unsigned room envelope remains archive provenance.
fn certificate_at(
    cert: &DeviceCert,
    root: &[u8; 32],
    revoked: &RevocationList,
    at: u64,
) -> Result<()> {
    ensure!(&cert.user_public_key == root, "certificate root mismatch");
    ensure!(at >= cert.issued_at_ns, "operation predates certificate");
    tilo_user_identity::verify_device_cert_at(cert, at)?;
    ensure!(
        !revoked.is_revoked_at(&cert.device_public_key, at),
        "historically revoked device"
    );
    Ok(())
}
fn revocations(root: &[u8; 32], entries: &[RevocationEntry]) -> Result<RevocationList> {
    let mut list = RevocationList::new();
    for entry in entries {
        ensure!(&entry.user_public_key == root, "revocation root mismatch");
        list.insert(entry.clone())?;
    }
    Ok(list)
}
struct Decoded {
    source: String,
    author: [u8; 32],
    at: u64,
    kind: tilo_peer_state::transport::Retention,
    message: NeighbourMsg,
}
fn decode(source: &Source, workspace: u64) -> Result<Decoded> {
    let Envelope::Delta {
        workspace_id,
        author_device_public_key,
        authored_at_ns,
        delta,
        ..
    } = decode_frame(&source.bytes)?
    else {
        bail!("retained entry is not Delta")
    };
    ensure!(workspace_id == workspace, "outer workspace mismatch");
    let (channel, body, kind) = match CoeditPayload::decode(&delta)
        .ok_or_else(|| anyhow::anyhow!("invalid coedit payload"))?
    {
        CoeditPayload::Checkpoint { channel, body } => (
            channel,
            body,
            tilo_peer_state::transport::Retention::Checkpoint,
        ),
        CoeditPayload::RetainedDelta { channel, body } => {
            (channel, body, tilo_peer_state::transport::Retention::Delta)
        }
    };
    let message: NeighbourMsg = postcard::from_bytes(&body)?;
    ensure!(
        tilo_peer_state::transport::retention(
            &message,
            kind == tilo_peer_state::transport::Retention::Checkpoint
        ) == Some((channel, kind)),
        "retained channel or kind mismatch"
    );
    // Filesystem labels are evidence too. Tests use fixture-N names; real input
    // labels are relative to retained/, and must agree with the actual codec.
    if source.name.starts_with("checkpoints/") {
        ensure!(
            kind == tilo_peer_state::transport::Retention::Checkpoint
                && source.name == format!("checkpoints/{}", hex::encode(channel)),
            "checkpoint filename/channel mismatch"
        );
    } else if source.name.starts_with("tails/") {
        ensure!(
            kind == tilo_peer_state::transport::Retention::Delta
                && source.name.split('/').nth(1) == Some(hex::encode(channel).as_str()),
            "tail filename/channel mismatch"
        );
    }
    Ok(Decoded {
        source: source.name.clone(),
        author: author_device_public_key,
        at: authored_at_ns,
        kind,
        message,
    })
}
fn complete(doc: &AutoCommit) -> Result<()> {
    ensure!(
        ReadDoc::get_missing_deps(doc, &[]).is_empty(),
        "missing causal dependencies"
    );
    Ok(())
}
fn strict_load(bytes: &[u8]) -> Result<AutoCommit> {
    let doc = AutoCommit::load_with_options(
        bytes,
        automerge::LoadOptions::new().on_partial_load(automerge::OnPartialLoad::Error),
    )?;
    complete(&doc)?;
    Ok(doc)
}
fn canonical(room_id: &str, app: &str, record: &str, doc: &AutoCommit) -> Result<()> {
    use ui::{
        portable_storage::{PortableStorage, StorageKey},
        record::{RecordCheckout, RecordId, RecordOwners},
    };
    let definition = match app {
        "so.tilo.word-processor" => word_processor::definition(),
        "so.tilo.chat" => chat::definition(),
        _ => bail!("unsupported canonical app"),
    };
    let (key, checkout) = if app == "so.tilo.chat" {
        ensure!(record.is_empty(), "Chat requires package-primary route");
        (
            StorageKey::Primary {
                package: app.into(),
                workspace: room_id.into(),
            },
            RecordCheckout::package_primary(definition.record_kind.clone()),
        )
    } else {
        let id =
            RecordId::parse(record).ok_or_else(|| anyhow::anyhow!("invalid ordinary record"))?;
        ensure!(!id.is_package_primary(), "Writer requires ordinary record");
        (
            StorageKey::Record {
                package: app.into(),
                workspace: room_id.into(),
                record: record.into(),
            },
            RecordCheckout::new(id, definition.record_kind.clone()),
        )
    };
    // Disposable memory-only validator, with a fresh non-enrolled actor. It is
    // never flushed and is never used to author or export changes.
    let cache =
        PortableStorage::new("offline-archive-validation", room_id).map_err(anyhow::Error::msg)?;
    cache
        .hydrate(
            key,
            doc.clone().save(),
            Some(uuid::Uuid::new_v4().as_bytes().to_vec()),
        )
        .map_err(anyhow::Error::msg)?;
    let mut owners = RecordOwners::portable(cache);
    owners.open(app, room_id)?.try_open_record(&checkout)?;
    if app == "so.tilo.chat" {
        ensure!(
            chat::doc::ChatDoc::is_seeded(doc),
            "Chat canonical schema absent"
        );
    } else {
        tilo_doc::model::open(doc, &automerge::ROOT)?;
    }
    Ok(())
}
fn inspect(auth: &PublicEnrollment, sources: &[Source]) -> Result<Recovered> {
    use collab_types::{
        ArtifactKind, Ed25519PublicKey,
        caps::{Actor, CapDecision, Principal, ResourceRef, decide_capability, verb_for_opclass},
    };
    use tilo_peer_state::transport::Retention;
    let b = &auth.bootstrap;
    let room_id = uuid::Uuid::parse_str(&b.room)?;
    ensure!(
        room_id.to_string() == b.room,
        "noncanonical room identifier"
    );
    let workspace = b.workspace_id.parse::<u64>()?;
    ensure!(
        tilo_relay::room_workspace_id(&b.room).map_err(anyhow::Error::msg)? == workspace,
        "workspace room binding mismatch"
    );
    let revoked = revocations(&auth.root_public_key, &auth.revocations)?;
    let directory_time = b.directory_update.authored_at_ns;
    certificate_at(
        &b.directory_author,
        &auth.root_public_key,
        &revoked,
        directory_time,
    )?;
    certificate_at(
        &auth.host_certificate,
        &auth.root_public_key,
        &revoked,
        directory_time,
    )?;
    ensure!(
        auth.host_certificate.capabilities.can_act_as_host,
        "host certificate lacks host capability"
    );
    let mut directory =
        WorkspaceDirectory::new(auth.root_public_key, b.directory_author.device_public_key);
    // This API checks the signed directory delta and cert, not live state
    // freshness. Supplying its authored timestamp is historical verification.
    directory.apply_incremental(
        &b.directory_update,
        &b.directory_author,
        &revoked,
        directory_time,
    )?;
    strict_load(&directory.save())?;
    let membership = directory
        .as_map()
        .remove(&workspace)
        .ok_or_else(|| anyhow::anyhow!("signed directory workspace missing"))?;
    ensure!(
        membership
            .host_devices
            .contains(&auth.host_certificate.device_public_key),
        "host absent from signed directory"
    );
    let mut certificates = BTreeMap::new();
    for cert in &b.certified_roster {
        certificate_at(cert, &auth.root_public_key, &revoked, directory_time)?;
        ensure!(
            membership.member_devices.contains(&cert.device_public_key),
            "roster member absent from signed directory"
        );
        ensure!(
            certificates
                .insert(cert.device_public_key, cert.clone())
                .is_none(),
            "duplicate roster certificate"
        );
    }
    ensure!(
        certificates.get(&b.directory_author.device_public_key) == Some(&b.directory_author),
        "pinned governor absent from roster"
    );
    let decoded = sources
        .iter()
        .map(|s| decode(s, workspace))
        .collect::<Result<Vec<_>>>()?;
    let mut room_doc = None;
    for row in &decoded {
        if let NeighbourMsg::SyncResp {
            changes,
            room: message_room,
        } = &row.message
        {
            ensure!(message_room == &b.room, "room message route mismatch");
            ensure!(
                row.author == b.directory_author.device_public_key,
                "room checkpoint not attributed to pinned governor"
            );
            certificate_at(&b.directory_author, &auth.root_public_key, &revoked, row.at)?;
            let candidate =
                room::candidate(room_doc.as_ref(), changes).map_err(anyhow::Error::msg)?;
            complete(&candidate)?;
            ensure!(
                candidate
                    .get(automerge::ROOT, "room")?
                    .and_then(|(v, _)| v.as_str().map(str::to_owned))
                    .as_deref()
                    == Some(&b.room),
                "room document declaration missing/mismatched"
            );
            coverage::monotonic(room_doc.as_ref(), &candidate).map_err(anyhow::Error::msg)?;
            room_doc = Some(candidate);
        }
    }
    let room_doc = room_doc.ok_or_else(|| anyhow::anyhow!("missing governed room checkpoint"))?;
    let coverages = coverage::entries(&room_doc).map_err(anyhow::Error::msg)?;
    let identities = room::governance_of(Some(&room_doc)).identities;
    let (_, owner, caps) = room::extract_governed_fields(&room_doc);
    let mut docs = BTreeMap::new();
    let mut checks = vec![];
    let mut pending = vec![];
    let mut seen = BTreeMap::new();
    // Verify all signed envelopes before reconstructing any exported state.
    for row in &decoded {
        let NeighbourMsg::AppDocument {
            peer,
            app_id,
            cluster_id,
            record_id,
            envelope,
        } = &row.message
        else {
            ensure!(
                matches!(row.message, NeighbourMsg::SyncResp { .. }),
                "unsupported archive message"
            );
            continue;
        };
        ensure!(
            cluster_id == &b.room
                && *peer == tilo_peer_state::peer_handle(&row.author)
                && envelope.author.as_bytes() == &row.author,
            "document source/room mismatch"
        );
        collab_types::verify_op(envelope)?;
        ensure!(envelope.seq > 0, "zero document sequence");
        let at = u64::try_from(
            envelope
                .timestamp
                .duration_since(web_time::SystemTime::UNIX_EPOCH)?
                .as_nanos(),
        )?;
        let cert = certificates
            .get(&row.author)
            .ok_or_else(|| anyhow::anyhow!("missing signer certificate"))?;
        let identity: Value = serde_json::from_slice(
            identities
                .get(&hex::encode(row.author))
                .ok_or_else(|| anyhow::anyhow!("signer absent from governed roster"))?,
        )?;
        let governed: DeviceCert = serde_json::from_value(
            identity
                .get("device_cert")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("governed certificate missing"))?,
        )?;
        ensure!(
            &governed == cert,
            "governed certificate differs from signed directory roster"
        );
        let mut entries = auth.revocations.clone();
        entries.extend(serde_json::from_value::<Vec<RevocationEntry>>(
            identity.get("revocations").cloned().unwrap_or(json!([])),
        )?);
        certificate_at(
            cert,
            &auth.root_public_key,
            &revocations(&auth.root_public_key, &entries)?,
            at,
        )?;
        let person = Principal::person_from_key(Ed25519PublicKey::from_bytes(cert.user_public_key));
        let role = room::role_of(owner.as_deref(), &caps, person.id(), at / 1_000_000_000)
            .ok_or_else(|| anyhow::anyhow!("missing room membership"))?;
        let actor = Actor::person(Some(role), person.id())
            .with_principal(Principal::App(app_id.clone()))
            .in_room(b.room.clone());
        let resource = ResourceRef::Artifact {
            app: Some(app_id.clone()),
            kind: ArtifactKind::Note,
            instance: envelope.artifact,
        };
        ensure!(
            collab_types::role_permits(role, ArtifactKind::Note, envelope.op_class)
                && decide_capability(
                    &actor,
                    &resource,
                    verb_for_opclass(envelope.op_class),
                    &caps,
                    Some(at / 1_000_000_000)
                ) == CapDecision::Allow,
            "historical document capability forbidden"
        );
        let collab_types::RoutedDocumentPayload::Cleartext(mutation) =
            collab_types::validate_document_payload_route(app_id, cluster_id, record_id, envelope)?
        else {
            bail!("incomplete: encrypted state requires authenticated MLS material")
        };
        let route = (app_id.clone(), record_id.clone());
        let duplicate = (route.clone(), row.author, envelope.seq);
        let hash = *blake3::hash(&postcard::to_stdvec(envelope)?).as_bytes();
        if let Some(prior) = seen.insert(duplicate, hash) {
            ensure!(prior == hash, "conflicting signed sequence");
        }
        if let DocumentMutation::Snapshot(bytes) = &mutation {
            ensure!(
                row.kind == Retention::Checkpoint
                    && row.author == b.directory_author.device_public_key,
                "snapshot must be pinned governor checkpoint"
            );
            let doc = strict_load(bytes)?;
            let coverage = coverages
                .get(&coverage::route_key(app_id, record_id))
                .ok_or_else(|| anyhow::anyhow!("missing snapshot coverage"))?;
            ensure!(
                coverage.room == b.room
                    && coverage.app == *app_id
                    && coverage.record == *record_id
                    && coverage.author == row.author
                    && coverage.sequence == envelope.seq
                    && coverage.payload_hash == *blake3::hash(&envelope.payload).as_bytes()
                    && coverage.heads
                        == doc
                            .base_heads()
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>(),
                "snapshot/coverage binding mismatch"
            );
            ensure!(
                coverage.watermarks.get(&hex::encode(row.author)) == Some(&envelope.seq),
                "snapshot author coverage mismatch"
            );
            for (author, seq) in &coverage.watermarks {
                ensure!(
                    hex::decode(author).is_ok_and(|key| key.len() == 32) && *seq > 0,
                    "invalid coverage watermark"
                );
            }
            canonical(&b.room, app_id, record_id, &doc)?;
            ensure!(
                docs.insert(route.clone(), doc).is_none(),
                "multiple retained snapshots for route"
            );
        } else {
            ensure!(
                row.kind == Retention::Delta,
                "incremental retained as checkpoint"
            );
            pending.push((route, mutation, row.author, envelope.seq));
        }
        checks.push(json!({"source":row.source,"app_id":app_id,"record_id":record_id,"signer":hex::encode(row.author),"sequence":envelope.seq,"signed_timestamp_ns":at,"signature":"verified","certificate":"verified_at_signed_timestamp","recorded_governance_policy":"allowed","snapshot_coverage":if row.kind==Retention::Checkpoint{"exact_binding_verified; authority_from_archived_transport"}else{"incremental"}}));
    }
    while !pending.is_empty() {
        let old = pending.len();
        let mut deferred = vec![];
        for (route, mutation, author, seq) in pending {
            let doc = docs
                .get_mut(&route)
                .ok_or_else(|| anyhow::anyhow!("incremental without retained snapshot"))?;
            // Strict standalone decoding rejects corrupt suffixes, but MissingDeps
            // is expected for ordinary increments. Probe queued dependencies before
            // shared merge, which need not preserve unresolvable queued changes.
            match AutoCommit::load_with_options(
                mutation.data(),
                automerge::LoadOptions::new().on_partial_load(automerge::OnPartialLoad::Error),
            ) {
                Ok(_) | Err(automerge::AutomergeError::MissingDeps) => {}
                Err(_) => bail!("malformed incremental bytes"),
            }
            let mut probe = doc.clone();
            tilo_automerge::ApplyIncoming::load_incoming(&mut probe, mutation.data())?;
            if complete(&probe).is_err() {
                deferred.push((route, mutation, author, seq));
                continue;
            }
            let before = doc.base_heads();
            let floor = coverages[&coverage::route_key(&route.0, &route.1)]
                .watermarks
                .get(&hex::encode(author))
                .copied()
                .unwrap_or(0);
            ensure!(
                seq > floor || probe.base_heads() == before,
                "covered sequence contains changes absent from checkpoint"
            );
            tilo_peer_state::document::merge_committed(doc, &mutation)
                .map_err(|_| anyhow::anyhow!("strict document merge failed"))?;
            complete(doc)?;
        }
        ensure!(
            deferred.len() < old,
            "incomplete: unresolved tail causal dependencies"
        );
        pending = deferred;
    }
    for ((app, record), doc) in &docs {
        complete(doc)?;
        canonical(&b.room, app, record, doc)?;
    }
    ensure!(
        !b.records.is_empty(),
        "bootstrap has no expected canonical routes"
    );
    let expected = b
        .records
        .iter()
        .map(|r| (r.app_id.clone(), r.record_id.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    ensure!(
        docs.keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            == expected,
        "missing or unexpected canonical route"
    );
    Ok(Recovered { docs, checks })
}
/// A readable projection accompanies, never replaces, the complete CRDT bytes.
fn readable(app: &str, doc: &AutoCommit) -> Result<Value> {
    if app == "so.tilo.word-processor" {
        let containers = tilo_doc::model::open(doc, &automerge::ROOT)?;
        let snapshot = tilo_doc::model::materialize(doc, &containers);
        let text = snapshot
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        Ok(
            json!({"app_id":app,"body_text":text,"rich_document":snapshot,"projection_note":"Body text is a convenience projection; rich_document and canonical bytes preserve structure."}),
        )
    } else if app == "so.tilo.chat" {
        let channels=chat::doc::ChatDoc::list_channels(doc).into_iter().map(|channel| {
            let messages=chat::doc::ChatDoc::list_messages(doc,&channel.id).into_iter().map(|message|json!({"id":message.id,"author":message.author,"timestamp":message.ts,"text":message.text,"parent":message.parent,"edited":message.edited,"history":message.history.iter().map(|h|json!({"timestamp":h.ts,"text":h.text})).collect::<Vec<_>>(),"attachment_present":message.attachment.is_some()})).collect::<Vec<_>>();
            json!({"id":channel.id,"name":channel.name,"archived":channel.archived,"draft":chat::doc::ChatDoc::draft_text(doc,&channel.id),"messages":messages})
        }).collect::<Vec<_>>();
        Ok(
            json!({"app_id":app,"channels":channels,"projection_note":"Canonical channel/message/draft API projection; full CRDT preserves other app fields and history. External attachment bytes are not in this archive export."}),
        )
    } else {
        bail!("unsupported readable app")
    }
}
fn create_private_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700).create(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::{
        io::Write,
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
fn read_source(path: &Path, name: String) -> Result<Source> {
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "archive input must be a regular non-symlink file"
    );
    ensure!(
        metadata.len() <= MAX_SOURCE_BYTES,
        "archive source too large"
    );
    let bytes = std::fs::read(path)?;
    let after = std::fs::symlink_metadata(path)?;
    ensure!(
        after.len() == metadata.len()
            && after.modified()? == metadata.modified()?
            && bytes.len() as u64 == metadata.len(),
        "source changed while being read"
    );
    Ok(Source { name, bytes })
}
fn tree(root: &Path, relative: &Path, sources: &mut Vec<Source>) -> Result<()> {
    let path = root.join(relative);
    let metadata = std::fs::symlink_metadata(&path)?;
    ensure!(
        !metadata.file_type().is_symlink(),
        "archive symlink refused"
    );
    if metadata.is_dir() {
        let mut paths = std::fs::read_dir(path)?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        for name in paths {
            tree(root, &relative.join(name), sources)?;
        }
    } else {
        ensure!(sources.len() < 4096, "too many archive sources");
        sources.push(read_source(
            &path,
            format!(
                "archive/{}",
                relative
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("non-UTF8 archive filename"))?
            ),
        )?);
        ensure!(
            sources.iter().map(|s| s.bytes.len()).sum::<usize>() <= 256 * 1024 * 1024,
            "archive exceeds size bound"
        );
    }
    Ok(())
}
#[derive(clap::Parser)]
#[command(about = "Offline historical archive inspection; never live admission or migration")]
struct Arguments {
    archive_root: PathBuf,
    public_enrollment: PathBuf,
    old_ready: PathBuf,
    /// A new private inspection-UUID directory is created beneath this directory.
    output_parent: PathBuf,
    #[arg(long)]
    expected_chat_head: Option<String>,
    #[arg(long)]
    expected_writer_head: Option<String>,
}
fn run(arguments: Arguments) -> Result<(PathBuf, bool, usize)> {
    let archive = arguments.archive_root.canonicalize()?;
    let parent = arguments.output_parent.canonicalize()?;
    ensure!(
        !parent.starts_with(&archive),
        "output must not be inside immutable source archive"
    );
    for head in [
        &arguments.expected_chat_head,
        &arguments.expected_writer_head,
    ]
    .into_iter()
    .flatten()
    {
        ensure!(
            hex::decode(head).is_ok_and(|b| b.len() == 32),
            "invalid comparison head"
        );
    }
    let mut sources = vec![];
    tree(&archive, Path::new(""), &mut sources)?;
    let enrollment = read_source(
        &arguments.public_enrollment,
        "public-enrollment.json".into(),
    )?;
    let ready = read_source(&arguments.old_ready, "old-ready.json".into())?;
    sources.push(enrollment.clone());
    sources.push(ready.clone());
    let out = parent.join(format!("inspection-{}", uuid::Uuid::new_v4()));
    create_private_directory(&out)?;
    create_private_directory(&out.join("raw"))?;
    let mut inventory = vec![];
    for (index, source) in sources.iter().enumerate() {
        let target = format!("raw/source-{index:04}.bin");
        write_private(&out.join(&target), &source.bytes)?;
        inventory.push(json!({"source":source.name,"bytes":source.bytes.len(),"blake3":blake3::hash(&source.bytes).to_hex().to_string(),"raw_copy":target}));
    }
    // Invalid/missing authentication or causal material leaves only raw evidence
    // and an incomplete manifest. No partially reconstructed placeholder escapes.
    let result = (|| -> Result<(PublicEnrollment, Recovered)> {
        let auth: PublicEnrollment = serde_json::from_slice(&enrollment.bytes)?;
        let ready: Value = serde_json::from_slice(&ready.bytes)?;
        ensure!(
            ready["room"] == auth.bootstrap.room
                && ready["workspace_id"] == auth.bootstrap.workspace_id,
            "ready/bootstrap room or workspace mismatch"
        );
        let prefix = format!(
            "archive/workspaces/{}/retained/",
            auth.bootstrap.workspace_id
        );
        let mut retained = sources
            .iter()
            .filter_map(|s| {
                s.name.strip_prefix(&prefix).map(|name| Source {
                    name: name.into(),
                    bytes: s.bytes.clone(),
                })
            })
            .collect::<Vec<_>>();
        // Per-channel checkpoint followed by lexicographic tails, as in store.rs.
        retained.sort_by_key(|s| {
            let mut parts = s.name.split('/');
            let kind = parts.next().unwrap_or("");
            (
                parts.next().unwrap_or("").to_owned(),
                kind != "checkpoints",
                s.name.clone(),
            )
        });
        ensure!(!retained.is_empty(), "retained source set is empty");
        let recovered = inspect(&auth, &retained)?;
        Ok((auth, recovered))
    })();
    let mut manifest = json!({"format":"tilo-offline-archive-inspection-v1","verification_mode":"historical_content_only","live_admission":false,"governance_authenticity":"unsigned room and coverage: recorded authenticated-transport provenance only; not independently signed authority","trust_anchor":"root/governor supplied by preserved public enrollment; not external identity attestation","browser_only_outbox_and_drafts":"not captured; IndexedDB untouched","external_assets":"not captured; canonical references and embedded content preserved","history_limits":"retained checkpoints and tails are not the full signed operation log or historical governance/revocation timeline","sources":inventory,"documents":[]});
    let mut documents = vec![];
    let complete = match result {
        Ok((auth, recovered)) => {
            manifest["source_room"] = json!(auth.bootstrap.room);
            manifest["workspace_id"] = json!(auth.bootstrap.workspace_id);
            manifest["directory_signature"] = json!("verified at signed authored timestamp");
            manifest["operation_checks"] = json!(recovered.checks);
            // Preflight every readable projection before writing any documents.
            let projections = recovered
                .docs
                .iter()
                .map(|((app, _), doc)| readable(app, doc))
                .collect::<Result<Vec<_>>>();
            match projections {
                Err(error) => {
                    manifest["status"] = json!("incomplete");
                    manifest["reason"] =
                        json!(format!("readable schema projection failed: {error}"));
                    false
                }
                Ok(projections) => {
                    for (index, (((app, record), doc), projection)) in
                        recovered.docs.iter().zip(projections).enumerate()
                    {
                        let heads = doc
                            .base_heads()
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>();
                        let bytes = doc.clone().save();
                        ensure!(
                            strict_load(&bytes)?.base_heads() == doc.base_heads(),
                            "export serialization altered heads"
                        );
                        let canonical = format!("document-{index}.automerge");
                        let readable = format!("document-{index}.readable.json");
                        write_private(&out.join(&canonical), &bytes)?;
                        write_private(
                            &out.join(&readable),
                            &serde_json::to_vec_pretty(&projection)?,
                        )?;
                        let expected = if app == "so.tilo.chat" {
                            &arguments.expected_chat_head
                        } else {
                            &arguments.expected_writer_head
                        };
                        let comparison = match expected {
                            None => "not_supplied",
                            Some(h) if heads == vec![h.clone()] => "exact_match",
                            Some(h) => {
                                let hash = h.parse::<automerge::ChangeHash>()?;
                                if doc.get_change_by_hash(&hash).is_some() {
                                    "causal_descendant_of_observed_head"
                                } else {
                                    "mismatch_recovery_gap"
                                }
                            }
                        };
                        let counts = if app == "so.tilo.chat" {
                            let channels = projection["channels"]
                                .as_array()
                                .ok_or_else(|| anyhow::anyhow!("missing channel projection"))?;
                            json!({"channels":channels.len(),"messages":channels.iter().map(|c|c["messages"].as_array().map_or(0,Vec::len)).sum::<usize>(),"draft_characters":channels.iter().map(|c|c["draft"].as_str().unwrap_or("").chars().count()).sum::<usize>()})
                        } else {
                            json!({"body_characters":projection["body_text"].as_str().unwrap_or("").chars().count()})
                        };
                        documents.push(json!({"app_id":app,"record_id":record,"heads":heads,"causally_complete":true,"canonical_record_schema":"verified","canonical_file":canonical,"canonical_blake3":blake3::hash(&bytes).to_hex().to_string(),"readable_file":readable,"readable_blake3":blake3::hash(&serde_json::to_vec_pretty(&projection)?).to_hex().to_string(),"observed_final_head":expected,"observed_head_comparison":comparison,"counts":counts}));
                    }
                    manifest["status"] = json!("historical_content_verified");
                    true
                }
            }
        }
        Err(error) => {
            manifest["status"] = json!("incomplete");
            manifest["reason"] = json!(error.to_string());
            false
        }
    };
    let count = documents.len();
    manifest["documents"] = json!(documents);
    write_private(
        &out.join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    std::fs::File::open(&out)?.sync_all()?;
    Ok((out, complete, count))
}
// The unit tests below open documents through the isolated Automerge
// worker; the test harness serves it in place of `main`.
tilo_automerge_isolate::test_worker!();

fn main() -> Result<()> {
    use clap::Parser;
    let (out, complete, count) = run(Arguments::parse())?;
    // Never print document text, credentials, keys, or raw validation payloads.
    println!(
        "{}; documents={count}; manifest={}",
        if complete {
            "historical content verified"
        } else {
            "incomplete archive"
        },
        out.join("manifest.json").display()
    );
    ensure!(complete, "archive incomplete; consult private manifest");
    Ok(())
}

#[cfg(test)]
#[path = "inspect-peer-archive/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "support/peer_fixture.rs"]
mod peer_fixture;
