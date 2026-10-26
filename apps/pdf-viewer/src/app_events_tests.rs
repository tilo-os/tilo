use super::*;
use serial_test::serial;
use winit::dpi::PhysicalPosition;
use winit::event::{Modifiers, TouchPhase};

fn sample_app() -> (PdfApp, ui::State) {
    let mut app = crate::scenarios::sample_reader();
    let mut st = crate::scenarios::sample_state();
    app.sync_settled(&mut st.doc, &st.root);
    app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    app.measure_tabs(
        &mut ui::text_render::TextRenderer::new(),
        &ui::theme::Theme::dark(),
    );
    app.recompute_regions();
    app.sync_viewport_frame();
    (app, st)
}

/// A ctrl-held `Modifiers`, mirroring the workspace convention (see
/// `ui::widget::command_palette`'s `ctrl_a_selects_query`).
fn ctrl_mods() -> Modifiers {
    let mut s = winit::keyboard::ModifiersState::empty();
    s.set(winit::keyboard::ModifiersState::CONTROL, true);
    Modifiers::from(s)
}

/// the same trick the terminal + `modal_dialog` tests use.
fn key_event(key: Key) -> WindowEvent {
    // Real winit reports the committed text alongside a printable key; the
    // shared TextInput inserts from that `text` field, so mirror it (named
    // keys other than Space carry no text).
    let text = match &key {
        Key::Character(s) => Some(s.clone()),
        Key::Named(NamedKey::Space) => Some(winit::keyboard::SmolStr::new(" ")),
        _ => None,
    };
    ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        key,
        text,
        ElementState::Pressed,
    ))
}

fn character(ch: &str) -> Key {
    Key::Character(ch.into())
}

fn centre(r: Rect) -> Point {
    Point::new(f64::midpoint(r.x0, r.x1), f64::midpoint(r.y0, r.y1))
}

/// The reader plus the taffy node it is driven through: every event goes
/// through `PdfApp::handle_event`, the same entry point the live widget
/// tree drives.
struct Rig {
    app: PdfApp,
    st: ui::State,
}

impl Rig {
    fn new() -> Self {
        let (app, st) = sample_app();
        Self::with(app, st)
    }

    fn with(app: PdfApp, st: ui::State) -> Self {
        Self { app, st }
    }

    /// Feed one real event; returns whether it was handled.
    fn feed(&mut self, mods: Modifiers, ev: &WindowEvent) -> bool {
        ui::testing::Dispatch::new()
            .modifiers(mods)
            .doc(&mut self.st.doc)
            .path(&self.st.root)
            .send(&mut self.app, ev)
            .handled
    }

    fn move_to(&mut self, p: (f64, f64)) {
        self.feed(
            Modifiers::default(),
            &ui::synthetic_input::cursor_moved(p.0, p.1),
        );
    }

    fn button(&mut self, state: ElementState, button: MouseButton) {
        self.feed(
            Modifiers::default(),
            &ui::synthetic_input::mouse_input(state, button),
        );
    }

    fn press(&mut self) {
        self.button(ElementState::Pressed, MouseButton::Left);
    }

    fn release(&mut self) {
        self.button(ElementState::Released, MouseButton::Left);
    }

    fn key(&mut self, mods: Modifiers, key: Key) -> bool {
        self.feed(mods, &key_event(key))
    }

    fn wheel(&mut self, mods: Modifiers, delta: MouseScrollDelta) {
        self.feed(
            mods,
            &WindowEvent::MouseWheel {
                device_id: winit::event::DeviceId::dummy(),
                delta,
                phase: TouchPhase::Moved,
            },
        );
    }

    /// Arm `tool` from its item in the window bar's Tools menu, and assert
    /// it is armed.
    fn arm_via_window_bar(&mut self, tool: Tool, why: &str) {
        self.app
            .run_window_bar_event(&ui::window_toolbar::ToolbarEvent::Action {
                id: super::window_bar::tool_id(tool),
            });
        assert_eq!(self.app.tool, tool, "{why}");
    }

    /// Right-click the seeded sticky note, asserting the context menu
    /// opens targeting it. Returns its id.
    fn right_click_seeded_note(&mut self) -> String {
        let sticky = self
            .app
            .annotations
            .iter()
            .find(|a| matches!(a.kind, AnnoKind::Note { .. }))
            .unwrap()
            .clone();
        let AnnoKind::Note { x: nx, y: ny } = sticky.kind else {
            unreachable!()
        };
        let origin = self.app.page_origins()[0];
        let zoom = self.app.zoom();
        self.move_to((origin.x + nx * zoom, origin.y + ny * zoom));
        self.button(ElementState::Pressed, MouseButton::Right);
        assert!(
            self.app.context_menu.is_some(),
            "right-click over the note opens its delete menu"
        );
        // The menu opens beside what it acts on, never over it.
        let target = self
            .app
            .delete_target_screen_rect(&DeleteTarget::Annotation(sticky.id.clone()))
            .expect("the note is on a laid-out page");
        let menu = self
            .app
            .context_menu
            .as_ref()
            .map(|m| Widget::get_rect(&m.widget))
            .unwrap();
        assert!(
            menu.y0 >= target.y1 - 1e-6 || menu.y1 <= target.y0 + 1e-6,
            "menu {menu:?} clears the note {target:?}"
        );
        sticky.id
    }

    fn draw_sidebar(&mut self, tr: &mut ui::text_render::TextRenderer) -> Vec<(SidebarHit, Rect)> {
        let mut scene = vello::Scene::new();
        let mut er = Rect::ZERO;
        let theme = self.app.layout_theme.clone();
        self.app
            .draw_sidebar(&mut scene, Affine::IDENTITY, tr, &theme, &mut er)
    }
}

