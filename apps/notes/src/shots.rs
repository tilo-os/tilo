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

const MULTILINE_FIRST: &str =
    "Let's go. Hi this is Quinten speaking. Why does pressing Enter not work?";
const MULTILINE_SECOND: &str = "This second paragraph should wrap naturally while I keep typing, and every line should remain visible below the first paragraph.";
const MULTILINE_CONTINUATION: &str = "This continues the same paragraph after Shift+Enter, with enough text to wrap onto another visible line.";

/// Parse a `WxH` string like `800x600` into a `(u32, u32)` viewport.
pub fn parse_viewport(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x').or_else(|| s.split_once('X'))?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Seed a small, realistic workspace into `root`: a handful of pages in
/// the tree plus one content-rich note selected so the shot shows real
/// blocks (headings, lists, checklist, code, an inline link).
#[allow(
    clippy::too_many_lines,
    reason = "one visual fixture keeps its mutually linked page corpus readable in one place"
)]
pub(crate) fn seed_sample_workspace(
    root: &Path,
    scenario: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    storage::ensure_layout(root)?;
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
    if scenario == Some("sidebar_scroll") {
        for index in 1..=28 {
            let page = ws.create_page(None);
            ws.rename(page, &format!("Reference {index:02}"));
        }
    }

    // Tags are workspace metadata rather than syntax hidden in the note body.
    // The sample keeps the chips visible in ordinary visual review while the
    // resource/action tests exercise their collaborative behavior.
    let release = ws.create_tag("Release");
    let writing = ws.create_tag("Writing");
    let _ = ws.set_tag_assignment(launch, release, true);
    let _ = ws.set_tag_assignment(launch, writing, true);

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

Read the [release writing guide](https://example.test/writing) before publishing.

![[Reading list]]

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
Active work, one note each.

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
    if scenario == Some("adjacent_code_fences") {
        storage::write_page_body(
            root,
            launch,
            "## Two separate examples\n\nEach Python example keeps its own code block.\n\n```python\nmessage = \"First example\"\nprint(message)\n```\n\n```python\nmessage = \"Second example\"\nprint(message.upper())\n```\n\nContinue writing below either block with Ctrl+Enter.\n",
        )?;
    }
    if scenario == Some("scrolled") {
        let mut body = storage::read_page_body(root, launch)?;
        for index in 1..=20 {
            body.push_str(&format!(
                "\nFollow-up item {index}: keep the document readable while scrolling.\n\n"
            ));
        }
        storage::write_page_body(root, launch, &body)?;
    }
    if matches!(scenario, Some("image" | "image_edit")) {
        // Reuse a checked-in PNG fixture to exercise the real decoder and
        // workspace-relative loader without network access.
        let fixture = include_bytes!("../../settings/shots/settings.png");
        std::fs::write(root.join(storage::PAGES_DIR).join("image.png"), fixture)?;
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
/// - `Some("first_note")` — an empty workspace, with the clear
///   first-note action a new person sees before any sample content exists.
/// - `Some("drag")` — begin dragging a sidebar row (floating ghost +
///   live reflow gap).
/// - `Some("block_drag")` — drag a document block by its six-dot handle,
///   showing the discoverable in-editor reorder affordance.
/// - `Some("menu")` — open a page row's right-click context menu.
/// - `Some("slash")` — open the editor's `/` block menu.
/// - `Some("slash_applied")` — pick a `/` command by click; the trigger is
///   stripped and the block converts.
/// - `Some("heading")` — type a `# ` shortcut to promote a paragraph to an H1.
/// - `Some("quote")` — show the semantic blockquote treatment.
/// - `Some("title_edit")` — open the prominent title's inline rename.
/// - `Some("recipes")` — select the Recipes page (shows a numbered list).
/// - `Some("formula")` — select the formula notebook with TeX and Typst previews.
/// - `Some("table")` — show editable GFM table source with its live native preview.
/// - `Some("image")` — show workspace-relative Markdown image preview behavior.
/// - `Some("transclusion")` — show a live, UUID-backed embedded page card.
/// - `Some("shortcuts")` — open the shared, focus-trapping shortcuts dialog.
/// - `Some("tag_palette")` — show Cmd-K narrowed to the stable Release tag.
///
/// # Errors
///
/// Returns an error if the hermetic workspace can't be created/seeded, the
/// app fails to spawn, or the layout cycle never runs.
pub fn render_to_path(
    viewport: (u32, u32),
    out: &Path,
    scenario: Option<&str>,
    text_scale: Option<f32>,
) -> Result<(), Box<dyn std::error::Error>> {
    if crate::capture::supports(scenario) {
        return crate::capture::render_to_path(viewport, out, scenario, text_scale)
            .map_err(Into::into);
    }
    // Hermetic workspace root — unique per process so parallel shots
    // don't collide, cleaned up on exit.
    let root: PathBuf = std::env::temp_dir().join(format!("notes-shots-{}", uuid::Uuid::new_v4()));
    storage::ensure_layout(&root)?;
    if scenario != Some("first_note") {
        seed_sample_workspace(&root, scenario)?;
    }

    let mut app = NotesApp::spawn(root.clone())?;
    // Search needs canonical bodies before querying the provider. Other
    // scenarios install isolated editor fixtures below; prematurely adopting
    // the legacy document would replace those fixtures on their next sync.
    if scenario == Some("fulltext_search") {
        app.bypass_seed_grace_for_test();
    }

    let (w, h) = viewport;
    // `TILO_SHOTS_THEME=light|high-contrast` renders the shot under that theme
    // so the same frame can be graded in both light and dark (default dark).
    let theme = match std::env::var("TILO_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    }
    // `LaunchSpec` normally threads these preferences into a generic shot
    // root. Notes owns a custom shot renderer so it has to preserve that same
    // contract itself: a `--text-scale` review must change both measurement
    // and drawing, and reduced-motion shots must use the production theme
    // policy rather than a private approximation.
    .with_text_scale(text_scale.unwrap_or(1.0))
    .with_reduce_motion(reduced_motion_requested());
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
    text_renderer.set_text_scale(theme.text_scale);

    let mut state = ui::State::new();
    app.sync(&mut state.doc, &state.root);

    // Run the layout cycle until the responsive structure settles, keeping
    // the final taffy tree so a scenario can dispatch events against a live
    // layout (the same tree the frame draws from).
    settle_layout(&mut app, &theme, &mut text_renderer, w, h)?;

    let scenario_result = drive_scenario(
        &mut app,
        &mut state,
        &theme,
        &mut text_renderer,
        viewport,
        scenario,
    );
    if scenario == Some("transclusion") {
        app.sync(&mut state.doc, &state.root);
    }

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
    // Keep the failed state as visual evidence, while still failing the check.
    scenario_result
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

fn scroll_event(lines: f32) -> WindowEvent {
    WindowEvent::MouseWheel {
        device_id: unsafe { std::mem::zeroed() },
        delta: winit::event::MouseScrollDelta::LineDelta(0.0, lines),
        phase: winit::event::TouchPhase::Moved,
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
    reveal_sidebar(app, send);
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

fn dispatch_block_drag(app: &mut NotesApp, send: &mut impl FnMut(&mut NotesApp, &WindowEvent)) {
    // The sample Welcome note has several blocks. Grab the first block's
    // dedicated handle and move over the third, stopping before release so
    // the capture truthfully shows the active interaction state.
    let points = {
        let editor = app.main_panel_mut().editor();
        editor.blocks().first().and_then(|first| {
            let from = first.line.get_rect();
            let target = editor.blocks().get(2)?.line.get_rect();
            Some((
                Point::new(from.x0 - 14.0, from.center().y),
                Point::new(from.x0 - 14.0, target.center().y),
            ))
        })
    };
    if let Some((from, target)) = points {
        send(app, &cursor_event(from.x, from.y));
        send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
        send(app, &cursor_event(target.x, target.y));
    }
}

fn dispatch_menu(app: &mut NotesApp, send: &mut impl FnMut(&mut NotesApp, &WindowEvent)) {
    reveal_sidebar(app, send);
    if let Some(row) = app.sidebar_row_center_for_test(1) {
        send(app, &cursor_event(row.x, row.y));
        send(app, &mouse_event(ElementState::Pressed, MouseButton::Right));
    }
}

fn dispatch_rename(app: &mut NotesApp, send: &mut impl FnMut(&mut NotesApp, &WindowEvent)) {
    reveal_sidebar(app, send);
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
    reveal_sidebar(app, send);
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
    reveal_sidebar(app, send);
    // Fresh page → open the block menu → filter to "Heading 1" → click that row.
    let btn = app.sidebar_new_page_btn_center_for_test();
    send(app, &cursor_event(btn.x, btn.y));
    send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
    for s in ["/", "h", "1"] {
        send(app, &char_key_event(s));
    }
    let Some(row) = app.main_panel_mut().editor().slash_menu().row_rect(0) else {
        return;
    };
    send(app, &cursor_event(row.center().x, row.center().y));
    send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
    for s in ["O", "v", "e", "r", "v", "i", "e", "w"] {
        send(app, &char_key_event(s));
    }
}

fn dispatch_heading(app: &mut NotesApp, send: &mut impl FnMut(&mut NotesApp, &WindowEvent)) {
    reveal_sidebar(app, send);
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
    reveal_sidebar(app, send);
    // Select the "Recipes" row (index 4) so the shot shows the numbered
    // "Method" list rendered with its ordinals.
    if let Some(row) = app.sidebar_row_center_for_test(4) {
        send(app, &cursor_event(row.x, row.y));
        send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
    }
}

fn dispatch_formula(app: &mut NotesApp, send: &mut impl FnMut(&mut NotesApp, &WindowEvent)) {
    reveal_sidebar(app, send);
    // Select the formula notebook row (index 5) so the shot shows TeX and
    // Typst previews.
    if let Some(row) = app.sidebar_row_center_for_test(5) {
        send(app, &cursor_event(row.x, row.y));
        send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
    }
}

fn dispatch_transclusion(app: &mut NotesApp, send: &mut impl FnMut(&mut NotesApp, &WindowEvent)) {
    // Hydrate the embedded legacy fixture page through the production open
    // path, then return to the source page so its live preview has real text.
    for index in [3, 2] {
        reveal_sidebar(app, send);
        if let Some(row) = app.sidebar_row_center_for_test(index) {
            send(app, &cursor_event(row.x, row.y));
            send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
        }
    }
}

/// Route one named `scenario` to the helper that drives its events, using
/// `send` to dispatch each synthetic [`WindowEvent`] into `app`.
fn dispatch_scenario(
    app: &mut NotesApp,
    theme: &Theme,
    send: &mut impl FnMut(&mut NotesApp, &WindowEvent),
    scenario: Option<&str>,
) {
    match scenario {
        Some("drawer") => app.open_drawer_for_shots(),
        Some("drag") => dispatch_drag(app, send),
        Some("block_drag") => dispatch_block_drag(app, send),
        Some("block_type_menu") => {
            let first_id = app
                .main_panel_mut()
                .editor()
                .blocks()
                .first()
                .map(|block| block.id.clone());
            if let Some(first_id) = first_id {
                let target = format!("notes.block-type:{first_id}");
                if let Some(handle) = semantic_target_rect(app.main_panel_mut().editor(), &target) {
                    send(app, &cursor_event(handle.center().x, handle.center().y));
                    send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
                    send(app, &mouse_event(ElementState::Released, MouseButton::Left));
                }
            }
        }
        Some("menu") => dispatch_menu(app, send),
        Some("note_menu") => {
            let title = app.main_title_center_for_test();
            send(app, &cursor_event(title.x, title.y));
            send(app, &mouse_event(ElementState::Pressed, MouseButton::Right));
        }
        Some("rename") => dispatch_rename(app, send),
        Some("rename_commit") => dispatch_rename_commit(app, send),
        // The editor holds focus on load; `/` opens the block menu.
        Some("slash") => send(app, &char_key_event("/")),
        Some("slash_applied") => dispatch_slash_applied(app, send),
        Some("heading") => dispatch_heading(app, send),
        Some("multiline_typing" | "paragraph_click") => {
            reveal_sidebar(app, send);
            let button = app.sidebar_new_page_btn_center_for_test();
            send(app, &cursor_event(button.x, button.y));
            send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
            send(app, &mouse_event(ElementState::Released, MouseButton::Left));
            for character in MULTILINE_FIRST.chars() {
                send(app, &char_key_event(&character.to_string()));
            }
            send(app, &named_key_event(winit::keyboard::NamedKey::Enter));
            for character in MULTILINE_SECOND.chars() {
                send(app, &char_key_event(&character.to_string()));
            }
            send(
                app,
                &WindowEvent::ModifiersChanged(Modifiers::from(
                    winit::keyboard::ModifiersState::SHIFT,
                )),
            );
            send(app, &named_key_event(winit::keyboard::NamedKey::Enter));
            send(app, &WindowEvent::ModifiersChanged(Modifiers::default()));
            for character in MULTILINE_CONTINUATION.chars() {
                send(app, &char_key_event(&character.to_string()));
            }
            if scenario == Some("paragraph_click") {
                for (index, offset) in [(0, 10), (1, 15), (0, 45)] {
                    let caret = app.main_panel_mut().editor().blocks()[index]
                        .line
                        .caret_rect(theme, offset);
                    send(app, &cursor_event(caret.x0 + 0.1, caret.center().y));
                    send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
                    send(app, &mouse_event(ElementState::Released, MouseButton::Left));
                }
            }
        }
        Some("rich_outline") => {
            app.main_panel_mut().editor_mut().set_blocks(crate::markdown::specs_to_blocks(&crate::markdown::parse(
                "1. Prepare the release\n\n  Keep the explanation with its list item. Paragraphs can wrap here without losing their place in the outline.\n\n  ```python\n  if ready:\n      publish()\n  ```\n\n  - [x] Check the writing experience\n    1. Review keyboard navigation\n    2. Read the screenshots critically\n\n2. Share the finished note\n",
            )));
        }
        Some("mixed_outline") => {
            app.main_panel_mut().editor_mut().set_blocks(vec![
                ui::widget::Block::heading(1, "A release, step by step"),
                ui::widget::Block::numbered(0, "Prepare the release"),
                ui::widget::Block::bullet(1, "Check the writing experience"),
                ui::widget::Block::checklist(2, true, "Long paragraphs wrap without clipping"),
                ui::widget::Block::checklist(2, false, "Review keyboard navigation\nKeep this explanation inside the same checklist item."),
                ui::widget::Block::numbered(1, "Ask for a second review"),
                ui::widget::Block::bullet(2, "Read the screenshots critically"),
                ui::widget::Block::numbered(1, "Address the findings"),
                ui::widget::Block::numbered(0, "Share the finished note"),
            ]);
        }
        Some("typed_lists") => {
            reveal_sidebar(app, send);
            let button = app.sidebar_new_page_btn_center_for_test();
            send(app, &cursor_event(button.x, button.y));
            send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
            for (index, line) in [
                "- Gather ideas",
                "Write a first draft",
                "",
                "1. Review the draft",
                "Share with the team",
                "",
                "- [ ] Follow up tomorrow",
            ]
            .iter()
            .enumerate()
            {
                if index > 0 {
                    send(app, &named_key_event(winit::keyboard::NamedKey::Enter));
                }
                for character in line.chars() {
                    send(app, &char_key_event(&character.to_string()));
                }
            }
        }
        Some("fenced_code" | "fenced_code_opening") => {
            reveal_sidebar(app, send);
            let button = app.sidebar_new_page_btn_center_for_test();
            send(app, &cursor_event(button.x, button.y));
            send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
            for character in "```python".chars() {
                send(app, &char_key_event(&character.to_string()));
            }
            if scenario == Some("fenced_code") {
                for line in [
                    "message = \"Hello from a Python code block\"",
                    "print(message)",
                    "for value in range(3):",
                    "    print(value)",
                    "```",
                    "A note below the code.",
                ] {
                    send(app, &named_key_event(winit::keyboard::NamedKey::Enter));
                    for character in line.chars() {
                        send(app, &char_key_event(&character.to_string()));
                    }
                }
            }
        }
        Some("code_keyboard" | "code_keyboard_active" | "code_group_drag" | "code_group_drop") => {
            reveal_sidebar(app, send);
            let button = app.sidebar_new_page_btn_center_for_test();
            send(app, &cursor_event(button.x, button.y));
            send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
            for character in "```python".chars() {
                send(app, &char_key_event(&character.to_string()));
            }
            send(app, &named_key_event(winit::keyboard::NamedKey::Enter));
            for character in "if ready:".chars() {
                send(app, &char_key_event(&character.to_string()));
            }
            send(app, &named_key_event(winit::keyboard::NamedKey::Enter));
            send(app, &named_key_event(winit::keyboard::NamedKey::Tab));
            for character in "print('Ready')".chars() {
                send(app, &char_key_event(&character.to_string()));
            }
            send(app, &named_key_event(winit::keyboard::NamedKey::Enter));
            for character in "print('Still code')".chars() {
                send(app, &char_key_event(&character.to_string()));
            }
            if scenario != Some("code_keyboard_active") {
                send(
                    app,
                    &WindowEvent::ModifiersChanged(Modifiers::from(
                        winit::keyboard::ModifiersState::CONTROL,
                    )),
                );
                send(app, &named_key_event(winit::keyboard::NamedKey::Enter));
                send(app, &WindowEvent::ModifiersChanged(Modifiers::default()));
                for character in "Back to writing, without touching the mouse.".chars() {
                    send(app, &char_key_event(&character.to_string()));
                }
            }
            if matches!(scenario, Some("code_group_drag" | "code_group_drop")) {
                let editor = app.main_panel_mut().editor();
                let first_id = editor.blocks()[0].id.clone();
                let handle =
                    semantic_target_rect(editor, &format!("notes.block-move:{first_id}:down"))
                        .expect("code movement handle");
                let destination = editor.blocks()[3].line.get_rect().center();
                send(app, &cursor_event(handle.center().x, handle.center().y));
                send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
                send(app, &cursor_event(handle.center().x + 48.0, destination.y));
                if scenario == Some("code_group_drop") {
                    send(app, &mouse_event(ElementState::Released, MouseButton::Left));
                }
            }
        }
        Some("quote") => {
            app.main_panel_mut().editor_mut().set_blocks(vec![
                ui::widget::Block::heading(2, "A thought worth keeping"),
                ui::widget::Block::quote(
                    "The best notes make ideas easier to find, connect, and reuse.",
                ),
                ui::widget::Block::paragraph(
                    "Quotes remain editable, linkable, draggable blocks—not pasted decoration.",
                ),
            ]);
        }
        Some("find") => {
            app.main_panel_mut().editor_mut().set_blocks(vec![
                ui::widget::Block::heading(1, "Migration plan"),
                ui::widget::Block::paragraph(
                    "The workspace migration keeps every linked decision close to the work.",
                ),
                ui::widget::Block::checklist(0, true, "Inventory the current workspace"),
                ui::widget::Block::checklist(0, false, "Review the migration guide"),
                ui::widget::Block::quote(
                    "A careful migration should feel boring, reversible, and easy to verify.",
                ),
            ]);
            send(
                app,
                &WindowEvent::ModifiersChanged(Modifiers::from(
                    winit::keyboard::ModifiersState::CONTROL,
                )),
            );
            send(app, &char_key_event("f"));
            send(app, &WindowEvent::ModifiersChanged(Modifiers::default()));
            for character in "migration".chars() {
                send(app, &char_key_event(&character.to_string()));
            }
        }
        Some("title_edit") => dispatch_title_edit(app, send),
        Some("recipes") => dispatch_recipes(app, send),
        Some("formula") => dispatch_formula(app, send),
        Some("transclusion") => dispatch_transclusion(app, send),
        Some("external_link") => {
            let text = "Read the complete writing guide before publishing.";
            let start = text.find("complete writing guide").unwrap_or(0);
            let end = start.saturating_add("complete writing guide".len());
            let mut linked = ui::widget::Block::paragraph(text);
            linked.line.set_marks(vec![ui::widget::MarkSpan::new(
                start..end,
                ui::widget::Mark::ExternalLink("https://example.test/writing".into()),
            )]);
            app.main_panel_mut().editor_mut().set_blocks(vec![
                ui::widget::Block::heading(1, "Links that behave like links"),
                ui::widget::Block::paragraph(
                    "External destinations stay editable Markdown, sync with the note, and open through the desktop portal.",
                ),
                linked,
                ui::widget::Block::quote(
                    "Unsafe schemes remain plain text instead of becoming executable actions.",
                ),
            ]);
        }
        Some("table") | Some("table_cell_edit") => {
            app.main_panel_mut().editor_mut().set_blocks(vec![
                ui::widget::Block::heading(1, "Tables without leaving the note"),
                ui::widget::Block::paragraph(
                    "Track owners and progress together.",
                ),
                ui::widget::Block::table(
                    "| Project | Owner | Status |\n| :--- | :--- | ---: |\n| Search | Mina | Shipped |\n| Import | Jules | In review |\n| Mobile polish | Sam | Next |",
                    true,
                ),
            ]);
            if scenario == Some("table_cell_edit") {
                let center = app.main_panel_mut().get_rect().center();
                send(app, &cursor_event(center.x, center.y));
                let target = app
                    .main_panel_mut()
                    .editor()
                    .blocks()
                    .last()
                    .map(|block| format!("notes.table-cell:{}:1:1", block.id));
                if let Some(target) = target
                    && let Some(rect) = semantic_target_rect(app.main_panel_mut().editor(), &target)
                {
                    send(app, &cursor_event(rect.center().x, rect.center().y));
                    send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
                    send(app, &mouse_event(ElementState::Released, MouseButton::Left));
                    send(app, &named_key_event(winit::keyboard::NamedKey::End));
                    send(app, &char_key_event(" Chen"));
                }
            }
        }
        Some("table_large") | Some("table_large_scrolled") => {
            let header = (1..=10)
                .map(|column| format!("Field {column}"))
                .collect::<Vec<_>>()
                .join(" | ");
            let separator = ["---"; 10].join(" | ");
            let mut source = format!("| {header} |\n| {separator} |\n");
            for row in 1..=16 {
                let cells = (1..=10).map(|column| {
                    if row == 1 && column == 1 {
                        "A long cell with important details that must remain readable on a narrow display".to_string()
                    } else {
                        format!("Row {row}, field {column}")
                    }
                }).collect::<Vec<_>>().join(" | ");
                source.push_str(&format!("| {cells} |\n"));
            }
            app.main_panel_mut().editor_mut().set_blocks(vec![
                ui::widget::Block::heading(1, "Every detail belongs in the note"),
                ui::widget::Block::table(&source, true),
            ]);
            if scenario == Some("table_large_scrolled") {
                let center = app.main_panel_mut().get_rect().center();
                send(app, &cursor_event(center.x, center.y));
                send(app, &scroll_event(-1000.0));
            }
        }
        Some("image") | Some("image_edit") => {
            app.main_panel_mut().editor_mut().set_blocks(vec![
                ui::widget::Block::heading(1, "Images stay part of the note"),
                ui::widget::Block::paragraph(
                    "Workspace-relative images load asynchronously without blocking typing or escaping the collaboration sandbox.",
                ),
                ui::widget::Block::paragraph("![Settings external accounts](image.png)"),
            ]);
            assert!(app.main_panel_mut().editor_mut().prewarm_images_for_shots());
            if scenario == Some("image_edit") {
                let center = app.main_panel_mut().get_rect().center();
                send(app, &cursor_event(center.x, center.y));
            }
        }
        Some("shortcuts") => app.shortcuts_mut().set_visible(true),
        Some("shortcuts_bottom") => {
            app.shortcuts_mut().set_visible(true);
            send(app, &cursor_event(0.0, 0.0));
            send(app, &named_key_event(winit::keyboard::NamedKey::End));
        }
        Some("tag_palette") => {
            app.palette_mut().set_anchor(Point::new(400.0, 80.0));
            app.palette_mut().set_visible(true);
            for s in ["R", "e", "l", "e", "a", "s", "e"] {
                send(app, &char_key_event(s));
            }
        }
        Some("fulltext_search") => {
            send(
                app,
                &WindowEvent::ModifiersChanged(Modifiers::from(
                    winit::keyboard::ModifiersState::CONTROL,
                )),
            );
            send(app, &char_key_event("k"));
            send(app, &WindowEvent::ModifiersChanged(Modifiers::default()));
            for character in "migration".chars() {
                send(app, &char_key_event(&character.to_string()));
            }
        }
        Some("scrolled") => {
            let first = app
                .main_panel_mut()
                .editor()
                .blocks()
                .first()
                .map(|block| block.line.get_rect());
            if let Some(rect) = first {
                send(app, &cursor_event(rect.center().x, rect.center().y));
            }
            for _ in 0..8 {
                send(app, &scroll_event(-3.0));
            }
        }
        Some("sidebar_scroll") => {
            app.open_drawer_for_shots();
            send(app, &cursor_event(120.0, 280.0));
            for _ in 0..6 {
                send(app, &scroll_event(-3.0));
            }
        }
        // Force the floating status bubble fully visible for the still.
        Some("status") => app.arm_status_bubble_for_shots(),
        Some("saving") => app.set_save_status_for_shots(storage::SaveStatus::Saving),
        Some("save_error") => app.set_save_status_for_shots(storage::SaveStatus::Failed),
        Some("import_success") => app.show_transfer_toast_for_shots(),
        _ => {}
    }
}

/// Keep the custom Notes renderer in lockstep with `ui::App` and the generic
/// `LaunchSpec` screenshot path. Invalid values are intentionally ignored,
/// matching the live app's preference parser.
fn reduced_motion_requested() -> bool {
    ["TILO_A11Y_REDUCE_MOTION", "TILO_REDUCE_MOTION"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .any(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

/// Reveal navigation through its real toggle before targeting sidebar controls.
fn reveal_sidebar(app: &mut NotesApp, send: &mut impl FnMut(&mut NotesApp, &WindowEvent)) {
    if app.built_narrow_for_test() && !app.drawer_open_for_test() {
        let toggle = app.menu_btn_center_for_test();
        send(app, &cursor_event(toggle.x, toggle.y));
        send(app, &mouse_event(ElementState::Pressed, MouseButton::Left));
        send(app, &mouse_event(ElementState::Released, MouseButton::Left));
    }
}

fn semantic_target_rect(widget: &dyn Widget, target: &str) -> Option<Rect> {
    semantic_target(widget, target)
        .and_then(|node| node.bounds.map(ui::a11y::AccessBounds::to_rect))
}

fn semantic_target(widget: &dyn Widget, target: &str) -> Option<ui::a11y::AccessNode> {
    fn find(node: ui::a11y::AccessNode, target: &str) -> Option<ui::a11y::AccessNode> {
        if node.action_target.as_deref() == Some(target) {
            return Some(node);
        }
        node.children
            .into_iter()
            .find_map(|child| find(child, target))
    }
    widget.accessibility().and_then(|node| find(node, target))
}

/// Retain intrinsic-size caches between input events, just like the live host.
/// Rebuilding every frame conceals stale wrapped-text measurements.
struct RetainedScenarioLayout {
    tree: TaffyTree<()>,
    nodes: HashMap<NodeId, *mut dyn Widget>,
    bindings: ui::bound_dispatch::BoundNodeMap,
    child: NodeId,
    root: NodeId,
}

fn settle_scenario_layout(
    cached: &mut Option<RetainedScenarioLayout>,
    app: &mut NotesApp,
    theme: &Theme,
    text: &mut TextRenderer,
    viewport: (u32, u32),
) -> Result<(), Box<dyn std::error::Error>> {
    let available = Size {
        width: AvailableSpace::Definite(num::f32_of_u32(viewport.0)),
        height: AvailableSpace::Definite(num::f32_of_u32(viewport.1)),
    };
    for _ in 0..8 {
        if app.take_structure_dirty() {
            *cached = None;
        }
        if cached.is_none() {
            let mut tree = TaffyTree::new();
            let mut nodes = HashMap::new();
            let child = app.build_layout(&mut tree, theme, &mut nodes);
            let root = tree.new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(num::f32_of_u32(viewport.0)),
                        height: Dimension::length(num::f32_of_u32(viewport.1)),
                    },
                    ..Default::default()
                },
                &[child],
            )?;
            let mut bindings = ui::bound_dispatch::BoundNodeMap::new();
            // SAFETY: app is exclusively borrowed and stable throughout this
            // scenario; structural invalidation discards these maps first.
            unsafe { bindings.rebuild(app, &tree, root, &nodes) };
            *cached = Some(RetainedScenarioLayout {
                tree,
                nodes,
                bindings,
                child,
                root,
            });
        }
        let layout = cached.as_mut().ok_or("scenario layout missing")?;
        // SAFETY: the maps were built together and no input is dispatched
        // between structure validation and this solve.
        unsafe {
            ui::widget::compute_widget_layout(
                &mut layout.tree,
                layout.root,
                available,
                &layout.nodes,
                &layout.bindings,
                text,
            )?;
        }
        app.apply_layout(&layout.tree, layout.child, Point::ORIGIN);
        if !app.take_structure_dirty() {
            return Ok(());
        }
        *cached = None;
    }
    Err("retained scenario layout did not settle".into())
}

/// Dispatch against retained layout and synchronize each input like the live loop.
fn drive_scenario(
    app: &mut NotesApp,
    state: &mut ui::State,
    theme: &Theme,
    text_renderer: &mut TextRenderer,
    viewport: (u32, u32),
    scenario: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let original_note = app.main_panel_mut().page_id();
    let mut layout_error = None;
    let mut modifiers = Modifiers::default();
    let mut retained = None;
    let mut send = |app: &mut NotesApp, ev: &WindowEvent| {
        if layout_error.is_some() {
            return;
        }
        if let Err(error) =
            settle_scenario_layout(&mut retained, app, theme, text_renderer, viewport)
        {
            layout_error = Some(error);
            return;
        }
        let layout = retained.as_ref().expect("settled scenario layout");
        if let WindowEvent::ModifiersChanged(value) = ev {
            modifiers = *value;
        }
        let mut handled = false;
        let mut ft = None;
        app.handle_event(
            ev,
            1.0,
            &modifiers,
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut ft,
            layout.child,
            &layout.tree,
        );
        // Real input is followed by the normal CRDT projection pass. Without
        // it a screenshot can show another note's tags, stale caret geometry,
        // or click the hidden sidebar using the previous viewport's layout.
        app.sync(&mut state.doc, &state.root);
        if let Err(error) =
            settle_scenario_layout(&mut retained, app, theme, text_renderer, viewport)
        {
            layout_error = Some(error);
            return;
        }
        // Retained rich controls publish their exact painted cell/button
        // geometry during the frame. Include that real pass before the next
        // synthetic pointer targets the accessibility tree.
        let mut frame = Scene::new();
        app.draw_with_presence(
            &mut frame,
            Affine::IDENTITY,
            text_renderer,
            theme,
            &PresenceCtx::empty(),
            &PermissionContext::permissive(),
        );
    };
    dispatch_scenario(app, theme, &mut send, scenario);
    if let Some(error) = layout_error {
        return Err(error);
    }
    if scenario == Some("image_edit") {
        // Exercise the exported semantic action explicitly. The shared
        // button's queued host dispatch belongs to ui::App, which this
        // offscreen NotesApp renderer does not impersonate.
        let target = app
            .main_panel_mut()
            .editor()
            .blocks()
            .last()
            .map(|block| format!("notes.image-edit:{}", block.id))
            .ok_or("image editing scenario has no image block")?;
        if !app.main_panel_mut().editor_mut().perform_semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut state.doc,
            &state.root,
        ) {
            return Err("image editing semantic action was not handled".into());
        }
        app.sync(&mut state.doc, &state.root);
        let (taffy, child) = settle_layout(app, theme, text_renderer, viewport.0, viewport.1)?;
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            &named_key_event(winit::keyboard::NamedKey::End),
            1.0,
            &Modifiers::default(),
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut focus,
            child,
            &taffy,
        );
        app.sync(&mut state.doc, &state.root);
        settle_layout(app, theme, text_renderer, viewport.0, viewport.1)?;
    }
    // Scenarios may replace blocks directly without sending an input event.
    // Validate the final measured geometry, never the new blocks' zero rects.
    settle_scenario_layout(&mut retained, app, theme, text_renderer, viewport)?;
    let observed = match scenario {
        Some("menu") => {
            app.sidebar_menu_open_for_test()
                && (!app.built_narrow_for_test() || app.drawer_open_for_test())
        }
        Some("note_menu") => app.note_menu_open_for_test(),
        Some("drag") => app.sidebar_drag_active_for_test(),
        Some("slash") => app.editor_slash_menu_visible_for_test(),
        Some("title_edit") => app.main_title_editing_for_test(),
        Some("fulltext_search") => {
            let palette = app.palette_mut();
            palette.is_visible()
                && palette.query() == "migration"
                && palette
                    .highlighted()
                    .is_some_and(|result| result.title == "Launch checklist")
        }
        Some("table_cell_edit") => {
            let editor = app.main_panel_mut().editor();
            editor.blocks().last().is_some_and(|block| {
                semantic_target(editor, &format!("notes.table-cell:{}:1:1", block.id))
                    .is_some_and(|cell| cell.value.as_deref() == Some("Mina Chen"))
            })
        }
        Some("image_edit") => {
            let editor = app.main_panel_mut().editor();
            editor.blocks().last().is_some_and(|block| {
                block.line.is_focused()
                    && semantic_target_rect(editor, &format!("notes.image-done:{}", block.id))
                        .is_some()
            })
        }
        Some("scrolled") | Some("table_large_scrolled") => {
            app.main_panel_mut().scroll_y_for_test() > 0.0
        }
        Some("typed_lists") => {
            let blocks = app.main_panel_mut().editor().blocks();
            blocks.len() == 5
                && matches!(blocks[0].kind, ui::widget::BlockKind::Bullet { .. })
                && matches!(blocks[1].kind, ui::widget::BlockKind::Bullet { .. })
                && matches!(blocks[2].kind, ui::widget::BlockKind::Numbered { .. })
                && matches!(blocks[3].kind, ui::widget::BlockKind::Numbered { .. })
                && matches!(
                    blocks[4].kind,
                    ui::widget::BlockKind::Checklist { done: false, .. }
                )
                && blocks[4].text() == "Follow up tomorrow"
        }
        Some("multiline_typing" | "paragraph_click") => {
            let blocks = app.main_panel_mut().editor().blocks();
            blocks.len() == 2
                && blocks[0].text() == MULTILINE_FIRST
                && blocks[1].text() == format!("{MULTILINE_SECOND}\n{MULTILINE_CONTINUATION}")
                && (scenario != Some("paragraph_click") || blocks[0].line.cursor_pos() == 45)
                && blocks
                    .windows(2)
                    .all(|pair| pair[1].line.get_rect().y0 + 0.5 >= pair[0].line.get_rect().y1)
                && blocks.iter().all(|block| {
                    let rect = block.line.get_rect();
                    let (_, text_height) = text_renderer.measure(
                        block.text(),
                        block.kind.font_size(),
                        Some(num::f32_of_f64(rect.width())),
                    );
                    rect.height() + 0.5 >= f64::from(text_height)
                })
        }
        Some("adjacent_code_fences") => {
            let blocks = app.main_panel_mut().editor().blocks();
            let code = blocks
                .iter()
                .filter(|block| matches!(block.kind, ui::widget::BlockKind::CodeBlock { .. }))
                .collect::<Vec<_>>();
            code.len() == 4
                && code[0].same_code_group(code[1])
                && !code[1].same_code_group(code[2])
                && code[2].same_code_group(code[3])
                // 28px language header + 4px background padding on
                // each box + a visible 8px gap at normal scale.
                && code[2].line.get_rect().y0 >= code[1].line.get_rect().y1 + 44.0
        }
        Some("code_group_drop") => {
            let blocks = app.main_panel_mut().editor().blocks();
            blocks.len() == 4
                && blocks[0].text() == "Back to writing, without touching the mouse."
                && blocks[1].text() == "if ready:"
                && blocks[2].text() == "    print('Ready')"
                && blocks[3].text() == "    print('Still code')"
        }
        Some("rich_outline") => {
            let blocks = app.main_panel_mut().editor().blocks();
            blocks.iter().map(|b| b.indent()).collect::<Vec<_>>() == [0, 1, 1, 1, 1, 2, 2, 0]
                && matches!(blocks[2].kind, ui::widget::BlockKind::CodeBlock { .. })
                && blocks[2].same_code_group(&blocks[3])
                && (blocks[1].line.get_rect().x0 - blocks[0].line.get_rect().x0).abs() < 1.0
                && blocks[5].line.get_rect().x0 > blocks[4].line.get_rect().x0
        }
        Some("code_keyboard" | "code_keyboard_active" | "code_group_drag") => {
            let blocks = app.main_panel_mut().editor().blocks();
            blocks.len()
                == if scenario != Some("code_keyboard_active") {
                    4
                } else {
                    3
                }
                && blocks[0].text() == "if ready:"
                && blocks[1].text() == "    print('Ready')"
                && blocks[2].text() == "    print('Still code')"
                && (scenario == Some("code_keyboard_active")
                    || matches!(blocks[3].kind, ui::widget::BlockKind::Paragraph))
        }
        Some("fenced_code" | "fenced_code_opening") => {
            let blocks = app.main_panel_mut().editor().blocks();
            blocks.first().is_some_and(|block| {
                matches!(&block.kind,
                ui::widget::BlockKind::CodeBlock { lang: Some(language) } if language == "python")
            }) && if scenario == Some("fenced_code_opening") {
                blocks.len() == 1 && blocks.first().is_some_and(|block| block.text().is_empty())
            } else {
                blocks.first().is_some_and(|block| {
                    block.text() == "message = \"Hello from a Python code block\""
                }) && blocks.last().is_some_and(|block| {
                    matches!(block.kind, ui::widget::BlockKind::Paragraph)
                        && block.text() == "A note below the code."
                })
            }
        }
        Some("heading") => {
            app.main_panel_mut().page_id() != original_note
                && app
                    .main_panel_mut()
                    .editor()
                    .blocks()
                    .first()
                    .is_some_and(|block| block.text() == "Roadmap")
        }
        Some("slash_applied") => {
            app.main_panel_mut().page_id() != original_note
                && app
                    .main_panel_mut()
                    .editor()
                    .blocks()
                    .first()
                    .is_some_and(|block| block.text() == "Overview")
        }
        _ => true,
    };
    if !observed {
        let detail = match scenario {
            Some("rich_outline") => format!(
                "{:?}",
                app.main_panel_mut()
                    .editor()
                    .blocks()
                    .iter()
                    .map(|block| (
                        block.text(),
                        block.indent(),
                        &block.kind,
                        block.line.get_rect()
                    ))
                    .collect::<Vec<_>>()
            ),
            Some("multiline_typing") => format!(
                "retained rows: {:?}",
                app.main_panel_mut()
                    .editor()
                    .blocks()
                    .iter()
                    .map(|block| {
                        let rect = block.line.get_rect();
                        let (_, text_height) = text_renderer.measure(
                            block.text(),
                            block.kind.font_size(),
                            Some(num::f32_of_f64(rect.width())),
                        );
                        (block.text(), block.soft_break, rect, text_height)
                    })
                    .collect::<Vec<_>>()
            ),
            Some("fulltext_search") => {
                let palette = app.palette_mut();
                format!(
                    "visible={} query={:?} result={:?}",
                    palette.is_visible(),
                    palette.query(),
                    palette.highlighted().map(|result| &result.title)
                )
            }
            Some("image_edit") => {
                let editor = app.main_panel_mut().editor();
                format!(
                    "image={:?}",
                    editor.blocks().last().map(|block| (
                        block.line.is_focused(),
                        semantic_target_rect(editor, &format!("notes.image-done:{}", block.id))
                    ))
                )
            }
            _ => String::new(),
        };
        return Err(format!(
            "screenshot scenario {scenario:?} did not reach its requested state: {detail}"
        )
        .into());
    }
    Ok(())
}
