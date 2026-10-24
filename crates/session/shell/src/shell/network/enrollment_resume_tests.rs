//! A workspace whose join was under way when the session before ended comes
//! back joining, and its join goes on by itself: the proof the host was sent
//! (or was about to be sent) is redeemed again, exactly, through the site's
//! host, into the same workspace. Nothing the state file holds besides is
//! changed or dropped, and with no host to reach the workspace stays as it
//! is and nothing asks again on every frame.
//!
//! The state is the one a computer is left with when its session ends after
//! the proof was written and before the room installed: the site's room in
//! `joining_rooms` with its own workspace, an active pending enrollment of
//! generation 5 at revision 5 whose target names that workspace and, under
//! the field name a build from before wrote, another room; a retired
//! `room_authority` entry for the room; one other room enrolled for an
//! archived workspace and a second archived workspace, the room the pending
//! target names; the hosted-rooms record of the site's room and the
//! person's workspace list naming it. The proof is made by a real exchange
//! with a test host, from test keys; the file around it is set by hand.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ed25519_dalek::Signer as _;
use sha2::Digest as _;
use tilo_relay_protocol::rooms::{PreAdmission, RoomRole};
use tilo_user_identity::DeviceCert;

use super::community::Pass;
use super::hosted_sites::Arrival;
use super::site_join::SiteJoin;
use super::site_join_tests::{Site, certify, computer, held, key, until};
use super::{HostIdentity, Reach, ShellNetwork};
use crate::shell::state::{RoomId, Workspace, WorkspaceId, WorkspaceTreeHandle};

const CHAT: &str = "so.tilo.chat";
const LABEL: &str = "Ada's computer";

/// 32 random bytes.
fn random() -> [u8; 32] {
    let mut bytes = [0; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes
}

/// The host the site's door admits people to now.
#[derive(Clone)]
struct Behind {
    rooms: tilo_relay::rooms::Rooms,
    host_cert: DeviceCert,
    room: String,
    origin: String,
}

impl Behind {
    fn of(site: &Site) -> Self {
        Self {
            rooms: site.relay().rooms().unwrap().clone(),
            host_cert: site.host_cert.clone(),
            room: site.room.clone(),
            origin: site.origin(),
        }
    }
}

/// The site's entry and door, in front of whichever host `behind` names.
/// With `spend_fails` the door hands out an admission token the host does
/// not know, so the computer's join stops after its proof is written and
/// before the host is sent it.
struct Door {
    behind: Arc<Mutex<Behind>>,
    spend_fails: Arc<AtomicBool>,
    entry: url::Url,
    _responder: test_http::Responder,
}

fn json(status: u16, value: &serde_json::Value) -> test_http::Reply {
    test_http::Reply::status(status)
        .header("content-type", "application/json")
        .body(value.to_string())
}

/// Pre-admit the person the request names to the room behind the door,
/// signed by the site's owner (the root that certified its host).
fn door_join(
    handle: &tokio::runtime::Handle,
    behind: &Behind,
    request: &test_http::Request,
    spend_fails: bool,
) -> Option<test_http::Reply> {
    let asked: serde_json::Value = serde_json::from_slice(&request.body).ok()?;
    let user: [u8; 32] = hex::decode(asked["userPublicKey"].as_str()?)
        .ok()?
        .try_into()
        .ok()?;
    let owner = key(40);
    let token = hex::encode(random());
    let mut grant = PreAdmission {
        room: behind.room.clone(),
        role: RoomRole::Editor,
        token_sha256: sha2::Sha256::digest(token.as_bytes()).into(),
        label: asked["displayName"].as_str().unwrap_or("Guest").into(),
        expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
        owner_user_public_key: owner.verifying_key().to_bytes(),
        signature: [0; 64],
    };
    grant.signature = owner.sign(&grant.digest().unwrap()).to_bytes();
    let issued = handle
        .block_on(behind.rooms.pre_admit(
            &behind.host_cert,
            tilo_relay::rooms::PreAdmitRequest {
                request_id: random(),
                grant,
                recipient_user_public_key: Some(user),
                inviter_name: Some("Tilo".into()),
            },
        ))
        .unwrap();
    let token = if spend_fails {
        hex::encode(random())
    } else {
        token
    };
    Some(json(
        200,
        &serde_json::json!({
            "invitation": tilo_peer_state::join::encode_invitation_text(&issued.signed_invitation),
            "admissionToken": token,
            "origin": behind.origin,
        }),
    ))
}

impl Door {
    fn start(site: &Site) -> Self {
        let behind = Arc::new(Mutex::new(Behind::of(site)));
        let spend_fails = Arc::new(AtomicBool::new(false));
        let responder = {
            let handle = site.runtime.handle().clone();
            let behind = Arc::clone(&behind);
            let spend_fails = Arc::clone(&spend_fails);
            test_http::Responder::handle(move |request| {
                let behind = behind.lock().unwrap().clone();
                match (request.method.as_str(), request.target.as_str()) {
                    ("GET", "/entry.json") => Some(json(
                        200,
                        &serde_json::json!({ "home": {
                            "name": "Lounge",
                            "host": {"origin": behind.origin},
                            "app": CHAT,
                            "admission": "door/",
                        }}),
                    )),
                    ("POST", "/door/join") => door_join(
                        &handle,
                        &behind,
                        request,
                        spend_fails.load(Ordering::SeqCst),
                    ),
                    _ => Some(test_http::Reply::status(404)),
                }
            })
        };
        let entry = url::Url::parse(&format!("{}/", responder.url())).unwrap();
        Self {
            behind,
            spend_fails,
            entry,
            _responder: responder,
        }
    }
}

/// How far the join got before the session ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Left {
    /// The host admitted the proof and answered; the room never installed.
    Answered,
    /// The proof was written and never reached the host.
    NeverSent,
}