#[test]
fn signature_pad_controls_and_ink_follow_the_scaled_pointer() {
    for scale in [1.0_f32, 1.5, 2.0] {
        let mut rig = Rig::new();
        rig.app.layout_theme = Theme::dark().with_ui_scale(scale);
        rig.app.rect = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        rig.app.reconcile_overlay();
        rig.app.reflow();
        rig.app.sidebar_tab = SidebarTab::Signatures;
        rig.app.sig_mode = SigMode::Draw;
        let mut text = TextRenderer::new();
        rig.app.sidebar_hits = rig.draw_sidebar(&mut text);
        let ink = rig.app.signature_ink_rect();
        let point = Point::new(ink.x0 + ink.width() * 0.3, ink.y0 + ink.height() * 0.7);
        rig.move_to((point.x, point.y));
        rig.press();
        let start = rig.app.sig_stroke.as_ref().unwrap()[0];
        assert!((start.0 - 0.3).abs() < 1e-6, "ink follows x at {scale}×");
        assert!((start.1 - 0.7).abs() < 1e-6, "ink follows y at {scale}×");
        rig.move_to((point.x + ink.width() * 0.2, point.y));
        rig.release();
        assert!(!rig.app.sig_strokes.is_empty());
        rig.app.sidebar_hits = rig.draw_sidebar(&mut text);
        let clear = rig
            .app
            .sidebar_hits
            .iter()
            .find(|(hit, _)| matches!(hit, SidebarHit::SigClear))
            .unwrap()
            .1
            .center();
        rig.move_to((clear.x, clear.y));
        rig.press();
        rig.release();
        assert!(
            rig.app.sig_strokes.is_empty(),
            "Clear removes ink at {scale}×"
        );
        assert!(
            rig.app.sig_stroke.is_none(),
            "Clear does not begin a stroke"
        );

        rig.app.sig_mode = SigMode::Type;
        rig.app.set_tool(Tool::Sign);
        rig.app.sidebar_hits = rig.draw_sidebar(&mut text);
        let geo = rig.app.signature_geometry(rig.app.sidebar_body());
        let (_, hint_h) = text.measure_style(
            SIGNATURE_PLACE_HINT,
            Some(geo.place.width() as f32),
            &rig.app.layout_theme.text(TextRole::Caption),
        );
        assert!(
            geo.hint_y + f64::from(hint_h) <= geo.place.y0,
            "wrapped placement hint clears Cancel at {scale}×"
        );
        let cancel = geo.place.center();
        rig.move_to((cancel.x, cancel.y));
        rig.press();
        rig.release();
        assert_eq!(
            rig.app.tool,
            Tool::Select,
            "Cancel disarms signing at {scale}×"
        );
    }
}

#[test]
fn an_idle_reader_asks_for_no_animation_frames() {
    // A freshly synced viewer with the pointer at its default (0,0) must
    // not ask for animation frames, or the loop would spin at ~60Hz while
    // idle.
    let (app, _st) = sample_app();
    assert!(
        !app.needs_animation_frame(),
        "idle app must settle to zero animation frames"
    );
}

