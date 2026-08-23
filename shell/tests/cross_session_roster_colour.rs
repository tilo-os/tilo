//! The same person, rendered from two sessions, in the same colour.
//!
//! A single session's screenshot cannot show this defect: within one
//! process every surface reads one roster and agrees with itself. The
//! disagreement is between machines, so the proof has to be two frames.
//!
//! Both frames come from the production top bar — `TopBarChrome::draw`
//! over a real `WorkspaceTree` — and are read back as pixels rather than
//! as scene encodings, because "what colour is that avatar" is a
//! question about what landed on screen.
//!
//! ## The fixture
//!
//! Three real-shaped peer identities. `XAVER` and `ADA` hash onto the
//! same palette entry; `QUINTEN` does not. `ADA` sorts first.
//!
//! Session A has heard of Ada — she has been invited to the room but has
//! not arrived, so **she is not drawn**. Session B has not heard of her
//! at all. Everything else about the two rooms is identical.
//!
//! That asymmetry is the ordinary state of a room mid-join, and it used
//! to move Xaver one step along the palette on session A only: a person
//! nobody could see changed the colour of a person everybody could. The
//! two frames showed the same man in emerald and in cyan.
//!
//! Frames land in `target/nirios-screenshots/cross-session-roster-*.png`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

use shell::shell::state::{Collaborator, WorkspaceTree, roster_colors};
use shell::shell::topbar_chrome::TopBarChrome;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

mod support;

use support::offscreen::OffscreenRenderer;

/// Collides with `ADA` and sorts after her — the person under test.
const XAVER: &str = "peer@548826014839375280";
/// Collides with `XAVER` and sorts before him — invited, never drawn.
const ADA: &str = "peer@13630592132760172438";
/// Collides with neither; present in both rooms so the rosters overlap
/// in more than the disputed person.
const QUINTEN: &str = "peer@15970126346341786989";

const BAR_W: u32 = 1280;
const BAR_H: u32 = 48;

fn screenshots_dir() -> PathBuf {
    let root = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap_or(Path::new("."))
                .join("target")
        });
    root.join("nirios-screenshots")
}

fn try_new_renderer(w: u32, h: u32) -> Option<OffscreenRenderer> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        OffscreenRenderer::new(w, h)
    }))
    .ok()
}

/// One session's view of the room, as a tree the shell would hold.
///
/// `tree.collaborators` is the field `PeerPresence::refresh_roster`
/// writes and every roster surface reads, so this is the same seam a
/// live session's room arrives through.
fn session_view(roster: Vec<Collaborator>) -> WorkspaceTree {
    let mut tree = WorkspaceTree::bootstrap_default();
    tree.collaborators = roster;
    tree
}

/// Render the real top bar over `tree` and hand back its pixels.
fn paint_bar(r: &mut OffscreenRenderer, tree: &WorkspaceTree, png: &Path) -> Vec<u8> {
    let mut chrome = TopBarChrome::from_tree(tree);
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = chrome.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(BAR_W as f32),
                    height: Dimension::length(BAR_H as f32),
                },
                ..Default::default()
            },
            &[child],
        )
        .expect("top bar root");
    let mut tr = TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(BAR_W as f32),
                height: AvailableSpace::Definite(BAR_H as f32),
            },
            |known, avail, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, avail, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    chrome.apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    chrome.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    let bg = Color::from_rgb8(0x0b, 0x0d, 0x12);
    if let Some(dir) = png.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    r.render_png(&scene, bg, png);
    r.render(&scene, bg)
}

/// How many pixels in `rgba` sit within `tol` of `want` on every channel.
fn count_close(rgba: &[u8], want: (u8, u8, u8), tol: i32) -> usize {
    rgba.chunks_exact(4)
        .filter(|p| p[3] > 200)
        .filter(|p| {
            (p[0] as i32 - want.0 as i32).abs() <= tol
                && (p[1] as i32 - want.1 as i32).abs() <= tol
                && (p[2] as i32 - want.2 as i32).abs() <= tol
        })
        .count()
}

