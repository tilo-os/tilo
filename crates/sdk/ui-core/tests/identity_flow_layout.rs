#![allow(
    clippy::similar_names,
    clippy::too_many_lines,
    reason = "one table-driven fixture per screen keeps every screen's setup in view"
)]

//! Layout and input of the identity ceremony screens.
//!
//! Every screen is mounted in the headless frame pipeline at a real
//! viewport, in every shipped theme family, and rasterised to
//! `$TILO_UI_SCREENSHOT_DIR/identity_<screen>_<theme>.png` when that
//! variable is set — the same opt-in the visual-journey tests use.
//!
//! The geometry assertion runs unconditionally: a screen must own the
//! whole viewport. A fixed 720×580 leaf centres its card in the top-left
//! corner of a 1280×800 window. Card centring and row alignment inside the card are pinned
//! by the unit tests in `ui/src/widget/identity_flow/sheet.rs`, which can
//! reach the pure layout directly.
//!
//! Every text field in the ceremony is the shared `TextInput`, so the
//! second test drives them the way a user, a screen reader and an
//! agent do — the host's Tab order, typed keys, the widget's own paste
//! shortcut and the accessibility `SetValue` action — and reads the result
//! back through the fields' accessibility values by `test_id`.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use collab_types::{ChannelId, Role, generate_keypair};
use getrandom::{SysRng, rand_core::UnwrapErr};
use threshold_identity::ThresholdIdentity;
use tilo_ui::State;
use tilo_ui::a11y::{Action, Role as AccessRole};
use tilo_ui::clipboard::{ClipboardBackend, ClipboardTarget};
use tilo_ui::headless::{FrameHarness, SemanticExpectation};
use tilo_ui::theme::Theme;
use tilo_ui::widget::QrWidget;
use tilo_ui::widget::identity_flow::{
    IdentityFlow, IdentityFlowError, IdentityFlowScreen, IdentityStore, InviteBrokerLocator,
    InviteTerms, MemoryIdentityStore, mint_invite_for_broker,
};
use tilo_ui::winit::dpi::PhysicalPosition;
use tilo_ui::winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
use vello::kurbo::Rect;

const VIEWPORT: (u32, u32) = (1280, 800);

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
    let mut rng = UnwrapErr(SysRng);
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
    let authority = tempfile::tempdir()?;
    let env = mint_invite_for_broker(
        InviteTerms {
            channel: ChannelId::new(),
            channel_name: "engineering".into(),
            owner_pubkey: owner_pk,
            inviter_label: "Alice".into(),
            role: Role::Editor,
            expires_at,
        },
        &owner_sk,
        InviteBrokerLocator {
            endpoint: "127.0.0.1:4433".into(),
            certificate: vec![0x2a; 64],
            authority_root: authority.path().into(),
        },
    )?;
    let url = env.to_url()?;
    flow.invite_mint.last_envelope = Some(env);
    flow.invite_mint.last_url = Some(url.clone());
    flow.invite_mint.qr = Some(QrWidget::new(url.as_bytes().to_vec()));
    flow.invite_accept.set_url(url);
    Ok(flow)
}

