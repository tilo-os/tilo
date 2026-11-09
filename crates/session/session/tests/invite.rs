//! Inviting people from the invite sheet into the workspace in front: the bar offers it to the
//! person the workspace lets invite, the sheet makes a link at the part they pick, and a second
//! person's own device joins with what was handed to them.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "test fixtures: a failed step is the assertion"
)]

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use ed25519_dalek::SigningKey;
use tilo_app::framework::testing::HeadlessApp;
use tilo_compositor::{InputEvent, OutputConfig};
use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId, WorkspaceSlot};
use tilo_network_actor::{HostIdentity, workspace_topic_id};
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::join::MemberRole;
use tilo_peer_state::owner::{self, NativeWorkspaceOwner, testing};
use tilo_session::{Options, Session};
use tilo_sheets::Seat as _;
use tilo_sheets::invite::InviteApp;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};
use ui::a11y::Action as A11yAction;

use common::{HEIGHT, WIDTH};

// An engine admits a workspace's records through an Automerge worker: this binary is that worker,
// as it is the session's own.
tilo_automerge_isolate::test_worker!();

/// The workspace this computer holds and the person invites into.
const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";

/// `key` 's certificate as a device of the person whose root key is `root` .
fn certified(root: &SigningKey, key: &SigningKey, label: &str) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.to_owned(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).expect("a certified device");
    cert
}

/// This computer's person: a device of theirs, certified for their own root key, kept in an
/// identity root of the test's own.
fn this_computer(
    env: &mut tilo_test_env::ScopedEnv,
) -> (tempfile::TempDir, SigningKey, DeviceCert) {
    let root = tempfile::tempdir().expect("an identity root");
    let paths = tilo_user_identity::IdentityPaths::under(root.path());
    std::fs::create_dir_all(paths.device_privkey.parent().expect("the key's directory"))
        .expect("this computer's key directory");
    let person = SigningKey::from_bytes(&[0x61; 32]);
    let device = SigningKey::from_bytes(&[0x62; 32]);
    tilo_user_identity::persist::write_device_key(&paths.device_privkey, &device)
        .expect("this computer's device key");
    let cert = certified(&person, &device, "The laptop");
    tilo_user_identity::persist::write_device_cert(&paths.device_cert, &cert)
        .expect("this computer's certificate");
    env.set("TILO_IDENTITY_ROOT", root.path());
    (root, device, cert)
}

/// The identity of `cert` 's device, with no identity root of its own: what redeems a link is a
/// device, and this one joins only what it is handed.
fn identity(cert: &DeviceCert, key: &SigningKey) -> HostIdentity {
    HostIdentity {
        device_cert: cert.clone(),
        device_signing_key: key.clone(),
        workspace_device_certs: HashMap::new(),
        workspace_revocations: HashMap::new(),
        custody: None,
    }
}

/// This computer's copy of the workspace `workspace` under `state` , held as `cert` 's device in a
/// workspace the same device governs: the state a session started on this directory finds waiting
/// for it.
fn workspace_held_here(state: &Path, workspace: WorkspaceId, cert: &DeviceCert, key: &SigningKey) {
    let record = state.join("tilo").join("rooms.json");
    std::fs::create_dir_all(record.parent().expect("the state's workspace directory"))
        .expect("this device's state directory");
    let owner = owner::open_file(&owner::owner_file_of(&record))
        .expect("an owner for this device's workspace");
    let governor = testing::enroll_interactive_governor_in(&owner, key, cert, WORKSPACE)
        .expect("this device governs the workspace it hosts");
    let host = governor.workspace_host().device_public_key;
    let frames: Vec<Vec<u8>> = governor
        .outgoing()
        .map(|(bytes, _)| bytes.to_vec())
        .collect();
    for frame in frames {
        owner
            .receive(
                workspace,
                TransportOrigin::RetainedRelayDelta(host),
                postcard::from_bytes(&frame).expect("a publication is a message"),
                None,
                SystemTime::now(),
            )
            .expect("the host admits the workspace's people");
    }
}