/// The certificate custody holds when the computer starts again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cert {
    /// The one the proof was made with.
    Same,
    /// Reissued after an identity restore: the same person, device key and
    /// capabilities, issued later, with no expiry.
    Reissued,
}

/// The computer at `root` again, its device certified as `cert` says: what
/// [`computer`] starts, with the certificate reissued when asked.
fn computer_again(root: &Path, cert: Cert) -> (ShellNetwork, WorkspaceTreeHandle) {
    if cert == Cert::Same {
        return computer(root, key(1), LABEL);
    }
    let state = state_path(root);
    let mut tree = crate::shell::persist::load_tree_from(&state).unwrap();
    tree.persist_path = Some(state);
    let tree = tree.shared();
    let net = ShellNetwork::spawn_with(
        tree.clone(),
        &root.join("device-private/keypair.bin"),
        &root.join("peer-sync"),
        Reach::Loopback,
        move |secret, _| {
            let person = key(1);
            let device = ed25519_dalek::SigningKey::from_bytes(&secret.to_bytes());
            let mut cert = certify(
                &person,
                &device,
                LABEL,
                tilo_user_identity::DeviceCapabilities::member(),
            );
            cert.issued_at_ns = 2;
            tilo_user_identity::sign_device_cert(&mut cert, &person).unwrap();
            Some(HostIdentity {
                device_cert: cert,
                device_signing_key: device,
                workspace_device_certs: std::collections::HashMap::new(),
                workspace_revocations: std::collections::HashMap::new(),
                identity_paths: None,
            })
        },
    )
    .unwrap();
    (net, tree)
}

/// A computer's state root left with a join under way, and what it holds.
struct Shape {
    dir: tempfile::TempDir,
    site: Site,
    door: Door,
    /// The computer's state root.
    root: PathBuf,
    /// The site's room being joined, and its workspace.
    lounge: RoomId,
    lounge_workspace: WorkspaceId,
    /// The room enrolled for an archived workspace.
    enrolled: RoomId,
    /// The archived workspace's room the pending target names.
    prior: RoomId,
    /// The computer's person.
    user: [u8; 32],
}

fn state_path(root: &Path) -> PathBuf {
    root.join("state.json")
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn workspace_of(tree: &WorkspaceTreeHandle, room: RoomId) -> WorkspaceId {
    tree.read()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .map(|workspace| workspace.id)
        .expect("the room has its workspace")
}

/// Every file under `root`, relative to it.
fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                files.push(path.strip_prefix(root).unwrap().to_path_buf());
            }
        }
    }
    files.sort();
    files
}

