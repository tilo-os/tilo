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

use std::time::{Duration, SystemTime};

use ed25519_dalek::SigningKey;
use tilo_app_test_host::TestApp;
use tilo_compositor::InputEvent;
use tilo_desktop_model::WorkspaceId;
use tilo_network_actor::DeviceIdentity;
use tilo_peer_state::join::Role;
use tilo_peer_state::owner::{self, NativeWorkspaceOwner};
use tilo_session::Session;
use tilo_sheets::SheetState as _;
use tilo_sheets::invite::InviteApp;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};
use ui::a11y::Action as A11yAction;

use crate::common::held::{certified, this_computer_with_an_identity};
use crate::common::{HEIGHT, WIDTH};

/// The identity of `cert` 's device, with no identity root of its own: what redeems a link is a
/// device, and this one joins only what it is handed.
fn identity(cert: &DeviceCert, key: &SigningKey) -> DeviceIdentity {
    DeviceIdentity::kept(cert.clone(), key.clone())
}

fn settle(session: &mut Session) {
    for _ in 0..6 {
        session.step(Duration::from_millis(1)).expect("a frame");
    }
}

/// Who `owner` 's copy of `workspace` has in it, by person, in the part each holds.
fn members(owner: &NativeWorkspaceOwner, workspace: WorkspaceId) -> Vec<(String, Option<Role>)> {
    owner
        .with_current(workspace, |view| {
            let members = view
                .peer()
                .governance()
                .governed_members(SystemTime::now())?
                .members;
            Ok(members
                .into_iter()
                .map(|member| {
                    (
                        member.id.person_id().unwrap_or_default().to_owned(),
                        member.role,
                    )
                })
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
            workspace,
            // Her owner holds no copy of the workspace yet: the enrollment makes its own slot for
            // it.
            None,
            link.to_owned(),
            owner.clone(),
        )
        .await?;
        owner
            .install(&enrollment, SystemTime::now())
            .map_err(|error| error.to_string())
    })?;
    Ok(owner)
}