/// The person a root key names, as a workspace writes its owner.
fn principal(root: &[u8; 32]) -> String {
    match collab_types::caps::Principal::person_from_key(
        collab_types::crypto::Ed25519PublicKey::from_bytes(*root),
    ) {
        collab_types::caps::Principal::Person(id) => id,
        other => panic!("{other:?} is not a person"),
    }
}

/// A desk showing the workspace, known by its id, owned by the person.
fn desk(root: &[u8; 32]) -> DesktopTree {
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let slot = WorkspaceSlot::of_workspace(workspace);
    let mut workspace = Workspace::in_workspace(slot, workspace, "Desk");
    workspace.set_owner(principal(root));
    DesktopTree {
        workspaces: vec![workspace.clone()],
        active: workspace.slot,
        ..DesktopTree::default()
    }
}

/// A session of this computer taking part in the collaboration plane, keeping what it holds under
/// `state` , on the desk showing the workspace.
fn hosted_session(state: &Path, root: &[u8; 32]) -> Session {
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            collab: true,
            person: "Ada".to_owned(),
            state: Some(state.to_path_buf()),
            ..Options::default()
        },
    );
    session.handle().replace_tree(desk(root));
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("a frame");
    }
    session
}

fn settle(session: &mut Session) {
    for _ in 0..6 {
        session.step(Duration::from_millis(1)).expect("a frame");
    }
}

/// Who `owner` 's copy of `workspace` has in it, by person, in the part each holds.
fn members(owner: &NativeWorkspaceOwner, workspace: WorkspaceId) -> Vec<(String, MemberRole)> {
    owner
        .with_current(workspace, |view| {
            let members = view
                .peer()
                .governance()
                .governed_members(SystemTime::now())?;
            Ok(members
                .into_iter()
                .map(|member| (hex::encode(member.user_public_key), member.role))
                .collect())
        })
        .expect("the workspace's people are readable")
}

/// Redeem `link` as `cert` 's device over this machine's own transport, and answer with the copy of
/// the workspace it left here.
fn joins_with(
    link: &str,
    workspace: WorkspaceId,
    cert: &DeviceCert,
    key: &SigningKey,
) -> Result<NativeWorkspaceOwner, String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let state = tempfile::tempdir().expect("her state directory");
    let owner = owner::open_file(&state.path().join("owner.json")).expect("her owner opens");
    runtime.block_on(async {
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
            .bind()
            .await
            .map_err(|error| error.to_string())?;
        let enrollment = owner::enroll::prepare_and_complete(
            tilo_peer_state::join_transport::WorkspaceJoinClient::new(endpoint),
            identity(cert, key),
            workspace_topic_id(workspace),
            // Her owner holds no copy of the workspace yet: the enrollment makes its own slot for
            // it.
            None,
            link.to_owned(),
            owner.clone(),
        )
        .await?;
        owner.install(&enrollment, SystemTime::now())
    })?;
    Ok(owner)
}

/// Where the bar's invite control stands, when the person may invite here.
fn bar_control(session: &Session) -> Option<(f64, f64, f64, f64)> {
    session
        .controls()
        .into_iter()
        .find(|(name, _)| name == "Invite" || name == "topbar:invite")
        .map(|(_, rect)| rect)
}

/// A press at the centre of `rect` , through the compositor's own input.
fn press(session: &mut Session, (x, y, width, height): (f64, f64, f64, f64)) {
    session
        .compositor()
        .input(InputEvent::PointerMotion {
            x: x + width / 2.0,
            y: y + height / 2.0,
        })
        .expect("the pointer");
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: 0x110,
                pressed,
            })
            .expect("the button");
        session.step(Duration::from_millis(16)).expect("a turn");
    }
}