/// Scrolling and zooming through the real `handle_event` dispatch, not the
/// `zoom_at` helper.
#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn wheel_pinch_pixel_scroll_and_arrows_zoom_or_scroll_through_the_event_path() {
    let (mut app, _st) = sample_app();
    let p0 = app.current_page;
    let (sx, m) = (app.scroll_x(), app.viewport.max_scroll_y());
    app.viewport.set_scroll(sx, m);
    app.current_page = app.nearest_page();
    assert!(app.current_page >= p0);
    assert_eq!(app.current_page, app.doc.page_count());

    // Ctrl + scroll wheel (the Wayland touchpad pinch surfaces as this): a
    // wheel-up with Ctrl held enlarges the page, a wheel-down shrinks it, and
    // a bare wheel (no Ctrl) scrolls instead of zooming.
    let mut rig = Rig::new();
    // Pointer parked in the canvas so the wheel isn't captured by a panel.
    rig.app.last_mouse = rig.app.canvas_rect.center();
    let z0 = rig.app.zoom();
    rig.wheel(ctrl_mods(), MouseScrollDelta::LineDelta(0.0, 1.0));
    let z_in = rig.app.zoom();
    assert!(
        z_in > z0 + 1e-3,
        "Ctrl+wheel-up zooms in through the event path ({z0} -> {z_in})"
    );
    rig.wheel(ctrl_mods(), MouseScrollDelta::LineDelta(0.0, -1.0));
    let z_out = rig.app.zoom();
    assert!(
        z_out < z_in - 1e-3,
        "Ctrl+wheel-down zooms back out ({z_in} -> {z_out})"
    );
    let z_before_scroll = rig.app.zoom();
    let sy0 = rig.app.scroll_y();
    rig.wheel(Modifiers::default(), MouseScrollDelta::LineDelta(0.0, -1.0));
    assert!(
        (rig.app.zoom() - z_before_scroll).abs() < 1e-6,
        "a bare wheel must not change zoom"
    );
    assert!(
        rig.app.scroll_y() > sy0,
        "a bare wheel scrolls the document instead"
    );

    // A touchpad pinch (`WindowEvent::PinchGesture`) zooms — positive delta
    // enlarges, negative delta shrinks — centred on the cursor.
    let mut rig = Rig::new();
    rig.app.last_mouse = rig.app.canvas_rect.center();
    let pinch = |rig: &mut Rig, delta: f64| {
        rig.feed(
            Modifiers::default(),
            &WindowEvent::PinchGesture {
                device_id: winit::event::DeviceId::dummy(),
                delta,
                phase: TouchPhase::Moved,
            },
        );
    };
    let z0 = rig.app.zoom();
    pinch(&mut rig, 0.25);
    let z_in = rig.app.zoom();
    assert!(z_in > z0 + 1e-3, "pinch-open zooms in ({z0} -> {z_in})");
    pinch(&mut rig, -0.25);
    assert!(
        rig.app.zoom() < z_in - 1e-3,
        "pinch-close zooms out ({z_in} -> {})",
        rig.app.zoom()
    );

    // The gesture the user actually gets on Wayland/GNOME: winit's Wayland
    // backend never emits `PinchGesture` (that is macOS/iOS only) and
    // delivers a touchpad two-finger scroll as `MouseWheel` with a
    // **`PixelDelta`** — so Ctrl + two-finger scroll is *the* touchpad zoom.
    // It zooms cursor-centred (the document point under the cursor stays
    // fixed), and the same scroll without Ctrl pans instead of zooming.
    let mut rig = Rig::new();
    // Park the cursor over the page canvas via a real CursorMoved, so the
    // wheel handler's focal point (`self.last_mouse`) is set the live way.
    // Use the canvas horizontal centre: at fit-page the page is narrower
    // than the viewport and stays centred, so only the scrollable (vertical)
    // axis can track the cursor — at the centre both axes hold, giving a
    // clean full cursor-centred check.
    let cursor = Point::new(
        rig.app.canvas_rect.center().x,
        rig.app.canvas_rect.y0 + 220.0,
    );
    rig.move_to((cursor.x, cursor.y));
    let doc_under_cursor_0 = rig.app.viewport.screen_to_content(cursor);
    let z0 = rig.app.zoom();
    rig.wheel(
        ctrl_mods(),
        MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 30.0)),
    );
    let z_in = rig.app.zoom();
    assert!(
        z_in > z0 + 1e-3,
        "Ctrl + PixelDelta scroll zooms in ({z0} -> {z_in})"
    );
    let drift = (rig.app.viewport.screen_to_content(cursor) - doc_under_cursor_0).hypot();
    assert!(
        drift < 0.5,
        "zoom stays centred on the cursor (doc-point drift {drift:.3}pt)"
    );
    let z_before = rig.app.zoom();
    let sy0 = rig.app.scroll_y();
    rig.wheel(
        Modifiers::default(),
        MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -40.0)),
    );
    assert!(
        (rig.app.zoom() - z_before).abs() < 1e-6,
        "a bare two-finger scroll must not zoom"
    );
    assert!(
        rig.app.scroll_y() > sy0,
        "a bare two-finger scroll pans the document"
    );

    // Arrow keys scroll the document by a small nudge through the real key
    // path — not a whole-page jump — and `PageDown` scrolls much further.
    let mut rig = Rig::new();
    rig.app.tool = Tool::Select;
    rig.app.go_to_page(1);
    let none = Modifiers::default();
    let sy0 = rig.app.scroll_y();
    rig.key(none, Key::Named(NamedKey::ArrowDown));
    let after_down = rig.app.scroll_y();
    assert!(
        after_down > sy0,
        "ArrowDown scrolls down ({sy0} -> {after_down})"
    );
    assert!(
        (after_down - sy0) <= KEY_SCROLL_STEP + 0.5,
        "ArrowDown is a small nudge (~{KEY_SCROLL_STEP}px), not a page jump: moved {}",
        after_down - sy0
    );
    rig.key(none, Key::Named(NamedKey::ArrowUp));
    assert!(
        (rig.app.scroll_y() - sy0).abs() < 0.5,
        "ArrowUp returns to the start"
    );
    rig.key(none, Key::Named(NamedKey::PageDown));
    assert!(
        rig.app.scroll_y() > sy0 + KEY_SCROLL_STEP,
        "PageDown scrolls a viewport-ful"
    );
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn sidebar_hits_toggle_delete_clip_to_the_body_and_draw_every_tab() {
    let mut tr = ui::text_render::TextRenderer::new();

    // A resolve chip beats its card (smallest rect wins).
    let mut rig = Rig::new();
    rig.app.sidebar_tab = SidebarTab::Comments;
    rig.app.sidebar_hits = rig.draw_sidebar(&mut tr);
    let chip = rig.app.sidebar_hits.iter().find_map(|(h, r)| match h {
        SidebarHit::Resolve(id) => Some((id.clone(), *r)),
        _ => None,
    });
    if let Some((id, r)) = chip {
        let before = rig
            .app
            .comments
            .iter()
            .find(|c| c.id == id)
            .unwrap()
            .resolved;
        assert!(
            rig.app
                .apply_sidebar(centre(r), &mut Edits::lend(&mut rig.st.doc))
        );
        let after = rig
            .app
            .comments
            .iter()
            .find(|c| c.id == id)
            .unwrap()
            .resolved;
        assert_ne!(
            before, after,
            "resolve chip should toggle, not open the card"
        );
    }

    // Hit rect == painted rect: the sidebar body is painted inside a clip,
    // so a card (and its delete / resolve chips) scrolled under the tab
    // strip must not keep an invisible, clickable rect there.
    // `apply_sidebar` picks the smallest containing rect, which is exactly
    // how a hidden 24px delete button used to beat the "Contents" tab.
    let mut rig = Rig::new();
    // A short window so the seeded thread list overflows its body.
    rig.app.rect = Rect::new(0.0, 0.0, 1280.0, 420.0);
    rig.app.recompute_regions();
    rig.app.sync_viewport_frame();
    rig.app.show_sidebar = true;
    rig.app.sidebar_tab = SidebarTab::Comments;
    // Scroll far enough that the first card sits under the tab strip.
    rig.app.sidebar_scroll = 180.0;
    let hits = rig.draw_sidebar(&mut tr);
    let body = rig.app.sidebar_body();
    let tabs = rig.app.sidebar_tab_rects();
    let body_hits: Vec<&(SidebarHit, Rect)> = hits
        .iter()
        .filter(|(h, _)| !matches!(h, SidebarHit::Tab(_)))
        .collect();
    assert!(
        !body_hits.is_empty(),
        "the scrolled list still exposes the cards that remain visible"
    );
    for (i, (_, r)) in body_hits.iter().enumerate() {
        assert!(
            r.x0 >= body.x0 - 0.01
                && r.x1 <= body.x1 + 0.01
                && r.y0 >= body.y0 - 0.01
                && r.y1 <= body.y1 + 0.01,
            "body hit #{i} {r:?} escapes the painted body {body:?}"
        );
        assert!(
            r.width() > 0.0 && r.height() > 0.0,
            "body hit #{i} {r:?} is empty and should have been dropped"
        );
        for (tab, t) in &tabs {
            let overlap = r.intersect(*t);
            assert!(
                overlap.width() <= 0.0 || overlap.height() <= 0.0,
                "body hit #{i} {r:?} reaches into the {tab:?} tab {t:?}"
            );
        }
    }

    // Sidebar delete (×) buttons remove comments, then signatures.
    let mut rig = Rig::new();
    rig.app.sidebar_tab = SidebarTab::Comments;
    rig.app.sidebar_hits = rig.draw_sidebar(&mut tr);
    let del = rig.app.sidebar_hits.iter().find_map(|(h, r)| match h {
        SidebarHit::DeleteComment(id) => Some((id.clone(), *r)),
        _ => None,
    });
    if let Some((id, r)) = del {
        let n = rig.app.comments.len();
        assert!(
            rig.app
                .apply_sidebar(centre(r), &mut Edits::lend(&mut rig.st.doc))
        );
        assert_eq!(rig.app.comments.len(), n - 1);
        assert!(!rig.app.comments.iter().any(|c| c.id == id));
    }
    rig.app.sidebar_tab = SidebarTab::Signatures;
    rig.app.sidebar_hits = rig.draw_sidebar(&mut tr);
    let dels = rig.app.sidebar_hits.iter().find_map(|(h, r)| match h {
        SidebarHit::DeleteSignature(id) => Some((id.clone(), *r)),
        _ => None,
    });
    if let Some((id, r)) = dels {
        let n = rig.app.signatures.len();
        assert!(
            rig.app
                .apply_sidebar(centre(r), &mut Edits::lend(&mut rig.st.doc))
        );
        assert_eq!(rig.app.signatures.len(), n - 1);
        assert!(!rig.app.signatures.iter().any(|s| s.id == id));
    }

    // Every sidebar tab draws without panicking.
    let (mut app, _st) = sample_app();
    let theme = ui::theme::Theme::dark();
    for tab in [
        SidebarTab::Comments,
        SidebarTab::Outline,
        SidebarTab::Signatures,
    ] {
        app.sidebar_tab = tab;
        let mut scene = vello::Scene::new();
        app.prepare_sidebar_scroll(&mut tr);
        let mut er = Rect::ZERO;
        let _ = app.draw_sidebar(&mut scene, Affine::IDENTITY, &mut tr, &theme, &mut er);
    }
}

