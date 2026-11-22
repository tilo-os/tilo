//! A device joining a workspace takes in what its host holds at the same cost
//! per record however much the workspace holds: a newcomer to a tracker of
//! eighty parts waits for eighty parts, not for something that grows faster
//! than they do or never arrives at all.

use super::{DIRECT_ALPN, DirectSync, Replica};
use crate::owner::testing::{APP, WORKSPACE, finalize, governed_fixture_at, member_key};
use crate::owner::{NativeWorkspaceOwner, open_file};
use crate::store::{Appetite, Store};
use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable as _};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, endpoint::presets};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tilo_desktop_model::WorkspaceId;

/// The bytes of one part's text: eighty of them are more than a link takes
/// in one frame, as the parts of a tracker of 16,000 issues are.
const PART_TEXT: usize = 1536 * 1024;

/// The changes each part takes after it started: an item each.
const CHANGES_EACH: usize = 6;

/// A localhost endpoint of its own whose identity is the device `key`.
async fn endpoint(key: &SigningKey) -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .secret_key(SecretKey::from_bytes(&key.to_bytes()))
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap()
}

fn addr(endpoint: &Endpoint) -> EndpointAddr {
    EndpointAddr::new(endpoint.id()).with_ip_addr(std::net::SocketAddr::from((
        std::net::Ipv4Addr::LOCALHOST,
        endpoint.bound_sockets().first().unwrap().port(),
    )))
}

const ALPHABET: &[u8; 64] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 .";

/// Text that does not compress: what a part's titles and descriptions are to
/// a deflate pass.
fn text(seed: usize) -> String {
    let mut state = u64::try_from(seed)
        .unwrap()
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        | 1;
    (0..PART_TEXT)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            char::from(ALPHABET[usize::try_from(state % 64).unwrap()])
        })
        .collect()
}

/// A device that joined a workspace whose host keeps `parts` records of a part's
/// text and [`CHANGES_EACH`] changes each, dialling that host: how long it took
/// to take in what the host holds, how many commits its owner made doing so,
/// and how many of the parts it then held whole.
fn join(parts: usize) -> (Duration, u64, usize) {
    let ((intent, bootstrap, _, _, now), mut governor, key) =
        governed_fixture_at(tilo_clock::wall());
    let directory = tempfile::tempdir().unwrap();
    let owner = open_file(&directory.path().join("joined-workspaces.json")).unwrap();
    let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    owner
        .install(&finalize(intent, pending, bootstrap).unwrap(), now)
        .unwrap();
    let workspace = WorkspaceId::parse(WORKSPACE).unwrap();
    // The parts as the host wrote them: each started with its text, then
    // changed an item at a time, every change kept at the host's store.
    let mut written = Vec::new();
    for part in 0..parts {
        let record = format!("part:{part}");
        let mut document = AutoCommit::new();
        document.put(ROOT, "text", text(part)).unwrap();
        document.commit_edit("issues", "text");
        written.push(
            governor
                .publication_mut()
                .publish_document(APP, &record, document.save(), &key, now)
                .unwrap(),
        );
        for change in 0..CHANGES_EACH {
            let before = document.get_heads();
            document
                .put(ROOT, format!("issue {change}"), "a title")
                .unwrap();
            document.commit_edit("issues", "items");
            written.push(
                governor
                    .publication_mut()
                    .publish_incremental(APP, &record, document.save_after(&before), &key, now)
                    .unwrap(),
            );
        }
    }
    let store = Arc::new(Store::open(directory.path().join("store"), Appetite::StoreAll).unwrap());
    let host = DirectSync::over(Replica::new(governor), key.clone()).with_store(store);
    for frame in &written {
        host.keep(frame, false).unwrap();
    }
    drop(written);
    let generation = |owner: &NativeWorkspaceOwner| {
        owner
            .with_current(workspace, |view| Ok(view.generation()))
            .unwrap()
    };
    let before = generation(&owner);
    let member = DirectSync::over(owner.replica(workspace).unwrap(), member_key())
        .owning(owner.clone(), workspace)
        .unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let took = runtime.block_on(async {
        let host_endpoint = endpoint(&key).await;
        let router = iroh::protocol::Router::builder(host_endpoint.clone())
            .accept(DIRECT_ALPN, host.clone())
            .spawn();
        let member_endpoint = endpoint(&member_key()).await;
        let mut caught_up = member.caught_up();
        let started = Instant::now();
        let linking = tokio::spawn({
            let member = member.clone();
            let member_endpoint = member_endpoint.clone();
            let host_addr = addr(&host_endpoint);
            async move { member.connect(&member_endpoint, host_addr).await }
        });
        let caught = tokio::time::timeout(Duration::from_secs(600), caught_up.wait_for(|up| *up))
            .await
            .map(|_| started.elapsed());
        let ended = if linking.is_finished() {
            Some(linking.await)
        } else {
            linking.abort();
            None
        };
        router.shutdown().await.unwrap();
        member_endpoint.close().await;
        caught.unwrap_or_else(|_| {
            panic!("a newcomer to {parts} parts never took the host's records in: {ended:?}")
        })
    });
    let held = (0..parts)
        .filter(|part| {
            member.read(|peer| {
                peer.document(APP, &format!("part:{part}"))
                    .and_then(|bytes| AutoCommit::load(&bytes).ok())
                    .is_some_and(|document| document.length(ROOT) == CHANGES_EACH + 1)
            })
        })
        .count();
    (took, generation(&owner).saturating_sub(before), held)
}

