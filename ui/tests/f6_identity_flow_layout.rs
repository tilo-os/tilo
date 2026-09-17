//! F6 (window chrome / identity flow) alignment review: the identity
//! ceremony screens.
//!
//! Every screen is mounted in the headless frame pipeline at a real
//! viewport, in every shipped theme family, and rasterised to
//! `$QUICK_UI_SCREENSHOT_DIR/identity_<screen>_<theme>.png` when that
//! variable is set — the same opt-in the visual-journey tests use.
//!
//! The geometry assertion runs unconditionally: a screen must own the
//! whole viewport. A fixed 720×580 leaf centres its card in the top-left
//! corner of a 1280×800 window, which is how the ceremony looked before
//! this review. Card centring and row alignment inside the card are pinned
//! by the unit tests in `ui/src/widget/identity_flow/sheet.rs`, which can
//! reach the pure layout directly.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use collab_types::{ChannelId, Role, generate_keypair};
use rand_core::OsRng;
use threshold_identity::ThresholdIdentity;
use ui::State;
use ui::headless::FrameHarness;
use ui::theme::Theme;
use ui::widget::identity_flow::{
    IdentityFlow, IdentityFlowError, IdentityFlowScreen, IdentityStore, MemoryIdentityStore,
    configure_invite_broker, mint_invite,
};
use vello::kurbo::Rect;

const VIEWPORT: (u32, u32) = (1280, 800);

/// `mint_invite` refuses to run until the shell has installed a redemption
/// broker (outside the crate's own `cfg(test)` there is no fallback). The
/// endpoint is never contacted here — the URL only has to parse and verify
/// — so a loopback address, an opaque certificate and a private temp
/// directory stand in for the shell's broker. Process-wide, like the real
/// configuration; a sibling test that already configured one wins.
fn ensure_invite_broker() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let root = std::env::temp_dir().join(format!(
            "qos-f6-invite-authority-{}",
            std::process::id()
        ));
        let _ = configure_invite_broker("127.0.0.1:4433", vec![0x2a; 64], root);
    });
}

/// `(screen, test_id, file tag)` for every screen the ceremony can show.
const SCREENS: &[(IdentityFlowScreen, &str, &str)] = &[
    (
        IdentityFlowScreen::CreateIdentity,
        "identity_flow_create",
        "create_identity",
    ),
    (
        IdentityFlowScreen::RecoveryShare,
        "identity_flow_recovery_share",
        "recovery_share",
    ),
    (
        IdentityFlowScreen::RecoveryVerify,
        "identity_flow_recovery_verify",
        "recovery_verify",
    ),
    (
        IdentityFlowScreen::AddDeviceHost,
        "identity_flow_add_device_host",
        "add_device_host",
    ),
    (
        IdentityFlowScreen::PairDevice,
        "identity_flow_pair_device",
        "pair_device",
    ),
    (
        IdentityFlowScreen::InviteMint,
        "identity_flow_invite_mint",
        "invite_mint",
    ),
    (
        IdentityFlowScreen::InviteAccept,
        "identity_flow_invite_accept",
        "invite_accept",
    ),
];

fn themes() -> Vec<(&'static str, Theme)> {
    vec![
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
        // The body outruns the card here on the word-grid screens; the rows
        // must scroll under a clip rather than run into the action row.
        ("text-2x", Theme::dark().with_text_scale(2.0)),
    ]
}