/// Write what the sheet shows where a person can look at it, when the run asked for shots:
/// `TILO_SURFACE_SHOTS` names where they go.
fn capture(app: &mut HeadlessApp<InviteApp>, name: &str) {
    let Some(directory) = std::env::var_os("TILO_SURFACE_SHOTS").map(std::path::PathBuf::from)
    else {
        return;
    };
    // Draw what the sheet holds now: the link arrives while the sheet is up,
    // and the frame may still be the one from before it.
    app.settle();
    std::fs::create_dir_all(&directory).expect("the directory for shots");
    app.harness()
        .render_png(&directory.join(format!("{name}.png")));
}

/// The invitations the desk's own action lists for the workspace in front.
fn invitations_at_the_desk(session: &mut Session) -> Vec<serde_json::Value> {
    let listed = session
        .invoke(
            tilo_surface_host::shell_part::DESK_RESOURCE,
            "invitations",
            &serde_json::Map::new(),
        )
        .expect("the desk lists the invitations it made")
        .completed()
        .expect("listing finishes on return")
        .value
        .expect("the listing's invitations");
    serde_json::from_value(listed).expect("a list of invitations")
}

/// What a QR scanner reads from the sheet's code, `code` , drawn as the sheet draws it — at the
/// width it gives a payload of that size — and read by a decoder that knows nothing of the widget.
/// The version the decoder found comes back with the bytes.
fn scanned(code: &[u8]) -> Result<(Vec<u8>, usize), String> {
    use ui::widget::QrWidget;

    let drawn = QrWidget::new(code.to_vec()).with_id("invite-code");
    let px = tilo_sheets::invite::module_px(drawn.total_modules());
    let widget = drawn.with_module_px(px);
    if let Some(error) = widget.error() {
        return Err(format!("the code does not fit: {error}"));
    }
    let quiet = widget.quiet_modules();
    let modules = widget
        .total_modules()
        .saturating_sub(quiet.saturating_mul(2));
    if modules == 0 {
        return Err("the code draws nothing".to_owned());
    }
    // The code is drawn at the width the sheet draws it: a scanner reads it there or it is
    // unreadable on the screen.
    let scale = usize::try_from(px).map_err(|_| "no module width".to_owned())?;
    let side = u32::try_from(widget.total_modules().saturating_mul(scale))
        .map_err(|_| "the code is too wide to draw".to_owned())?;
    let mut luma = image::GrayImage::from_pixel(side, side, image::Luma([255]));
    for y in 0..modules {
        for x in 0..modules {
            if !widget.module(x, y) {
                continue;
            }
            let left = u32::try_from(x.saturating_add(quiet).saturating_mul(scale)).unwrap_or(0);
            let top = u32::try_from(y.saturating_add(quiet).saturating_mul(scale)).unwrap_or(0);
            for dy in 0..u32::try_from(scale).unwrap_or(1) {
                for dx in 0..u32::try_from(scale).unwrap_or(1) {
                    luma.put_pixel(
                        left.saturating_add(dx),
                        top.saturating_add(dy),
                        image::Luma([0]),
                    );
                }
            }
        }
    }
    let mut prepared = rqrr::PreparedImage::prepare(luma);
    let grids = prepared.detect_grids();
    let grid = grids.first().ok_or("a scanner finds no code")?;
    let mut read = Vec::new();
    let meta = grid
        .decode_to(&mut read)
        .map_err(|error| format!("a scanner cannot read the code: {error:?}"))?;
    Ok((read, meta.version.0))
}

/// The sheet's code for the link it made carries what the person hands over: the invitation's own
/// text, or the short link that names it. A scanner reads it back byte for byte.
fn scanned_back(created: &tilo_sheets::invite::Created) -> Result<Vec<u8>, String> {
    let handed_over = if tilo_workspace_join::sealed::parse_link(&created.link).is_some() {
        created.link.clone().into_bytes()
    } else {
        tilo_workspace_join::invitation_bytes(&created.link)?
    };
    if handed_over != created.code {
        return Err("the code is not what the sheet hands over".to_owned());
    }
    scanned(&created.code).map(|(read, _)| read)
}

