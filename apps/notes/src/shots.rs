//! Headless `--shots` mode: render one offscreen frame of the notes
//! app to a PNG and exit. Used for visual review and CI snapshots at a
//! chosen viewport, without opening a window.
//!
//! The workspace is seeded into a throwaway temp directory so a shot is
//! hermetic and never touches the user's real `~/notes`. Sample content
//! only ever lives on this path — the shipped app starts from the
//! user's own pages.

use std::{fmt::Write as _, path::Path};

use crate::model::Workspace;
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
            let _ = write!(
                body,
                "\nFollow-up item {index}: keep the document readable while scrolling.\n\n"
            );
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

/// Render one canonical component capture. Unknown scenarios fail instead of
/// falling back to a second, app-owned renderer.
///
/// # Errors
/// Returns an error for unsupported scenarios or when fixture setup/rendering fails.
pub fn render_to_path(
    viewport: (u32, u32),
    out: &Path,
    scenario: Option<&str>,
    theme: Theme,
) -> Result<(), Box<dyn std::error::Error>> {
    if !crate::capture::supports(scenario) {
        return Err(format!(
            "unsupported Notes capture scenario: {}",
            scenario.unwrap_or("overview")
        )
        .into());
    }

    let mut app = NotesApp::spawn(root.clone())?;

    let (w, h) = viewport;
    // The theme is the one screenshot policy every app shares
    // (`ui::launch::shot_theme`: `QOS_SHOTS_THEME`, `QOS_SHOTS_TEXT_SCALE`,
    // `--text-scale`, reduced motion). Notes owns a custom renderer, so it
    // takes that resolved theme rather than re-deriving a private one.
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
    let kept = settle_layout(&mut app, &theme, &mut text_renderer, w, h)?;

    drive_scenario(&mut app, &mut state, &kept.0, kept.1, scenario);
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
    // Fresh page → open the block menu → filter to "Heading 1" → click that row.
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

fn dispatch_transclusion(app: &mut NotesApp, send: &mut impl FnMut(&mut NotesApp, &WindowEvent)) {
    // Hydrate the embedded legacy fixture page through the production open
    // path, then return to the source page so its live preview has real text.
    for index in [3, 2] {
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
    send: &mut impl FnMut(&mut NotesApp, &WindowEvent),
    scenario: Option<&str>,
) {
    match scenario {
        Some("drawer") => app.open_drawer_for_shots(),
        Some("drag") => dispatch_drag(app, send),
        Some("block_drag") => dispatch_block_drag(app, send),
        Some("menu") => dispatch_menu(app, send),
        Some("rename") => dispatch_rename(app, send),
        Some("rename_commit") => dispatch_rename_commit(app, send),
        // The editor holds focus on load; `/` opens the block menu.
        Some("slash") => send(app, &char_key_event("/")),
        Some("slash_applied") => dispatch_slash_applied(app, send),
        Some("heading") => dispatch_heading(app, send),
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
        Some("table") => {
            app.main_panel_mut().editor_mut().set_blocks(vec![
                ui::widget::Block::heading(1, "Tables without leaving the page"),
                ui::widget::Block::paragraph(
                    "Edit familiar Markdown source while the structured, themed table updates underneath.",
                ),
                ui::widget::Block::table(
                    "| Project | Owner | Status |\n| :--- | :--- | ---: |\n| Search | Mina | Shipped |\n| Import | Jules | In review |\n| Mobile polish | Sam | Next |",
                    false,
                ),
            ]);
        }
        Some("image") => {
            app.main_panel_mut().editor_mut().set_blocks(vec![
                ui::widget::Block::heading(1, "Images stay part of the note"),
                ui::widget::Block::paragraph(
                    "Workspace-relative images load asynchronously without blocking typing or escaping the collaboration sandbox.",
                ),
                ui::widget::Block::paragraph("![A warm red square](image.png)"),
            ]);
            assert!(app.main_panel_mut().editor_mut().prewarm_images_for_shots());
        }
        Some("shortcuts") => app.shortcuts_mut().set_visible(true),
        Some("tag_palette") => {
            app.palette_mut().set_anchor(Point::new(400.0, 80.0));
            app.palette_mut().set_visible(true);
            for s in ["R", "e", "l", "e", "a", "s", "e"] {
                send(app, &char_key_event(s));
            }
        }
        Some("scrolled") => {
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