#[tokio::test]
async fn a_link_does_not_report_a_frame_it_could_not_keep() {
    let now = tilo_clock::wall();
    let (_, mut governor, key) = governed_fixture_at(now);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store");
    let store = Arc::new(Store::open(&path, Appetite::StoreAll).unwrap());
    let link = DirectSync::over(Replica::new(governor.clone()), key.clone()).with_store(store);
    let mut heard = link.0.heard.subscribe();
    let mut document = AutoCommit::new();
    document.put(ROOT, "text", "first").unwrap();
    document.commit_edit("issues", "text");
    let keys = key_group::KeyGroup::create(
        WorkspaceId::parse(WORKSPACE).unwrap(),
        key_group::MemberKey::generate("link-test").unwrap(),
    )
    .unwrap()
    .content_keys()
    .unwrap();
    let artifact = collab_types::document_artifact_id(APP, WORKSPACE, "part:0");
    let message = governor
        .publication_mut()
        .publish_sealed_checkpoint(
            APP,
            "part:0",
            document.save(),
            &mut |plaintext| {
                keys.seal_content(
                    artifact,
                    collab_types::ContentRetention::Checkpoint,
                    plaintext,
                )
                .map_err(|error| error.to_string())
            },
            &key,
            now,
        )
        .unwrap();
    let author = key.verifying_key().to_bytes();
    std::fs::remove_dir_all(&path).unwrap();
    std::fs::write(&path, []).unwrap();

    assert_eq!(
        link.admit_kept(author, message.clone(), false, false).await,
        (true, None)
    );
    assert!(heard.try_recv().is_err());

    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        link.admit_kept(author, message, false, false).await,
        (false, Some(super::Kept::Durable))
    );
    assert!(heard.try_recv().unwrap().report.is_ok());
}

#[test]
fn a_newcomer_to_eighty_parts_takes_each_in_as_fast_as_a_newcomer_to_two() {
    // Enough pairs that the median holds while the machine runs other work: a
    // join takes seconds, so one of them can be slowed by a burst.
    const PAIRS: usize = 3;
    let mut ratios: Vec<f64> = Vec::with_capacity(PAIRS);
    for pair in 0..PAIRS {
        // A newcomer to eighty parts is timed right after one to two, so a
        // burst of other work slows the pair alike: the median of the pairs'
        // ratios is what eighty parts cost a part, whatever the machine did
        // meanwhile.
        let (small, small_commits, small_held) = join(2);
        let (large, large_commits, large_held) = join(80);
        assert_eq!(small_held, 2, "the newcomer holds both parts");
        assert_eq!(large_held, 80, "the newcomer holds all eighty parts");
        assert!(
            large_commits < 40,
            "taking in 80 parts committed the device's workspace {large_commits} times"
        );
        let per_part = |took: Duration, parts: u32| took.as_secs_f64() / f64::from(parts);
        let ratio = per_part(large, 80) / per_part(small, 2);
        eprintln!(
            "pair {pair}: 2 parts: {small:?}, {small_commits} commits; 80 parts: {large:?}, \
             {large_commits} commits; per part, large/small: {ratio:.2}"
        );
        ratios.push(ratio);
    }
    ratios.sort_by(f64::total_cmp);
    let ratio = ratios[PAIRS / 2];
    assert!(
        ratio < 3.0,
        "a part of 80 took {ratio:.1} times as long as a part of 2 (pairs' ratios: {ratios:.2?})"
    );
}

