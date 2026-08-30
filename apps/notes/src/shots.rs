//! Headless `--shots` mode: render one offscreen frame of the notes
//! app to a PNG and exit. Used for visual review and CI snapshots at a
//! chosen viewport, without opening a window.
//!
//! The workspace is seeded into a throwaway temp directory so a shot is
//! hermetic and never touches the user's real `~/notes`. Sample content
//! only ever lives on this path — the shipped app starts from the
//! user's own pages.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use taffy::prelude::*;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::{PermissionContext, UserId};
use ui::presence::{PathSegment, PresenceCtx, PresenceKey, SelectionRange, draw_remote_presence};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, SmolStr};

use crate::NotesApp;
use crate::model::Workspace;
use crate::num;
use crate::storage;

/// Default viewport when `--viewport` is omitted. A 16:9-ish desktop
/// size that shows the sidebar and a full page.
pub const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 720);

/// Parse a `WxH` string like `800x600` into a `(u32, u32)` viewport.
pub fn parse_viewport(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x').or_else(|| s.split_once('X'))?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Seed a small, realistic workspace into `root`: a handful of pages in
/// the tree plus one content-rich note selected so the shot shows real
/// blocks (headings, lists, checklist, code, an inline link).
fn seed_sample_workspace(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut ws = Workspace::new();

    let inbox = ws.create_page(None);
    ws.rename(inbox, "Inbox");
    let projects = ws.create_page(None);
    ws.rename(projects, "Projects");
    let launch = ws.create_page(Some(projects));
    ws.rename(launch, "Launch checklist");
    let reading = ws.create_page(None);
    ws.rename(reading, "Reading list");
    let recipes = ws.create_page(None);
    ws.rename(recipes, "Recipes");
    let formulas = ws.create_page(None);
    ws.rename(formulas, "Formula notebook");

    ws.set_selected(Some(launch));

    let bodies: [(uuid::Uuid, &str); 6] = [
        (
            launch,
            "\
What has to land before we ship. Owner in **bold**.

## This week

- [x] Finalise the onboarding copy

- [x] Cut the release branch

- [ ] Write the migration guide

- [ ] Record the walkthrough video

## Notes

See [[Reading list]] for sources.

```sh
git checkout -b release/v1.0
git tag -a v1.0 -m \"release\"
git push origin v1.0 --tags
```
",
        ),
        (
            inbox,
            "\
Quick captures to sort later.

- Call the dentist

- Reply to the venue about the date

- Look into a standing desk
",
        ),
        (
            reading,
            "\
Things worth coming back to.

- *The Timeless Way of Building* — Alexander

- `SICP`, chapter 2

- **Notes on the Synthesis of Form**
",
        ),
        (
            projects,
            "\
Active work, one page each.

- [[Launch checklist]]
",
        ),
        (
            recipes,
            "\
## Weeknight pasta

- 200g pasta

- garlic, chili, good olive oil

- a handful of parsley

## Method

1. Boil the pasta in well-salted water.

2. Warm the oil, garlic and chili gently.

3. Toss the drained pasta through the oil.

4. Finish with parsley off the heat.
",
        ),
        (
            formulas,
            r#"# Formula notebook

Edit the source; the mathematical preview stays live underneath.

## TeX

```tex
\sum_{i=1}^{n} \frac{x_i^2}{\sqrt{1+x_i}} + \begin{bmatrix}a&b\\c&d\end{bmatrix}
```

## Typst

```typst visual
integral_0^infinity frac(1, 1+x^2) + vec(alpha, beta, gamma)
```

## Typst cases

```typst
f(x) = cases(x^2 & "if" x >= 0, -x & "otherwise")
```
"#,
        ),
    ];

    storage::save_workspace(root, &ws)?;
    for (id, body) in bodies {
        storage::write_page_body(root, id, body)?;
    }
    Ok(())
}

/// Run the five-pass layout cycle until the responsive structure settles,
/// returning the final taffy tree + root child node so a scenario can
/// dispatch events against — and the frame draw from — the same live tree.
fn settle_layout(
    app: &mut NotesApp,
    theme: &Theme,
    text_renderer: &mut TextRenderer,
    w: u32,
    h: u32,
) -> Result<(TaffyTree<()>, NodeId), Box<dyn std::error::Error>> {
    let mut kept: Option<(TaffyTree<()>, NodeId)> = None;
    for _ in 0..5 {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = app.build_layout(&mut taffy, theme, &mut node_map);
        let root_node = taffy.new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(num::f32_of_u32(w)),
                    height: Dimension::length(num::f32_of_u32(h)),
                },
                ..Default::default()
            },
            &[child],
        )?;
        {
            let measure_map = node_map.clone();
            let tr = &mut *text_renderer;
            taffy.compute_layout_with_measure(
                root_node,
                taffy::Size {
                    width: AvailableSpace::Definite(num::f32_of_u32(w)),
                    height: AvailableSpace::Definite(num::f32_of_u32(h)),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )?;
        }
        app.apply_layout(&taffy, child, Point::ORIGIN);
        let settled = !app.take_structure_dirty();
        kept = Some((taffy, child));
        if settled {
            break;
        }
    }
    let Some(result) = kept else {
        return Err("layout cycle never ran".into());
    };
    Ok(result)
}

