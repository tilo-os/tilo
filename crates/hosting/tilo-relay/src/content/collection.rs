//! Letting go of content no workspace references any more.
//!
//! A workspace's store only grows by itself: a file replaced in the Files
//! app leaves its old manifest and chunks behind, an upload a record stopped
//! naming stays, and so does a picture a message no longer shows. So, now
//! and then, the host reads every record of each workspace it governs and
//! removes from that workspace's store what none of them names.
//!
//! What a record names is any content address in it, anywhere in its
//! history: a version the workspace can go back to keeps its content. And
//! what is named keeps what it is made of, the chunks a file's manifest
//! lists. Content written lately is spared ([`GRACE`]): an upload is kept
//! before the record that names it is written, and writing content that is
//! already here counts as writing it.
//!
//! A pass reads a workspace's records from a copy of its state taken in a
//! moment, and holds the workspace only while it removes what they do not
//! name: then, a record written since it was read is read again first, so
//! content a write names now stays.
//!
//! Nothing is removed from a workspace whose records the host cannot all
//! read whole (one sealed for a channel the host has no keys of, or with a
//! write that does not open yet): what it cannot read may name anything.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use automerge::{AutoCommit, ObjId, ObjType, ReadDoc as _, ScalarValue, ValueRef};
use fs::content::Manifest;
use tilo_content::{Collected, WorkspaceStores};

use crate::workspaces::{WholeRecords, Workspaces};

/// How long content stays after it was last written, named or not.
pub const GRACE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// How often the host looks for content its workspaces no longer name.
const EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// How often a pass reads again the records written while it read them,
/// before it leaves the workspace to the next pass.
const ATTEMPTS: usize = 4;
/// The length of a content address: BLAKE3, in hex.
const ADDRESS_LEN: usize = 64;

/// Collect what the workspaces governed in `workspaces` no longer reference
/// from their stores in `stores`, now and every few hours from now on. A
/// pass holds a workspace only while it removes what nothing names, so the
/// workspace goes on taking writes while its records are read.
pub async fn run(workspaces: Workspaces, stores: Arc<WorkspaceStores>) {
    let mut every = tokio::time::interval(EVERY);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        let before = SystemTime::now()
            .checked_sub(GRACE)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        for summary in workspaces.list().await {
            collect(&workspaces, &stores, &summary.workspace, before).await;
        }
    }
}

/// Remove from `workspace`'s store what none of its records names and was
/// last written before `before`. `None` when nothing was removed for want of
/// knowing what the records name: they could not all be read whole, or
/// they kept changing while the pass read them.
pub async fn collect(
    workspaces: &Workspaces,
    stores: &Arc<WorkspaceStores>,
    workspace: &str,
    before: SystemTime,
) -> Option<Collected> {
    collect_after(workspaces, stores, workspace, before, async {}).await
}

/// [`collect`], waiting for `scanned` between reading the records and
/// removing what they do not name.
async fn collect_after(
    workspaces: &Workspaces,
    stores: &Arc<WorkspaceStores>,
    workspace: &str,
    before: SystemTime,
    scanned: impl std::future::Future<Output = ()>,
) -> Option<Collected> {
    let store = stores.get(workspace).ok()?;
    // Read with no hold on the workspace: it takes writes all the while.
    let mut read = whole_records(workspaces, workspace, None).await?;
    let mut live = names(workspace, &read.documents).await?;
    scanned.await;
    for _ in 0..ATTEMPTS {
        let mut named = live.clone();
        made_of(&|hash| store.read(hash), &mut named);
        // Removed with the workspace held, and only if no record changed
        // since it was read: a write that names an item now keeps it. The
        // store spares what was written within the grace on its own.
        let removed = workspaces
            .while_unwritten(workspace, &read, || {
                stores.collect(workspace, &named, before)
            })
            .await;
        let changed = match removed {
            Ok(Ok(Ok(collected))) => {
                if !collected.hashes.is_empty() {
                    tracing::info!(
                        workspace,
                        items = collected.hashes.len(),
                        bytes = collected.bytes,
                        "let go of content the workspace no longer references"
                    );
                }
                return Some(collected);
            }
            Ok(Ok(Err(error))) => {
                tracing::warn!(workspace, %error, "could not collect a workspace's unreferenced content");
                return None;
            }
            Err(error) => {
                tracing::debug!(workspace, %error, "a workspace's records are not readable here");
                return None;
            }
            Ok(Err(changed)) => changed,
        };
        let newer = whole_records(workspaces, workspace, Some(&changed)).await?;
        live.extend(names(workspace, &newer.documents).await?);
        read.extend(newer);
    }
    tracing::debug!(
        workspace,
        "the workspace kept changing while its content was looked over; the next pass collects it"
    );
    None
}

