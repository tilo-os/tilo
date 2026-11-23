//! What an app writes on one device reaches the workspace's other devices: the broker commits it to
//! this device's owner, the canonical copy of the workspace here, and the owner's outgoing ownership
//! leaves on the peer network's session with the workspace's host, which hands it to every other member.
//! Two sessions' peer networks on this machine's loopback, each with its own engine, join one workspace
//! governed by a host on this machine whose certificate takes part in no channels: the workspace's
//! records are documents the owner commits.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the host's server deliberately stays alive until each test ends"
)]

tilo_automerge_isolate::test_worker!();

use std::time::Duration;

use ed25519_dalek::SigningKey;
use tilo_desktop_model::WorkspaceId;
use tilo_user_identity::DeviceCapabilities;

use super::{Client, Enrolling, Entering, Entry, OwnPresence};
use crate::engine::Engine;
use crate::platform::fabric::Continuity;
use crate::workspace_keys::KeyLink;

/// One session's side of the workspace: its engine, and its peer network.
struct Device {
    engine: std::sync::Arc<Engine>,
    network: Client,
    /// The session's question sheet: what its person is asked here.
    questions: tilo_service::Questions,
    _state: tempfile::TempDir,
}

impl Device {
    /// A session of a device certified by `root`, relaying through `host`'s server: two of them
    /// are two devices of one person. Each device mints a key no other run shares, as each host
    /// does.
    fn of(root: &SigningKey, label: &str, host: &test_server::Host) -> Self {
        let state = tempfile::tempdir().expect("the session's directory");
        let key = test_server::fresh_signing_key(label);
        let cert = test_server::certified(root, &key, label, DeviceCapabilities::member());
        let identity = tilo_network_actor::DeviceIdentity::kept(cert, key);
        let engine = std::sync::Arc::new(
            Engine::open(
                identity.clone(),
                Some(state.path()),
                std::iter::empty(),
                crate::platform::membership_check::of_this_session(),
            )
            .expect("the session's engine opens"),
        );
        engine.attach_link(KeyLink::new());
        let questions = tilo_service::Questions::new();
        let network = Client::start(
            Entry {
                identity: Some(identity),
                own: OwnPresence::default(),
                hosts: Vec::new(),
                continuity: Continuity::default(),
                wake: std::sync::Arc::new(|| ()),
                relays: host.relays(),
                lan: None,
                records: crate::platform::records::Changes::default(),
            },
            Some(std::sync::Arc::clone(&engine)),
            Some(questions.clone()),
            Vec::new(),
            None,
            tilo_workspace_service::service::ServiceSlot::default(),
        )
        .expect("the session's peer network starts");
        Self {
            engine,
            network,
            questions,
            _state: state,
        }
    }

    fn person(&self) -> [u8; 32] {
        self.engine.identity().device_cert.user_public_key
    }

    /// Join the host's workspace with an invitation for this person, and wait until the workspace
    /// is held here.
    fn join(&self, host: &test_server::Host, request: u8) -> WorkspaceId {
        self.join_to(host, host.workspace(), request)
    }

    /// Join `workspace`, one `host` governs, with an invitation for this person, and wait until the
    /// workspace is held here.
    fn join_to(&self, host: &test_server::Host, workspace: &str, request: u8) -> WorkspaceId {
        let workspace_id = WorkspaceId::parse(workspace).expect("a workspace id");
        self.engine.bind(workspace_id);
        self.network
            .enroll(Enrolling {
                workspace_id,
                way: tilo_workspace_directory::Way::Invitation(host.invitation_to(
                    workspace,
                    self.person(),
                    request,
                )),
                name: String::new(),
            })
            .expect("the peer network takes the enrollment");
        self.enrolled()
            .result
            .expect("the workspace admits this device");
        workspace_id
    }