/// Render the notes app at `viewport` into a PNG at `out`, optionally in a
/// specific interaction `scenario` so states that only exist mid-gesture
/// can be reviewed offscreen:
///
/// - `Some("drawer")` — open the narrow-mode page drawer.
/// - `Some("drag")` — begin dragging a sidebar row (floating ghost +
///   live reflow gap).
/// - `Some("menu")` — open a page row's right-click context menu.
/// - `Some("slash")` — open the editor's `/` block menu.
/// - `Some("slash_applied")` — pick a `/` command by click; the trigger is
///   stripped and the block converts.
/// - `Some("heading")` — type a `# ` shortcut to promote a paragraph to an H1.
/// - `Some("title_edit")` — open the prominent title's inline rename.
/// - `Some("recipes")` — select the Recipes page (shows a numbered list).
/// - `Some("formula")` — select the formula notebook with TeX and Typst previews.
///
/// # Errors
///
/// Returns an error if the hermetic workspace can't be created/seeded, the
/// app fails to spawn, or the layout cycle never runs.
pub fn render_to_path(
    viewport: (u32, u32),
    out: &Path,
    scenario: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Hermetic workspace root — unique per process so parallel shots
    // don't collide, cleaned up on exit.
    let root: PathBuf = std::env::temp_dir().join(format!("notes-shots-{}", uuid::Uuid::new_v4()));
    storage::ensure_layout(&root)?;
    seed_sample_workspace(&root)?;

    let mut app = NotesApp::spawn(root.clone())?;

    let (w, h) = viewport;
    // `QOS_SHOTS_THEME=light|high-contrast` renders the shot under that theme
    // so the same frame can be graded in both light and dark (default dark).
    let theme = match std::env::var("QOS_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
    // The `presence` scenario seeds two simulated peers so the shot shows
    // the live presence layer — labeled carets, a fading typing swipe,
    // a peer name pill, sidebar presence-in-nav mist, and an off-screen
    // edge mist. Every other scenario draws with no peers.
    let presence = if scenario == Some("presence") {
        seed_two_peers(&app)
    } else {
        PresenceCtx::empty()
    };
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();

    let mut state = ui::State::new();
    app.sync(&mut state.doc, &state.root);

    // Run the layout cycle until the responsive structure settles, keeping
    // the final taffy tree so a scenario can dispatch events against a live
    // layout (the same tree the frame draws from).
    let kept = settle_layout(&mut app, &theme, &mut text_renderer, w, h)?;

    drive_scenario(&mut app, &mut state, &kept.0, kept.1, scenario);

    // A scenario that switches the selected page (or converts a block)
    // changes the editor's block set, so re-settle the tree before drawing —
    // otherwise the new page's blocks would have no layout and render blank.
    let kept = settle_layout(&mut app, &theme, &mut text_renderer, w, h)?;
    let _ = kept;

    let mut scene = Scene::new();
    app.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
    );
    // Mirror what `ui::App`'s frame loop paints on top of the widget tree:
    // the cross-process peer overlay — name pills for on-screen peers and
    // edge mist for off-screen ones. The widget tree already drew the
    // labeled carets, typing swipe, and sidebar mist from `presence`.
    if scenario == Some("presence") {
        let window_rect = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
        draw_remote_presence(
            &mut scene,
            Affine::IDENTITY,
            window_rect,
            &mut text_renderer,
            &theme,
            &presence,
        );
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::from_rgba8(15, 17, 21, 255), out);
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// Build a `PresenceCtx` with two simulated collaborators for the
/// `presence` shot:
///
/// - **Alice** is editing *this* page: a labeled caret sits in one of the
///   checklist blocks, a fresh typing highlighter swipes the run she just
///   inserted, and her mouse pointer shows a name pill on the canvas.
/// - **Bob** is elsewhere: he's on a different page (so his colour glows on
///   that page's sidebar row — presence-in-navigation), and his mouse
///   pointer has drifted off the right edge (so an edge mist points to
///   where he went).
///
/// This is hand-seeded sample presence, exactly like the sample workspace
/// — it never ships; the live app derives presence from real peers.
fn seed_two_peers(app: &NotesApp) -> PresenceCtx {
    let mut p = PresenceCtx::empty();
    let alice = UserId("alice".to_string());
    let bob = UserId("bob".to_string());
    p.set_display_name(alice.clone(), "Alice");
    p.set_display_name(bob, "Bob");

    // Alice's caret + typing swipe on a mid-document block. The key matches
    // `block_editor::block_presence_key(4)` — a checklist row on the
    // seeded "Launch checklist" page.
    let block = |i: u32| PresenceKey::Widget {
        path: vec![
            PathSegment::Field("notes_block".to_string()),
            PathSegment::Index(i),
        ],
    };
    p.set_caret(&block(4), alice.clone(), 18);
    p.record_peer_typing(
        alice,
        block(4),
        SelectionRange { start: 0, end: 18 },
        std::time::Instant::now(),
    );
    // Alice's on-canvas pointer, with a name pill.
    p.upsert_remote_cursor("alice".to_string(), [560.0, 300.0], Some("Alice"));

    // Bob is reading a different page → glow that sidebar row in his colour.
    let selected = app.selected_for_test();
    if let Some(other) = app
        .workspace_pages_for_test()
        .into_iter()
        .find(|pg| Some(pg.id) != selected)
    {
        p.register_peer_at(
            "bob".to_string(),
            PresenceKey::Tab {
                id: other.id.to_string(),
            },
        );
    }
    // Bob's mouse pointer has drifted off the right edge → edge mist.
    p.upsert_remote_cursor("bob".to_string(), [1500.0, 380.0], Some("Bob"));

    p
}

fn cursor_event(x: f64, y: f64) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: unsafe { std::mem::zeroed() },
        position: winit::dpi::PhysicalPosition::new(x, y),
    }
}