fn write_shot(harness: &FrameHarness, name: &str) {
    if let Some(dir) = tilo_ui::env::var_os("TILO_UI_SCREENSHOT_DIR") {
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
            let harness =
                FrameHarness::new_with_theme(State::new(), Box::new(flow), VIEWPORT, theme);
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

/// An in-process clipboard for the paste leg. The test must never read or
/// write the developer's real clipboard, and the toolkit's in-process
/// fallback only engages when no system clipboard exists at all — on a
/// desktop it would reach `arboard`.
struct MemoryClipboard(Mutex<Option<String>>);

impl ClipboardBackend for MemoryClipboard {
    fn set_text(&self, _target: ClipboardTarget, text: &str) -> bool {
        match self.0.lock() {
            Ok(mut slot) => {
                *slot = Some(text.to_owned());
                true
            }
            Err(_) => false,
        }
    }

    fn text(&self, _target: ClipboardTarget) -> Option<String> {
        self.0.lock().ok().and_then(|slot| slot.clone())
    }

    fn name(&self) -> &'static str {
        "f6-memory-clipboard"
    }
}

fn dark_harness(flow: IdentityFlow) -> FrameHarness {
    FrameHarness::new_with_theme(State::new(), Box::new(flow), VIEWPORT, Theme::dark())
}

fn focused_name(harness: &FrameHarness) -> Option<String> {
    harness.focused_access_node().and_then(|node| node.name)
}

fn expect_value(harness: &mut FrameHarness, target: &str, value: &str) {
    harness
        .expect(&SemanticExpectation::named(target).value(value))
        .unwrap_or_else(|e| panic!("{target} should read {value:?}: {e}"));
}

/// Paste the clipboard into the focused field through the shared widget's
/// own Ctrl+V — the same path a user's keystroke takes.
fn paste(harness: &mut FrameHarness, clipboard: &MemoryClipboard, text: &str) {
    assert!(clipboard.set_text(ClipboardTarget::Clipboard, text));
    harness.set_modifiers(&["ctrl".to_string()]);
    harness.key("v");
    harness.set_modifiers(&[]);
}

#[test]
fn identity_fields_are_shared_text_inputs_reached_by_tab_typing_and_paste() {
    let clipboard = Arc::new(MemoryClipboard(Mutex::new(None)));
    let services = tilo_ui::host::HostServices::null().with_clipboard(clipboard.clone());
    let _host = tilo_ui::host::enter(&services);

    // Create identity: the one field is a Tab stop, named for its label,
    // carries the seeded value, takes typed keys and a paste.
    let store: Arc<dyn IdentityStore> = Arc::new(MemoryIdentityStore::new());
    let mut flow = seeded_flow(store).expect("seed the identity ceremony");
    flow.set_screen(IdentityFlowScreen::CreateIdentity);
    let mut h = dark_harness(flow);
    let targets = h.targets();
    let screen = targets
        .get("identity_flow_create")
        .copied()
        .expect("the screen is addressable");
    let field = targets
        .get("identity_name_input")
        .copied()
        .expect("the name field is addressable by test_id");
    assert!(
        field.area() > 0.0 && screen.contains(field.center()),
        "the field {field:?} sits inside the screen {screen:?}"
    );
    assert!(h.tab(false).is_some(), "the field is a Tab stop");
    let node = h.focused_access_node().expect("Tab lands on a field");
    assert_eq!(node.role, AccessRole::TextInput);
    assert_eq!(node.name.as_deref(), Some("Display name"));
    assert_eq!(node.value.as_deref(), Some("Alice"), "the seeded value");
    h.key("End");
    h.type_text(" B");
    expect_value(&mut h, "identity_name_input", "Alice B");
    h.set_modifiers(&["ctrl".to_string()]);
    h.key("a");
    h.set_modifiers(&[]);
    paste(&mut h, &clipboard, "Carol");
    expect_value(&mut h, "identity_name_input", "Carol");
    write_shot(&h, "identity_create_identity_after_paste");

    // Recovery verify: Tab walks the blanks by word position, not by the
    // order the challenge stored them, and each is named for its word.
    let store: Arc<dyn IdentityStore> = Arc::new(MemoryIdentityStore::new());
    let mut flow = seeded_flow(store).expect("seed the identity ceremony");
    flow.set_screen(IdentityFlowScreen::RecoveryVerify);
    flow.recovery_verify.seed_challenge([21, 2, 11]);
    let word_two = flow
        .recovery_verify
        .challenge()
        .expect("a seeded challenge")
        .expected_word(1);
    let mut h = dark_harness(flow);
    h.tab(false);
    assert_eq!(focused_name(&h).as_deref(), Some("Word 2"));
    h.tab(false);
    assert_eq!(focused_name(&h).as_deref(), Some("Word 11"));
    h.tab(false);
    assert_eq!(focused_name(&h).as_deref(), Some("Word 21"));
    h.tab(true);
    h.tab(true);
    assert_eq!(
        focused_name(&h).as_deref(),
        Some("Word 2"),
        "Shift+Tab walks back in reading order"
    );
    h.type_text(&word_two);
    expect_value(&mut h, "identity_recovery_blank_1", &word_two);
    write_shot(&h, "identity_recovery_verify_after_typing");

    // A drag that ends after Tab has moved focus on: the release still
    // reaches the blank it started in, so the drag ends there and later
    // pointer moves no longer extend its selection.
    let blank = h
        .targets()
        .get("identity_recovery_blank_1")
        .copied()
        .expect("the typed blank is addressable");
    let cy = blank.center().y;
    let start = (blank.x0 + 14.0, cy);
    let end = (blank.x1 - 6.0, cy);
    h.send(&cursor_moved(start));
    h.send(&mouse(ElementState::Pressed));
    h.send(&cursor_moved(end));
    let selecting = selection_of(&h, "Word 2");
    assert!(
        selecting.is_some(),
        "the drag reports a selection through accessibility"
    );
    h.tab(false);
    assert_eq!(focused_name(&h).as_deref(), Some("Word 11"));
    h.send(&mouse(ElementState::Released));
    let settled = selection_of(&h, "Word 2");
    h.send(&cursor_moved(start));
    assert_eq!(
        selection_of(&h, "Word 2"),
        settled,
        "after the release a pointer move no longer drags the old blank's selection"
    );
    expect_value(&mut h, "identity_recovery_blank_1", &word_two);

    // Pair device: a typed digit moves on, a paste spreads across the
    // cells, and Tab from the last cell reaches the payload textarea.
    let store: Arc<dyn IdentityStore> = Arc::new(MemoryIdentityStore::new());
    let mut flow = seeded_flow(store).expect("seed the identity ceremony");
    flow.set_screen(IdentityFlowScreen::PairDevice);
    let mut h = dark_harness(flow);
    // The screen opens on the code: the first cell already holds focus and
    // the host has adopted it, so the digits land without a click or Tab.
    assert_eq!(focused_name(&h).as_deref(), Some("Pairing code digit 1"));
    // Moving the caret first (End, ArrowRight) must not turn the next digit
    // into a second character: a filled cell is replaced, never appended to.
    h.key("End");
    h.key("ArrowRight");
    h.type_text("9");
    expect_value(&mut h, "identity_pair_code_0", "9");
    assert_eq!(
        focused_name(&h).as_deref(),
        Some("Pairing code digit 2"),
        "a digit moves on to the next cell"
    );
    paste(&mut h, &clipboard, "2468");
    for (i, digit) in ["9", "2", "4", "6", "8", ""].iter().enumerate() {
        expect_value(&mut h, &format!("identity_pair_code_{i}"), digit);
    }
    assert_eq!(focused_name(&h).as_deref(), Some("Pairing code digit 6"));
    // A selection change through the accessibility door is not an edit:
    // no digit spreads and focus does not move.
    h.perform_named(
        "identity_pair_code_2",
        &Action::SetSelection {
            anchor: 0,
            focus: 1,
        },
    )
    .expect("SetSelection on a cell");
    for (i, digit) in ["9", "2", "4", "6", "8", ""].iter().enumerate() {
        expect_value(&mut h, &format!("identity_pair_code_{i}"), digit);
    }
    assert_eq!(
        focused_name(&h).as_deref(),
        Some("Pairing code digit 6"),
        "a SetSelection neither spreads digits nor moves focus"
    );
    h.tab(false);
    let payload = h.focused_access_node().expect("Tab reaches the payload");
    assert_eq!(payload.role, AccessRole::MultilineTextInput);
    assert_eq!(payload.name.as_deref(), Some("Pairing payload"));
    h.type_text("payload");
    expect_value(&mut h, "identity_pair_payload", "payload");
    write_shot(&h, "identity_pair_device_after_paste");

    // Invite accept: the automation door. An agent sets the link with the
    // accessibility `SetValue` action and reads it back by test_id.
    let store: Arc<dyn IdentityStore> = Arc::new(MemoryIdentityStore::new());
    let mut flow = seeded_flow(store).expect("seed the identity ceremony");
    flow.set_screen(IdentityFlowScreen::InviteAccept);
    flow.invite_accept.decline();
    let (owner_sk, owner_pk) = generate_keypair();
    let authority = tempfile::tempdir().expect("private invite authority");
    let url = mint_invite_for_broker(
        InviteTerms {
            channel: ChannelId::new(),
            channel_name: "design".into(),
            owner_pubkey: owner_pk,
            inviter_label: "Alice".into(),
            role: Role::Editor,
            expires_at: SystemTime::now()
                .checked_add(Duration::from_secs(3600))
                .unwrap_or_else(SystemTime::now),
        },
        &owner_sk,
        InviteBrokerLocator {
            endpoint: "127.0.0.1:4433".into(),
            certificate: vec![0x2a; 64],
            authority_root: authority.path().into(),
        },
    )
    .and_then(|env| env.to_url())
    .expect("mint an invite");
    let mut h = dark_harness(flow);
    expect_value(&mut h, "identity_invite_url", "");
    h.perform_named("identity_invite_url", &Action::SetValue(url.clone()))
        .expect("SetValue through the accessibility door");
    expect_value(&mut h, "identity_invite_url", &url);
    // Typing after a caret move edits at the caret, not at the end.
    h.tab(false);
    h.key("Home");
    h.type_text("x");
    expect_value(&mut h, "identity_invite_url", &format!("x{url}"));
    write_shot(&h, "identity_invite_accept_after_set_value");
}

fn cursor_moved(at: (f64, f64)) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: PhysicalPosition::new(at.0, at.1),
    }
}

fn mouse(state: ElementState) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state,
        button: MouseButton::Left,
    }
}

/// The text selection the exported accessibility tree reports for the
/// field named `name`, as a comparable string; `None` when the field has
/// no selection or is not in the tree.
fn selection_of(harness: &FrameHarness, name: &str) -> Option<String> {
    harness
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some(name))
        .and_then(|(_, node)| node.text_selection().map(|sel| format!("{sel:?}")))
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
