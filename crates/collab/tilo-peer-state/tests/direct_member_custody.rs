//! A member's custody of its own writes to a workspace another device hosts, over the direct
//! link: the hosting device answers each write it lands or refuses for good, and the member lets
//! it go. A write refused for good still reaches its writer as the hosting device's verdict, and
//! a sealed write that waited past its epoch leaves renewed, is answered and leaves custody. A
//! cleartext write that waited past the live window lands as it is: the hosting device judges a
//! member's own writes on its link as history.
//!
//! Each test drives what the member's network actor does with its custody (one pass of
//! `Sending::drive`, `retire_answered` on the answers, `renew_outgoing` when a pass held writes
//! back) by the same owner calls, over a real link to a hosting device whose engine owns the
//! workspace.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::common::{addr, device, localhost_endpoint};
use crate::direct_host_link::{
    commit_membership, commit_record, commit_record_at, hint, invitation, joins, owner_at, until,
};

use automerge::{AutoCommit, ROOT, transaction::Transactable};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use iroh::protocol::Router;
use neighbour_wire::NeighbourMsg;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tilo_peer_state::direct::{Answered, DIRECT_ALPN, DirectSync, Kept, Owning};
use tilo_peer_state::join_answer::JoinAnswer;
use tilo_peer_state::owner::{HostAnswer, NativeWorkspaceOwner, OwnerJoinWorkspaces};
use tilo_peer_state::store::{Appetite, Store};
use tilo_user_identity::DeviceCapabilities;
use web_time::SystemTime;

const WORKSPACE: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const APP: &str = "test.app";
const RECORD: &str = "tab:1";

/// A device hosting the workspace its engine owns, serving it on the direct link, and a member
/// device of another person linked to it with the host's record in hand: the member's link
/// syncs the replica its own owner decides on, as a session's link does.
struct Hosted {
    host: NativeWorkspaceOwner,
    host_key: SigningKey,
    host_cert: tilo_user_identity::DeviceCert,
    workspace: tilo_desktop_model::WorkspaceId,
    member: NativeWorkspaceOwner,
    member_key: SigningKey,
    /// The member's person, by their root key.
    member_person: [u8; 32],
    link: DirectSync,
    /// What the host answers of the member's writes, subscribed before the member dialled.
    answers: tokio::sync::broadcast::Receiver<Answered>,
    running: Vec<tokio::task::JoinHandle<Result<(), tilo_relay_protocol::refusal::Refusal>>>,
    router: Router,
    endpoints: [iroh::Endpoint; 2],
    _state: [tempfile::TempDir; 3],
}

impl Hosted {
    async fn new(seed: u8) -> Self {
        let host_root = SigningKey::from_bytes(&[seed; 32]);
        let member_root = SigningKey::from_bytes(&[seed.wrapping_add(1); 32]);
        let (host_key, host_cert) = crate::common::device_with(
            &host_root,
            seed.wrapping_add(2),
            DeviceCapabilities::primary(),
        );
        let (member_key, member_cert) = device(&member_root, seed.wrapping_add(3));
        let host_endpoint = localhost_endpoint(&host_key).await;
        let host_state = tempfile::tempdir().unwrap();
        let host = owner_at(host_state.path());
        let workspace = tilo_desktop_model::WorkspaceId::parse(WORKSPACE).unwrap();
        let route = hint(workspace, &host_endpoint, &host_cert, &host_key);
        host.create_workspace(
            workspace,
            "Hosted",
            &host_cert,
            &host_key,
            Some(&route),
            SystemTime::now(),
        )
        .await
        .expect("the workspace is founded here");
        let frames = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(frames.path(), Appetite::StoreAll).unwrap());
        let serving = Owning::new(host.clone(), host_key.clone(), store);
        let router = Router::builder(host_endpoint.clone())
            .accept(DIRECT_ALPN, serving)
            .spawn();
        commit_record(&host, workspace, &host_key, "from the host");