#[test]
fn a_browser_types_into_a_new_note_through_ime_commits() {
    // A page's hidden text editor hands typed text over as IME commits;
    // the note just placed must take them, and Enter keeps them.
    let mut rig = Rig::new();
    let c0 = rig.app.comments.len();
    rig.app
        .create_note(&mut Edits::lend(&mut rig.st.doc), 1, (120.0, 200.0));
    assert!(rig.app.editing.is_some(), "the new note's field is open");
    assert!(rig.feed(
        Modifiers::default(),
        &WindowEvent::Ime(winit::event::Ime::Commit("Check this figure".into())),
    ));
    assert_eq!(
        rig.app.editing.as_ref().map(Editing::text).as_deref(),
        Some("Check this figure")
    );
    rig.key(Modifiers::default(), Key::Named(NamedKey::Enter));
    assert!(rig.app.editing.is_none(), "Enter sends the comment");
    assert_eq!(rig.app.comments.len(), c0 + 1);
    assert!(
        rig.app
            .comments
            .iter()
            .any(|comment| comment.body == "Check this figure"),
        "the typed comment is kept"
    );
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn canvas_tools_highlight_note_pan_select_and_dismiss_through_the_event_path() {
    // A highlight drag across page one, with the tool armed from the
    // window bar, adds an annotation to the shared document.
    let mut rig = Rig::new();
    rig.arm_via_window_bar(Tool::Highlight, "the window bar arms the tool");
    let origin = rig.app.page_origins()[0];
    let zoom = rig.app.zoom();
    let before = rig.app.annotations.len();
    rig.move_to((origin.x + 80.0 * zoom, origin.y + 200.0 * zoom));
    rig.press();
    rig.move_to((origin.x + 320.0 * zoom, origin.y + 214.0 * zoom));
    rig.release();
    assert!(
        rig.app.annotations.len() > before,
        "a highlight drag through handle_event adds an annotation"
    );
    assert!(
        PdfMarkup::of(&rig.st.doc)
            .expect("the PDF markup reads")
            .list_annotations()
            .len()
            > before,
        "and it landed in the shared document"
    );

    // The Note tool drops a note whose body is typed and committed through
    // the real keyboard path.
    let mut rig = Rig::new();
    rig.arm_via_window_bar(Tool::Note, "the window bar arms the note tool");
    let origin = rig.app.page_origins()[0];
    let zoom = rig.app.zoom();
    let comments_before = rig.app.comments.len();
    rig.move_to((origin.x + 200.0 * zoom, origin.y + 260.0 * zoom));
    rig.press();
    assert_eq!(
        rig.app.comments.len(),
        comments_before + 1,
        "placing a note opens a new thread"
    );
    assert!(
        matches!(&rig.app.editing, Some(e) if matches!(e.target, EditTarget::CommentBody(_))),
        "the note's body is focused for typing"
    );
    for ch in "Relay path".chars() {
        let key = if ch == ' ' {
            Key::Named(NamedKey::Space)
        } else {
            Key::Character(ch.to_string().into())
        };
        rig.key(Modifiers::default(), key);
    }
    rig.key(Modifiers::default(), Key::Named(NamedKey::Enter));
    assert!(
        rig.app.editing.is_none(),
        "Enter commits and closes the editor"
    );
    let saved = PdfMarkup::of(&rig.st.doc)
        .expect("the PDF markup reads")
        .list_comments();
    assert!(
        saved.iter().any(|c| c.body == "Relay path"),
        "the typed comment body landed in the shared document: {:?}",
        saved.iter().map(|c| c.body.clone()).collect::<Vec<_>>()
    );

    // A Select-tool press-move-release past `PAN_SLOP` pans the canvas and
    // selects nothing. The sample seeds a pre-selected note; clear it so we
    // can prove a pan never triggers a click-selection of its own.
    let mut rig = Rig::new();
    rig.app.tool = Tool::Select;
    rig.app.go_to_page(1);
    rig.app.selected_anno = None;
    let a = (
        rig.app.canvas_rect.x0 + 200.0,
        rig.app.canvas_rect.y0 + 300.0,
    );
    let sy0 = rig.app.scroll_y();
    rig.move_to(a);
    rig.press();
    rig.move_to((a.0, a.1 - 200.0)); // drag up -> content scrolls down
    rig.release();
    assert!(
        rig.app.scroll_y() > sy0 + 1.0,
        "a Select drag pans the canvas ({sy0} -> {})",
        rig.app.scroll_y()
    );
    assert!(
        rig.app.selected_anno.is_none(),
        "a pan does not select an annotation"
    );

    let mut rig = Rig::new();
    rig.app.tool = Tool::Select;
    rig.app.go_to_page(1);
    let hl = rig
        .app
        .annotations
        .iter()
        .find(|a| matches!(a.kind, AnnoKind::Markup { .. }))
        .unwrap()
        .clone();
    let AnnoKind::Markup { quads, .. } = hl.kind else {
        unreachable!()
    };
    let origin = rig.app.page_origins()[0];
    let (cx, cy) = quads[0].center();
    let zoom = rig.app.zoom();
    rig.move_to((origin.x + cx * zoom, origin.y + cy * zoom));
    rig.press();
    rig.release();
    assert_eq!(
        rig.app.selected_anno.as_deref(),
        Some(hl.id.as_str()),
        "a Select click selects the annotation under it"
    );

    // On a narrow (overlay) window, clicking the canvas scrim beside an
    // open panel dismisses the panels.
    let mut app = crate::scenarios::sample_reader();
    let mut st = crate::scenarios::sample_state();
    app.sync_settled(&mut st.doc, &st.root);
    // Narrow window -> overlay mode; panels auto-close on the transition.
    app.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
    app.reconcile_overlay();
    app.recompute_regions();
    app.sync_viewport_frame();
    assert!(app.overlay_mode, "narrow window is in overlay mode");
    app.toggle_sidebar();
    assert!(app.show_sidebar, "sidebar opened as an overlay");
    let mut rig = Rig::with(app, st);
    // Click the scrim: a canvas point on the far left, clear of the panel.
    rig.move_to((rig.app.canvas_rect.x0 + 8.0, rig.app.canvas_rect.y0 + 40.0));
    rig.press();
    assert!(
        !rig.app.show_sidebar && !rig.app.show_rail,
        "clicking the scrim closes the overlay panels"
    );
}

/// The inline editor: empty notes are discarded, the focused field's rect
/// is captured on draw, replies append, and Ctrl+C / Ctrl+X / Ctrl+V are
/// real clipboard chords, not typed characters.
///
/// `#[serial]` because it *writes* and reads the shared system clipboard,
/// a shared, process-external resource.
#[test]
#[serial]
fn inline_editor_commits_replies_discards_empty_notes_and_handles_clipboard_chords() {
    // Placing a note then committing without typing must not litter an
    // empty marker + thread.
    let (mut app, mut st) = sample_app();
    let a0 = app.annotations.len();
    let c0 = app.comments.len();
    app.create_note(&mut Edits::lend(&mut st.doc), 1, (120.0, 200.0));
    assert_eq!(app.comments.len(), c0 + 1);
    assert!(app.editing.is_some());
    app.commit_edit(&mut Edits::lend(&mut st.doc)); // committed empty
    assert_eq!(app.comments.len(), c0, "empty note thread removed");
    assert_eq!(app.annotations.len(), a0, "and its page marker removed");

    let (mut app, _st) = sample_app();
    app.sidebar_tab = SidebarTab::Comments;
    let body = app.comments[0].body.clone();
    let cid = app.comments[0].id.clone();
    app.begin_edit(EditTarget::CommentBody(cid), &body);
    let theme = ui::theme::Theme::dark();
    let mut tr = ui::text_render::TextRenderer::new();
    app.prepare_sidebar_scroll(&mut tr);
    let mut scene = vello::Scene::new();
    let mut er = Rect::ZERO;
    let _ = app.draw_sidebar(&mut scene, Affine::IDENTITY, &mut tr, &theme, &mut er);
    assert!(
        er.area() > 1.0,
        "the focused comment field's rect is captured"
    );
    // A click inside that rect must keep the edit alive; outside commits.
    assert!(
        app.edit_rect.area() < 1.0,
        "edit_rect is only set by draw_all, not draw_sidebar"
    );

    // A reply composed into a seeded comment lands in its thread.
    let (mut app, mut st) = sample_app();
    let cid = app.comments[0].id.clone();
    let replies_before = app.comments[0].replies.len();
    app.begin_edit(EditTarget::Reply(cid.clone()), "");
    app.editing.as_mut().unwrap().set_text("Looks good");
    app.commit_edit(&mut Edits::lend(&mut st.doc));
    let after = app.comments.iter().find(|c| c.id == cid).unwrap();
    assert_eq!(
        after.replies.len(),
        replies_before + 1,
        "reply landed in the thread"
    );
    assert_eq!(after.replies.last().unwrap().body, "Looks good");

    // Select the whole field first (Ctrl+A) so copy/cut act on the text —
    // the shared TextInput has a real selection model. The cut's
    // field-clear is deterministic regardless of platform-clipboard
    // availability.
    let mut rig = Rig::new();
    let cid = rig.app.comments[0].id.clone();
    rig.app.begin_edit(EditTarget::CommentBody(cid), "hello");
    rig.key(ctrl_mods(), character("a"));
    // Ctrl+C is a clipboard chord, not a keystroke: the field stays "hello".
    assert!(
        rig.key(ctrl_mods(), character("c")),
        "the editor consumes the chord"
    );
    assert_eq!(
        rig.app.editing.as_ref().unwrap().text(),
        "hello",
        "Ctrl+C does not type a literal 'c'"
    );
    // Ctrl+X cuts the selection: the whole field clears.
    rig.key(ctrl_mods(), character("x"));
    assert_eq!(
        rig.app.editing.as_ref().unwrap().text(),
        "",
        "Ctrl+X clears the selected field"
    );

    // Paste inserts the system clipboard's text at the caret via the real
    // editor path. The expectation is derived from the clipboard's *actual*
    // content at paste time (read back with no concurrent writer under
    // `#[serial]`), not from the value we tried to copy, so the assertion
    // is exact whether or not the platform clipboard is available: where it
    // round-trips our write, the buffer becomes "AB" + that text at the
    // caret; where it is unavailable (headless CI), paste is a safe no-op
    // leaving "AB".
    let mut rig = Rig::new();
    let cid = rig.app.comments[0].id.clone();
    rig.app.begin_edit(EditTarget::CommentBody(cid), "AB"); // caret at end
    ui::clipboard_copy("XY");
    rig.key(ctrl_mods(), character("v"));
    let after = rig.app.editing.as_ref().unwrap().text();
    let clip: String = ui::clipboard_paste()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    if clip.is_empty() {
        assert_eq!(after, "AB", "paste with no clipboard is a safe no-op");
    } else {
        assert_eq!(
            after,
            format!("AB{clip}"),
            "Ctrl+V inserts the clipboard text at the caret"
        );
        if ui::clipboard_paste().as_deref() == Some("XY") {
            assert_eq!(after, "ABXY", "the pasted text is exactly what was copied");
        }
    }
}

/// The window bar's Open goes to the **host portal**, never an in-app
/// picker, under a recording host so no real dialog is spawned. A chosen
/// file loads through the per-frame drain in `sync`; a cancel is a no-op.
#[test]
#[serial(pdfium)]
fn open_goes_through_the_host_portal_and_loads_or_cancels_on_the_frame_drain() {
    let open = || ui::window_toolbar::ToolbarEvent::Action {
        id: super::window_bar::OPEN.into(),
    };
    let (mut app, _) = sample_app();
    assert!(
        app.window_toolbar()
            .actions
            .iter()
            .any(|action| action.id == super::window_bar::OPEN && action.enabled),
        "the window bar offers Open"
    );
    let (services, recorder) = ui::host::HostServices::recording();
    ui::host::scoped(&services, || {
        app.run_window_bar_event(&open());
    });
    assert!(
        app.portal_rx.is_some(),
        "Open routes to the host portal request path (not an in-app dialog)"
    );
    let requests = recorder.pick_requests();
    assert_eq!(requests.len(), 1, "exactly one host chooser request");
    assert_eq!(requests[0].title(), "Open a PDF");

    // A portal-chosen file loads through the per-frame drain.
    let mut app = PdfApp::empty();
    let mut st = crate::scenarios::record_state();
    app.sync_settled(&mut st.doc, &st.root);
    app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    app.recompute_regions();
    app.sync_viewport_frame();
    assert!(!app.has_document, "empty app starts with no document");
    let path = std::path::PathBuf::from(concat!(env!("OUT_DIR"), "/sample.pdf"));
    let (services, recorder) = ui::host::HostServices::recording();
    recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(path));
    // With no document open the bar's menu still offers Open.
    assert!(
        app.window_toolbar()
            .actions
            .iter()
            .any(|action| action.id == super::window_bar::OPEN && action.enabled),
        "the bar offers Open with no document"
    );
    ui::host::scoped(&services, || {
        app.run_window_bar_event(&open());
    });
    app.sync_settled(&mut st.doc, &st.root);
    assert!(
        app.pending_open.is_some(),
        "the frame drain hands the portal-chosen PDF to the worker"
    );
    super::tests::settle_until(&mut app, &mut st, |app| app.pending_open.is_none());
    assert!(
        app.has_document,
        "a portal-chosen PDF loads through the frame drain"
    );
    assert_eq!(app.doc_name, "sample.pdf");
    assert!(
        app.portal_rx.is_none(),
        "the portal channel clears after delivering"
    );
    assert!(app.doc.page_count() >= 1, "the loaded document has pages");

    let (mut app, mut st) = sample_app(); // sample document already loaded
    let before = app.has_document;
    let name_before = app.doc_name.clone();
    let (services, recorder) = ui::host::HostServices::recording();
    recorder.respond_to_pick(ui::portal::PortalOutcome::Cancelled);
    ui::host::scoped(&services, || {
        app.run_window_bar_event(&open());
    });
    assert!(
        app.portal_rx.is_some(),
        "Open issued a host chooser request"
    );
    app.sync_settled(&mut st.doc, &st.root);
    assert_eq!(
        app.has_document, before,
        "cancel does not change the open state"
    );
    assert_eq!(
        app.doc_name, name_before,
        "cancel keeps the current document"
    );
    assert!(
        app.portal_rx.is_none(),
        "the portal channel clears after a cancel"
    );
}