/// Run a computer until it enrolled one room through the site and is
/// joining the site's next room, `left` as far as the join got, then end
/// its session and set the state file around that join as described above.
#[allow(clippy::too_many_lines, reason = "one scenario read top to bottom")]
fn leave_a_join_pending(left: Left) -> Shape {
    let dir = tempfile::tempdir().unwrap();
    let mut site = Site::start(dir.path());
    let door = Door::start(&site);
    let root = dir.path().join("computer");
    let (net, tree) = computer(&root, key(1), LABEL);
    let identity = net.host_identity.as_ref().unwrap().device_cert.clone();

    // A room enrolled before, whose workspace is archived now.
    let enrolled = RoomId::parse(&site.room).unwrap();
    net.join_site(&door.entry).unwrap();
    held(&net, &tree, enrolled);
    let archived = workspace_of(&tree, enrolled);
    assert!(tree.write().archive_workspace(archived));
    // The room a build from before named in the pending target: an archived
    // workspace of its own.
    let studio = Workspace::create("Studio");
    let prior = studio.room();
    assert!(tree.write().keep_archived(studio));

    // The site set its room up on another host; the computer joins it.
    site.reset(dir.path(), 42);
    *door.behind.lock().unwrap() = Behind::of(&site);
    net.hosted_sites.forget(enrolled).unwrap();
    door.spend_fails
        .store(left == Left::NeverSent, Ordering::SeqCst);
    let SiteJoin::Enrolling {
        room: lounge,
        local: lounge_workspace,
        enrollment,
    } = net.join_site_now(&door.entry).unwrap()
    else {
        panic!("the site's room is being joined");
    };
    assert_eq!(lounge, RoomId::parse(&site.room).unwrap());
    // The exchange ends; the session ends before the room installs.
    let attempt = net
        .native_enrollment
        .attempts
        .lock()
        .unwrap()
        .get(&enrollment)
        .cloned()
        .expect("the join runs");
    let exchanged = until("the join's exchange to end", || attempt.finish(|_| Ok(())));
    assert_eq!(
        exchanged.is_ok(),
        left == Left::Answered,
        "the exchange ended as the fixture means it to: {exchanged:?}"
    );
    drop(attempt);
    drop(net);
    crate::shell::persist::wait_for_saves();

    // The state file as such a session leaves it.
    let path = state_path(&root);
    let mut state = read_json(&path);
    let lounge_key = lounge.to_string();
    {
        let authority = &mut state["native_authority"];
        let pending = &mut authority["pending"][&lounge_key];
        assert_eq!(pending["status"], "Active", "{pending}");
        assert_eq!(pending["target"]["workspace"], lounge_workspace.0);
        pending["id"]["generation"] = 5.into();
        pending["target"]["prior_room"] = prior.to_string().into();
        authority["revision"] = 5.into();
        // The room enrolled for the archived workspace, beside this
        // device's own first workspace, which its host holds.
        assert!(authority["rooms"].get(enrolled.to_string()).is_some());
        assert!(authority["rooms"].get(&lounge_key).is_none());
    }
    assert!(
        state["joining_rooms"]
            .as_array()
            .unwrap()
            .contains(&lounge_key.clone().into())
    );
    let archived_rooms: Vec<_> = state["archived_workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|workspace| workspace["room"].clone())
        .collect();
    assert_eq!(
        archived_rooms,
        vec![
            serde_json::Value::from(enrolled.to_string()),
            prior.to_string().into()
        ]
    );
    state["room_authority"] = serde_json::json!({ lounge_key.clone(): "Unpinned" });
    std::fs::write(&path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    // The site's room as the hosted-rooms record holds it.
    let peer_sync = root.join("peer-sync");
    std::fs::write(
        peer_sync.join("hosted-rooms.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "rooms": { lounge_key: {
                "origin": site.origin(),
                "site": door.entry.to_string(),
                "host": hex::encode(site.host_cert.device_public_key),
                "app": CHAT,
            }},
            "left": [],
        }))
        .unwrap(),
    )
    .unwrap();

    // The person's workspace list names it, with the site's host.
    let mut list = crate::shell::personal_workspaces::PersonalWorkspaces::open(
        identity.user_public_key,
        identity.device_public_key,
        super::personal_workspaces_path(&peer_sync),
    );
    let held_rooms = BTreeMap::from([(lounge, site.host_cert.device_public_key)]);
    assert!(list.publish_local(&tree.read(), &held_rooms));
    drop(list);
    drop(tree);
    crate::shell::persist::wait_for_saves();

    Shape {
        dir,
        site,
        door,
        root,
        lounge,
        lounge_workspace,
        enrolled,
        prior,
        user: identity.user_public_key,
    }
}