        let service: Arc<dyn tilo_peer_state::join_transport::LocalJoinService> = Arc::new(
            JoinAnswer(OwnerJoinWorkspaces::new(host.clone(), host_key.clone())),
        );
        let member_state = tempfile::tempdir().unwrap();
        let (member, joined) = joins(
            member_state.path(),
            &member_cert,
            &member_key,
            invitation(&host, workspace, &host_cert, &host_key, route, [seed; 32]).await,
            service,
        )
        .await;
        assert_eq!(joined, workspace);
        let link = DirectSync::over(member.replica(workspace).unwrap(), member_key.clone())
            .owning(member.clone(), workspace)
            .unwrap();
        let answers = link.answers();
        let mut caught_up = link.caught_up();
        let member_endpoint = localhost_endpoint(&member_key).await;
        let running = tokio::spawn({
            let (link, member_endpoint, host_addr) =
                (link.clone(), member_endpoint.clone(), addr(&host_endpoint));
            async move { link.connect(&member_endpoint, host_addr).await }
        });
        until(&link, "the member has the host's record", |peer| {
            peer.document(APP, RECORD).is_some()
        })
        .await;
        tokio::time::timeout(Duration::from_secs(30), caught_up.wait_for(|up| *up))
            .await
            .expect("the member catches up")
            .unwrap();
        Self {
            host,
            host_key,
            host_cert,
            workspace,
            member,
            member_key,
            member_person: member_root.verifying_key().to_bytes(),
            link,
            answers,
            running: vec![running],
            router,
            endpoints: [host_endpoint, member_endpoint],
            _state: [host_state, frames, member_state],
        }
    }

    /// End the member's link, as a device that goes offline does.
    fn unlink(&mut self) {
        for running in self.running.drain(..) {
            running.abort();
        }
    }

    /// Dial the host again: the link carries what the member sends from the answered dial on,
    /// and takes nothing in until `run` runs it.
    async fn dial(&self) -> tilo_peer_state::direct::Dialed {
        let host = &self.endpoints[0];
        self.link
            .dial(&self.endpoints[1], addr(host), *host.id().as_bytes())
            .await
            .expect("the host answers the member's dial")
    }

    /// Run the link `dialed`.
    fn run(&mut self, dialed: tilo_peer_state::direct::Dialed) {
        let link = self.link.clone();
        self.running
            .push(tokio::spawn(async move { dialed.run(&link).await }));
    }

    fn member_device(&self) -> [u8; 32] {
        self.member_key.verifying_key().to_bytes()
    }

    /// One pass of the member's custody onto its link, as its network actor makes one: every
    /// write custody lets leave now is sent. Answers every write custody holds, and the ones sent.
    fn custody_pass(&self) -> (Vec<[u8; 32]>, Vec<[u8; 32]>) {
        self.member
            .with_outgoing(self.workspace, tilo_clock::wall(), |_, outgoing| {
                let queued = outgoing.queued().collect();
                let mut sent = Vec::new();
                for entry in outgoing.entries() {
                    let message: NeighbourMsg =
                        postcard::from_bytes(entry.bytes()).map_err(|error| error.to_string())?;
                    self.link.send(message)?;
                    sent.push(entry.id());
                }
                Ok((queued, sent))
            })
            .expect("the member's custody is read")
    }

    /// The host's answers to the member's `writes`, each once it came, within 30 seconds.
    async fn answered(&mut self, writes: &[[u8; 32]]) -> BTreeMap<[u8; 32], Answered> {
        let mut answered = BTreeMap::new();
        tokio::time::timeout(Duration::from_secs(30), async {
            while writes.iter().any(|write| !answered.contains_key(write)) {
                match self.answers.recv().await {
                    Ok(answer) if writes.contains(&answer.write) => {
                        answered.insert(answer.write, answer);
                    }
                    Ok(_) => {}
                    Err(error) => panic!("the host's answers ended: {error}"),
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the host answers each write: {answered:?} of {writes:?}"));
        answered
    }

    /// Let the writes `answered` go from the member's custody, as its network actor does with
    /// what the host answered.
    fn retire(&self, answered: &BTreeMap<[u8; 32], Answered>) {
        let answers = answered
            .iter()
            .map(|(write, answer)| {
                let answer = match answer.kept {
                    Kept::Durable => HostAnswer::Durable(answer.seq),
                    Kept::Refused => HostAnswer::Refused,
                };
                (*write, answer)
            })
            .collect();
        self.member
            .retire_answered(self.workspace, &answers)
            .expect("the member lets its answered writes go");
    }

    async fn close(self) {
        for running in self.running {
            running.abort();
        }
        self.router.shutdown().await.unwrap();
        for endpoint in self.endpoints {
            endpoint.close().await;
        }
    }
}

/// The write custody holds now that it did not hold in `before`: the one just committed.
fn committed_since(before: &[[u8; 32]], now: &[[u8; 32]]) -> [u8; 32] {
    let new: Vec<_> = now.iter().filter(|id| !before.contains(id)).collect();
    let [write] = new.as_slice() else {
        panic!("one write was committed into custody: {} new", new.len());
    };
    **write
}

/// A write the hosting device refuses for good is answered as refused: the member's custody lets
/// it go rather than holding it for ever, and its writer still reads the host's verdict on it in
/// the workspace, where a person is told the workspace will not take their change.
///
/// The member writes offline while it may, and the host makes it a viewer meanwhile: the member's
/// own replica took the write, and sends it as its link comes back, before it hears of its new
/// role; the workspace refuses it.
#[tokio::test(flavor = "multi_thread")]
async fn a_write_the_host_refuses_for_good_leaves_custody_and_its_verdict_reaches_the_writer() {
    let mut hosted = Hosted::new(40).await;
    let (workspace, member_device) = (hosted.workspace, hosted.member_device());
    hosted.unlink();
    let before = hosted.member.owed_writes(workspace).unwrap();
    let seq = commit_record(
        &hosted.member,
        workspace,
        &hosted.member_key,
        "from the member",
    );
    let write = committed_since(&before, &hosted.member.owed_writes(workspace).unwrap());

    let person = hex::encode(hosted.member_person);
    commit_membership(
        &hosted.host,
        workspace,
        (&hosted.host_cert, &hosted.host_key),
        tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Member {
            change: tilo_action_policy::Change::SetRole {
                person: person.clone(),
                role: tilo_action_policy::Role::Viewer,
            },
        },
    )
    .await
    .expect("the host's owner makes the member a viewer");

    // The member's custody goes to its link as the dial is answered, as its network actor sends
    // it, before anything the host holds reached the member.
    let dialed = hosted.dial().await;
    let (queued, sent) = hosted.custody_pass();
    assert!(queued.contains(&write), "the write waits in custody");
    assert!(sent.contains(&write), "the write goes to the host");
    hosted.run(dialed);
    let answered = hosted.answered(&[write]).await;
    assert_eq!(
        (answered[&write].kept, answered[&write].seq),
        (Kept::Refused, seq),
        "the host refuses the write for good, named as custody names it"
    );
    hosted.retire(&answered);
    assert!(
        !hosted
            .member
            .owed_writes(workspace)
            .unwrap()
            .contains(&write),
        "custody lets a write refused for good go"
    );

    // The host's verdict reaches the writer in the workspace, naming the write.
    let host_device = hosted.host_key.verifying_key().to_bytes();
    let names_the_write = |refusal: &neighbour_wire::WriteRefusal| {
        // One verdict per route and author, at the highest sequence it refused: a snapshot the
        // member's link signed after the write may be the one named, and covers it.
        refusal.author == member_device
            && refusal.seq >= seq
            && (refusal.app.as_str(), refusal.record.as_str()) == (APP, RECORD)
            && refusal.by == host_device
    };
    until(
        &hosted.link,
        "the member reads the host's verdict",
        |peer| {
            peer.workspace()
                .write_refusals()
                .iter()
                .any(names_the_write)
        },
    )
    .await;
    // The member's owner takes in what its link's replica holds a moment later.
    let refusal = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(refusal) = hosted
                .member
                .write_refusals(workspace)
                .into_iter()
                .find(|refusal| names_the_write(refusal))
            {
                break refusal;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the member's owner holds the host's verdict on its write");
    assert!(
        refusal.reason.permanent(),
        "the verdict says the write is refused for good: {refusal:?}"
    );

    hosted.close().await;
}

/// Channel keys that open a sealed write and seal it again under the same keys: what a device
/// holding its channel's current epoch renews an expired write with.
struct Renewing(key_group::ContentKeys);

impl tilo_peer_state::owner::SealedHistoryKeys for Renewing {
    fn open(
        &self,
        _: tilo_desktop_model::WorkspaceId,
        write: &collab_types::SignedOp,
    ) -> Result<Vec<u8>, String> {
        let metadata = write.encryption.ok_or("not sealed")?;
        self.0
            .open_content(write.artifact, &metadata, &write.payload)
            .map(|plaintext| plaintext.to_vec())
            .map_err(|error| error.to_string())
    }

    fn seal_checkpoint(
        &self,
        _: tilo_desktop_model::WorkspaceId,
        _: &str,
        _: &str,
        _: &[u8],
    ) -> Result<(collab_types::EncryptionMetadata, Vec<u8>), String> {
        Err("this device compacts nothing here".into())
    }

    fn renew(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        write: &collab_types::SignedOp,
    ) -> Result<(collab_types::EncryptionMetadata, Vec<u8>), String> {
        let plaintext = self.open(workspace, write)?;
        self.0
            .seal_content(
                write.artifact,
                collab_types::ContentRetention::Delta,
                &plaintext,
            )
            .map_err(|error| error.to_string())
    }
}

/// Commit the member's edit of the test record sealed under `keys`, written at `when`, into its
/// custody, as an app seals what it writes.
fn commit_sealed(hosted: &Hosted, keys: &key_group::ContentKeys, when: SystemTime) {
    let key = &hosted.member_key;
    let message = hosted
        .member
        .with_current(hosted.workspace, |view| {
            let mut document =
                AutoCommit::load(&view.peer().document(APP, RECORD).unwrap()).unwrap();
            document.set_actor(automerge::ActorId::from(
                key.verifying_key().to_bytes().to_vec(),
            ));
            let heads = document.get_heads();
            document.put(ROOT, "value", "sealed by the member").unwrap();
            document.commit_edit("fixture", "value");
            view.peer().clone().publication_mut().publish_incremental(
                APP,
                RECORD,
                document.save_after(&heads),
                key,
                when,
            )
        })
        .expect("the write signs");
    let NeighbourMsg::AppDocument { mut envelope, .. } = message else {
        panic!("a record's write is a document");
    };
    let (metadata, sealed) = keys
        .seal_content(
            envelope.artifact,
            collab_types::ContentRetention::Delta,
            &envelope.payload,
        )
        .unwrap();
    envelope.payload = sealed;
    envelope.encryption = Some(metadata);
    collab_types::sign_op(&mut envelope, key).unwrap();
    hosted
        .member
        .commit_document(
            tilo_peer_state::owner_commit::OwnerCommitRequest {
                workspace: hosted.workspace.to_string(),
                operation: tilo_peer_state::owner_commit::CommitOperation::Document {
                    app: APP.into(),
                    record: RECORD.into(),
                    envelope,
                    own: None,
                },
            },
            when,
        )
        .expect("the member's owner takes its sealed write");
}

/// A sealed write that waited in the member's custody past its epoch is held back unsent (the
/// host would refuse it as a late frame); renewed, it goes to the host over the direct link, is
/// answered as durable, and custody ends empty.
#[tokio::test(flavor = "multi_thread")]
async fn an_expired_sealed_write_is_renewed_sent_answered_and_leaves_custody() {
    let mut hosted = Hosted::new(50).await;
    let workspace = hosted.workspace;
    // Whatever the member's link committed as it started has gone and been answered first.
    let (_, sent) = hosted.custody_pass();
    let answered = hosted.answered(&sent).await;
    hosted.retire(&answered);
    assert!(hosted.member.owed_writes(workspace).unwrap().is_empty());

    let keys = key_group::KeyGroup::create(
        collab_types::WorkspaceId::parse(WORKSPACE).unwrap(),
        key_group::MemberKey::generate("member").unwrap(),
    )
    .unwrap()
    .content_keys()
    .unwrap();
    // Written six minutes ago, past the five a sealed write is fresh for.
    let written = SystemTime::now()
        .checked_sub(Duration::from_secs(360))
        .unwrap();
    commit_sealed(&hosted, &keys, written);
    let [original] = hosted
        .member
        .owed_writes(workspace)
        .unwrap()
        .try_into()
        .expect("custody holds the sealed write");

    let (queued, sent) = hosted.custody_pass();
    assert_eq!(queued, vec![original], "the expired write waits in custody");
    assert!(sent.is_empty(), "the expired write is held back unsent");

    // The member's network actor renews what a pass held back, and sends the renewal.
    hosted
        .member
        .keep_sealed_histories(Arc::new(Renewing(keys)));
    let renewed = hosted
        .member
        .renew_outgoing(workspace, &hosted.member_key, tilo_clock::wall())
        .await
        .expect("the expired write renews");
    assert_eq!(renewed, 1);
    let (queued, sent) = hosted.custody_pass();
    let [renewal] = sent
        .try_into()
        .expect("the renewal, and nothing else, goes to the host");
    assert_ne!(renewal, original, "the renewal replaces its original");
    assert_eq!(queued, vec![renewal]);

    let answered = hosted.answered(&[renewal]).await;
    assert_eq!(
        answered[&renewal].kept,
        Kept::Durable,
        "the host takes the renewal in"
    );
    hosted.retire(&answered);
    assert!(
        hosted.member.owed_writes(workspace).unwrap().is_empty(),
        "custody ends empty"
    );
    let writes = hosted.member.document_writes(workspace)[&(APP.to_owned(), RECORD.to_owned())];
    assert_eq!((writes.pending, writes.refused), (0, 0));
    assert!(writes.durable >= 1, "the renewal is counted durable");
    let device = hosted.member_device();
    let held_by_host = hosted
        .host
        .with_current(workspace, |view| {
            Ok(view
                .peer()
                .workspace()
                .write_refusals()
                .iter()
                .all(|refusal| refusal.author != device))
        })
        .unwrap();
    assert!(held_by_host, "the host refused none of the member's writes");

    hosted.close().await;
}

/// The value of the test record as `owner` holds it in `workspace`.
fn document_value(
    owner: &NativeWorkspaceOwner,
    workspace: tilo_desktop_model::WorkspaceId,
) -> Option<String> {
    owner
        .with_current(workspace, |view| {
            let bytes = view.peer().document(APP, RECORD).ok_or("no record")?;
            let document = AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
            Ok(automerge::ReadDoc::get(&document, ROOT, "value")
                .map_err(|error| error.to_string())?
                .and_then(|(value, _)| value.into_string().ok()))
        })
        .unwrap()
}

/// Whether the host filed no verdict against any of `device`'s writes.
fn host_refused_none(hosted: &Hosted, device: [u8; 32]) -> bool {
    hosted
        .host
        .with_current(hosted.workspace, |view| {
            Ok(view
                .peer()
                .workspace()
                .write_refusals()
                .iter()
                .all(|refusal| refusal.author != device))
        })
        .unwrap()
}

/// A member's cleartext write that waited in its custody past the live window, while its host was
/// unreachable for six minutes, lands on the host once the member links again: the hosting device
/// judges the member's own writes on its link as history, as a relay host judges what a member's
/// session hands it, so a write is decided by its signature, its author's sequence and the
/// author's standing, never by how long it waited. The host answers it durable, holds the edit,
/// and the member keeps it and lets it go from custody. Judged as a live frame, the host refused
/// it as late and it never landed.
#[tokio::test(flavor = "multi_thread")]
async fn a_cleartext_write_that_waited_past_the_live_window_lands_on_the_host() {
    let mut hosted = Hosted::new(60).await;
    let workspace = hosted.workspace;
    let (_, sent) = hosted.custody_pass();
    let answered = hosted.answered(&sent).await;
    hosted.retire(&answered);
    hosted.unlink();

    // Written offline six minutes ago, past the five a live frame is taken for.
    let written = SystemTime::now()
        .checked_sub(Duration::from_secs(360))
        .unwrap();
    let seq = commit_record_at(
        &hosted.member,
        workspace,
        &hosted.member_key,
        "written offline",
        written,
    );
    let [write] = hosted
        .member
        .owed_writes(workspace)
        .unwrap()
        .try_into()
        .expect("custody holds the write made offline");

    let dialed = hosted.dial().await;
    let (_, sent) = hosted.custody_pass();
    assert!(sent.contains(&write), "the write goes to the host");
    hosted.run(dialed);
    let answered = hosted.answered(&[write]).await;
    assert_eq!(
        (answered[&write].kept, answered[&write].seq),
        (Kept::Durable, seq),
        "the host takes the late write in, named as custody names it"
    );
    hosted.retire(&answered);
    assert!(
        hosted.member.owed_writes(workspace).unwrap().is_empty(),
        "custody ends empty"
    );
    assert_eq!(
        document_value(&hosted.host, workspace).as_deref(),
        Some("written offline"),
        "the host's replica holds the member's edit"
    );
    assert_eq!(
        document_value(&hosted.member, workspace).as_deref(),
        Some("written offline"),
        "the member's replica keeps its edit"
    );
    assert!(
        host_refused_none(&hosted, hosted.member_device()),
        "the host refused none of the member's writes"
    );

    hosted.close().await;
}

/// A sealed write the member made while its host was unreachable for six minutes is held back
/// past its epoch, renewed as the member links again, and lands on the host: answered durable,
/// and custody ends empty. The regression check beside the cleartext one above.
#[tokio::test(flavor = "multi_thread")]
async fn a_sealed_write_made_offline_past_the_live_window_is_renewed_and_lands() {
    let mut hosted = Hosted::new(70).await;
    let workspace = hosted.workspace;
    let (_, sent) = hosted.custody_pass();
    let answered = hosted.answered(&sent).await;
    hosted.retire(&answered);
    hosted.unlink();

    let keys = key_group::KeyGroup::create(
        collab_types::WorkspaceId::parse(WORKSPACE).unwrap(),
        key_group::MemberKey::generate("member").unwrap(),
    )
    .unwrap()
    .content_keys()
    .unwrap();
    let written = SystemTime::now()
        .checked_sub(Duration::from_secs(360))
        .unwrap();
    commit_sealed(&hosted, &keys, written);
    let [original] = hosted
        .member
        .owed_writes(workspace)
        .unwrap()
        .try_into()
        .expect("custody holds the sealed write made offline");

    let dialed = hosted.dial().await;
    let (queued, sent) = hosted.custody_pass();
    assert_eq!(queued, vec![original], "the expired write waits in custody");
    assert!(sent.is_empty(), "the expired write is held back unsent");
    hosted
        .member
        .keep_sealed_histories(Arc::new(Renewing(keys)));
    let renewed = hosted
        .member
        .renew_outgoing(workspace, &hosted.member_key, tilo_clock::wall())
        .await
        .expect("the expired write renews");
    assert_eq!(renewed, 1);
    let (_, sent) = hosted.custody_pass();
    let [renewal] = sent
        .try_into()
        .expect("the renewal, and nothing else, goes to the host");
    hosted.run(dialed);

    let answered = hosted.answered(&[renewal]).await;
    assert_eq!(
        answered[&renewal].kept,
        Kept::Durable,
        "the host takes the renewal in"
    );
    hosted.retire(&answered);
    assert!(
        hosted.member.owed_writes(workspace).unwrap().is_empty(),
        "custody ends empty"
    );
    assert!(
        host_refused_none(&hosted, hosted.member_device()),
        "the host refused none of the member's writes"
    );

    hosted.close().await;
}