/// A PDF dropped on the empty viewer opens like one chosen, with no
/// hover before it (Wayland may report only the drop).
#[test]
fn a_pdf_dropped_on_the_empty_viewer_opens() {
    let mut app = PdfApp::empty();
    let mut st = crate::scenarios::record_state();
    app.sync_settled(&mut st.doc, &st.root);
    let path = std::path::PathBuf::from(concat!(env!("OUT_DIR"), "/sample.pdf"));
    let mut rig = Rig::with(app, st);
    assert!(rig.feed(Modifiers::default(), &WindowEvent::DroppedFile(path)));
    assert!(
        rig.app.pending_open.is_some(),
        "the drop reaches the worker"
    );
    super::tests::settle_until(&mut rig.app, &mut rig.st, |app| app.pending_open.is_none());
    assert!(rig.app.has_document, "the dropped PDF loads");
}

/// A file dragged over the window rings the page rail as a drop target;
/// leaving the window without dropping clears it.
#[test]
fn a_file_dragged_in_arms_the_page_rail() {
    let mut rig = Rig::new();
    rig.app.has_document = true;
    rig.app.show_rail = true;
    let path = std::path::PathBuf::from("/tmp/pages.pdf");
    rig.feed(Modifiers::default(), &WindowEvent::HoveredFile(path));
    assert!(rig.app.page_sidebar.is_drop_target_armed());
    rig.feed(Modifiers::default(), &WindowEvent::HoveredFileCancelled);
    assert!(!rig.app.page_sidebar.is_drop_target_armed());
}