/// Point this computer's community at `entry` for the test's life.
fn community_is(home: &mut tilo_test_env::TestHome, dir: &Path, entry: &url::Url) {
    let config = dir.join("community.toml");
    std::fs::write(&config, format!("site = \"{entry}\"\n")).unwrap();
    home.env().set(super::community::CONFIG_ENV, &config);
}

/// The proof the pending enrollment of `room` in `state` keeps.
fn pending_proof(state: &serde_json::Value, room: &str) -> Vec<u8> {
    serde_json::from_value(state["native_authority"]["pending"][room]["intent"].clone()).unwrap()
}

/// The proof the enrolled `room` in `state` was enrolled with: the room
/// record's enrollment is flattened into it, its proof base64 text.
fn enrolled_proof(state: &serde_json::Value, room: &str) -> Vec<u8> {
    let record = &state["native_authority"]["rooms"][room];
    let text = record["enrollment_intent"].as_str().unwrap_or_else(|| {
        panic!(
            "the enrolled room keeps no enrollment proof; its fields: {:?}",
            record
                .as_object()
                .map(|fields| fields.keys().collect::<Vec<_>>())
        )
    });
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, text).unwrap()
}

/// Start the computer from the state a join `left` behind, custody holding
/// `cert`, with its community following on its own, and check the join
/// goes on by itself into the same workspace with the proof it kept, with
/// nothing else of the state file changed or dropped. A host that answered
/// has moved on by then: someone else joined the room.
#[expect(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
fn a_pending_join_resumes_on_start(left: Left, cert: Cert) {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let shape = leave_a_join_pending(left);
    community_is(&mut home, shape.dir.path(), &shape.door.entry);
    let lounge = shape.lounge;
    if left == Left::Answered {
        drop(shape.site.browser(&key(2), key(3), "Grace"));
    }
    let files = files_under(&shape.root);
    let before = read_json(&state_path(&shape.root));
    let proof = pending_proof(&before, &lounge.to_string());

    let (net, tree) = computer_again(&shape.root, cert);
    assert!(
        tree.read().is_joining(lounge),
        "the workspace comes back joining"
    );
    let marked: Vec<String> = serde_json::from_value(before["joining_rooms"].clone()).unwrap();
    assert!(!marked.is_empty());
    for room in marked {
        let room = RoomId::parse(&room).unwrap();
        assert!(
            tree.read().is_joining(room),
            "a room being joined is still marked so: the start repairs nothing"
        );
    }
    assert_eq!(workspace_of(&tree, lounge), shape.lounge_workspace);

    // Nobody asks: the session follows its community, as it does on start.
    let net = Arc::new(net);
    net.follow_community();
    let mut arrivals: Vec<Arrival> = Vec::new();
    until("the join to go on and install the room", || {
        net.tick();
        arrivals.extend(net.take_arrivals());
        (tree.read().holds_room(lounge) && arrivals.iter().any(|arrival| arrival.room == lounge))
            .then_some(())
    });
    assert!(
        !tree.read().is_joining(lounge),
        "the workspace left joining"
    );
    assert_eq!(
        arrivals,
        vec![Arrival {
            room: lounge,
            app: Some(CHAT.into())
        }],
        "the person arrives in the room, in its site's app"
    );
    assert!(
        shape.site.member(shape.user),
        "the host lists the computer's person"
    );
    {
        let tree = tree.read();
        let rooms: Vec<_> = tree
            .desktop
            .workspaces
            .iter()
            .filter(|workspace| workspace.room() == lounge)
            .map(|workspace| workspace.id)
            .collect();
        assert_eq!(rooms, vec![shape.lounge_workspace], "the same workspace");
        assert_eq!(tree.one_record_violations(), Vec::<String>::new());
    }
    drop(net);
    drop(tree);
    crate::shell::persist::wait_for_saves();
    assert_only_the_enrollment_completed(&shape, &before, &files, &proof);
    shape.site.shutdown();
}

