//! The first press of **Invite** opens the composer.
//!
//! The collaborator dropdown and the notification stack are anchored to
//! the same top-right corner, and `draw` lays the stack down first — so
//! the panel is painted *over* whatever toasts are up. The pointer path
//! disagreed: toast click-to-dismiss ran before the dropdown's own
//! hit-test, so a press aimed at Invite was spent dismissing a card the
//! person could not even see. It looked like a dead button, and the
//! second press worked because the toast it ate was gone by then.
//!
//! That is not a hypothetical arrangement. Opening an app raises launch
//! toasts (`search::providers::applications`), and the gesture that
//! follows in the demo is exactly: open the room's people, press Invite.
//! The invite that beat produced was minted for the wrong person,
//! because the first attempt silently did nothing and only the second
//! one ran.
//!
//! Everything below drives the production chain — `FrameBuilder::
//! dispatch_event` → `ShellOverlayHost::handle_event` → the per-frame
//! pump — and clicks the rect the compositor publishes for
//! `collab_invite`, which is the rect the pixels land in. The press is
//! the **first** one on a panel opened once: a test that pressed twice,
//! or reopened the panel, would pass against the broken code.

use automerge::AutoCommit;
use shell::compositor::frame::FrameBuilder;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};

const W: u32 = 1920;
const H: u32 = 1080;

/// One production frame: the state pump, layout, paint. The paint is
/// what publishes the chrome's hit rects, so a click by name after this
/// aims at what was drawn.
fn frame(fb: &mut FrameBuilder, doc: &mut AutoCommit) {
    let root = automerge::ROOT;
    fb.pump_apps(doc, &root);
    fb.relayout();
    let _ = fb.paint();
}

/// A real click: move, press, release — each its own dispatch, so the
/// per-event hit-tests run in the order hardware produces them.
fn click(fb: &mut FrameBuilder, doc: &mut AutoCommit, at: vello::kurbo::Point) {
    let root = automerge::ROOT;
    let mods = Modifiers::default();
    let mut focus = None;
    for ev in [
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(at.x, at.y),
        },
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        },
    ] {
        fb.dispatch_event(&ev, &mods, doc, &root, &mut focus);
    }
}

/// A session that may invite: peer-sync on, and the local user owning
/// the room they are looking at. Both are what the demo's host has, and
/// `may_invite_here` refuses — painting no button at all — without them.
fn a_room_its_owner_can_invite_into(fb: &FrameBuilder, toasts: usize) {
    let chrome = fb.chrome_handle();
    let mut st = chrome.borrow_mut();
    st.peer_sync_enabled = true;
    let me = st.local_user_id.clone();
    {
        let mut tree = st.tree_mut();
        let active = tree.active;
        let ws = tree
            .workspaces
            .iter_mut()
            .find(|w| w.id == active)
            .expect("the session boots into a workspace");
        ws.owner = Some(me);
    }
    // What a launch leaves on screen: the progress toast and the one
    // that says it finished. Two is the count that reaches the Invite
    // button — the first card sits above it, the second lands on it.
    for i in 0..toasts {
        st.push_notification(format!("Opening an app ({i})"), "in progress");
    }
    st.bump();
}

#[test]
fn the_first_press_of_invite_opens_the_composer_with_toasts_on_screen() {
    let mut fb = FrameBuilder::new((W, H));
    let mut doc = AutoCommit::new();
    a_room_its_owner_can_invite_into(&fb, 2);
    frame(&mut fb, &mut doc);
    let toasts_before = fb.chrome().notifications().len();
    assert_eq!(
        toasts_before, 2,
        "the arrangement this test is about needs the toasts to be live"
    );

    // Opened the way a person opens it: the top bar's avatar stack.
    let stack = fb
        .widget_registry()
        .get("topbar_avatar_stack")
        .copied()
        .expect("the top bar publishes an avatar stack to click");
    click(&mut fb, &mut doc, stack.center());
    frame(&mut fb, &mut doc);
    assert!(
        fb.chrome().overlays.collaborators,
        "the avatar stack must open the room's people"
    );

    let invite = fb
        .widget_registry()
        .get("collab_invite")
        .copied()
        .expect("the panel paints an Invite button, and publishes its rect");

    // The first press. One.
    click(&mut fb, &mut doc, invite.center());
    frame(&mut fb, &mut doc);

    assert!(
        fb.chrome().peer_sync_invite_name.is_some(),
        "the first press of Invite must raise the composer that asks who \
         the invitation is for; it opened nothing, and the press at \
         {:?} went somewhere else",
        invite.center()
    );
    assert_eq!(
        fb.chrome().notifications().len(),
        toasts_before,
        "and it must not have been spent on a toast underneath the panel"
    );
}