/// `key`'s channel governance for the workspace's group `group`: its genesis,
/// its commit at `epoch`, or its access request.
fn channel_frame(key: &SigningKey, group: collab_types::WorkspaceId, kind: u8) -> crate::NeighbourMsg {
    use collab_types::{MlsAccessRequestV1, MlsGovernanceAction, MlsGovernanceFrame};
    match kind {
        0 => crate::NeighbourMsg::MlsGovernance {
            workspace: WORKSPACE.into(),
            frame: Box::new(
                MlsGovernanceFrame::signed(group, [0; 32], 0, MlsGovernanceAction::Genesis, key)
                    .unwrap(),
            ),
            delivery: 0,
        },
        2 => crate::NeighbourMsg::MlsAccessRequest {
            workspace: WORKSPACE.into(),
            request: Box::new(MlsAccessRequestV1::signed(group, vec![1], 1, key).unwrap()),
            delivery: 0,
        },
        epoch => crate::NeighbourMsg::MlsGovernance {
            workspace: WORKSPACE.into(),
            frame: Box::new(
                MlsGovernanceFrame::signed(
                    group,
                    [epoch; 32],
                    u64::from(epoch),
                    MlsGovernanceAction::Commit {
                        removed: Vec::new(),
                        added: Vec::new(),
                        commit: vec![epoch],
                        joining: None,
                        history: None,
                    },
                    key,
                )
                .unwrap(),
            ),
            delivery: 0,
        },
    }
}

/// A device that links the workspace's host after a restart is handed every
/// channel's genesis, its newest commit and each device's newest access
/// request, read from the host's store alone: what it joins each channel's
/// key group from. A commit the newest replaced is not handed.
#[tokio::test(flavor = "multi_thread")]
async fn a_newcomer_joins_each_channel_from_what_its_host_kept_at_its_store() {
    let ((intent, bootstrap, _, _, now), governor, key) = governed_fixture_at(tilo_clock::wall());
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store");
    let group = collab_types::WorkspaceId::new();
    let founder = SigningKey::from_bytes(&[41; 32]);
    let genesis = channel_frame(&founder, group, 0);
    let first = channel_frame(&founder, group, 1);
    let newest = channel_frame(&founder, group, 3);
    let request = channel_frame(&key, group, 2);
    {
        let store = Arc::new(Store::open(&path, Appetite::StoreAll).unwrap());
        let host =
            DirectSync::over(Replica::new(governor.clone()), key.clone()).with_store(store);
        let serving = super::Serving::default();
        serving.serve(host.clone()).unwrap();
        let founder_device = founder.verifying_key().to_bytes();
        // What reached the host from a member's session and from its own
        // channel service, carried onto the link.
        host.remember_genesis(founder_device, &genesis);
        assert!(host.carry(founder_device, &first));
        assert!(host.carry(founder_device, &newest));
        host.send(request.clone()).unwrap();
    }

    // The host starts again over its store, with nothing else of the
    // channels' history: no server's retained copy is read.
    let store = Arc::new(Store::open(&path, Appetite::StoreAll).unwrap());
    let host = DirectSync::over(Replica::new(governor), key.clone()).with_store(store);
    let serving = super::Serving::default();
    serving.serve(host.clone()).unwrap();
    let owner = open_file(&directory.path().join("joined-workspaces.json")).unwrap();
    let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    owner
        .install(&finalize(intent, pending, bootstrap).unwrap(), now)
        .unwrap();
    let workspace = WorkspaceId::parse(WORKSPACE).unwrap();
    let newcomer = DirectSync::over(owner.replica(workspace).unwrap(), member_key())
        .owning(owner.clone(), workspace)
        .unwrap();
    let mut heard = newcomer.heard();
    let host_endpoint = endpoint(&key).await;
    let router = iroh::protocol::Router::builder(host_endpoint.clone())
        .accept(DIRECT_ALPN, host.clone())
        .spawn();
    let newcomer_endpoint = endpoint(&member_key()).await;
    let linking = tokio::spawn({
        let newcomer = newcomer.clone();
        let newcomer_endpoint = newcomer_endpoint.clone();
        let host_addr = addr(&host_endpoint);
        async move { newcomer.connect(&newcomer_endpoint, host_addr).await }
    });
    let wanted = [genesis, newest, request];
    let mut handed = Vec::new();
    let handed_all = tokio::time::timeout(Duration::from_secs(60), async {
        while !wanted.iter().all(|frame| handed.contains(frame)) {
            let Ok(frame) = heard.recv().await else {
                break;
            };
            if frame.message.governance_workspace().is_some() {
                handed.push(frame.message);
            }
        }
    })
    .await;
    linking.abort();
    router.shutdown().await.unwrap();
    newcomer_endpoint.close().await;
    assert!(handed_all.is_ok(), "the newcomer was handed only {handed:?}");
    assert_eq!(
        handed.iter().position(|frame| *frame == wanted[0]),
        Some(0),
        "the group's genesis comes before what builds on it"
    );
    assert!(
        !handed.contains(&first),
        "the commit the newest replaced is not handed"
    );
}