    /// What became of this device's enrollment or entry, once its host answered.
    fn enrolled(&self) -> tilo_session_client::Enrolled {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if let Some(enrolled) = self.network.take_enrolled().pop() {
                return enrolled;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the workspace's host answers"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Enter the host's workspace as a further device of this person, at the host's own signed
    /// address: nothing is invited here, and the workspace answers with what it holds, so this
    /// device takes part in it from then on.
    fn enter(&self, host: &test_server::Host) -> WorkspaceId {
        let workspace_id = WorkspaceId::parse(host.workspace()).expect("a workspace id");
        self.network
            .enter(Entering {
                workspace_id,
                name: "Notes".into(),
                host: host.host_hint(),
                held: false,
            })
            .expect("the peer network takes the entry");
        let enrolled = self.enrolled();
        assert!(enrolled.member, "the entry is one of the person's own");
        enrolled.result.expect("the workspace admits this device");
        workspace_id
    }
}

/// The receipt of an effect this device's app had in `workspace` , as the broker records it:
/// redacted, its actor this device's person.
fn receipt(
    device: &Device,
    workspace_id: WorkspaceId,
    invocation: uuid::Uuid,
) -> (String, Vec<u8>) {
    let mut effect = tilo_desktop_model::WorkspaceEffectReceipt::redacted(
        tilo_actions::InvocationId(invocation),
        &tilo_actions::MemberId::person(hex::encode(device.person())),
        tilo_actions::ActionContext::for_resource("groceries", "note", "Groceries"),
        tilo_actions::EffectReceiptStatus::Succeeded,
        tilo_clock::unix_now_i64(),
    );
    effect.completed_at = 100;
    (
        workspace_id.to_string(),
        serde_json::to_vec(&effect).expect("a redacted receipt"),
    )
}

/// Whether `device` 's copy of `workspace` holds the receipt of `invocation` .
fn holds_receipt(device: &Device, workspace_id: WorkspaceId, invocation: uuid::Uuid) -> bool {
    device
        .engine
        .owner()
        .with_current(workspace_id, |view| {
            Ok(view.peer().admitted_workspace().is_some_and(|workspace| {
                tilo_desktop_model::projection::workspace_effect_receipts_of(workspace)
                    .iter()
                    .any(|held| held.invocation.0 == invocation)
            }))
        })
        .unwrap_or(false)
}

/// An effect an app had in a workspace this session holds: the broker the
/// session serves its apps from commits its receipt to the session's owner,
/// and the owner's ownership leaves for the workspace's other session, which
/// then holds it too.
#[test]
fn a_receipt_the_owner_commits_reaches_the_workspaces_other_session() {
    let host = test_server::Host::of(&SigningKey::from_bytes(&[0x71; 32]), "Notes");
    let writer = Device::of(&SigningKey::from_bytes(&[0x51; 32]), "The writer", &host);
    let reader = Device::of(&SigningKey::from_bytes(&[0x61; 32]), "The reader", &host);
    let workspace = writer.join(&host, 0x01);
    assert_eq!(reader.join(&host, 0x02), workspace);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("the broker's runtime");
    let invocation = uuid::Uuid::new_v4();
    write(&runtime, &writer, workspace, invocation);
    wait_for(&reader, workspace, invocation);
}

/// Have `device` 's app write `invocation` into `workspace` , as the broker commits what an app
/// did: the owner's ownership leaves for the workspace's host, which hands it to the workspace's
/// other devices.
fn write(
    runtime: &tokio::runtime::Runtime,
    device: &Device,
    workspace_id: WorkspaceId,
    invocation: uuid::Uuid,
) {
    let publisher = crate::app_wire::owner_publisher(
        Some(std::sync::Arc::clone(&device.engine)),
        tilo_workspace_service::service::ServiceSlot::default(),
        None,
    );
    assert!(
        publisher.records_effects_for(&workspace_id.to_string()),
        "the workspace's effect receipts are the owner's to commit"
    );
    let (workspace, redacted) = receipt(device, workspace_id, invocation);
    runtime
        .block_on(publisher.record_effect(workspace, redacted))
        .expect("the record takes the app's write");
    assert!(
        holds_receipt(device, workspace_id, invocation),
        "the write is the writing device's owner's"
    );
}

/// Wait until `device` 's copy of the workspace holds the receipt of `invocation` .
fn wait_for(device: &Device, workspace_id: WorkspaceId, invocation: uuid::Uuid) {
    let deadline = tilo_test_env::hang_deadline();
    while !holds_receipt(device, workspace_id, invocation) {
        assert!(
            std::time::Instant::now() < deadline,
            "the write reaches the workspace's other device"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// One person's further device takes their workspace in at its host without an invitation: a device
/// certified by a member's root is that member's device, so the workspace admits it and answers
/// with what it holds. What was written on the person's other device before this one came up is
/// here, and what this one writes reaches the other.
#[test]
fn a_person_s_further_device_enters_their_workspace_at_its_host() {
    let root = SigningKey::from_bytes(&[0x71; 32]);
    let host = test_server::Host::of(&root, "Notes");
    let laptop = Device::of(&root, "The laptop", &host);
    let desktop = Device::of(&root, "The desktop", &host);
    assert_eq!(laptop.person(), desktop.person(), "one person, two devices");
    let workspace = laptop.join(&host, 0x01);

    // What the laptop wrote before the desktop was up.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("the broker's runtime");
    let earlier = uuid::Uuid::new_v4();
    write(&runtime, &laptop, workspace, earlier);

    // The desktop enters the workspace at its host: no invitation, and the workspace's whole
    // history comes with the entry.
    assert_eq!(desktop.enter(&host), workspace);
    wait_for(&desktop, workspace, earlier);

    // And back: what the desktop writes reaches the laptop.
    let later = uuid::Uuid::new_v4();
    write(&runtime, &desktop, workspace, later);
    wait_for(&laptop, workspace, later);
}

/// The session a fresh device starts from: one workspace of its own, nothing of the person's.
fn fresh_session() -> tilo_desktop_surface::DesktopHandle {
    let mut minter = tilo_desktop_model::IdMinter::new(1);
    let (policy, _surface) = tilo_desktop_policy::DesktopPolicy::with_tree(
        tilo_session_core::desktop::seed_tree(&mut minter, "Session", Vec::new()),
        1,
    );
    policy.handle().clone()
}

/// The workspace ids `session`'s shell shows, in its order.
fn on_shell(session: &tilo_desktop_surface::DesktopHandle) -> Vec<WorkspaceId> {
    session.read(|tree| {
        tree.workspaces
            .iter()
            .map(tilo_desktop_model::Workspace::id)
            .collect()
    })
}

/// A computer restored from a person's root, with what it reached of them: the site that keeps
/// their workspace, the laptop they wrote it on, and the sessions the restored computer follows
/// their list on.
struct Restore {
    host: test_server::Host,
    /// The person's workspace on the site: the site's own, which the person was invited into.
    notes: WorkspaceId,
    laptop: Device,
    restored: Device,
    /// The site's home, which a computer set up for a site joins on its own.
    lounge: WorkspaceId,
    runtime: tokio::runtime::Runtime,
    hosting: crate::hosted_workspaces::HostedWorkspaces,
    woken: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Restore {
    /// A person (`seed` their root's bytes) invited into the site's Notes on their laptop, and a
    /// computer restored from their root alone that joined the site's home and follows what the
    /// site lists for them.
    fn new(seed: u8) -> Self {
        let host = test_server::Host::site(&SigningKey::from_bytes(&[seed ^ 0x20; 32]), "Lounge");
        let notes = host.create("Notes");
        let root = SigningKey::from_bytes(&[seed; 32]);
        let laptop = Device::of(&root, "The laptop", &host);
        let notes = laptop.join_to(&host, &notes, 0x01);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("the broker's runtime");
        let restored = Device::of(&root, "The restored computer", &host);
        let lounge = restored.join(&host, 0x02);
        // The hosts of what the computer holds list the person's workspaces to it.
        let hosting = crate::hosted_workspaces::HostedWorkspaces::service(
            Some(&restored.engine),
            Some(restored.network.endpoint_handle()),
            Some(restored.network.answering()),
            runtime.handle().clone(),
        )
        .expect("a session holding workspaces reaches their hosts");
        let woken = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        Self {
            host,
            notes,
            laptop,
            restored,
            lounge,
            runtime,
            hosting,
            woken,
        }
    }

    /// Follow the person's list on the hosts of what the restored computer holds.
    fn follow(&self) {
        let woken = std::sync::Arc::clone(&self.woken);
        let wake: tilo_service::Wake =
            std::sync::Arc::new(move || woken.store(true, std::sync::atomic::Ordering::Relaxed));
        self.hosting.follow_lists(&wake);
    }

    /// The workspaces the hosts listed that the restored computer holds no copy of, once they list
    /// the person's Notes, and what every host lists then.
    fn listed(
        &self,
    ) -> (
        std::collections::BTreeMap<WorkspaceId, crate::hosted_workspaces::Listed>,
        std::collections::BTreeSet<WorkspaceId>,
    ) {
        let deadline = tilo_test_env::hang_deadline();
        let mut listed = std::collections::BTreeMap::new();
        loop {
            listed.append(&mut self.hosting.take_listed());
            if listed.contains_key(&self.notes)
                && let Some(everywhere) = self.hosting.listed_everywhere()
            {
                return (listed, everywhere);
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the host lists the person's workspace to the restored computer"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The restored computer enters the person's Notes where its list says the host answers, and
    /// holds it.
    fn enter_notes(&self, personal: &mut crate::workspaces::Personal) {
        let adoption = personal.adopt_into(&fresh_session(), &|_| false);
        let entering = adoption
            .enter
            .iter()
            .find(|opened| opened.workspace == self.notes)
            .expect("the restored computer enters the person's workspace");
        assert_eq!(entering.name, "Notes");
        let hint = personal
            .host_hint(self.notes)
            .expect("the list says where the workspace's host is reached");
        assert!(
            hint.host.is_of_device(&entering.host),
            "it is entered at the host the list names"
        );
        self.restored.engine.bind(self.notes);
        self.restored
            .network
            .enter(Entering {
                workspace_id: self.notes,
                name: entering.name.clone(),
                host: hint,
                held: false,
            })
            .expect("the peer network takes the entry");
        let enrolled = self.restored.enrolled();
        assert!(enrolled.member, "the entry is one of the person's own");
        enrolled
            .result
            .expect("the host takes the restored computer in");
    }
}

/// A computer restored from its person's root alone, with no invitation to their workspace and no
/// other device of theirs about, learns the workspace from the host that keeps it for them — the
/// list every device of theirs reads there — and enters it at that host as their further device:
/// what they wrote on their laptop before is on the restored computer. The host is someone else's,
/// as a hosting site is; the restored computer reaches it through the site's home workspace, which
/// a computer set up for a site joins on its own.
#[test]
fn a_computer_restored_from_its_root_enters_the_workspaces_its_host_lists_for_the_person() {
    let restore = Restore::new(0x52);
    let earlier = uuid::Uuid::new_v4();
    write(&restore.runtime, &restore.laptop, restore.notes, earlier);
    assert!(
        !restore.restored.engine.holds(restore.notes),
        "nothing of the person's workspace is on the restored computer yet"
    );

    restore.follow();
    let (listed, _) = restore.listed();
    assert!(
        restore.woken.load(std::sync::atomic::Ordering::Relaxed),
        "the session is woken to take the list in"
    );
    assert!(
        !listed.contains_key(&restore.lounge),
        "a workspace the computer holds is not entered again"
    );

    // The session takes the list in and enters the workspace where the host said it answers.
    let mut personal =
        crate::workspaces::Personal::open(restore.restored.engine.identity().clone(), None);
    assert!(
        personal.take_hosted(listed),
        "the person's list gains the workspace"
    );
    restore.enter_notes(&mut personal);
    wait_for(&restore.restored, restore.notes, earlier);
}

/// A restored computer's shell starts, as every fresh shell does, with a workspace of its own. It
/// waits, neither founded nor listed, until the host has listed what the person has: a person with
/// workspaces has it taken off the shell, so restoring them again and again leaves them with exactly
/// the workspaces they have; a person with none keeps it as their first.
#[test]
fn a_restored_computer_founds_no_workspace_beside_the_persons() {
    let restore = Restore::new(0x53);
    let session = fresh_session();
    let [seed] = on_shell(&session)
        .try_into()
        .expect("a fresh shell shows one workspace");
    let mut personal =
        crate::workspaces::Personal::open(restore.restored.engine.identity().clone(), None);
    personal.hold_seed(seed);
    // The site's home came onto the shell as the computer joined it, as a joined workspace does.
    personal.joined(restore.lounge);
    let mut tree = session.read(Clone::clone);
    tree.workspaces
        .push(tilo_desktop_model::Workspace::new(restore.lounge, "Lounge"));
    session.replace_tree(tree);

    // Before the host answered, nothing says what the person has: the workspace waits, unlisted.
    let held = restore.restored.engine.workspaces();
    assert!(!personal.settle_seed(&session, &held, None, &std::collections::BTreeSet::new()));
    assert_eq!(personal.seed(), Some(seed), "it waits for the host's list");
    personal.publish_local(&session);
    let listed: Vec<String> = personal
        .readable(
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
        )
        .workspaces
        .into_iter()
        .map(|entry| entry.workspace)
        .collect();
    assert!(
        listed.is_empty(),
        "neither the waiting workspace nor the site's home is on the person's list: {listed:?}"
    );

    // The host lists the person's Notes: they have a workspace, so the shell's own goes.
    restore.follow();
    let (from_hosts, everywhere) = restore.listed();
    assert!(personal.take_hosted(from_hosts));
    assert!(personal.settle_seed(
        &session,
        &held,
        Some(&everywhere),
        &std::collections::BTreeSet::new()
    ));
    assert_eq!(personal.seed(), None);
    assert_eq!(
        on_shell(&session),
        [restore.lounge],
        "the workspace the shell started with is taken off it, unfounded"
    );
    personal.publish_local(&session);
    let listed: Vec<String> = personal
        .readable(
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
        )
        .workspaces
        .into_iter()
        .map(|entry| entry.workspace)
        .collect();
    assert_eq!(
        listed,
        [restore.notes.to_string()],
        "the person's list is the workspaces they have, and no other"
    );

    // A person whose hosts list nothing of theirs has none: the shell's workspace is their first.
    let alone = fresh_session();
    let [first] = on_shell(&alone)
        .try_into()
        .expect("a fresh shell shows one workspace");
    let key = test_server::fresh_signing_key("a new person's computer");
    let cert = test_server::certified(
        &SigningKey::from_bytes(&[0x54; 32]),
        &key,
        "A new person's computer",
        DeviceCapabilities::member(),
    );
    let mut new = crate::workspaces::Personal::open(
        tilo_network_actor::DeviceIdentity::kept(cert, key),
        None,
    );
    new.hold_seed(first);
    assert!(new.settle_seed(
        &alone,
        &[],
        Some(&std::collections::BTreeSet::new()),
        &std::collections::BTreeSet::new(),
    ));
    assert_eq!(new.seed(), None);
    assert_eq!(on_shell(&alone), [first], "it stays on the shell");
    assert!(new.made_here(first), "and is founded here, as theirs");
    new.publish_local(&alone);
    let listed: Vec<String> = new
        .readable(
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
        )
        .workspaces
        .into_iter()
        .map(|entry| entry.workspace)
        .collect();
    assert_eq!(listed, [first.to_string()], "and listed as theirs");
}

/// The shell's first workspace goes when the person has a workspace of their own, and one their
/// governance names them the owner of is theirs however it came to this device: a computer restored
/// from their words that joined their own site's workspace does not found a first workspace beside
/// it, and needs no host's list to say so.
#[test]
fn a_joined_workspace_the_person_owns_is_theirs_before_any_host_answers() {
    let session = fresh_session();
    let [seed] = on_shell(&session)
        .try_into()
        .expect("a fresh shell shows one workspace");
    let key = test_server::fresh_signing_key("A restored computer");
    let cert = test_server::certified(
        &SigningKey::from_bytes(&[0x5a; 32]),
        &key,
        "A restored computer",
        DeviceCapabilities::member(),
    );
    let mut personal = crate::workspaces::Personal::open(
        tilo_network_actor::DeviceIdentity::kept(cert, key),
        None,
    );
    personal.hold_seed(seed);
    let site = WorkspaceId::parse("0b7a5f4e-2c1d-4e8a-9f3b-6d5c4b3a2918").expect("a workspace id");
    personal.joined(site);
    let mut tree = session.read(Clone::clone);
    tree.workspaces
        .push(tilo_desktop_model::Workspace::new(site, "Lounge"));
    session.replace_tree(tree);

    // No host has answered a list, and the workspace's governance names the person its owner: they
    // have a workspace of their own, so the one this shell started with goes.
    let owned = std::collections::BTreeSet::from([site]);
    assert!(personal.settle_seed(&session, &[site], None, &owned));
    assert_eq!(personal.seed(), None);
    assert_eq!(
        on_shell(&session),
        [site],
        "the workspace the shell started with is taken off it, unfounded"
    );
}

/// A workspace the restored computer takes from its host's list is not claimed as the person's
/// own: the person was invited into it, and the part they take in it is what its governance says
/// once the workspace is here, which is what the list their keys read names.
#[test]
fn an_invited_workspace_a_restored_computer_takes_from_its_host_is_listed_as_invited() {
    let restore = Restore::new(0x55);
    restore.follow();
    let (listed, _) = restore.listed();
    let mut personal =
        crate::workspaces::Personal::open(restore.restored.engine.identity().clone(), None);
    assert!(personal.take_hosted(listed));
    assert!(
        !personal.made_here(restore.notes),
        "a workspace entered from the host's list is not one this device founds, nor claims"
    );
    restore.enter_notes(&mut personal);

    // The shell shows the workspace with the people its governance names, as every pass projects
    // them.
    let session = fresh_session();
    let deadline = tilo_test_env::hang_deadline();
    let policy = loop {
        if let Ok(policy) = restore.restored.engine.policy(&restore.notes.to_string())
            && policy.owner.is_some()
        {
            break policy;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the workspace's governance reaches the restored computer"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut tree = session.read(Clone::clone);
    let mut notes = tilo_desktop_model::Workspace::new(restore.notes, "Notes");
    tilo_session_client::membership::project(&mut notes, &policy);
    tree.workspaces.push(notes);
    session.replace_tree(tree);
    let person = hex::encode(restore.restored.person());
    let roles = session.read(|tree| crate::workspaces::governed_roles(tree, &person));
    assert_eq!(
        roles.get(&restore.notes).map(String::as_str),
        Some("editor"),
        "the person takes the part they were invited at"
    );
    let entry = personal
        .readable(&std::collections::BTreeMap::new(), &roles)
        .workspaces
        .into_iter()
        .find(|entry| entry.workspace == restore.notes.to_string())
        .expect("the person's list names the workspace");
    assert_eq!(
        entry.role.as_deref(),
        Some("editor"),
        "the list names the workspace as one they were invited into, not as their own"
    );
}

/// A workspace the host takes the person out of goes from the host's list, and the restored
/// computer, which follows that list both ways, is handed it to let go of: nothing else of the
/// person's goes with it.
#[test]
fn a_workspace_its_host_drops_from_the_persons_list_leaves_the_device() {
    let restore = Restore::new(0x56);
    restore.follow();
    let (listed, _) = restore.listed();
    let mut personal =
        crate::workspaces::Personal::open(restore.restored.engine.identity().clone(), None);
    assert!(personal.take_hosted(listed));
    restore.enter_notes(&mut personal);
    assert!(restore.restored.engine.holds(restore.notes));
    // A list that still names it lets nothing go.
    let deadline = tilo_test_env::hang_deadline();
    while !restore
        .hosting
        .listed_everywhere()
        .is_some_and(|named| named.contains(&restore.notes))
    {
        assert!(std::time::Instant::now() < deadline, "the host lists Notes");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(restore.hosting.take_dropped().is_empty());

    restore
        .host
        .remove(&restore.notes.to_string(), restore.restored.person(), 0x03);
    let deadline = tilo_test_env::hang_deadline();
    let mut dropped = std::collections::BTreeSet::new();
    while !dropped.contains(&restore.notes) {
        assert!(
            std::time::Instant::now() < deadline,
            "the restored computer is handed the workspace its host no longer lists"
        );
        std::thread::sleep(Duration::from_millis(20));
        dropped.append(&mut restore.hosting.take_dropped());
    }
    assert_eq!(
        dropped,
        std::collections::BTreeSet::from([restore.notes]),
        "only the workspace the person was taken out of goes; the site's home stays"
    );
}

/// A question the host of a workspace placed away from this device puts to its person — as it does
/// before it opens a Terminal there that their access leaves to them — reaches this device's
/// question sheet, though the peer network was pointed at no host: the device answers the host of
/// each workspace it holds there on the endpoint it reaches it from. The person's Allow on the sheet
/// is the host's answer.
#[test]
fn a_question_from_a_hosted_workspaces_host_reaches_the_sheet_and_its_answer_reaches_the_host() {
    let restore = Restore::new(0x54);
    let (changed, change) = std::sync::mpsc::channel();
    restore.restored.questions.on_change(move || {
        let _ = changed.send(());
    });
    restore.follow();

    let id = [0x54; 32];
    let answer = restore.host.ask(
        restore.restored.person(),
        tilo_server::certify::PendingConfirmation {
            id,
            agent_label: "Terminal".into(),
            title: "Open a terminal in Lounge".into(),
            action_label: "open a terminal".into(),
            workspace: "Lounge".into(),
            expires_at_ns: tilo_clock::unix_now_ns().saturating_add(60_000_000_000),
            ..tilo_server::certify::PendingConfirmation::default()
        },
    );

    let handle = format!("agent-question:{}", hex::encode(id));
    let deadline = tilo_test_env::hang_deadline();
    let asked = loop {
        if let Some(asked) = restore.restored.questions.question(&handle) {
            break asked;
        }
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(
            change.recv_timeout(left).is_ok(),
            "the host's question reaches the session's sheet"
        );
    };
    assert_eq!(asked.kind, crate::network::AGENT_QUESTION);
    assert_eq!(asked.label, "Terminal");

    assert!(restore.restored.questions.answer(
        &handle,
        tilo_service::Answer {
            choice: Some(0),
            ..tilo_service::Answer::default()
        },
    ));
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    assert_eq!(
        answer
            .recv_timeout(left)
            .expect("the host hears the answer"),
        Ok(tilo_server::certify::ConfirmationAnswer::AllowOnce),
        "the person's Allow is the host's answer"
    );
}