/// The sidebar / context-menu Delete on an annotation dispatches the
/// byte-identical `EncodedAction` the Ctrl+K palette's own delete spec
/// carries — one source of truth, proven on the real affordance path —
/// and the shared context menu deletes by pointer and by keyboard.
#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn delete_affordances_and_the_context_menu_dispatch_the_palette_action() {
    fn find<'a>(
        resource: &'a tilo_actions::Resource,
        id: &str,
    ) -> Option<&'a tilo_actions::Resource> {
        if resource.id.as_str() == id {
            return Some(resource);
        }
        resource.children.iter().find_map(|child| find(child, id))
    }
    let (mut app, mut st) = sample_app();
    let anno = app
        .annotations
        .first()
        .expect("sample seeds annotations")
        .clone();
    let published = ui::tilo_resource::publish::<crate::PdfMarkup>(
        "pdf.document",
        "PDF",
        &<crate::PdfMarkup as ui::tilo_resource::Resource>::read(&st.doc).unwrap(),
    );
    let want_id = ui::tilo_resource::items::item_resource_id::<crate::PdfMarkup>(
        "annotations",
        &ui::tilo_resource::Id::from_raw(anno.id.clone()),
    );
    let res = find(&published, &want_id).expect("the annotation is published as a resource");
    assert!(
        res.actions.iter().any(|a| a.id == "delete-annotation"),
        "{:?}",
        res.actions
            .iter()
            .map(|a| a.id.as_str())
            .collect::<Vec<_>>()
    );
    let mut palette = st.doc.fork();
    crate::PdfMarkup::apply_to(
        &mut palette,
        crate::actions::delete_annotation_action(&anno.id),
    );
    // …and the real affordance path (Delete key / rail button / context menu).
    app.delete_target(
        &DeleteTarget::Annotation(anno.id.clone()),
        &mut Edits::lend(&mut st.doc),
    );
    let ids = |annotations: Vec<Annotation>| -> Vec<String> {
        annotations.into_iter().map(|a| a.id).collect()
    };
    let comment_ids =
        |comments: Vec<Comment>| -> Vec<String> { comments.into_iter().map(|c| c.id).collect() };
    assert_eq!(
        ids(PdfMarkup::of(&st.doc)
            .expect("the PDF markup reads")
            .list_annotations()),
        ids(PdfMarkup::of(&palette)
            .expect("the PDF markup reads")
            .list_annotations()),
        "one source of truth"
    );
    assert_eq!(
        comment_ids(
            PdfMarkup::of(&st.doc)
                .expect("the PDF markup reads")
                .list_comments()
        ),
        comment_ids(
            PdfMarkup::of(&palette)
                .expect("the PDF markup reads")
                .list_comments()
        )
    );
    assert!(
        app.annotations.iter().all(|a| a.id != anno.id),
        "the annotation was deleted via the dispatched action"
    );

    let (mut app, _st) = sample_app();
    app.context_menu = Some(ContextMenu::delete(
        DeleteTarget::Annotation("anno".into()),
        Rect::new(400.0, 400.0, 400.0, 400.0),
        app.rect,
    ));
    let menu = app.context_menu.as_ref().unwrap();
    assert!(
        menu.widget
            .item_rect(0)
            .is_some_and(|rect| rect.area() > 1.0),
        "the shared Delete row reports exact hit geometry"
    );

    let mut rig = Rig::new();
    rig.app.go_to_page(1);
    let note_id = rig.right_click_seeded_note();
    let semantics = Widget::accessibility(&rig.app).expect("PDF semantic root");
    let menu = semantics
        .children
        .iter()
        .find(|node| node.role == ui::a11y::Role::Menu)
        .expect("open shared menu is exposed");
    assert_eq!(menu.children[0].name.as_deref(), Some("Delete"));
    assert!(
        !menu.children[0].state.focused,
        "destructive-only menus require an explicit choice"
    );
    assert!(Widget::traps_focus(&rig.app));
    assert!(Widget::modal_capture(&rig.app).is_some());
    let item = rig
        .app
        .context_menu
        .as_ref()
        .and_then(|menu| menu.widget.item_rect(0))
        .unwrap();
    assert!(item.area() > 1.0, "the shared Delete row has hit bounds");
    let before = rig.app.annotations.len();
    let ic = centre(item);
    rig.move_to((ic.x, ic.y));
    rig.press();
    assert!(
        rig.app.context_menu.is_none(),
        "the menu closes after the delete"
    );
    assert_eq!(
        rig.app.annotations.len(),
        before - 1,
        "the targeted annotation is removed"
    );
    assert!(
        !rig.app.annotations.iter().any(|a| a.id == note_id),
        "specifically the right-clicked note"
    );
    assert!(
        !PdfMarkup::of(&rig.st.doc)
            .expect("the PDF markup reads")
            .list_annotations()
            .iter()
            .any(|a| a.id == note_id),
        "and it is gone from the shared document"
    );

    let mut rig = Rig::new();
    rig.app.go_to_page(1);
    let note_id = rig.right_click_seeded_note();
    rig.key(Modifiers::default(), Key::Named(NamedKey::ArrowDown));
    rig.key(Modifiers::default(), Key::Named(NamedKey::Enter));
    assert!(rig.app.context_menu.is_none());
    assert!(
        !rig.app
            .annotations
            .iter()
            .any(|annotation| annotation.id == note_id)
    );
    assert!(
        !PdfMarkup::of(&rig.st.doc)
            .expect("the PDF markup reads")
            .list_annotations()
            .iter()
            .any(|annotation| annotation.id == note_id)
    );
}