fn mouse_event(state: ElementState, button: MouseButton) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: unsafe { std::mem::zeroed() },
        state,
        button,
    }
}

fn char_key_event(s: &str) -> WindowEvent {
    let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = Key::Character(SmolStr::new(s));
    ev.physical_key =
        winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = Some(SmolStr::new(s));
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event: ev,
        is_synthetic: false,
    }
}

fn named_key_event(k: winit::keyboard::NamedKey) -> WindowEvent {
    let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = Key::Named(k);
    ev.physical_key =
        winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = None;
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event: ev,
        is_synthetic: false,
    }
}

fn dispatch_drag(app: &mut NotesApp, send: &mut impl FnMut(&mut NotesApp, &WindowEvent)) {
    // Grab a lower row and drag it up between the first two rows — the
    // ghost floats at the cursor and the list reflows a gap.
    if let (Some(from), Some(dst)) = (
        app.sidebar_row_center_for_test(2),
        app.sidebar_row_center_for_test(1),
    ) {
        send(app, &cursor_event(from.x, from.y));
        send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
        // Drop the cursor near the top of row 1 so the drop lands between
        // rows 0 and 1 (a clear reflow gap, no re-parent).
        send(app, &cursor_event(from.x, dst.y - 8.0));
    }
}

fn dispatch_menu(app: &mut NotesApp, send: &mut impl FnMut(&mut NotesApp, &WindowEvent)) {
    if let Some(row) = app.sidebar_row_center_for_test(1) {
        send(app, &cursor_event(row.x, row.y));
        send(app, &mouse_event(ElementState::Pressed, MouseButton::Right));
    }
}

fn dispatch_rename(app: &mut NotesApp, send: &mut impl FnMut(&mut NotesApp, &WindowEvent)) {
    // Double-click a page row to open the in-place rename field —
    // plain-chrome, sitting exactly over the row's label.
    if let Some(row) = app.sidebar_row_center_for_test(1) {
        send(app, &cursor_event(row.x, row.y));
        send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
        send(app, &cursor_event(row.x, row.y));
        send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
    }
}

fn dispatch_rename_commit(app: &mut NotesApp, send: &mut impl FnMut(&mut NotesApp, &WindowEvent)) {
    // Open the rename field, edit it, then click well outside it —
    // focus-out commits the edit and closes the field.
    if let Some(row) = app.sidebar_row_center_for_test(1) {
        send(app, &cursor_event(row.x, row.y));
        send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
        send(app, &cursor_event(row.x, row.y));
        send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
        send(app, &char_key_event(" v2"));
        send(app, &cursor_event(900.0, 400.0));
        send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
    }
}