/// The records of `workspace` read whole, or of `only` among them; `None`
/// when they do not all read whole.
async fn whole_records(
    workspaces: &Workspaces,
    workspace: &str,
    only: Option<&BTreeSet<(String, String)>>,
) -> Option<WholeRecords> {
    match workspaces.whole_records(workspace, only).await {
        Ok(records) if records.is_whole() => Some(records),
        Ok(records) => {
            tracing::info!(
                workspace,
                unread = records.unread.len(),
                "not every record of the workspace reads whole; its content is kept as it is"
            );
            None
        }
        Err(error) => {
            tracing::debug!(workspace, %error, "a workspace's records are not readable here");
            None
        }
    }
}

/// A record by its app and id.
type Route = (String, String);

/// Every content address `documents` name, and the records among them that
/// do not load, with why; each of those is logged.
async fn names_read(
    workspace: &str,
    documents: &[(Route, Vec<u8>)],
) -> (BTreeSet<String>, Vec<(Route, String)>) {
    let documents = documents.to_vec();
    let workspace = workspace.to_owned();
    // Reading every record is work for a thread that may block.
    tokio::task::spawn_blocking(move || {
        let mut live = BTreeSet::new();
        let mut unread = Vec::new();
        for ((app, record), document) in documents {
            if let Err(why) = named_in(&document, &mut live) {
                tracing::warn!(
                    workspace,
                    %app,
                    %record,
                    %why,
                    "a record of the workspace does not load"
                );
                unread.push(((app, record), format!("it does not load: {why}")));
            }
        }
        (live, unread)
    })
    .await
    .unwrap_or_else(|_| {
        (
            BTreeSet::new(),
            vec![(Route::default(), "reading the records stopped".to_owned())],
        )
    })
}

/// Every content address `documents` name; `None` when one does not load.
async fn names(workspace: &str, documents: &[(Route, Vec<u8>)]) -> Option<BTreeSet<String>> {
    let (live, unread) = names_read(workspace, documents).await;
    unread.is_empty().then_some(live)
}

/// What the records of a workspace that read name ([`named`]).
pub struct Named {
    /// Every content address they name, with what that content is made of.
    pub names: BTreeSet<String>,
    /// The records that do not read whole or load, by app and id, with why:
    /// what they name is not among [`Self::names`].
    pub unread: Vec<(Route, String)>,
}

/// Every content address the records of `workspace` that read whole name, in
/// any version of them, with what that content is made of: the chunks of
/// each file manifest `read` finds among them. A record that does not read
/// whole is logged and listed in [`Named::unread`], and the rest are read all
/// the same. `None` when the workspace is not governed here.
pub async fn named(
    workspaces: &Workspaces,
    workspace: &str,
    read: impl Fn(&str) -> Option<Vec<u8>> + Send + 'static,
) -> Option<Named> {
    let records = match workspaces.whole_records(workspace, None).await {
        Ok(records) => records,
        Err(error) => {
            tracing::warn!(workspace, %error, "a workspace's records are not readable here");
            return None;
        }
    };
    let (mut live, mut unread) = names_read(workspace, &records.documents).await;
    unread.extend(records.unread);
    let names = tokio::task::spawn_blocking(move || {
        made_of(&read, &mut live);
        live
    })
    .await
    .ok()?;
    Some(Named { names, unread })
}

/// Add every content address `record` (a saved Automerge document) names to
/// `live`: in any value its history ever held, and in the text it holds now.
///
/// # Errors
///
/// Why it does not load.
fn named_in(record: &[u8], live: &mut BTreeSet<String>) -> Result<(), String> {
    let mut doc = AutoCommit::load(record).map_err(|error| error.to_string())?;
    for change in doc.get_changes(&[]) {
        // Text typed in one change lies in one run of its values.
        addresses_in(change.raw_bytes(), live);
        for op in change.decode().operations {
            match op.action {
                automerge::legacy::OpType::Put(ScalarValue::Str(text)) => {
                    addresses_in(text.as_bytes(), live);
                }
                automerge::legacy::OpType::Put(ScalarValue::Bytes(bytes)) => {
                    if bytes.len() == ADDRESS_LEN / 2 {
                        live.insert(hex::encode(&bytes));
                    }
                    addresses_in(&bytes, live);
                }
                _ => {}
            }
        }
    }
    // Text is written a character at a time: an address in it is whole only
    // in the text it makes.
    texts_in(&doc, &automerge::ROOT, live);
    Ok(())
}