/// Mirrors `examples/identity_flow_demo.rs::seed_flow`: a real 2-of-2 DKG
/// for the recovery words, an in-progress enrolment for the host screen,
/// a minted invite for both invite screens, and a name plus typed code so
/// every field paints its populated state rather than a placeholder.
fn seeded_flow(store: Arc<dyn IdentityStore>) -> Result<IdentityFlow, IdentityFlowError> {
    ensure_invite_broker();
    let mut rng = OsRng;
    let mut flow = IdentityFlow::new(store);
    let id = ThresholdIdentity::bootstrap_single_device(&mut rng)?;

    let mut recovery_host = ThresholdIdentity::bootstrap_single_device(&mut rng)?;
    let (mut host_session, host_round1) = recovery_host.begin_enrolment(1, 2, 2, &mut rng)?;
    let mut recovery_peer = ThresholdIdentity::bootstrap_single_device(&mut rng)?;
    let (mut peer_session, peer_round1) = recovery_peer.begin_enrolment(2, 2, 2, &mut rng)?;
    let _host_round2 = host_session.receive_round1(std::slice::from_ref(&peer_round1))?;
    let peer_round2 = peer_session.receive_round1(std::slice::from_ref(&host_round1))?;
    let peer_to_host = peer_round2
        .into_iter()
        .find(|(recipient, _)| *recipient == 1)
        .map(|(_, package)| package)
        .ok_or_else(|| IdentityFlowError::DecodeFailed("missing peer DKG reply".to_string()))?;
    host_session.receive_round2(&[peer_to_host])?;
    recovery_host.adopt_dkg_outcome(host_session)?;
    let recovery = recovery_host.export_recovery_share()?;
    flow.recovery_share.set_mnemonic(&recovery.mnemonic);
    flow.recovery_verify.seed_mnemonic(recovery.mnemonic);
    flow.recovery_verify.seed_challenge([2, 11, 21]);
    flow.recovery_verify.set_answer(0, "typed");

    {
        let (sess, r1) = {
            let mut id_local = ThresholdIdentity::bootstrap_single_device(&mut rng)?;
            id_local.begin_enrolment(1, 2, 2, &mut rng)?
        };
        flow.add_device_host
            .begin(r1, sess, *id.group_public_key())?;
    }

    flow.create_identity.set_display_name("Alice");
    flow.pair_device.set_code("4815");

    let (owner_sk, owner_pk) = generate_keypair();
    let expires_at = SystemTime::now()
        .checked_add(Duration::from_secs(3600))
        .unwrap_or_else(SystemTime::now);
    flow.invite_mint
        .set_channel(ChannelId::new(), "engineering");
    flow.invite_mint
        .mint_now(owner_pk, &owner_sk, "Alice", Role::Editor, SystemTime::now())?;
    let env = mint_invite(
        ChannelId::new(),
        "engineering",
        owner_pk,
        &owner_sk,
        "Alice",
        Role::Editor,
        expires_at,
    )?;
    flow.invite_accept.set_url(env.to_url()?);
    Ok(flow)
}

fn write_shot(harness: &FrameHarness, name: &str) {
    if let Some(dir) = std::env::var_os("QUICK_UI_SCREENSHOT_DIR") {
        let path = Path::new(&dir).join(format!("{name}.png"));
        harness.render_window_png(&path);
        eprintln!("wrote {}", path.display());
    }
}

#[test]
fn identity_screens_fill_the_viewport() {
    let viewport = Rect::new(0.0, 0.0, f64::from(VIEWPORT.0), f64::from(VIEWPORT.1));
    let mut failures = Vec::new();
    for (screen, test_id, tag) in SCREENS {
        for (theme_name, theme) in themes() {
            let store: Arc<dyn IdentityStore> = Arc::new(MemoryIdentityStore::new());
            let mut flow = seeded_flow(store).expect("seed the identity ceremony");
            flow.set_screen(*screen);
            let harness = FrameHarness::new_with_theme(State::new(), Box::new(flow), VIEWPORT, theme);
            write_shot(&harness, &format!("identity_{tag}_{theme_name}"));
            let targets = harness.targets();
            let Some(rect) = targets.get(*test_id).copied() else {
                failures.push(format!("{tag}/{theme_name}: no target named {test_id}"));
                continue;
            };
            if (rect.width() - viewport.width()).abs() > 0.5
                || (rect.height() - viewport.height()).abs() > 0.5
            {
                failures.push(format!(
                    "{tag}/{theme_name}: screen is {}x{}, must fill the {}x{} viewport",
                    rect.width(),
                    rect.height(),
                    viewport.width(),
                    viewport.height()
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} identity layout defect(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn done_card_renders_for_review() {
    let store: Arc<dyn IdentityStore> = Arc::new(MemoryIdentityStore::new());
    let mut seeded = seeded_flow(store.clone()).expect("seed the identity ceremony");
    seeded
        .create_identity
        .submit_to_store(store.as_ref())
        .expect("a named identity saves");
    for (theme_name, theme) in themes() {
        let mut flow = IdentityFlow::new(store.clone());
        flow.set_screen(IdentityFlowScreen::Done);
        assert!(flow.is_done(), "the terminal screen reports done");
        let harness = FrameHarness::new_with_theme(State::new(), Box::new(flow), VIEWPORT, theme);
        write_shot(&harness, &format!("identity_done_{theme_name}"));
    }
}