/// A session on a desk that holds [`WORKSPACE`], with the invite sheet up as the person who may
/// invite there sees it: everything either test does before it can make a link.
struct Hosted {
    /// The identity root and the state directory the session reads from.
    _kept: Vec<tempfile::TempDir>,
    session: Session,
    seat: tilo_sheets::invite::InviteSeat,
    cert: DeviceCert,
}

/// [`Hosted`] — this computer holding the workspace, its bar offering Invite,
/// and the sheet the person's press puts up.
fn hosted(home: &mut tilo_test_env::TestHome) -> Hosted {
    let (identity, key, cert) = this_computer(home.env());
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let state = tempfile::tempdir().expect("a state directory");
    workspace_held_here(state.path(), workspace, &cert, &key);
    let mut session = hosted_session(state.path(), &cert.user_public_key);
    settle(&mut session);

    // The person the workspace lets invite finds Invite on the bar, and
    // pressing it puts the sheet up. The pointer comes up to the bar first,
    // as the person's does to press it.
    session
        .compositor()
        .input(InputEvent::PointerMotion { x: 1100.0, y: 16.0 })
        .expect("the pointer");
    let mut place = None;
    common::until(&mut session, "the bar offers Invite", |session| {
        place = bar_control(session);
        place.is_some()
    });
    press(&mut session, place.expect("the bar's Invite"));
    settle(&mut session);
    let seat = session
        .invite_sheet()
        .expect("the session hosts the invite sheet")
        .clone();
    assert!(
        seat.presentation().is_open(),
        "the bar's Invite puts the invite sheet up"
    );
    assert!(seat.invite().may_invite, "the person may invite here");
    Hosted {
        _kept: vec![identity, state],
        session,
        seat,
        cert,
    }
}

/// The sheet for `seat` , drawn on a screen.
fn sheet_app(seat: &tilo_sheets::invite::InviteSeat) -> HeadlessApp<InviteApp> {
    let mut app = HeadlessApp::<InviteApp>::for_person_with_lent("person", seat.clone())
        .expect("the sheet draws");
    app.harness()
        .resize((WIDTH as u32, HEIGHT as u32))
        .expect("a screen");
    app.settle();
    app
}

/// Press the sheet's create button, and answer with the link it shows once the session has it: a
/// short link is shown when its site has kept it.
fn press_create(app: &mut HeadlessApp<InviteApp>, session: &mut Session) -> String {
    app.harness()
        .perform_named("invite-create", &A11yAction::Activate)
        .expect("the create button is pressed");
    app.settle();
    settle(session);
    let mut link = String::new();
    common::until(session, "the sheet shows the link it made", |session| {
        link = session
            .invite_sheet()
            .and_then(|seat| {
                seat.invite()
                    .created
                    .as_ref()
                    .map(|created| created.link.clone())
            })
            .unwrap_or_default();
        !link.is_empty()
    });
    link
}