/// Add the addresses in the text objects under `obj` of `doc` to `live`.
fn texts_in(doc: &AutoCommit, obj: &ObjId, live: &mut BTreeSet<String>) {
    let children: Vec<(ObjType, ObjId)> = match doc.object_type(obj) {
        Ok(ObjType::Map | ObjType::Table) => doc
            .map_range(obj, ..)
            .filter_map(|item| match item.value {
                ValueRef::Object(kind) => Some((kind, item.id())),
                ValueRef::Scalar(_) => None,
            })
            .collect(),
        Ok(ObjType::List) => doc
            .list_range(obj, ..)
            .filter_map(|item| match item.value {
                ValueRef::Object(kind) => Some((kind, item.id())),
                ValueRef::Scalar(_) => None,
            })
            .collect(),
        Ok(ObjType::Text) => {
            if let Ok(text) = doc.text(obj) {
                addresses_in(text.as_bytes(), live);
            }
            return;
        }
        Err(_) => return,
    };
    for (_, child) in children {
        texts_in(doc, &child, live);
    }
}

/// Add each run of exactly [`ADDRESS_LEN`] hex digits in `bytes` to `live`,
/// in lower case; a longer run adds each address it could hold.
fn addresses_in(bytes: &[u8], live: &mut BTreeSet<String>) {
    let mut start = 0;
    while start < bytes.len() {
        let run = bytes
            .get(start..)
            .unwrap_or_default()
            .iter()
            .take_while(|byte| byte.is_ascii_hexdigit())
            .count();
        if run >= ADDRESS_LEN {
            for at in start..=start.saturating_add(run).saturating_sub(ADDRESS_LEN) {
                if let Some(address) = bytes.get(at..at.saturating_add(ADDRESS_LEN)) {
                    live.insert(String::from_utf8_lossy(address).to_ascii_lowercase());
                }
            }
        }
        start = start.saturating_add(run).saturating_add(1);
    }
}