#[test]
fn the_review_panel_gives_way_to_the_pages_and_its_edge_resizes_it() {
    fn has_door(node: &ui::a11y::AccessNode) -> bool {
        node.action_target.as_deref() == Some("inspector-resize")
            || node.children.iter().any(has_door)
    }
    let mut rig = Rig::new();
    let m = rig.app.layout_theme.tokens.metrics;
    let lay_out = |app: &mut PdfApp, width: f64| {
        app.rect = Rect::new(0.0, 0.0, width, 800.0);
        app.reconcile_overlay();
        app.recompute_regions();
    };

    lay_out(&mut rig.app, 1280.0);
    assert!(!rig.app.overlay_mode && rig.app.show_sidebar);
    assert!((rig.app.sidebar_rect.width() - f64::from(m.inspector_width)).abs() < 0.01);

    // Narrower, the panel shrinks before the pages lose their minimum,
    // and still docks.
    let (rail_min, rail_max) = rig.app.rail_width_range();
    let rail = rig.app.rail_w.clamp(rail_min, rail_max);
    let tight = rail + f64::from(m.sidebar_min_width) + f64::from(m.detail_min_width) + 40.0;
    lay_out(&mut rig.app, tight);
    assert!(!rig.app.overlay_mode, "the panel docks at its narrowest");
    let panel = rig.app.sidebar_rect.width();
    assert!(panel < f64::from(m.inspector_width) && panel >= f64::from(m.sidebar_min_width));
    assert!(rig.app.canvas_rect.width() >= f64::from(m.detail_min_width) - 0.5);

    // Dragging the panel's leading edge widens it.
    lay_out(&mut rig.app, 1280.0);
    let edge = rig.app.sidebar_rect.x0;
    let y = rig.app.sidebar_rect.center().y;
    let door = ui::widget::Widget::accessibility(&rig.app).is_some_and(|tree| has_door(&tree));
    assert!(door, "the divider has a semantic door");
    rig.move_to((edge, y));
    rig.press();
    rig.move_to((edge - 40.0, y));
    rig.move_to((edge - 80.0, y));
    rig.release();
    let widened = rig.app.sidebar_rect.width();
    assert!(
        (widened - f64::from(m.inspector_width) - 80.0).abs() < 1.0,
        "the panel follows the drag: {widened}"
    );
    assert!((rig.app.canvas_rect.x1 - rig.app.sidebar_rect.x0).abs() < 0.01);

    // Dragged under its minimum it closes.
    let edge = rig.app.sidebar_rect.x0;
    rig.move_to((edge, y));
    rig.press();
    rig.move_to((1270.0, y));
    rig.release();
    assert!(!rig.app.show_sidebar, "dragged shut, the panel closes");
}