fn dispatch_slash_applied(app: &mut NotesApp, send: &mut impl FnMut(&mut NotesApp, &WindowEvent)) {
    // Fresh page → open the block menu → filter to "Heading 1" → click
    // that row. The `/` trigger is stripped and the block becomes a
    // heading, which we then type into. Proves the slash command applies
    // AND removes its trigger text.
    let btn = app.sidebar_new_page_btn_center_for_test();
    send(app, &cursor_event(btn.x, btn.y));
    send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
    for s in ["/", "h", "1"] {
        send(app, &char_key_event(s));
    }
    let (cx, cy) = {
        let rect = app.main_panel_mut().editor().slash_menu().rect();
        (rect.center().x, rect.y0 + 14.0)
    };
    send(app, &cursor_event(cx, cy));
    send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
    for s in ["O", "v", "e", "r", "v", "i", "e", "w"] {
        send(app, &char_key_event(s));
    }
}

fn dispatch_heading(app: &mut NotesApp, send: &mut impl FnMut(&mut NotesApp, &WindowEvent)) {
    // Fresh page → type a `# ` markdown shortcut: the paragraph promotes
    // to an H1 and the prefix is stripped as the space lands.
    let btn = app.sidebar_new_page_btn_center_for_test();
    send(app, &cursor_event(btn.x, btn.y));
    send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
    for s in ["#", " ", "R", "o", "a", "d", "m", "a", "p"] {
        send(app, &char_key_event(s));
    }
    // A second line of body under the heading, for context.
    send(app, &named_key_event(winit::keyboard::NamedKey::Enter));
    for s in ["S", "h", "i", "p", " ", "b", "y", " ", "Q", "3"] {
        send(app, &char_key_event(s));
    }
}

fn dispatch_title_edit(app: &mut NotesApp, send: &mut impl FnMut(&mut NotesApp, &WindowEvent)) {
    // Click the prominent title to open its inline rename, then type —
    // shows the editable title mid-edit (caret + selection replace).
    let c = app.main_title_center_for_test();
    send(app, &cursor_event(c.x, c.y));
    send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
    for s in ["L", "a", "u", "n", "c", "h", " ", "p", "l", "a", "n"] {
        send(app, &char_key_event(s));
    }
}

fn dispatch_recipes(app: &mut NotesApp, send: &mut impl FnMut(&mut NotesApp, &WindowEvent)) {
    // Select the "Recipes" row (index 4) so the shot shows the numbered
    // "Method" list rendered with its ordinals.
    if let Some(row) = app.sidebar_row_center_for_test(4) {
        send(app, &cursor_event(row.x, row.y));
        send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
    }
}

fn dispatch_formula(app: &mut NotesApp, send: &mut impl FnMut(&mut NotesApp, &WindowEvent)) {
    // Select the formula notebook row (index 5) so the shot shows TeX and
    // Typst previews.
    if let Some(row) = app.sidebar_row_center_for_test(5) {
        send(app, &cursor_event(row.x, row.y));
        send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
    }
}

/// Route one named `scenario` to the helper that drives its events, using
/// `send` to dispatch each synthetic [`WindowEvent`] into `app`.
fn dispatch_scenario(
    app: &mut NotesApp,
    send: &mut impl FnMut(&mut NotesApp, &WindowEvent),
    scenario: Option<&str>,
) {
    match scenario {
        Some("drawer") => app.open_drawer_for_shots(),
        Some("drag") => dispatch_drag(app, send),
        Some("menu") => dispatch_menu(app, send),
        Some("rename") => dispatch_rename(app, send),
        Some("rename_commit") => dispatch_rename_commit(app, send),
        // The editor holds focus on load; `/` opens the block menu.
        Some("slash") => send(app, &char_key_event("/")),
        Some("slash_applied") => dispatch_slash_applied(app, send),
        Some("heading") => dispatch_heading(app, send),
        Some("title_edit") => dispatch_title_edit(app, send),
        Some("recipes") => dispatch_recipes(app, send),
        Some("formula") => dispatch_formula(app, send),
        // Force the floating status bubble fully visible for the still.
        Some("status") => app.arm_status_bubble_for_shots(),
        _ => {}
    }
}

/// Dispatch the events that drive `app` into the requested interaction
/// state, against the settled `taffy`/`child` layout, before the frame is
/// drawn. Produces output identical to the live binary reaching the same
/// state through real input.
fn drive_scenario(
    app: &mut NotesApp,
    state: &mut ui::State,
    taffy: &TaffyTree<()>,
    child: NodeId,
    scenario: Option<&str>,
) {
    let mut send = |app: &mut NotesApp, ev: &WindowEvent| {
        let no_mods = Modifiers::default();
        let mut handled = false;
        let mut ft = None;
        app.handle_event(
            ev,
            1.0,
            &no_mods,
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut ft,
            child,
            taffy,
        );
    };
    dispatch_scenario(app, &mut send, scenario);
}