/// Add to `live` what the content it names is made of: the chunks each file
/// manifest `read` finds among it lists.
fn made_of(read: &impl Fn(&str) -> Option<Vec<u8>>, live: &mut BTreeSet<String>) {
    let chunks: Vec<String> = live
        .iter()
        .filter_map(|hash| read(hash))
        .filter_map(|bytes| Manifest::decode(&bytes).ok())
        .flat_map(|manifest| {
            manifest
                .chunks()
                .iter()
                .map(|chunk| hex::encode(chunk.hash))
                .collect::<Vec<_>>()
        })
        .collect();
    live.extend(chunks);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use automerge::transaction::Transactable as _;
    use automerge::{AutoCommit, ObjType, ROOT};

    use super::*;

    #[test]
    fn a_record_names_the_content_any_version_of_it_held() {
        let first = "a1".repeat(32);
        let second = "B2".repeat(32);
        let typed = "c3".repeat(32);
        let raw = [7_u8; 32];
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "file", first.as_str()).unwrap();
        doc.commit();
        doc.put(ROOT, "file", format!("tilo.content:{second}"))
            .unwrap();
        doc.put(ROOT, "thumbnail", raw.to_vec()).unwrap();
        let note = doc.put_object(ROOT, "note", ObjType::Text).unwrap();
        doc.commit();
        for (at, character) in typed.chars().enumerate() {
            doc.splice_text(&note, at, 0, &character.to_string())
                .unwrap();
            doc.commit();
        }
        let mut live = BTreeSet::new();
        named_in(&doc.save(), &mut live).unwrap();
        for address in [first, second.to_ascii_lowercase(), typed, hex::encode(raw)] {
            assert!(live.contains(&address), "{address} is named: {live:?}");
        }
    }

    #[test]
    fn only_whole_addresses_are_names() {
        let mut live = BTreeSet::new();
        addresses_in(
            format!("{} and {}", "d4".repeat(31), "e5".repeat(32)).as_bytes(),
            &mut live,
        );
        assert_eq!(live, BTreeSet::from(["e5".repeat(32)]));
    }

    /// A workspace host as tilo.sh runs one, under `state`.
    async fn host(state: &std::path::Path) -> crate::daemon::Relay {
        let root = ed25519_dalek::SigningKey::from_bytes(&[81; 32]);
        let host = ed25519_dalek::SigningKey::from_bytes(&tilo_test_env::node_key());
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: host.verifying_key().to_bytes(),
            device_label: "collecting host".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::host(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
        crate::daemon::Relay::start(
            crate::RelayConfig {
                state_root: state.join("relay"),
                identity_root: state.join("identity"),
                full_replica_status: state.join("status.json"),
                home_peer_bundle: state.join("home-peer.toml"),
                ..Default::default()
            },
            crate::daemon::RelayIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_signing_key: host,
                device_cert: cert,
                revocations: tilo_user_identity::RevocationList::new(),
            },
        )
        .await
        .unwrap()
    }

    /// A pass that has read a workspace's records holds nothing of it while
    /// it looks them over: an edit then lands at once, and content the edit
    /// names stays, though the records the pass read named none of it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the host serves for the whole scenario"
    )]
    async fn a_workspace_takes_edits_while_a_pass_runs_and_keeps_what_they_name() {
        let state = tempfile::tempdir().unwrap();
        let relay = host(state.path()).await;
        let workspaces = relay.workspaces().unwrap().clone();
        let workspace = workspaces.create("Notes").await.unwrap().workspace;
        let governor = workspaces.serving_certificate().clone();
        let stores = relay.content();
        let store = stores.get(&workspace).unwrap();
        let picture = store.put(b"a picture written long ago").unwrap();
        let stray = store.put(b"an upload nothing names").unwrap();
        let long_after = SystemTime::now() + Duration::from_secs(60);

        let (scanned, paused) = tokio::sync::oneshot::channel::<()>();
        let (resume, resumed) = tokio::sync::oneshot::channel::<()>();
        let pass = tokio::spawn({
            let workspaces = workspaces.clone();
            let stores = Arc::clone(&stores);
            let workspace = workspace.clone();
            async move {
                collect_after(&workspaces, &stores, &workspace, long_after, async move {
                    let _ = scanned.send(());
                    let _ = resumed.await;
                })
                .await
            }
        });
        paused.await.unwrap();

        // The pass has read every record and holds the workspace no longer.
        let edited = tokio::time::timeout(
            Duration::from_secs(5),
            workspaces.author_room_record(&workspace, &governor, ("so.tilo.notes", ""), |doc| {
                doc.put(automerge::ROOT, "picture", picture.as_str())
                    .map_err(|error| error.to_string())
            }),
        )
        .await;
        assert!(
            matches!(edited, Ok(Ok(()))),
            "an edit lands while a pass runs: {:?}",
            edited.map(|edit| edit.map_err(|error| error.to_string()))
        );
        assert!(!pass.is_finished(), "the pass was still running");

        resume.send(()).unwrap();
        let collected = pass
            .await
            .unwrap()
            .expect("the workspace's records read whole");
        assert_eq!(collected.hashes, std::slice::from_ref(&stray));
        assert!(
            store.has(&picture),
            "what the edit named after the pass read the records stays"
        );
        assert!(!store.has(&stray));
        relay.shutdown().await;
    }

    #[test]
    fn a_record_that_does_not_load_names_nothing_and_says_so() {
        assert!(named_in(b"not a document", &mut BTreeSet::new()).is_err());
    }

    /// The store a page keeps its content in: the profile's app files, which
    /// outlive the page ([`tilo_content::ContentStore::local_for_app`] where
    /// files have no native path). A second handle over the same profile is
    /// the page after a reload.
    fn page_store(profile: &str) -> tilo_content::ContentStore {
        let content = tilo_files::VPath::new("content").unwrap();
        let location = tilo_paths::AppPaths::memory(profile).data().join(&content);
        tilo_content::ContentStore::new(tilo_content::FilesBackend::new(
            location,
            tilo_content::ContentLimits::default(),
        ))
    }

    /// Whether `frames` is one word that the host keeps none of the content.
    fn lacks(frames: &[Vec<u8>]) -> bool {
        matches!(
            frames,
            [frame] if matches!(
                postcard::from_bytes(frame),
                Ok(neighbour_wire::NeighbourMsg::RecordContentLacks { .. })
            )
        )
    }

    /// What a draft's attachment goes through: the host keeps a copy, lets it
    /// go once no record it reads names it (the draft is the person's own,
    /// sealed for their devices), and has it back from the device that
    /// uploaded it when the draft is sent days later. The device kept its own
    /// upload all along, and answers the host's ask with it, so the message's
    /// attachment reads.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(
        clippy::significant_drop_tightening,
        clippy::too_many_lines,
        reason = "one story: the draft, the attachment the device keeps, and the host's copy going"
    )]
    async fn an_attachment_a_draft_named_comes_back_from_the_device_that_uploaded_it() {
        const APP: &str = "so.tilo.chat";
        let state = tempfile::tempdir().unwrap();
        let relay = host(state.path()).await;
        let workspaces = relay.workspaces().unwrap().clone();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        let governor = workspaces.serving_certificate().clone();
        let host_device = governor.device_public_key;
        let stores = relay.content();
        let store = stores.get(&workspace).unwrap();

        // The device a person writes from keeps the attachment it uploaded,
        // and the host has a copy of it too.
        let attachment: Vec<u8> = (0..=u8::MAX).cycle().take(200 * 1024).collect();
        let device_store = page_store("so.tilo.content");
        let hash = device_store.put(&attachment).unwrap();
        assert_eq!(
            store.put(&attachment).unwrap(),
            hash,
            "the host has the bytes under the address the device names"
        );

        // The draft that names it is the person's own record, sealed for
        // their devices: no record the host reads names the attachment, and
        // it lets its copy go after the grace.
        let long_after = SystemTime::now() + Duration::from_secs(60);
        let collected = collect(&workspaces, &stores, &workspace, long_after)
            .await
            .expect("the workspace's records read whole");
        assert_eq!(collected.hashes, std::slice::from_ref(&hash));
        assert!(
            !store.has(&hash),
            "the host let go of the draft's attachment"
        );
        assert!(
            device_store.read(&hash).as_deref() == Some(attachment.as_slice()),
            "the device kept what it uploaded"
        );
        assert!(
            page_store("so.tilo.content").read(&hash).is_some(),
            "and keeps it across a reload of the page"
        );

        // Days later the draft is sent: the conversation's record names the
        // attachment, and the message the host reads carries it.
        workspaces
            .author_room_record(&workspace, &governor, (APP, "c_general"), |doc| {
                doc.put(automerge::ROOT, "body", "look at this")
                    .map_err(|error| error.to_string())?;
                doc.put(automerge::ROOT, "attachment", hash.as_str())
                    .map_err(|error| error.to_string())?;
                Ok(())
            })
            .await
            .unwrap();
        let records = workspaces.whole_records(&workspace, None).await.unwrap();
        assert!(records.is_whole(), "{:?}", records.unread);
        let sent = records
            .documents
            .iter()
            .find(|((app, record), _)| app == APP && record == "c_general")
            .map(|(_, document)| document.clone())
            .expect("the sent message is one of the workspace's records");
        let recorded = AutoCommit::load(&sent)
            .unwrap()
            .get(automerge::ROOT, "attachment")
            .unwrap()
            .and_then(|(value, _)| value.into_string().ok())
            .expect("the message names its attachment");
        assert_eq!(recorded, hash);

        // The host names it now and keeps none of it: it asks the workspace,
        // and the device answers with what it kept.
        let member = [7; 32];
        let want = postcard::to_allocvec(&neighbour_wire::NeighbourMsg::RecordContentWant {
            requester: neighbour_wire::peer_handle(&member),
            app_id: APP.into(),
            cluster_id: workspace.clone(),
            hash: hash.clone(),
            have: Vec::new(),
        })
        .unwrap();
        assert!(
            lacks(&workspaces.content_answer(&workspace, &want)),
            "the host keeps none of the attachment any more"
        );
        let mut asked = neighbour_wire::content::ContentWants::default();
        let request = asked
            .ask(&host_device, (), (APP, &workspace), &hash, 0)
            .expect("the host asks the workspace for it");
        let pieces = neighbour_wire::content::answer(
            &member,
            &host_device,
            &request,
            |_, cluster, asked| {
                (cluster == workspace)
                    .then(|| device_store.read(asked))
                    .flatten()
            },
        );
        let arrived = pieces
            .iter()
            .find_map(|piece| asked.piece(&host_device, piece, tilo_clock::unix_now_ms()))
            .expect("the device's answer is whole and hashes to what was asked");
        assert_eq!(
            arrived.bytes, attachment,
            "the device handed back its own upload"
        );

        // Kept again, the message's attachment reads.
        store.put(&arrived.bytes).unwrap();
        assert_eq!(
            store.read(&recorded).as_deref(),
            Some(attachment.as_slice()),
            "the message's attachment reads"
        );
        relay.shutdown().await;
    }
}