#[test]
fn two_sessions_render_the_same_person_in_the_same_colour() {
    // State the fixture's premise rather than trusting it. If the hash
    // or the palette moves, this fails as a stale fixture instead of
    // quietly passing without exercising a collision.
    assert_eq!(
        ui::presence::user_color(XAVER),
        ui::presence::user_color(ADA),
        "fixture is stale: {XAVER} and {ADA} no longer collide, so the \
         frames below would agree for the wrong reason"
    );
    assert!(
        ADA < XAVER,
        "fixture is stale: {ADA} must sort before {XAVER} for the old \
         sorted-walk to have moved him"
    );

    // Session A: Ada is in the room document but has not arrived, so the
    // avatar cluster does not draw her. Session B has not heard of her.
    // Both draw quinten and Xaver, in that order, so the two frames put
    // their discs in the same places and can be compared directly.
    let a = session_view(vec![
        Collaborator::new(QUINTEN, "quinten"),
        Collaborator::new(XAVER, "Xaver"),
        Collaborator::new(ADA, "Ada").as_invited(),
    ]);
    let b = session_view(vec![
        Collaborator::new(QUINTEN, "quinten"),
        Collaborator::new(XAVER, "Xaver"),
    ]);

    let want = ui::presence::user_color(XAVER);

    // Paint before asserting anything, so a failing run still leaves the
    // two frames on disk — a screenshot test whose evidence only exists
    // when it passes cannot be used to look at the defect.
    let Some(mut r) = try_new_renderer(BAR_W, BAR_H) else {
        eprintln!("no wgpu adapter — skipping");
        return;
    };
    let dir = screenshots_dir();
    let pa = dir.join("cross-session-roster-a-has-heard-of-ada.png");
    let pb = dir.join("cross-session-roster-b-has-not.png");
    let fa = paint_bar(&mut r, &a, &pa);
    let fb = paint_bar(&mut r, &b, &pb);

    // What each session decided, which the frames should now be showing.
    let decided_a = roster_colors(&a.collaborators).get(XAVER).copied();
    let decided_b = roster_colors(&b.collaborators).get(XAVER).copied();
    assert_eq!(
        decided_a,
        decided_b,
        "the two sessions disagree about Xaver: {decided_a:?} vs \
         {decided_b:?}. Frames: {} and {}",
        pa.display(),
        pb.display(),
    );
    assert_eq!(
        decided_a,
        Some(want),
        "Xaver must be his own identity's colour, not one settled around \
         whoever else the session can see"
    );

    // The avatar disc is a filled circle, so its colour is not one pixel
    // but a region. A handful of pixels would be antialiasing against
    // something else.
    let in_a = count_close(&fa, want, 6);
    let in_b = count_close(&fb, want, 6);
    assert!(
        in_a > 50 && in_b > 50,
        "Xaver's avatar must be painted in his own colour {want:?} on \
         both sessions (session A: {in_a} px, session B: {in_b} px). \
         Frames: {} and {}",
        pa.display(),
        pb.display(),
    );

    // And the colour the old sorted walk moved him onto must appear on
    // neither, or one of the two frames is still showing the wrong man.
    let bumped = ui::presence::PEER_PALETTE[(ui::presence::PEER_PALETTE
        .iter()
        .position(|c| *c == want)
        .expect("every user_color is a palette member")
        + 1)
        % ui::presence::PEER_PALETTE.len()];
    let bumped_a = count_close(&fa, bumped, 6);
    let bumped_b = count_close(&fb, bumped, 6);
    assert_eq!(
        (bumped_a, bumped_b),
        (0, 0),
        "the next palette entry {bumped:?} is on screen ({bumped_a} px on \
         session A, {bumped_b} px on session B) — a session moved Xaver \
         because of somebody it could see and the other could not. \
         Frames: {} and {}",
        pa.display(),
        pb.display(),
    );
}