/// What the state file under `shape` holds once its pending join went on,
/// against what it held `before`: the enrollment completed with the `proof`
/// it kept, and nothing else changed, nor any of the `files` went.
fn assert_only_the_enrollment_completed(
    shape: &Shape,
    before: &serde_json::Value,
    files: &[PathBuf],
    proof: &[u8],
) {
    let after = read_json(&state_path(&shape.root));
    let room = shape.lounge.to_string();
    assert!(
        after["native_authority"]["pending"].get(&room).is_none(),
        "the enrollment completed"
    );
    assert_eq!(
        enrolled_proof(&after, &room),
        proof,
        "the room enrolled with the proof its pending enrollment kept, \
         whatever room its stored target named"
    );
    let enrolled = shape.enrolled.to_string();
    assert_eq!(
        after["native_authority"]["rooms"][&enrolled],
        before["native_authority"]["rooms"][&enrolled],
        "the room enrolled before is untouched"
    );
    assert_eq!(
        after["archived_workspaces"], before["archived_workspaces"],
        "the archived workspaces are untouched"
    );
    assert!(
        after["archived_workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|workspace| workspace["room"] == shape.prior.to_string().as_str())
    );
    assert_eq!(
        after["room_authority"], before["room_authority"],
        "the retired room_authority is written back as it was"
    );
    let gone: Vec<_> = files
        .iter()
        .filter(|file| !shape.root.join(file).exists())
        .collect();
    assert!(gone.is_empty(), "files under the state root went: {gone:?}");
}

/// The host had admitted the proof and answered; the session ended before
/// the room installed, and the room moved on since.
#[test]
#[serial_test::serial]
fn a_join_the_host_answered_before_the_session_ended_installs_on_start() {
    a_pending_join_resumes_on_start(Left::Answered, Cert::Same);
}

/// As above, after an identity restore reissued the device's certificate:
/// the same member takes the room with the reissued certificate.
#[test]
#[serial_test::serial]
fn a_join_the_host_answered_installs_on_start_after_an_identity_restore() {
    a_pending_join_resumes_on_start(Left::Answered, Cert::Reissued);
}

/// The proof was written and never reached the host.
#[test]
#[serial_test::serial]
fn a_join_whose_proof_never_reached_the_host_completes_on_start() {
    a_pending_join_resumes_on_start(Left::NeverSent, Cert::Same);
}

/// With neither the site nor its host answering, the workspace comes back
/// joining, stays usable, and nothing asks the site again on any frame:
/// a join goes on when something happens, never on the frame clock.
#[test]
#[serial_test::serial]
#[expect(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
fn a_pending_join_with_no_host_to_reach_stays_joining_and_asks_nothing_per_frame() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let shape = leave_a_join_pending(Left::NeverSent);
    let lounge = shape.lounge;
    // The site and its host go away; whatever asks there is cut off.
    let Shape {
        dir,
        site,
        door,
        root,
        lounge_workspace,
        ..
    } = shape;
    site.shutdown();
    drop(door);
    let gone = test_http::Responder::handle(|_| None);
    let entry = url::Url::parse(&format!("{}/", gone.url())).unwrap();
    std::fs::write(
        root.join("peer-sync/hosted-rooms.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "rooms": { lounge.to_string(): {
                "origin": gone.url(),
                "site": entry.to_string(),
                "host": hex::encode([7; 32]),
                "app": CHAT,
            }},
            "left": [],
        }))
        .unwrap(),
    )
    .unwrap();
    community_is(&mut home, dir.path(), &entry);

    let (net, tree) = computer(&root, key(1), LABEL);
    assert_eq!(net.keep_community(&entry), Pass::Unreachable);
    let asked = gone.drain_requests().len();
    assert!(asked > 0, "the community pass asked the site");
    let attempts = net.native_enrollment.attempts.lock().unwrap().len();

    for _ in 0..120 {
        net.frame_tick();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        gone.drain_requests().len(),
        0,
        "frames asked the site again"
    );
    assert_eq!(
        net.native_enrollment.attempts.lock().unwrap().len(),
        attempts,
        "frames started another join"
    );
    {
        let tree = tree.read();
        assert!(tree.is_joining(lounge), "the workspace stays joining");
        assert!(
            tree.native_authority.requires(lounge),
            "the pending enrollment is kept"
        );
        assert_eq!(tree.one_record_violations(), Vec::<String>::new());
    }
    assert_eq!(workspace_of(&tree, lounge), lounge_workspace);
    // The workspace is usable: it can be shown.
    tree.write().set_active(lounge_workspace);
    net.frame_tick();
    assert_eq!(tree.read().desktop.active, lounge_workspace);
    assert!(tree.read().is_joining(lounge));
    drop(net);
}