/// Where the bar's invite control stands, when the person may invite here.
fn bar_control(session: &Session) -> Option<(f64, f64, f64, f64)> {
    session
        .controls()
        .into_iter()
        .find(|control| control.name == "Invite" || control.name == "topbar:invite")
        .map(|control| control.rect)
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

/// The invitations the invite sheet lists for the workspace in front, once there are `count`.
fn invitations_on_the_sheet(session: &mut Session, count: usize) -> usize {
    let mut listed = 0;
    crate::common::until(session, "the sheet lists the links made", |session| {
        listed = session
            .invite_sheet()
            .map_or(0, |state| state.invite().pending.len());
        listed == count
    });
    listed
}

/// Decode pixels painted by the invitation sheet on its screen, using the same
/// detector as Camera. The decoder returns the QR payload and version.
fn scanned(app: &mut TestApp<InviteApp>) -> Result<(Vec<u8>, usize), String> {
    app.settle();
    let harness = app.harness();
    let (width, height) = harness.device_size();
    let rgba = image::RgbaImage::from_raw(width, height, harness.render_rgba())
        .ok_or("the screen did not paint")?;
    let mut prepared =
        rqrr::PreparedImage::prepare(image::DynamicImage::ImageRgba8(rgba).to_luma8());
    for grid in prepared.detect_grids() {
        let mut read = Vec::new();
        if let Ok(meta) = grid.decode_to(&mut read) {
            return Ok((read, meta.version.0));
        }
    }
    Err("a scanner finds no readable QR on the sheet's screen".into())
}

/// A session on a shell showing the workspace it made, as the shell of the device's daemon,
/// with the invite sheet up as the person who may invite there sees it: everything either
/// test does before it can make a link.
struct Hosted {
    session: crate::common::held::Hosted,
    state: tilo_sheets::invite::InviteState,
    /// This computer's person, by their key.
    person: [u8; 32],
    workspace: WorkspaceId,
}

/// [`Hosted`] — this computer holding the workspace its session made, its bar offering Invite,
/// and the sheet the person's press puts up.
fn hosted(home: &mut tilo_test_env::TestHome) -> Hosted {
    let (_root, person) = this_computer_with_an_identity(home);
    let mut session = crate::common::held::hosted(
        tilo_session::Options {
            collab: true,
            person: "Ada".to_owned(),
            ..tilo_session::Options::default()
        },
        &[tilo_compositor::OutputConfig::new(
            "HEADLESS-1",
            WIDTH,
            HEIGHT,
        )],
    );
    settle(&mut session);
    // The daemon founds the person's own workspace: Invite is offered once it is here.
    crate::common::until(&mut session, "the daemon's workspace is here", |session| {
        session
            .handle()
            .read(|tree| tree.active_workspace().is_some())
    });

    // The person the workspace lets invite finds Invite on the bar, and
    // pressing it puts the sheet up. The pointer comes up to the bar first,
    // as the person's does to press it.
    session
        .compositor()
        .input(InputEvent::PointerMotion { x: 1100.0, y: 16.0 })
        .expect("the pointer");
    let mut place = None;
    crate::common::until(&mut session, "the bar offers Invite", |session| {
        place = bar_control(session);
        place.is_some()
    });
    press(&mut session, place.expect("the bar's Invite"));
    settle(&mut session);
    let sheet = session
        .invite_sheet()
        .expect("the session hosts the invite sheet")
        .clone();
    assert!(
        sheet.presentation().is_open(),
        "the bar's Invite puts the invite sheet up"
    );
    assert!(sheet.invite().may_invite, "the person may invite here");
    let workspace = session
        .handle()
        .read(|tree| {
            tree.active_workspace()
                .map(tilo_desktop_model::Workspace::id)
        })
        .expect("the workspace the session made");
    Hosted {
        session,
        state: sheet,
        person,
        workspace,
    }
}

/// The sheet for `state` , drawn on a screen.
fn sheet_app(state: &tilo_sheets::invite::InviteState) -> TestApp<InviteApp> {
    let mut app = TestApp::<InviteApp>::for_person_with_lent("person", state.clone())
        .expect("the sheet draws");
    app.harness()
        .resize((WIDTH as u32, HEIGHT as u32))
        .expect("a screen");
    app.settle();
    app
}

/// Press the sheet's create button, and answer with the link it shows once the session has it: a
/// short link is shown when its site has kept it.
fn press_create(app: &mut TestApp<InviteApp>, session: &mut Session) -> String {
    app.harness()
        .perform_named("invite-create", &A11yAction::Activate)
        .expect("the create button is pressed");
    app.settle();
    settle(session);
    let mut link = String::new();
    let mut note = None;
    let made = crate::common::reached(session, |session| {
        let invite = session
            .invite_sheet()
            .map(tilo_sheets::invite::InviteState::invite);
        link = invite
            .as_ref()
            .and_then(|invite| invite.created.as_ref())
            .map(|created| created.link.clone())
            .unwrap_or_default();
        note = invite
            .as_ref()
            .and_then(|invite| invite.note.as_ref())
            .map(|note| note.text.clone());
        !link.is_empty()
    });
    assert!(
        made,
        "the sheet shows the link it made: it says {note:?}; the shell's refusals: {:?}; the \
         session says: {:?}",
        session.shell_refusals(),
        session.accessible_labels()
    );
    link
}

#[test]
fn a_link_the_sheet_makes_admits_a_second_persons_device() {
    let _settled = ui::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    // The sheet hands over the link that names the invitation this computer keeps in the
    // workspace, and her device fetches it from here: no site is asked for anything.
    let community = home.config().join("community.toml");
    std::fs::write(&community, "site = ''").expect("no community site");
    home.env().set("TILO_COMMUNITY_CONFIG", &community);
    // It relays through no one: the link names the addresses of this computer's own
    // network, which her device dials directly, rather than a public relay a test cannot count on.
    home.env().set(settings_store::relay::ENV, "none");
    let Hosted {
        mut session,
        state,
        person,
        workspace,
    } = hosted(&mut home);
    let person = hex::encode(person);

    // They make a link at the part the sheet opens on: the sheet asks, the session makes it through
    // the shell, and the sheet shows the link back.
    let mut app = sheet_app(&state);
    let link = press_create(&mut app, &mut session);
    let locator = tilo_workspace_join::native::InvitationLink::parse(&link)
        .unwrap_or_else(|| panic!("the link names the invitation its workspace keeps: {link:?}"));
    assert_eq!(locator.reference.workspace, workspace);
    assert_eq!(
        invitations_on_the_sheet(&mut session, 1),
        1,
        "the link is the workspace's invitation the person made"
    );
    crate::common::app_shot(&mut app, "invite-sheet");

    // The link is shown as a code to scan: a scanner reads back exactly what the person hands over.
    let made = session
        .invite_sheet()
        .and_then(|state| state.invite().created)
        .expect("the link the sheet made");
    assert_eq!(
        scanned(&mut app).map(|(read, _)| read),
        Ok(made.link.clone().into_bytes()),
        "the code is the link, read back out of the QR the sheet draws"
    );
    // Every part the sheet offers makes a link of its own, and every link's code is that link the
    // same way.
    let mut shown = made.link;
    for part in 0..state.invite().roles.len() {
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
        // The link is signed off the loop and shows once it landed.
        crate::common::until(&mut session, "the sheet shows the part's link", |session| {
            session
                .invite_sheet()
                .and_then(|state| state.invite().created)
                .is_some_and(|made| made.link != shown)
        });
        let made = session
            .invite_sheet()
            .and_then(|state| state.invite().created)
            .expect("a link for the part");
        shown.clone_from(&made.link);
        assert_eq!(
            scanned(&mut app).map(|(read, _)| read),
            Ok(made.link.clone().into_bytes()),
            "the code of a link at part {part} is its link, read back"
        );
    }

    // Her own device, of her own person, joins with what was handed to her.
    let her_root = SigningKey::from_bytes(&[0x81; 32]);
    let her_key = SigningKey::from_bytes(&tilo_test_env::node_key());
    let her = certified(
        &her_root,
        &her_key,
        "Her laptop",
        DeviceCapabilities::member(),
    );
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
        Some(Some(Role::Editor)),
        "in the part the sheet asked for: {people:?}"
    );

    // The session that made the link serves the workspace on the direct connection as well: her device
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
         such link: {dialed:?}"
    );
    runtime.block_on(endpoint.close());
}