#[test]
fn a_link_the_sheet_makes_admits_a_second_persons_device() {
    let _settled = ui::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    // This computer knows no community site, so the sheet hands over the invitation's own text: the
    // long form, as every build has.
    let community = home.config().join("community.toml");
    std::fs::write(&community, "site = \"\"\n").expect("a config naming no site");
    home.env().set("TILO_COMMUNITY_CONFIG", &community);
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let Hosted {
        _kept,
        mut session,
        seat,
        cert,
    } = hosted(&mut home);
    let person = hex::encode(cert.user_public_key);

    // They make a link at the part the sheet opens on: the sheet asks, the session makes it through
    // the desk, and the sheet shows the link back.
    let mut app = sheet_app(&seat);
    let link = press_create(&mut app, &mut session);
    assert!(
        link.len() > 40,
        "the link the sheet shows is a signed invitation: {link:?}"
    );
    assert_eq!(
        invitations_at_the_desk(&mut session).len(),
        1,
        "the link is the desk's own invitation"
    );
    capture(&mut app, "invite-sheet");

    // The link is shown as a code to scan, and the code is the invitation itself: a scanner reads
    // back exactly what the person hands over.
    let made = session
        .invite_sheet()
        .and_then(|seat| seat.invite().created)
        .expect("the link the sheet made");
    assert_eq!(
        scanned_back(&made),
        Ok(made.code.clone()),
        "the code is the invitation, read back out of the QR the sheet draws"
    );
    // Every part the sheet offers makes a link of its own, and every link's code is that invitation
    // the same way.
    for part in 0..seat.invite().roles.len() {
        app.harness()
            .perform_named(
                &format!("invite-role:segment:{part}"),
                &A11yAction::Activate,
            )
            .expect("the part's chip is pressed");
        app.settle();
        app.harness()
            .perform_named("invite-create", &A11yAction::Activate)
            .expect("the create button is pressed");
        app.settle();
        settle(&mut session);
        let made = session
            .invite_sheet()
            .and_then(|seat| seat.invite().created)
            .expect("a link for the part");
        assert_eq!(
            scanned_back(&made),
            Ok(made.code.clone()),
            "the code of a link at part {part} is the invitation, read back"
        );
    }

    // Her own device, of her own person, joins with what was handed to her.
    let her_root = SigningKey::from_bytes(&[0x81; 32]);
    let her_key = SigningKey::from_bytes(&[0x82; 32]);
    let her = certified(&her_root, &her_key, "Her laptop");
    let hers = joins_with(&link, workspace, &her, &her_key)
        .expect("the link admits her device into the workspace");

    // The workspace she took in has this computer's person in it already, and her in the part the
    // link grants.
    let people = members(&hers, workspace);
    assert!(
        people.iter().any(|(who, _)| who == &person),
        "she took in the workspace's people, not only her own admission: {people:?}"
    );
    assert_eq!(
        people
            .iter()
            .find(|(who, _)| *who == hex::encode(her.user_public_key))
            .map(|(_, role)| *role),
        Some(MemberRole::Editor),
        "in the part the sheet asked for: {people:?}"
    );

    // The session that made the link serves the workspace on the direct lane as well: her device
    // dials it there, names the workspace, and is admitted as one of its devices — the same link
    // every workspace's devices sync on.
    let (_, host_addr) = hers
        .host_addr(workspace)
        .expect("the workspace's host is named");
    let her_link = tilo_peer_state::direct::DirectSync::over(
        hers.replica(workspace).expect("her copy of the workspace"),
        her_key.clone(),
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime for the link");
    let endpoint = runtime
        .block_on(async {
            iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .relay_mode(iroh::RelayMode::Disabled)
                .secret_key(iroh::SecretKey::from_bytes(&her_key.to_bytes()))
                .bind()
                .await
        })
        .expect("her endpoint");
    let dialed = runtime.block_on(async {
        tokio::time::timeout(
            Duration::from_secs(5),
            her_link.connect(&endpoint, host_addr),
        )
        .await
    });
    assert!(
        dialed.is_err(),
        "the session keeps the workspace's direct link up, rather than refusing it or serving no \
         such lane: {dialed:?}"
    );
    runtime.block_on(endpoint.close());
}

/// A community site's shelf on this machine: it keeps the sealed invitations it is left, under the
/// handle the request names, and answers them back.
fn shelf() -> (test_http::Responder, Arc<Mutex<HashMap<String, String>>>) {
    let kept = Arc::new(Mutex::new(HashMap::<String, String>::new()));
    let serving = Arc::clone(&kept);
    let responder = test_http::Responder::handle(move |request| {
        let Some((_, handle)) = request.target.rsplit_once("/i/") else {
            return Some(test_http::Reply::status(404));
        };
        let handle = handle.to_owned();
        Some(match request.method.as_str() {
            "POST" => {
                let stored: serde_json::Value = serde_json::from_slice(&request.body).ok()?;
                let sealed = stored.get("sealed")?.as_str()?.to_owned();
                serving
                    .lock()
                    .expect("the shelf's invitations")
                    .insert(handle, sealed);
                test_http::Reply::status(204)
            }
            "GET" => {
                let sealed = serving
                    .lock()
                    .expect("the shelf's invitations")
                    .get(&handle)
                    .cloned()?;
                test_http::Reply::status(200)
                    .header("Content-Type", "application/json")
                    .body(serde_json::json!({ "sealed": sealed }).to_string())
            }
            _ => test_http::Reply::status(405),
        })
    });
    (responder, kept)
}

#[test]
fn a_link_left_on_the_community_site_is_short_and_the_site_never_sees_its_key() {
    let _settled = ui::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");

    // This computer's community site is a shelf on this machine, and the sheet is configured with
    // it: what it makes goes there, sealed.
    let (responder, kept) = shelf();
    let community = home.config().join("community.toml");
    std::fs::write(&community, format!("site = \"{}/\"\n", responder.url()))
        .expect("a community site");
    home.env().set("TILO_COMMUNITY_CONFIG", &community);
    let Hosted {
        _kept,
        mut session,
        seat,
        ..
    } = hosted(&mut home);

    // They make a link at the part the sheet opens on. What the sheet hands over is where the
    // invitation waits and the key that opens it — not a kilobyte of signed invitation.
    let mut app = sheet_app(&seat);
    let link = press_create(&mut app, &mut session);
    assert!(
        link.starts_with(&format!("{}/i/", responder.url())),
        "the sheet hands over the site's own link: {link}"
    );
    assert_eq!(
        link.chars().filter(|c| *c == '#').count(),
        1,
        "the key follows the fragment, and is the only one: {link}"
    );
    assert!(
        link.len() <= tilo_workspace_join::sealed::MAX_LINK_CHARS,
        "the link is short enough to scan: {link}"
    );
    let key = link
        .rsplit_once('#')
        .expect("a key in the link")
        .1
        .to_owned();
    assert_eq!(
        key.chars().count(),
        tilo_workspace_join::sealed::KEY_CHARS,
        "the fragment is the key"
    );

    // The code the sheet draws carries exactly that link, and a scanner reads it back: forty-one
    // modules, a version 6 code at medium error correction, not the version 39 the invitation's own
    // kilobytes need.
    let made = seat.invite().created.expect("the link the sheet made");
    assert_eq!(
        made.code,
        link.clone().into_bytes(),
        "the code is the link the person hands over"
    );
    capture(&mut app, "invite-sheet-short");
    let (read, version) = scanned(&made.code).expect("a scanner reads the short code");
    assert_eq!(read, made.code, "the scanner reads the link back whole");
    assert!(
        version <= 6,
        "a short link fits a version 6 code, and this one is version {version}"
    );

    // The site was left the invitation sealed: it holds ciphertext, and the key the link carries
    // never reached it.
    let requests = responder.drain_requests();
    let post = requests
        .iter()
        .find(|request| request.method == "POST")
        .expect("the shelf was left the invitation");
    assert!(post.target.starts_with("/i/"), "{}", post.target);
    let body = post.body_text();
    assert!(
        body.contains("\"sealed\""),
        "the invitation is sealed: {body}"
    );
    assert!(!body.contains(&key), "the key is in the link alone: {body}");
    assert!(
        !body.contains(&link),
        "the link itself is not sent either: {body}"
    );
    let handle = post.target.rsplit('/').next().expect("the handle");
    let kept = kept.lock().expect("the shelf's invitations");
    let sealed = kept
        .get(handle)
        .expect("the shelf keeps it under its handle");
    assert!(
        !sealed.contains(&key),
        "what the site holds does not carry the key: {sealed}"
    );
    drop(kept);
}