#[test]
fn a_native_invitation_scans_from_the_sheet_and_admits_over_iroh() {
    let _settled = ui::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let community = home.config().join("community.toml");
    std::fs::write(&community, "site = ''").expect("no community site");
    home.env().set("TILO_COMMUNITY_CONFIG", &community);
    // Use the signed direct routes.
    home.env().set(settings_store::relay::ENV, "none");
    let Hosted {
        mut session,
        state,
        workspace,
        ..
    } = hosted(&mut home);
    let mut app = sheet_app(&state);
    let link = press_create(&mut app, &mut session);
    let locator = tilo_workspace_join::native::InvitationLink::parse(&link)
        .expect("a compact native invitation");
    assert_eq!(locator.reference.workspace, workspace);
    assert_eq!(invitations_on_the_sheet(&mut session, 1), 1);
    crate::common::app_shot(&mut app, "invite-sheet-native");
    let mut read = String::new();
    for theme in [ui::theme::Theme::light(), ui::theme::Theme::dark()] {
        app.harness().set_theme(theme);
        let (bytes, _) = scanned(&mut app).expect("Camera can read the painted QR");
        assert_eq!(bytes, link.as_bytes());
        read = String::from_utf8(bytes).expect("the QR carries a link");
    }

    let root = SigningKey::from_bytes(&[0x81; 32]);
    let key = SigningKey::from_bytes(&tilo_test_env::node_key());
    let cert = certified(&root, &key, "Guest", DeviceCapabilities::member());
    let owner = joins_with(&read, workspace, &cert, &key)
        .expect("the scanned link fetches the signed invitation and joins");
    assert!(
        members(&owner, workspace)
            .contains(&(hex::encode(cert.user_public_key), Some(Role::Editor),))
    );
    assert!(
        joins_with(&read, workspace, &cert, &key).is_err(),
        "a spent link cannot be fetched again"
    );
}
