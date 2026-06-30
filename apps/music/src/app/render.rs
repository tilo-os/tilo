//! Drawing + event handling for [`MusicApp`].
//!
//! Split out of `app.rs` to keep the data/state logic separate from the
//! (large) rendering surface. Everything here draws straight into the Vello
//! scene and registers click `Hit`s for the event handler.

use super::*;

use vello::kurbo::Shape;
use vello::peniko::BlendMode;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

// ---- owned per-frame display rows (decouple draw from &self.lib borrow) ---

struct TrackDisp {
    id: String,
    title: String,
    artist: String,
    album: String,
    album_id: String,
    dur: u32,
    cache: CacheState,
    no: u32,
}

struct AlbumDisp {
    id: String,
    title: String,
    artist: String,
    year: u32,
    queue: Vec<String>,
}

struct ArtistDisp {
    id: String,
    name: String,
    genre: String,
    listeners: u64,
}

struct PlaylistDisp {
    id: String,
    name: String,
    count: usize,
}

// Vertical-center a single text line of `size` within a row of height `h`.
fn vy(top: f64, h: f64, size: f32) -> f64 {
    top + (h - size as f64 * 1.25) / 2.0
}

fn human_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.0}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

impl Widget for MusicApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        self.ensure_seeded(state);
        if !self.playback_loaded {
            self.reload(state);
            self.audio.set_volume(self.pb.volume as f32 / 100.0);
            self.playback_loaded = true;
        } else {
            self.reload_library(state);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.render_frame(scene, transform, text_renderer, theme);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        // Root rect is the whole window — always paint.
        self.render_frame(scene, transform, text_renderer, theme);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // While the context menu is open it owns the pointer + keyboard: route
        // the event to it, then drain any selection into the Action pipeline.
        if self.context.is_open() {
            let mut menu_handled = false;
            let mut ft = None;
            self.context.handle_event(
                event, scale_factor, modifiers, state, _path, &mut menu_handled, &mut ft, _node_id, _taffy,
            );
            if self.drain_menu_selection(state) {
                *handled = true;
                self.dirty = true;
                return;
            }
            if menu_handled || matches!(event, WindowEvent::MouseInput { state: ElementState::Pressed, .. }) {
                // The menu consumed the click (selection / outside-dismiss).
                *handled = true;
                self.dirty = true;
                return;
            }
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.dirty = true;
                if let Some(drag) = self.drag {
                    match drag {
                        Drag::Scrubber => {
                            let f = frac_in(self.scrubber_rect, self.mouse.x);
                            self.dispatch(Action::SeekFrac(f), state);
                        }
                        Drag::Volume => {
                            let f = frac_in(self.volume_rect, self.mouse.x);
                            self.dispatch(Action::VolumeFrac(f), state);
                        }
                    }
                    *handled = true;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                // Right-click opens a track's context menu where one is registered.
                if let Some(action) = self.row_menu_at(self.mouse) {
                    self.dispatch(action, state);
                    *handled = true;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                // Hamburger toggle wins over everything (it sits above the
                // scrim hit that otherwise covers the whole content in overlay).
                if self.show_hamburger() && self.hamburger_rect.contains(self.mouse) {
                    self.dispatch(Action::ToggleSidebar, state);
                    *handled = true;
                    return;
                }
                // Transport bars get drag priority.
                if self.scrubber_rect.contains(self.mouse) {
                    self.drag = Some(Drag::Scrubber);
                    let f = frac_in(self.scrubber_rect, self.mouse.x);
                    self.dispatch(Action::SeekFrac(f), state);
                    *handled = true;
                    return;
                }
                if self.volume_rect.contains(self.mouse) {
                    self.drag = Some(Drag::Volume);
                    let f = frac_in(self.volume_rect, self.mouse.x);
                    self.dispatch(Action::VolumeFrac(f), state);
                    *handled = true;
                    return;
                }
                // Topmost matching hit wins (later-drawn = on top).
                if let Some(action) = self.hit_test(self.mouse) {
                    // Clicking anywhere outside the search field blurs it.
                    if !matches!(action, Action::FocusSearch) {
                        self.search_focused = false;
                    }
                    self.dispatch(action, state);
                    *handled = true;
                } else {
                    self.search_focused = false;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                if self.drag.is_some() {
                    self.drag = None;
                    *handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y as f64 * 48.0,
                    MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
                };
                let max = (self.content_height - self.body_rect.height()).max(0.0);
                self.scroll = (self.scroll - dy).clamp(0.0, max);
                self.dirty = true;
                *handled = true;
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, text, .. },
                ..
            } => {
                self.on_key(logical_key, text.as_deref(), modifiers, state, handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "MusicApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_animation_frame(&self) -> bool {
        self.pb.playing || self.toast.is_some()
    }

    fn needs_redraw(&self) -> bool {
        true
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

/// Map an x coordinate to a `0..=1` fraction within `r`.
fn frac_in(r: Rect, x: f64) -> f64 {
    if r.width() <= 0.0 {
        return 0.0;
    }
    ((x - r.x0) / r.width()).clamp(0.0, 1.0)
}

impl MusicApp {
    fn hit_test(&self, p: Point) -> Option<Action> {
        for h in self.hits.iter().rev() {
            if h.rect.contains(p) {
                return Some(h.action.clone());
            }
        }
        None
    }

    fn on_key(
        &mut self,
        key: &Key,
        text: Option<&str>,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        // The name-input overlay owns typing while open.
        if self.name_prompt.is_some() {
            match key {
                Key::Named(NamedKey::Escape) => {
                    self.dispatch(Action::CloseNamePrompt, state);
                    *handled = true;
                }
                Key::Named(NamedKey::Backspace) => {
                    self.name_input.pop();
                    self.dirty = true;
                    *handled = true;
                }
                Key::Named(NamedKey::Enter) => {
                    let commit = match self.name_prompt.clone() {
                        Some(NamePrompt::NewPlaylist) => Action::CreatePlaylist,
                        Some(NamePrompt::Rename(id)) => Action::CommitRename(id),
                        None => Action::None,
                    };
                    self.dispatch(commit, state);
                    *handled = true;
                }
                _ => {
                    if let Some(tx) = text {
                        if !tx.is_empty() && !tx.chars().any(|c| c.is_control()) {
                            self.name_input.push_str(tx);
                            self.dirty = true;
                            *handled = true;
                        }
                    }
                }
            }
            return;
        }

        // Search field owns typing while focused.
        if self.search_focused {
            match key {
                Key::Named(NamedKey::Escape) => {
                    self.search_focused = false;
                    *handled = true;
                }
                Key::Named(NamedKey::Backspace) => {
                    self.search_query.pop();
                    self.dirty = true;
                    *handled = true;
                }
                Key::Named(NamedKey::Enter) => {
                    self.search_focused = false;
                    *handled = true;
                }
                _ => {
                    if let Some(t) = text {
                        if !t.is_empty() && !t.chars().any(|c| c.is_control()) {
                            self.search_query.push_str(t);
                            self.dirty = true;
                            *handled = true;
                        }
                    }
                }
            }
            return;
        }

        match key {
            Key::Named(NamedKey::Escape) => {
                // Dismiss an open overlay sidebar.
                if self.sidebar_layout() == SplitLayout::Overlay && self.flap.is_open(self.sidebar_layout()) {
                    self.dispatch(Action::CloseSidebar, state);
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Space) => {
                self.dispatch(Action::TogglePlay, state);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowRight) => {
                if modifiers.state().shift_key() {
                    self.dispatch(Action::Next, state);
                } else {
                    let dur = self.current_duration_ms() as f64;
                    let f = if dur > 0.0 { (self.playhead_ms + 5000.0) / dur } else { 0.0 };
                    self.dispatch(Action::SeekFrac(f), state);
                }
                *handled = true;
            }
            Key::Named(NamedKey::ArrowLeft) => {
                if modifiers.state().shift_key() {
                    self.dispatch(Action::Prev, state);
                } else {
                    let dur = self.current_duration_ms() as f64;
                    let f = if dur > 0.0 { (self.playhead_ms - 5000.0) / dur } else { 0.0 };
                    self.dispatch(Action::SeekFrac(f), state);
                }
                *handled = true;
            }
            Key::Character(c) => {
                let lc = c.to_lowercase();
                match lc.as_str() {
                    "/" => {
                        self.dispatch(Action::FocusSearch, state);
                        *handled = true;
                    }
                    "n" => {
                        self.dispatch(Action::Next, state);
                        *handled = true;
                    }
                    "p" => {
                        self.dispatch(Action::Prev, state);
                        *handled = true;
                    }
                    "s" => {
                        self.dispatch(Action::ToggleShuffle, state);
                        *handled = true;
                    }
                    "r" => {
                        self.dispatch(Action::CycleRepeat, state);
                        *handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // ---- master frame -----------------------------------------------------

    pub(super) fn render_frame(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        self.hits.clear();
        self.row_menu_hits.clear();
        // Apply any deferred `--shots` staging now that `rect` is known.
        self.apply_shot_stage();
        self.tick();
        if let Some((_, at)) = &self.toast {
            if at.elapsed().as_secs_f32() > 2.6 {
                self.toast = None;
            }
        }

        // Advance the adaptive sidebar: reconcile a stale preference across a
        // breakpoint crossing, then ease the reveal toward its target.
        let layout = self.sidebar_layout();
        self.flap.reconcile(layout);
        if !self.flap_primed {
            // First frame: the layout is only now known. Snap the reveal to the
            // resolved openness so a cold start into a narrow window opens
            // closed (and a `--shots` frame is settled), rather than animating
            // in from the wide default. Honours an explicit scenario snap.
            let open = self.flap.is_open(layout);
            self.flap.snap(open);
            self.flap_primed = true;
        }
        let now = Instant::now();
        let dt = now.duration_since(self.flap_tick).as_secs_f64();
        self.flap_tick = now;
        self.flap.tick(layout, dt);

        let c = Palette::from(theme);
        let r = self.rect;

        // App background.
        scene.fill(Fill::NonZero, t, c.bg, None, &r);

        // Content column begins after the docked sidebar inset (0 in overlay,
        // where the content uses the full width and the sidebar floats over).
        let content_x0 = r.x0 + self.content_inset();
        let header = Rect::new(content_x0, r.y0, r.x1, r.y0 + HEADER_H);
        let body = Rect::new(content_x0, r.y0 + HEADER_H, r.x1, r.y1 - NOWPLAYING_H);
        // The now-playing transport runs along the bottom of the content column,
        // starting to the right of the docked sidebar so the full-height sidebar
        // is never occluded (full-width in overlay, where the inset is 0).
        let nowplaying = self.nowplaying_rect();
        self.body_rect = body;

        // Body first so chrome draws on top (and chrome hits win).
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &body);
        let start_y = body.y0 - self.scroll;
        let end_y = self.draw_body(scene, t, tr, &c, body, start_y);
        scene.pop_layer();
        self.content_height = (end_y + self.scroll) - body.y0 + 24.0;
        let max = (self.content_height - body.height()).max(0.0);
        if self.scroll > max {
            self.scroll = max;
        }

        self.draw_header(scene, t, tr, &c, header);
        self.draw_nowplaying(scene, t, tr, &c, nowplaying);

        // Overlay flap: dim the content behind a scrim that fades with the
        // reveal, then float the sidebar above it. A click on the scrim
        // dismisses the flap (registered as a hit so the press handler closes
        // it without matching a content target).
        let reveal = self.flap.reveal();
        if layout == SplitLayout::Overlay && reveal > 0.001 {
            let scrim = c.scrim.multiply_alpha((reveal * 0.55) as f32);
            scene.fill(Fill::NonZero, t, scrim, None, &r);
            self.push_hit(r, Action::CloseSidebar);
        }

        // Sidebar (docked beside the content, or floating in overlay). Skip the
        // draw entirely when fully collapsed so its hits don't shadow content.
        let side = self.sidebar_rect();
        if side.width() > 0.5 {
            self.draw_sidebar(scene, t, tr, &c, side);
        }

        // Hamburger / close toggle over the content's top-left corner,
        // whenever the sidebar isn't fully docked-open.
        if self.show_hamburger() {
            self.draw_hamburger(scene, t, &c, header);
        }

        self.draw_toast(scene, t, tr, &c);

        // Inline name-input overlay (create / rename playlist).
        if self.name_prompt.is_some() {
            self.draw_name_prompt(scene, t, tr, &c);
        }

        // The context menu paints last so it sits on top of all content.
        if self.context.is_open() {
            // Dimming scrim that also dismisses on click (registered as a hit).
            self.context
                .draw(scene, t, tr, theme, &PresenceCtx::empty());
        }
    }

    /// The hamburger (or close-chevron) toggle that summons / dismisses the
    /// overlay sidebar. Sits in the header's top-left, left of the title.
    fn draw_hamburger(&mut self, scene: &mut Scene, t: Affine, c: &Palette, header: Rect) {
        let size = 34.0;
        let x0 = header.x0 + 14.0;
        let y0 = header.y0 + (HEADER_H - size) / 2.0;
        let btn = Rect::new(x0, y0, x0 + size, y0 + size);
        self.hamburger_rect = btn;
        let hover = btn.contains(self.mouse);
        fill_rrect(scene, t, btn, 10.0, if hover { c.white10 } else { c.white5 });
        let cx = btn.center();
        let open = self.flap.reveal() > 0.5;
        if open && self.sidebar_layout() == SplitLayout::Overlay {
            // Close affordance: a left chevron.
            icon::draw(scene, t, Icon::ChevronLeft, cx, 18.0, c.text);
        } else {
            // Three-line hamburger.
            let lw = 15.0;
            for i in 0..3 {
                let ly = cx.y - 5.0 + i as f64 * 5.0;
                fill_rrect(
                    scene,
                    t,
                    Rect::new(cx.x - lw / 2.0, ly - 1.0, cx.x + lw / 2.0, ly + 1.0),
                    1.0,
                    c.text,
                );
            }
        }
    }

    // ---- sidebar ----------------------------------------------------------

    fn draw_sidebar(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, c: &Palette, r: Rect) {
        scene.fill(Fill::NonZero, t, c.panel, None, &r);
        // Right divider.
        scene.fill(
            Fill::NonZero,
            t,
            c.border,
            None,
            &Rect::new(r.x1 - 1.0, r.y0, r.x1, r.y1),
        );

        // Brand.
        let bx = r.x0 + 24.0;
        icon::draw(scene, t, Icon::Disc, Point::new(bx + 10.0, r.y0 + 34.0), 24.0, c.accent);
        draw_text(tr, scene, t, "Music", bx + 30.0, r.y0 + 22.0, 20.0, c.text, TextWeight::Bold, None);

        // Docked-collapse affordance: a left-chevron button at the brand row's
        // right edge, shown only when the sidebar is docked + open (so there's
        // always a visible way to collapse it — the hamburger only reappears
        // once collapsed). NOTE for ui promotion: NavSplit/FlapState provide
        // the docked-collapse *state machine* but no collapse *button* or a
        // collapsed-rail mode; this affordance is added app-locally.
        if self.sidebar_layout() == SplitLayout::Docked && self.flap.reveal() > 0.5 {
            let cb = Rect::new(r.x1 - 44.0, r.y0 + 18.0, r.x1 - 16.0, r.y0 + 46.0);
            let hover = self.push_hit(cb, Action::ToggleSidebar);
            if hover {
                fill_rrect(scene, t, cb, 8.0, c.white10);
            }
            icon::draw(scene, t, Icon::ChevronLeft, cb.center(), 16.0, if hover { c.text } else { c.dim });
        }

        let mut y = r.y0 + 70.0;
        let nav = [
            (Icon::Home, "Home", View::Home),
            (Icon::Search, "Search", View::Search),
            (Icon::Songs, "Songs", View::Songs),
            (Icon::Albums, "Albums", View::Albums),
            (Icon::Artists, "Artists", View::Artists),
            (Icon::HeartFilled, "Liked Songs", View::Liked),
            (Icon::Download, "Downloads", View::Downloads),
            (Icon::Queue, "Queue", View::Queue),
        ];
        for (ic, label, view) in nav {
            let active = self.view == view;
            y = self.nav_row(scene, t, tr, c, r, y, ic, label, view, active);
        }

        // Playlists header.
        y += 14.0;
        draw_text(tr, scene, t, "PLAYLISTS", r.x0 + 24.0, y, 11.0, c.dim, TextWeight::Semibold, None);
        // New-playlist plus — opens the inline create prompt.
        let plus_rect = Rect::new(r.x1 - 40.0, y - 4.0, r.x1 - 16.0, y + 20.0);
        let plus_hover = self.push_hit(plus_rect, Action::BeginNewPlaylist);
        icon::draw(
            scene,
            t,
            Icon::Plus,
            plus_rect.center(),
            16.0,
            if plus_hover { c.text } else { c.dim },
        );
        y += 26.0;

        let playlists = self.gather_playlists();
        for p in playlists {
            let active = self.view == View::Playlist(p.id.clone());
            let row = Rect::new(r.x0 + 12.0, y, r.x1 - 12.0, y + 34.0);
            let hover = self.push_hit(row, Action::Nav(View::Playlist(p.id.clone())));
            if active || hover {
                fill_rrect(scene, t, row, 8.0, c.white5);
            }
            let col = if active { c.text } else { c.dim };
            let name = elide(tr, &p.name, 13.0, TextWeight::Medium, row.width() - 28.0);
            draw_text(tr, scene, t, &name, r.x0 + 24.0, vy(y, 34.0, 13.0), 13.0, col, TextWeight::Medium, None);
            y += 34.0;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn nav_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        side: Rect,
        y: f64,
        ic: Icon,
        label: &str,
        view: View,
        active: bool,
    ) -> f64 {
        let h = 42.0;
        let row = Rect::new(side.x0 + 12.0, y, side.x1 - 12.0, y + h);
        let hover = self.push_hit(row, Action::Nav(view));
        if active {
            fill_rrect(scene, t, row, 10.0, c.accent.multiply_alpha(0.16));
            // Active marker bar.
            fill_rrect(
                scene,
                t,
                Rect::new(side.x0, y + 9.0, side.x0 + 4.0, y + h - 9.0),
                2.0,
                c.accent,
            );
        } else if hover {
            fill_rrect(scene, t, row, 10.0, c.white5);
        }
        let fg = if active { c.accent } else if hover { c.text } else { c.dim };
        icon::draw(scene, t, ic, Point::new(row.x0 + 18.0, y + h / 2.0), 19.0, fg);
        draw_text(tr, scene, t, label, row.x0 + 38.0, vy(y, h, 14.0), 14.0, fg, TextWeight::Medium, None);
        y + h + 2.0
    }

    // ---- header -----------------------------------------------------------

    fn draw_header(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, c: &Palette, r: Rect) {
        scene.fill(Fill::NonZero, t, c.bg, None, &r);
        // Leave room for the hamburger toggle in the corner when it's shown.
        let lead = if self.show_hamburger() { 56.0 } else { 24.0 };
        let mut x = r.x0 + lead;

        if !self.nav_back.is_empty() {
            let bz = Rect::new(x, r.y0 + 20.0, x + 32.0, r.y0 + 52.0);
            let hover = self.push_hit(bz, Action::Back);
            fill_rrect(scene, t, bz, 16.0, if hover { c.white10 } else { c.white5 });
            icon::draw(scene, t, Icon::ChevronLeft, bz.center(), 18.0, c.text);
            x += 44.0;
        }

        let title = self.view_title();
        draw_text(tr, scene, t, &title, x, vy(r.y0, HEADER_H, 26.0), 26.0, c.text, TextWeight::Bold, None);

        // Search pill on the right.
        let pill_w = 320.0_f64.min(r.width() * 0.4);
        let pill = Rect::new(r.x1 - 24.0 - pill_w, r.y0 + 18.0, r.x1 - 24.0, r.y0 + 54.0);
        let focused = self.search_focused;
        let hover = self.push_hit(pill, Action::FocusSearch);
        fill_rrect(scene, t, pill, 18.0, c.panel);
        if focused {
            scene.stroke(&Stroke::new(1.5), t, c.accent, None, &rrect(pill, 18.0));
        } else if hover {
            scene.stroke(&Stroke::new(1.0), t, c.border, None, &rrect(pill, 18.0));
        }
        icon::draw(scene, t, Icon::Search, Point::new(pill.x0 + 20.0, pill.center().y), 16.0, c.dim);
        let q = &self.search_query;
        let (txt, col) = if q.is_empty() {
            ("Search songs, albums, artists".to_string(), c.dim)
        } else {
            (q.clone(), c.text)
        };
        let shown = elide(tr, &txt, 14.0, TextWeight::Regular, pill.width() - 60.0);
        draw_text(tr, scene, t, &shown, pill.x0 + 38.0, vy(pill.y0, 36.0, 14.0), 14.0, col, TextWeight::Regular, None);
        if focused {
            // Caret tracks the *typed query* width, never the placeholder — an
            // empty field puts the caret at the text start, not at the end of the
            // dim "Search songs, albums, artists" hint.
            let cw = tr.measure_weighted(&self.search_query, 14.0, None, TextWeight::Regular).0 as f64;
            let cx = (pill.x0 + 38.0 + cw).min(pill.x1 - 14.0);
            scene.fill(
                Fill::NonZero,
                t,
                c.accent,
                None,
                &Rect::new(cx, pill.y0 + 9.0, cx + 1.5, pill.y1 - 9.0),
            );
        }
    }

    fn view_title(&self) -> String {
        match &self.view {
            View::Home => "Home".into(),
            View::Songs => "Songs".into(),
            View::Albums => "Albums".into(),
            View::Artists => "Artists".into(),
            View::Playlists => "Playlists".into(),
            View::Downloads => "Downloads".into(),
            View::Liked => "Liked Songs".into(),
            View::Search => "Search".into(),
            View::Queue => "Queue".into(),
            View::Lyrics => "Lyrics".into(),
            View::Album(id) => self.lib.album_title(id).to_string(),
            View::Artist(id) => self.lib.artist(id).map(|a| a.name.clone()).unwrap_or_default(),
            View::Playlist(id) => self.lib.playlist(id).map(|p| p.name.clone()).unwrap_or_default(),
        }
    }

    // ---- body dispatch ----------------------------------------------------

    fn draw_body(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, c: &Palette, body: Rect, y: f64) -> f64 {
        match self.view.clone() {
            View::Home => self.draw_home(scene, t, tr, c, body, y),
            View::Songs => {
                let ids: Vec<String> = self.lib.tracks_sorted().iter().map(|tk| tk.id.clone()).collect();
                let rows = self.gather_tracks(&ids);
                self.draw_track_table(scene, t, tr, c, body, y, &rows, &ids, true, true)
            }
            View::Albums => {
                let albums = self.gather_albums(None);
                self.draw_album_grid(scene, t, tr, c, body, y, &albums)
            }
            View::Artists => {
                let artists = self.gather_artists();
                self.draw_artist_grid(scene, t, tr, c, body, y, &artists)
            }
            View::Playlists => {
                let pls = self.gather_playlists();
                self.draw_playlist_grid(scene, t, tr, c, body, y, &pls)
            }
            View::Downloads => self.draw_downloads(scene, t, tr, c, body, y),
            View::Liked => self.draw_liked(scene, t, tr, c, body, y),
            View::Queue => self.draw_queue(scene, t, tr, c, body, y),
            View::Lyrics => self.draw_lyrics(scene, t, tr, c, body, y),
            View::Search => self.draw_search(scene, t, tr, c, body, y),
            View::Album(id) => self.draw_album_detail(scene, t, tr, c, body, y, &id),
            View::Artist(id) => self.draw_artist_detail(scene, t, tr, c, body, y, &id),
            View::Playlist(id) => self.draw_playlist_detail(scene, t, tr, c, body, y, &id),
        }
    }

    fn draw_home(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, c: &Palette, body: Rect, mut y: f64) -> f64 {
        let pad = 28.0;
        y += 20.0;
        draw_text(tr, scene, t, "Good evening", body.x0 + pad, y, 28.0, c.text, TextWeight::Bold, None);
        y += 46.0;

        // Quick-pick tiles: a mix of playlists rendered as wide chips.
        let pls = self.gather_playlists();
        let chip_h = 64.0;
        let cols = 2usize;
        let gap = 16.0;
        let chip_w = (body.width() - 2.0 * pad - gap * (cols as f64 - 1.0)) / cols as f64;
        for (i, p) in pls.iter().enumerate() {
            let cx = body.x0 + pad + (i % cols) as f64 * (chip_w + gap);
            let cy = y + (i / cols) as f64 * (chip_h + gap);
            let chip = Rect::new(cx, cy, cx + chip_w, cy + chip_h);
            let hover = self.push_hit(chip, Action::Nav(View::Playlist(p.id.clone())));
            fill_rrect(scene, t, chip, 10.0, if hover { c.card_hover } else { c.card });
            art::draw_album_art(scene, t, Rect::new(cx, cy, cx + chip_h, cy + chip_h), &p.id, &p.name, tr, 10.0);
            let name = elide(tr, &p.name, 15.0, TextWeight::Semibold, chip_w - chip_h - 24.0);
            draw_text(tr, scene, t, &name, cx + chip_h + 14.0, vy(cy, chip_h, 15.0), 15.0, c.text, TextWeight::Semibold, None);
        }
        let rows_n = pls.len().div_ceil(cols);
        y += rows_n as f64 * (chip_h + gap) + 16.0;

        // Jump back in — albums.
        y = self.section_label(scene, t, tr, c, body, y, "Jump back in");
        let albums = self.gather_albums(Some(6));
        y = self.draw_album_grid(scene, t, tr, c, body, y, &albums);
        y += 10.0;

        // Top tracks.
        y = self.section_label(scene, t, tr, c, body, y, "Top tracks");
        let ids: Vec<String> = self.lib.top_tracks(6).iter().map(|tk| tk.id.clone()).collect();
        let rows = self.gather_tracks(&ids);
        y = self.draw_track_table(scene, t, tr, c, body, y, &rows, &ids, false, true);
        y
    }

    fn section_label(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, c: &Palette, body: Rect, y: f64, label: &str) -> f64 {
        draw_text(tr, scene, t, label, body.x0 + 28.0, y, 19.0, c.text, TextWeight::Bold, None);
        y + 36.0
    }

    // ---- album grid -------------------------------------------------------

    fn draw_album_grid(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, c: &Palette, body: Rect, y: f64, albums: &[AlbumDisp]) -> f64 {
        let pad = 28.0;
        let gap = 22.0;
        let target = 176.0;
        let avail = body.width() - 2.0 * pad;
        let cols = ((avail + gap) / (target + gap)).floor().max(1.0) as usize;
        let card_w = (avail - gap * (cols as f64 - 1.0)) / cols as f64;
        let card_h = card_w + 56.0;

        for (i, a) in albums.iter().enumerate() {
            let cx = body.x0 + pad + (i % cols) as f64 * (card_w + gap);
            let cy = y + (i / cols) as f64 * (card_h + gap);
            let card = Rect::new(cx, cy, cx + card_w, cy + card_h);
            let hover = self.push_hit(card, Action::Nav(View::Album(a.id.clone())));
            if hover {
                fill_rrect(scene, t, card.inflate(8.0, 8.0), 14.0, c.card_hover);
            }
            let art_rect = Rect::new(cx, cy, cx + card_w, cy + card_w);
            art::draw_album_art(scene, t, art_rect, &a.id, &a.title, tr, 12.0);
            let saved = self.lib.is_album_saved(&a.id);
            // Add-to-library quick toggle: always shown when saved (so the
            // saved state reads at a glance), and on hover otherwise.
            if hover || saved {
                let add_c = Point::new(art_rect.x0 + 26.0, art_rect.y0 + 26.0);
                let ah = self.push_hit(square_around(add_c, 34.0), Action::ToggleSaveAlbum(a.id.clone()));
                scene.fill(Fill::NonZero, t, c.scrim.multiply_alpha(0.55), None, &Circle::new(add_c, 16.0));
                icon::draw(
                    scene,
                    t,
                    if saved { Icon::Downloaded } else { Icon::Plus },
                    add_c,
                    if saved { 15.0 } else { 16.0 },
                    if saved { c.success } else if ah { c.text } else { c.text.multiply_alpha(0.85) },
                );
            }
            // Play FAB on hover.
            if hover {
                let fab = Circle::new(Point::new(art_rect.x1 - 26.0, art_rect.y1 - 26.0), 20.0);
                scene.fill(Fill::NonZero, t, c.accent, None, &fab);
                icon::draw(scene, t, Icon::Play, Point::new(fab.center.x + 1.0, fab.center.y), 16.0, c.bg);
                self.push_hit(
                    Rect::new(fab.center.x - 20.0, fab.center.y - 20.0, fab.center.x + 20.0, fab.center.y + 20.0),
                    Action::Play { queue: a.queue.clone(), index: 0 },
                );
            }
            let title = elide(tr, &a.title, 14.0, TextWeight::Semibold, card_w);
            draw_text(tr, scene, t, &title, cx, cy + card_w + 8.0, 14.0, c.text, TextWeight::Semibold, None);
            let sub = elide(tr, &format!("{} · {}", a.artist, a.year), 12.0, TextWeight::Regular, card_w);
            draw_text(tr, scene, t, &sub, cx, cy + card_w + 28.0, 12.0, c.dim, TextWeight::Regular, None);
        }
        let rows = albums.len().div_ceil(cols).max(1);
        y + rows as f64 * (card_h + gap)
    }

    // ---- artist grid ------------------------------------------------------

    fn draw_artist_grid(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, c: &Palette, body: Rect, y: f64, artists: &[ArtistDisp]) -> f64 {
        let pad = 28.0;
        let gap = 22.0;
        let target = 176.0;
        let avail = body.width() - 2.0 * pad;
        let cols = ((avail + gap) / (target + gap)).floor().max(1.0) as usize;
        let card_w = (avail - gap * (cols as f64 - 1.0)) / cols as f64;
        let card_h = card_w + 50.0;

        for (i, a) in artists.iter().enumerate() {
            let cx = body.x0 + pad + (i % cols) as f64 * (card_w + gap);
            let cy = y + (i / cols) as f64 * (card_h + gap);
            let card = Rect::new(cx, cy, cx + card_w, cy + card_h);
            let hover = self.push_hit(card, Action::Nav(View::Artist(a.id.clone())));
            if hover {
                fill_rrect(scene, t, card.inflate(8.0, 8.0), 14.0, c.card_hover);
            }
            // Circular avatar.
            let d = card_w;
            let center = Point::new(cx + d / 2.0, cy + d / 2.0);
            let (base, accent) = art::art_colors(&a.id);
            scene.fill(Fill::NonZero, t, base, None, &Circle::new(center, d / 2.0));
            scene.fill(Fill::NonZero, t, accent.multiply_alpha(0.8), None, &Circle::new(Point::new(center.x - d * 0.12, center.y - d * 0.12), d * 0.34));
            let initials = art::initials(&a.name);
            let sz = (d * 0.3) as f32;
            let (tw, th) = tr.measure_weighted(&initials, sz, None, TextWeight::Bold);
            draw_text(tr, scene, t, &initials, center.x - tw as f64 / 2.0, center.y - th as f64 / 2.0, sz, c.text.multiply_alpha(0.95), TextWeight::Bold, None);

            let name = elide(tr, &a.name, 14.0, TextWeight::Semibold, card_w);
            let (nw, _) = tr.measure_weighted(&name, 14.0, None, TextWeight::Semibold);
            draw_text(tr, scene, t, &name, cx + (card_w - nw as f64) / 2.0, cy + d + 8.0, 14.0, c.text, TextWeight::Semibold, None);
            let genre = elide(tr, &a.genre, 12.0, TextWeight::Regular, card_w);
            let (gw, _) = tr.measure_weighted(&genre, 12.0, None, TextWeight::Regular);
            draw_text(tr, scene, t, &genre, cx + (card_w - gw as f64) / 2.0, cy + d + 26.0, 12.0, c.dim, TextWeight::Regular, None);
            let _ = a.listeners;
        }
        let rows = artists.len().div_ceil(cols).max(1);
        y + rows as f64 * (card_h + gap)
    }

    // ---- playlist grid ----------------------------------------------------

    fn draw_playlist_grid(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, c: &Palette, body: Rect, mut y: f64, pls: &[PlaylistDisp]) -> f64 {
        let pad = 28.0;
        let gap = 22.0;
        let target = 176.0;
        let avail = body.width() - 2.0 * pad;
        let cols = ((avail + gap) / (target + gap)).floor().max(1.0) as usize;
        let card_w = (avail - gap * (cols as f64 - 1.0)) / cols as f64;
        let card_h = card_w + 56.0;

        // "Create playlist" pill above the grid.
        let create = Rect::new(body.x0 + pad, y, body.x0 + pad + 184.0, y + 40.0);
        let ch = self.push_hit(create, Action::BeginNewPlaylist);
        fill_rrect(scene, t, create, 20.0, if ch { c.accent_hover } else { c.accent });
        icon::draw(scene, t, Icon::Plus, Point::new(create.x0 + 24.0, create.center().y), 16.0, c.bg);
        draw_text(tr, scene, t, "New playlist", create.x0 + 44.0, vy(create.y0, 40.0, 14.0), 14.0, c.bg, TextWeight::Semibold, None);
        y += 56.0;

        for (i, p) in pls.iter().enumerate() {
            let cx = body.x0 + pad + (i % cols) as f64 * (card_w + gap);
            let cy = y + (i / cols) as f64 * (card_h + gap);
            let card = Rect::new(cx, cy, cx + card_w, cy + card_h);
            let hover = self.push_hit(card, Action::Nav(View::Playlist(p.id.clone())));
            if hover {
                fill_rrect(scene, t, card.inflate(8.0, 8.0), 14.0, c.card_hover);
            }
            let art_rect = Rect::new(cx, cy, cx + card_w, cy + card_w);
            art::draw_album_art(scene, t, art_rect, &p.id, &p.name, tr, 12.0);
            icon::draw(scene, t, Icon::Playlist, Point::new(art_rect.x0 + 26.0, art_rect.y0 + 26.0), 22.0, c.text.multiply_alpha(0.85));
            let title = elide(tr, &p.name, 14.0, TextWeight::Semibold, card_w);
            draw_text(tr, scene, t, &title, cx, cy + card_w + 8.0, 14.0, c.text, TextWeight::Semibold, None);
            draw_text(tr, scene, t, &format!("{} songs", p.count), cx, cy + card_w + 28.0, 12.0, c.dim, TextWeight::Regular, None);
        }
        let rows = pls.len().div_ceil(cols).max(1);
        y + rows as f64 * (card_h + gap)
    }

    // ---- track table ------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn draw_track_table(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        mut y: f64,
        rows: &[TrackDisp],
        queue: &[String],
        show_album: bool,
        number_col: bool,
    ) -> f64 {
        let pad = 28.0;
        let left = body.x0 + pad;
        let right = body.x1 - pad;
        let row_h = 52.0;
        let dur_x = right - 56.0;
        let dl_x = right - 96.0;
        let album_x = (right - 320.0).max(left + 360.0);

        for (i, tk) in rows.iter().enumerate() {
            let rr = Rect::new(left, y, right, y + row_h);
            let is_current = self.pb.current.as_deref() == Some(tk.id.as_str());
            let hovered = self.body_hit(rr, Action::Play { queue: queue.to_vec(), index: i });
            // Right-click anywhere on the row opens its context menu.
            if rr.y1 >= body.y0 && rr.y0 <= body.y1 {
                self.push_row_menu(
                    rr,
                    Action::OpenTrackMenu { track: tk.id.clone(), in_playlist: None, queue_index: None },
                );
            }
            if hovered {
                fill_rrect(scene, t, rr, 8.0, c.white5);
            }
            if rr.y1 >= body.y0 && rr.y0 <= body.y1 {
                // Index / play indicator.
                if number_col {
                    if hovered {
                        icon::draw(scene, t, Icon::Play, Point::new(left + 14.0, y + row_h / 2.0), 14.0, c.text);
                    } else if is_current {
                        // little equalizer bars
                        for b in 0..3 {
                            let bx = left + 6.0 + b as f64 * 6.0;
                            let bh = 6.0 + ((i + b) % 3) as f64 * 5.0;
                            scene.fill(Fill::NonZero, t, c.accent, None, &Rect::new(bx, y + row_h / 2.0 - bh / 2.0, bx + 3.0, y + row_h / 2.0 + bh / 2.0));
                        }
                    } else {
                        let num = format!("{}", tk.no.max(i as u32 + 1));
                        draw_text(tr, scene, t, &num, left + 8.0, vy(y, row_h, 13.0), 13.0, c.dim, TextWeight::Regular, None);
                    }
                }
                let text_x = if number_col { left + 38.0 } else { left + 8.0 };
                let title_col = if is_current { c.accent } else { c.text };
                let title = elide(tr, &tk.title, 14.0, TextWeight::Medium, album_x - text_x - 16.0);
                draw_text(tr, scene, t, &title, text_x, y + 9.0, 14.0, title_col, TextWeight::Medium, None);
                let artist = elide(tr, &tk.artist, 12.0, TextWeight::Regular, album_x - text_x - 16.0);
                draw_text(tr, scene, t, &artist, text_x, y + 28.0, 12.0, c.dim, TextWeight::Regular, None);

                if show_album && right - album_x > 80.0 {
                    let al = elide(tr, &tk.album, 13.0, TextWeight::Regular, dl_x - 48.0 - album_x - 16.0);
                    draw_text(tr, scene, t, &al, album_x, vy(y, row_h, 13.0), 13.0, c.dim, TextWeight::Regular, None);
                }

                // Like toggle (heart) just left of the download control.
                let like_x = dl_x - 34.0;
                let liked = self.lib.is_liked(&tk.id);
                let like_rect = Rect::new(like_x - 14.0, y + row_h / 2.0 - 14.0, like_x + 14.0, y + row_h / 2.0 + 14.0);
                let like_hover = self.push_hit(like_rect, Action::ToggleLike(tk.id.clone()));
                if liked || like_hover || hovered {
                    icon::draw(
                        scene,
                        t,
                        if liked { Icon::HeartFilled } else { Icon::Heart },
                        Point::new(like_x, y + row_h / 2.0),
                        15.0,
                        if liked { c.accent } else if like_hover { c.text } else { c.dim },
                    );
                }

                // Download toggle.
                let dl_rect = Rect::new(dl_x - 12.0, y + row_h / 2.0 - 14.0, dl_x + 16.0, y + row_h / 2.0 + 14.0);
                let dl_hover = self.push_hit(dl_rect, Action::ToggleDownload(tk.id.clone()));
                match tk.cache {
                    CacheState::Cached => {
                        let badge = Circle::new(Point::new(dl_x, y + row_h / 2.0), 9.0);
                        scene.fill(Fill::NonZero, t, c.success, None, &badge);
                        icon::draw(scene, t, Icon::Downloaded, Point::new(dl_x, y + row_h / 2.0), 12.0, c.bg);
                    }
                    CacheState::Downloading(f) => {
                        let ring = Circle::new(Point::new(dl_x, y + row_h / 2.0), 9.0);
                        scene.stroke(&Stroke::new(2.0), t, c.dim, None, &ring);
                        let arc = vello::kurbo::Arc::new(Point::new(dl_x, y + row_h / 2.0), (9.0, 9.0), -std::f64::consts::FRAC_PI_2, std::f64::consts::TAU * f as f64, 0.0);
                        scene.stroke(&Stroke::new(2.0), t, c.accent, None, &arc.to_path(0.2));
                    }
                    CacheState::Streamed => {
                        icon::draw(scene, t, Icon::Download, Point::new(dl_x, y + row_h / 2.0), 16.0, if dl_hover { c.text } else { c.dim });
                    }
                }

                // Duration.
                let dur = fmt_duration(tk.dur as u64);
                let (dw, _) = tr.measure_weighted(&dur, 13.0, None, TextWeight::Regular);
                draw_text(tr, scene, t, &dur, dur_x + (48.0 - dw as f64), vy(y, row_h, 13.0), 13.0, c.dim, TextWeight::Regular, None);
            }
            y += row_h;
        }
        y
    }

    /// Track rows for a playlist detail: like the standard table but each row
    /// carries reorder (up/down) + remove affordances bound to the playlist,
    /// and a right-click opens the track context menu in this playlist's
    /// context.
    #[allow(clippy::too_many_arguments)]
    fn draw_playlist_tracks(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        mut y: f64,
        rows: &[TrackDisp],
        ids: &[String],
        playlist_id: &str,
    ) -> f64 {
        let pad = 28.0;
        let left = body.x0 + pad;
        let right = body.x1 - pad;
        let row_h = 52.0;
        let n = rows.len();
        // Right-side control cluster: up, down, remove, then duration.
        let dur_x = right - 48.0;
        let rm_x = dur_x - 34.0;
        let dn_x = rm_x - 30.0;
        let up_x = dn_x - 30.0;

        if n == 0 {
            draw_text(tr, scene, t, "This playlist is empty. Right-click a song \u{2192} Add to playlist\u{2026}", left, y + 6.0, 13.0, c.dim, TextWeight::Regular, None);
            return y + 40.0;
        }

        for (i, tk) in rows.iter().enumerate() {
            let rr = Rect::new(left, y, right, y + row_h);
            let is_current = self.pb.current.as_deref() == Some(tk.id.as_str());
            let hovered = self.body_hit(rr, Action::Play { queue: ids.to_vec(), index: i });
            // Right-click → context menu in this playlist.
            self.push_row_menu(
                rr,
                Action::OpenTrackMenu { track: tk.id.clone(), in_playlist: Some(playlist_id.to_string()), queue_index: None },
            );
            if hovered {
                fill_rrect(scene, t, rr, 8.0, c.white5);
            }
            if rr.y1 >= body.y0 && rr.y0 <= body.y1 {
                let num = format!("{}", i + 1);
                draw_text(tr, scene, t, &num, left + 8.0, vy(y, row_h, 13.0), 13.0, c.dim, TextWeight::Regular, None);
                let title_col = if is_current { c.accent } else { c.text };
                let title = elide(tr, &tk.title, 14.0, TextWeight::Medium, up_x - left - 60.0);
                draw_text(tr, scene, t, &title, left + 36.0, y + 9.0, 14.0, title_col, TextWeight::Medium, None);
                let artist = elide(tr, &tk.artist, 12.0, TextWeight::Regular, up_x - left - 60.0);
                draw_text(tr, scene, t, &artist, left + 36.0, y + 28.0, 12.0, c.dim, TextWeight::Regular, None);

                if hovered {
                    // Move up (disabled visually on the first row).
                    if i > 0 {
                        let uc = Point::new(up_x, y + row_h / 2.0);
                        let uh = self.push_hit(square_around(uc, 26.0), Action::MovePlaylistItem { playlist: playlist_id.to_string(), from: i, to: i - 1 });
                        draw_chevron(scene, t, uc, true, if uh { c.text } else { c.dim });
                    }
                    if i + 1 < n {
                        let dc = Point::new(dn_x, y + row_h / 2.0);
                        let dh = self.push_hit(square_around(dc, 26.0), Action::MovePlaylistItem { playlist: playlist_id.to_string(), from: i, to: i + 1 });
                        draw_chevron(scene, t, dc, false, if dh { c.text } else { c.dim });
                    }
                    let rc = Point::new(rm_x, y + row_h / 2.0);
                    let rh = self.push_hit(square_around(rc, 26.0), Action::RemoveFromPlaylist { playlist: playlist_id.to_string(), track: tk.id.clone() });
                    draw_x(scene, t, rc, if rh { c.accent } else { c.dim });
                }

                let dur = fmt_duration(tk.dur as u64);
                let (dw, _) = tr.measure_weighted(&dur, 13.0, None, TextWeight::Regular);
                draw_text(tr, scene, t, &dur, dur_x + (40.0 - dw as f64), vy(y, row_h, 13.0), 13.0, c.dim, TextWeight::Regular, None);
            }
            y += row_h;
        }
        y
    }

    // ---- detail pages -----------------------------------------------------

    fn draw_detail_header(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &Palette,
        body: Rect,
        y: f64,
        seed: &str,
        kind: &str,
        title: &str,
        subtitle: &str,
        queue: Vec<String>,
        circular: bool,
        lib_button: Option<(String, bool)>,
    ) -> f64 {
        let pad = 28.0;
        let art_d = 168.0;
        let ax = body.x0 + pad;
        let ay = y + 12.0;
        if circular {
            let center = Point::new(ax + art_d / 2.0, ay + art_d / 2.0);
            let (base, accent) = art::art_colors(seed);
            scene.fill(Fill::NonZero, t, base, None, &Circle::new(center, art_d / 2.0));
            scene.fill(Fill::NonZero, t, accent.multiply_alpha(0.8), None, &Circle::new(Point::new(center.x - 20.0, center.y - 20.0), art_d * 0.32));
            let initials = art::initials(title);
            let sz = (art_d * 0.3) as f32;
            let (tw, th) = tr.measure_weighted(&initials, sz, None, TextWeight::Bold);
            draw_text(tr, scene, t, &initials, center.x - tw as f64 / 2.0, center.y - th as f64 / 2.0, sz, c.text, TextWeight::Bold, None);
        } else {
            art::draw_album_art(scene, t, Rect::new(ax, ay, ax + art_d, ay + art_d), seed, title, tr, 14.0);
        }

        let tx = ax + art_d + 28.0;
        draw_text(tr, scene, t, kind, tx, ay + 8.0, 12.0, c.dim, TextWeight::Semibold, None);
        let title_s = elide(tr, title, 38.0, TextWeight::Bold, body.x1 - tx - pad);
        draw_text(tr, scene, t, &title_s, tx, ay + 28.0, 38.0, c.text, TextWeight::Bold, None);
        draw_text(tr, scene, t, subtitle, tx, ay + 84.0, 14.0, c.dim, TextWeight::Regular, None);

        // Action buttons.
        let by = ay + 116.0;
        let play = Rect::new(tx, by, tx + 120.0, by + 40.0);
        let ph = self.push_hit(play, Action::Play { queue: queue.clone(), index: 0 });
        fill_rrect(scene, t, play, 20.0, if ph { c.accent_hover } else { c.accent });
        icon::draw(scene, t, Icon::Play, Point::new(play.x0 + 24.0, play.center().y), 15.0, c.bg);
        draw_text(tr, scene, t, "Play", play.x0 + 44.0, vy(by, 40.0, 14.0), 14.0, c.bg, TextWeight::Semibold, None);

        let shuf = Rect::new(play.x1 + 12.0, by, play.x1 + 12.0 + 44.0, by + 40.0);
        let sh = self.push_hit(shuf, Action::ToggleShuffle);
        scene.stroke(&Stroke::new(1.0), t, c.border, None, &rrect(shuf, 20.0));
        icon::draw(scene, t, Icon::Shuffle, shuf.center(), 18.0, if self.pb.shuffle { c.accent } else if sh { c.text } else { c.dim });

        // Optional trailing "Add to library" pill (album header). `saved`
        // flips it to an "In Library" confirmed state.
        if let Some((id, saved)) = lib_button {
            let pw = 150.0;
            let lib = Rect::new(shuf.x1 + 12.0, by, shuf.x1 + 12.0 + pw, by + 40.0);
            let lh = self.push_hit(lib, Action::ToggleSaveAlbum(id));
            if saved {
                fill_rrect(scene, t, lib, 20.0, c.success.multiply_alpha(0.18));
                icon::draw(scene, t, Icon::Downloaded, Point::new(lib.x0 + 22.0, lib.center().y), 16.0, c.success);
                draw_text(tr, scene, t, "In Library", lib.x0 + 42.0, vy(by, 40.0, 14.0), 14.0, c.success, TextWeight::Semibold, None);
            } else {
                scene.stroke(&Stroke::new(1.0), t, if lh { c.text } else { c.border }, None, &rrect(lib, 20.0));
                icon::draw(scene, t, Icon::Plus, Point::new(lib.x0 + 22.0, lib.center().y), 16.0, if lh { c.text } else { c.dim });
                draw_text(tr, scene, t, "Add to Library", lib.x0 + 42.0, vy(by, 40.0, 14.0), 14.0, if lh { c.text } else { c.dim }, TextWeight::Semibold, None);
            }
        }

        y + art_d + 36.0
    }

    fn draw_album_detail(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, c: &Palette, body: Rect, y: f64, id: &str) -> f64 {
        let (title, artist, year, ids) = {
            let al = self.lib.album(id);
            let title = al.map(|a| a.title.clone()).unwrap_or_default();
            let artist = al.map(|a| self.lib.artist_name(&a.artist_id).to_string()).unwrap_or_default();
            let year = al.map(|a| a.year).unwrap_or(0);
            let ids: Vec<String> = self.lib.album_tracks(id).iter().map(|tk| tk.id.clone()).collect();
            (title, artist, year, ids)
        };
        let subtitle = format!("{artist} · {year} · {} songs", ids.len());
        let saved = self.lib.is_album_saved(id);
        let y = self.draw_detail_header(scene, t, tr, c, body, y, id, "ALBUM", &title, &subtitle, ids.clone(), false, Some((id.to_string(), saved)));
        let rows = self.gather_tracks(&ids);
        self.draw_track_table(scene, t, tr, c, body, y, &rows, &ids, false, true)
    }

    fn draw_artist_detail(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, c: &Palette, body: Rect, y: f64, id: &str) -> f64 {
        let (name, genre, listeners, top_ids, albums) = {
            let ar = self.lib.artist(id);
            let name = ar.map(|a| a.name.clone()).unwrap_or_default();
            let genre = ar.map(|a| a.genre.clone()).unwrap_or_default();
            let listeners = ar.map(|a| a.monthly_listeners).unwrap_or(0);
            let mut top: Vec<&crate::model::Track> = self.lib.tracks.values().filter(|tk| tk.artist_id == id).collect();
            top.sort_by(|a, b| b.plays.cmp(&a.plays));
            let top_ids: Vec<String> = top.iter().take(5).map(|tk| tk.id.clone()).collect();
            let albums = self.gather_albums_for_artist(id);
            (name, genre, listeners, top_ids, albums)
        };
        let subtitle = format!("{genre} · {} monthly listeners", human_count(listeners));
        let y = self.draw_detail_header(scene, t, tr, c, body, y, id, "ARTIST", &name, &subtitle, top_ids.clone(), true, None);

        let mut y = self.section_label(scene, t, tr, c, body, y + 8.0, "Popular");
        let rows = self.gather_tracks(&top_ids);
        y = self.draw_track_table(scene, t, tr, c, body, y, &rows, &top_ids, false, true);
        y += 12.0;
        y = self.section_label(scene, t, tr, c, body, y, "Albums");
        self.draw_album_grid(scene, t, tr, c, body, y, &albums)
    }

    fn draw_playlist_detail(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, c: &Palette, body: Rect, y: f64, id: &str) -> f64 {
        let (name, desc, ids) = {
            let p = self.lib.playlist(id);
            let name = p.map(|p| p.name.clone()).unwrap_or_default();
            let desc = p.map(|p| p.description.clone()).unwrap_or_default();
            let ids = p.map(|p| p.track_ids.clone()).unwrap_or_default();
            (name, desc, ids)
        };
        let total: u32 = self.gather_tracks(&ids).iter().map(|r| r.dur).sum();
        let subtitle = format!("{} · {} songs · {}", desc, ids.len(), fmt_duration(total as u64));
        let mut y = self.draw_detail_header(scene, t, tr, c, body, y, id, "PLAYLIST", &name, &subtitle, ids.clone(), false, None);

        // Playlist management row: rename + delete (below the play/shuffle row).
        let pad = 28.0;
        let mx = body.x0 + pad;
        let ren = Rect::new(mx, y - 8.0, mx + 96.0, y + 24.0);
        let rh = self.push_hit(ren, Action::BeginRenamePlaylist(id.to_string()));
        scene.stroke(&Stroke::new(1.0), t, if rh { c.text } else { c.border }, None, &rrect(ren, 16.0));
        draw_text(tr, scene, t, "Rename", ren.x0 + 18.0, vy(ren.y0, 32.0, 13.0), 13.0, if rh { c.text } else { c.dim }, TextWeight::Medium, None);
        let del = Rect::new(ren.x1 + 10.0, y - 8.0, ren.x1 + 10.0 + 90.0, y + 24.0);
        let dh = self.push_hit(del, Action::DeletePlaylist(id.to_string()));
        scene.stroke(&Stroke::new(1.0), t, if dh { c.accent } else { c.border }, None, &rrect(del, 16.0));
        draw_text(tr, scene, t, "Delete", del.x0 + 18.0, vy(del.y0, 32.0, 13.0), 13.0, if dh { c.accent } else { c.dim }, TextWeight::Medium, None);
        y += 36.0;

        let rows = self.gather_tracks(&ids);
        // Playlist rows carry remove + reorder affordances bound to this id.
        self.draw_playlist_tracks(scene, t, tr, c, body, y, &rows, &ids, id)
    }

    // ---- queue ------------------------------------------------------------

    fn draw_queue(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, c: &Palette, body: Rect, mut y: f64) -> f64 {
        let pad = 28.0;
        y += 20.0;
        if let Some(cur) = self.pb.current.clone() {
            draw_text(tr, scene, t, "Now playing", body.x0 + pad, y, 15.0, c.dim, TextWeight::Semibold, None);
            y += 28.0;
            let row = self.gather_tracks(&[cur.clone()]);
            if let Some(tk) = row.first() {
                let rr = Rect::new(body.x0 + pad, y, body.x1 - pad, y + 64.0);
                fill_rrect(scene, t, rr, 10.0, c.accent.multiply_alpha(0.12));
                art::draw_album_art(scene, t, Rect::new(rr.x0 + 8.0, rr.y0 + 8.0, rr.x0 + 56.0, rr.y0 + 56.0), &tk.album_seed(), &tk.title, tr, 8.0);
                draw_text(tr, scene, t, &tk.title, rr.x0 + 68.0, rr.y0 + 14.0, 15.0, c.accent, TextWeight::Semibold, None);
                draw_text(tr, scene, t, &tk.artist, rr.x0 + 68.0, rr.y0 + 36.0, 13.0, c.dim, TextWeight::Regular, None);
                y += 80.0;
            }
        }
        // Up next.
        let upnext: Vec<String> = self.pb.queue.iter().skip(self.pb.queue_index + 1).cloned().collect();
        draw_text(tr, scene, t, &format!("Next up · {} tracks", upnext.len()), body.x0 + pad, y, 15.0, c.dim, TextWeight::Semibold, None);
        y += 28.0;
        let rows = self.gather_tracks(&upnext);
        // Build a queue context: index offset by current+1.
        let full = self.pb.queue.clone();
        let offset = self.pb.queue_index + 1;
        let y2 = self.draw_track_rows_with_offset(scene, t, tr, c, body, y, &rows, &full, offset);
        if upnext.is_empty() {
            draw_text(tr, scene, t, "Queue is empty — play an album or playlist.", body.x0 + pad, y + 6.0, 13.0, c.dim, TextWeight::Regular, None);
            return y + 40.0;
        }
        y2
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_track_rows_with_offset(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, c: &Palette, body: Rect, mut y: f64, rows: &[TrackDisp], full_queue: &[String], offset: usize) -> f64 {
        let pad = 28.0;
        let left = body.x0 + pad;
        let right = body.x1 - pad;
        let row_h = 52.0;
        let qlen = full_queue.len();
        let dur_x = right - 48.0;
        let rm_x = dur_x - 34.0;
        let dn_x = rm_x - 30.0;
        let up_x = dn_x - 30.0;
        for (i, tk) in rows.iter().enumerate() {
            let abs = offset + i; // absolute queue index
            let rr = Rect::new(left, y, right, y + row_h);
            let hovered = self.body_hit(rr, Action::Play { queue: full_queue.to_vec(), index: abs });
            if rr.y1 >= body.y0 && rr.y0 <= body.y1 {
                self.push_row_menu(
                    rr,
                    Action::OpenTrackMenu { track: tk.id.clone(), in_playlist: None, queue_index: Some(abs) },
                );
            }
            if hovered {
                fill_rrect(scene, t, rr, 8.0, c.white5);
            }
            if rr.y1 >= body.y0 && rr.y0 <= body.y1 {
                draw_text(tr, scene, t, &tk.title, left + 8.0, y + 9.0, 14.0, c.text, TextWeight::Medium, None);
                draw_text(tr, scene, t, &tk.artist, left + 8.0, y + 28.0, 12.0, c.dim, TextWeight::Regular, None);
                // Reorder + remove on hover.
                if hovered {
                    if abs > offset {
                        let uc = Point::new(up_x, y + row_h / 2.0);
                        let uh = self.push_hit(square_around(uc, 26.0), Action::MoveQueueItem { from: abs, to: abs - 1 });
                        draw_chevron(scene, t, uc, true, if uh { c.text } else { c.dim });
                    }
                    if abs + 1 < qlen {
                        let dc = Point::new(dn_x, y + row_h / 2.0);
                        let dh = self.push_hit(square_around(dc, 26.0), Action::MoveQueueItem { from: abs, to: abs + 1 });
                        draw_chevron(scene, t, dc, false, if dh { c.text } else { c.dim });
                    }
                    let rc = Point::new(rm_x, y + row_h / 2.0);
                    let rh = self.push_hit(square_around(rc, 26.0), Action::RemoveFromQueue(abs));
                    draw_x(scene, t, rc, if rh { c.accent } else { c.dim });
                }
                let dur = fmt_duration(tk.dur as u64);
                let (dw, _) = tr.measure_weighted(&dur, 13.0, None, TextWeight::Regular);
                draw_text(tr, scene, t, &dur, dur_x + (40.0 - dw as f64), vy(y, row_h, 13.0), 13.0, c.dim, TextWeight::Regular, None);
            }
            y += row_h;
        }
        y
    }

    // ---- downloads --------------------------------------------------------

    fn draw_downloads(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, c: &Palette, body: Rect, mut y: f64) -> f64 {
        let pad = 28.0;
        y += 16.0;
        let cached = self.lib.downloaded_tracks();
        let count = cached.len();
        let total_s: u64 = cached.iter().map(|tk| tk.duration_s as u64).sum();
        // Approx size at 256 kbps.
        let mb = total_s as f64 * 256.0 / 8.0 / 1024.0;
        let ids: Vec<String> = cached.iter().map(|tk| tk.id.clone()).collect();

        // Stat banner.
        let banner = Rect::new(body.x0 + pad, y, body.x1 - pad, y + 64.0);
        fill_rrect(scene, t, banner, 12.0, c.card);
        icon::draw(scene, t, Icon::Downloaded, Point::new(banner.x0 + 34.0, banner.center().y), 24.0, c.success);
        draw_text(tr, scene, t, &format!("{count} songs available offline"), banner.x0 + 60.0, banner.y0 + 14.0, 15.0, c.text, TextWeight::Semibold, None);
        draw_text(tr, scene, t, &format!("{} · ~{:.0} MB cached at /var/cache/qos-fs", fmt_duration(total_s), mb), banner.x0 + 60.0, banner.y0 + 36.0, 12.0, c.dim, TextWeight::Regular, None);
        y += 84.0;

        if ids.is_empty() {
            draw_text(tr, scene, t, "No downloads yet. Tap the download icon on any track.", body.x0 + pad, y, 13.0, c.dim, TextWeight::Regular, None);
            return y + 40.0;
        }
        let rows = self.gather_tracks(&ids);
        self.draw_track_table(scene, t, tr, c, body, y, &rows, &ids, true, false)
    }

    // ---- liked songs ------------------------------------------------------

    fn draw_liked(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, c: &Palette, body: Rect, mut y: f64) -> f64 {
        let pad = 28.0;
        let ids = self.lib.liked_ids();
        let total_s: u64 = ids.iter().filter_map(|id| self.lib.track(id)).map(|tk| tk.duration_s as u64).sum();

        // Banner with a heart sigil.
        let banner = Rect::new(body.x0 + pad, y + 16.0, body.x1 - pad, y + 16.0 + 88.0);
        // A gradient-ish two-tone block (reuse album-art palette of the heart).
        let (base, accent) = art::art_colors("liked-songs");
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &rrect(banner, 14.0));
        scene.fill(Fill::NonZero, t, base, None, &banner);
        scene.fill(Fill::NonZero, t, accent.multiply_alpha(0.7), None, &Circle::new(Point::new(banner.x1 - 40.0, banner.y1), 80.0));
        scene.pop_layer();
        icon::draw(scene, t, Icon::HeartFilled, Point::new(banner.x0 + 50.0, banner.center().y), 44.0, Color::WHITE);
        draw_text(tr, scene, t, "Liked Songs", banner.x0 + 92.0, banner.y0 + 22.0, 24.0, Color::WHITE, TextWeight::Bold, None);
        draw_text(tr, scene, t, &format!("{} songs · {}", ids.len(), fmt_duration(total_s)), banner.x0 + 92.0, banner.y0 + 54.0, 14.0, Color::WHITE.multiply_alpha(0.85), TextWeight::Regular, None);
        // Play-all FAB.
        if !ids.is_empty() {
            let fab = Circle::new(Point::new(banner.x1 - 44.0, banner.center().y), 22.0);
            let ph = self.push_hit(square_around(fab.center, 50.0), Action::Play { queue: ids.clone(), index: 0 });
            scene.fill(Fill::NonZero, t, if ph { c.accent_hover } else { c.accent }, None, &fab);
            icon::draw(scene, t, Icon::Play, Point::new(fab.center.x + 1.0, fab.center.y), 18.0, c.bg);
        }
        y += 16.0 + 88.0 + 16.0;

        if ids.is_empty() {
            draw_text(tr, scene, t, "No liked songs yet. Tap the heart on any track.", body.x0 + pad, y, 13.0, c.dim, TextWeight::Regular, None);
            return y + 40.0;
        }
        let rows = self.gather_tracks(&ids);
        self.draw_track_table(scene, t, tr, c, body, y, &rows, &ids, true, true)
    }

    // ---- lyrics -----------------------------------------------------------

    fn draw_lyrics(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, c: &Palette, body: Rect, mut y: f64) -> f64 {
        let pad = 28.0;
        // Resolve lyrics for the current track (cached by id).
        self.ensure_lyrics();
        y += 16.0;

        let cur = self.pb.current.clone().and_then(|id| self.gather_tracks(&[id]).into_iter().next());
        if let Some(tk) = &cur {
            draw_text(tr, scene, t, &tk.title, body.x0 + pad, y, 22.0, c.text, TextWeight::Bold, None);
            draw_text(tr, scene, t, &tk.artist, body.x0 + pad, y + 30.0, 14.0, c.dim, TextWeight::Regular, None);
            y += 64.0;
        } else {
            draw_text(tr, scene, t, "Play a track to see its lyrics.", body.x0 + pad, y, 14.0, c.dim, TextWeight::Regular, None);
            return y + 40.0;
        }

        if self.lyrics.is_empty() {
            draw_text(tr, scene, t, "No lyrics found for this track.", body.x0 + pad, y, 14.0, c.dim, TextWeight::Regular, None);
            let note = if cfg!(feature = "lyrics") {
                "Fetching from lrclib\u{2026}"
            } else {
                "Live lyrics fetch is gated behind the `lyrics` feature; bundled samples shown when available."
            };
            draw_text(tr, scene, t, note, body.x0 + pad, y + 24.0, 12.0, c.dim, TextWeight::Regular, None);
            return y + 60.0;
        }

        // Highlight the current synced line against the playhead.
        let pos = self.playhead_ms as u64;
        let current = if self.lyrics_synced {
            lyrics::Lyrics { lines: self.lyrics.clone(), synced: true }.current_line(pos)
        } else {
            None
        };

        let line_h = 30.0;
        for (i, line) in self.lyrics.iter().enumerate() {
            let active = Some(i) == current;
            let col = if active { c.accent } else if self.lyrics_synced { c.dim } else { c.text };
            let weight = if active { TextWeight::Bold } else { TextWeight::Medium };
            let size = if active { 17.0 } else { 15.0 };
            let text = if line.text.is_empty() { " " } else { line.text.as_str() };
            draw_text(tr, scene, t, text, body.x0 + pad, y, size, col, weight, Some((body.width() - 2.0 * pad) as f32));
            y += line_h;
        }
        let src = if self.lyrics_synced { "Synced lyrics" } else { "Lyrics" };
        let gated = if cfg!(feature = "lyrics") { "live fetch via lrclib" } else { "bundled sample (live fetch gated behind `lyrics`)" };
        draw_text(tr, scene, t, &format!("{src} · {gated}"), body.x0 + pad, y + 8.0, 11.0, c.dim, TextWeight::Regular, None);
        y + 28.0
    }

    // ---- search -----------------------------------------------------------

    fn draw_search(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, c: &Palette, body: Rect, mut y: f64) -> f64 {
        let pad = 28.0;
        let q = self.search_query.clone();
        if q.trim().is_empty() {
            y += 16.0;
            draw_text(tr, scene, t, "Browse all", body.x0 + pad, y, 19.0, c.text, TextWeight::Bold, None);
            y += 36.0;
            // Genre tiles derived from artists' genres.
            let mut genres: Vec<String> = self.lib.artists.values().map(|a| a.genre.clone()).collect();
            genres.sort();
            genres.dedup();
            let gap = 16.0;
            let cols = 3usize;
            let tile_w = (body.width() - 2.0 * pad - gap * (cols as f64 - 1.0)) / cols as f64;
            let tile_h = 96.0;
            for (i, g) in genres.iter().enumerate() {
                let cx = body.x0 + pad + (i % cols) as f64 * (tile_w + gap);
                let cy = y + (i / cols) as f64 * (tile_h + gap);
                let tile = Rect::new(cx, cy, cx + tile_w, cy + tile_h);
                let (base, accent) = art::art_colors(g);
                scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &rrect(tile, 12.0));
                scene.fill(Fill::NonZero, t, base, None, &tile);
                scene.fill(Fill::NonZero, t, accent.multiply_alpha(0.7), None, &Circle::new(Point::new(tile.x1 - 16.0, tile.y1 - 6.0), 42.0));
                scene.pop_layer();
                draw_text(tr, scene, t, g, cx + 16.0, cy + 14.0, 16.0, Color::WHITE, TextWeight::Bold, Some((tile_w - 24.0) as f32));
            }
            let rows = genres.len().div_ceil(cols).max(1);
            return y + rows as f64 * (tile_h + gap);
        }

        let res = self.lib.search(&q);
        if res.is_empty() {
            y += 24.0;
            draw_text(tr, scene, t, &format!("No results for \u{201c}{}\u{201d}", q), body.x0 + pad, y, 15.0, c.dim, TextWeight::Regular, None);
            return y + 40.0;
        }

        // Artists row.
        if !res.artists.is_empty() {
            y = self.section_label(scene, t, tr, c, body, y + 8.0, "Artists");
            let artists: Vec<ArtistDisp> = res.artists.iter().filter_map(|id| self.lib.artist(id)).map(|a| ArtistDisp { id: a.id.clone(), name: a.name.clone(), genre: a.genre.clone(), listeners: a.monthly_listeners }).collect();
            y = self.draw_artist_grid(scene, t, tr, c, body, y, &artists);
            y += 8.0;
        }
        // Albums row.
        if !res.albums.is_empty() {
            y = self.section_label(scene, t, tr, c, body, y, "Albums");
            let albums: Vec<AlbumDisp> = res.albums.iter().filter_map(|id| self.lib.album(id)).map(|a| AlbumDisp {
                id: a.id.clone(),
                title: a.title.clone(),
                artist: self.lib.artist_name(&a.artist_id).to_string(),
                year: a.year,
                queue: self.lib.album_tracks(&a.id).iter().map(|tk| tk.id.clone()).collect(),
            }).collect();
            y = self.draw_album_grid(scene, t, tr, c, body, y, &albums);
            y += 8.0;
        }
        // Songs.
        if !res.tracks.is_empty() {
            y = self.section_label(scene, t, tr, c, body, y, "Songs");
            let ids = res.tracks.clone();
            let rows = self.gather_tracks(&ids);
            y = self.draw_track_table(scene, t, tr, c, body, y, &rows, &ids, true, true);
        }
        y
    }

    // ---- now playing bar --------------------------------------------------

    fn draw_nowplaying(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, c: &Palette, r: Rect) {
        scene.fill(Fill::NonZero, t, c.panel, None, &r);
        scene.fill(Fill::NonZero, t, c.border, None, &Rect::new(r.x0, r.y0, r.x1, r.y0 + 1.0));

        let cur = self.pb.current.clone().and_then(|id| self.gather_tracks(&[id]).into_iter().next());

        // Left: art + title/artist + like + lyrics affordance.
        let lx = r.x0 + 16.0;
        if let Some(tk) = &cur {
            let art = Rect::new(lx, r.y0 + 14.0, lx + 64.0, r.y0 + 78.0);
            art::draw_album_art(scene, t, art, &tk.album_seed(), &tk.title, tr, 8.0);
            // Clicking the art jumps to its album.
            self.push_hit(art, Action::Nav(View::Album(tk.album_id.clone())));
            let tx = art.x1 + 14.0;
            // Reserve room on the right of the title block for the heart +
            // lyrics buttons so they never overlap the title text.
            let heart_c = Point::new(r.x0 + 300.0 - 48.0, r.y0 + 30.0);
            let lyr_c = Point::new(r.x0 + 300.0 - 22.0, r.y0 + 30.0);
            let maxw = (heart_c.x - tx - 28.0).max(40.0);
            let title = elide(tr, &tk.title, 14.0, TextWeight::Semibold, maxw);
            draw_text(tr, scene, t, &title, tx, r.y0 + 26.0, 14.0, c.text, TextWeight::Semibold, None);
            let artist = elide(tr, &tk.artist, 12.0, TextWeight::Regular, maxw);
            draw_text(tr, scene, t, &artist, tx, r.y0 + 48.0, 12.0, c.dim, TextWeight::Regular, None);

            // Heart / like toggle next to the song name.
            let liked = self.lib.is_liked(&tk.id);
            let hh = self.push_hit(square_around(heart_c, 28.0), Action::ToggleLike(tk.id.clone()));
            icon::draw(
                scene,
                t,
                if liked { Icon::HeartFilled } else { Icon::Heart },
                heart_c,
                18.0,
                if liked { c.accent } else if hh { c.text } else { c.dim },
            );
            // Lyrics shortcut.
            let lh = self.push_hit(square_around(lyr_c, 28.0), Action::Nav(View::Lyrics));
            icon::draw(
                scene,
                t,
                Icon::Note,
                lyr_c,
                18.0,
                if self.view == View::Lyrics { c.accent } else if lh { c.text } else { c.dim },
            );
        } else {
            draw_text(tr, scene, t, "Nothing playing", lx, vy(r.y0, NOWPLAYING_H, 14.0), 14.0, c.dim, TextWeight::Regular, None);
        }

        // Right block reserved for queue + volume.
        let right_w = 240.0;
        let center = Rect::new(r.x0 + 300.0, r.y0, r.x1 - right_w, r.y1);

        // Transport buttons.
        let cxm = (center.x0 + center.x1) / 2.0;
        let btn_y = r.y0 + 30.0;
        let spacing = 44.0;
        // shuffle
        let shuf_c = Point::new(cxm - 2.0 * spacing, btn_y);
        let sh = self.push_hit(square_around(shuf_c, 30.0), Action::ToggleShuffle);
        icon::draw(scene, t, Icon::Shuffle, shuf_c, 18.0, if self.pb.shuffle { c.accent } else if sh { c.text } else { c.dim });
        // prev
        let prev_c = Point::new(cxm - spacing, btn_y);
        let pv = self.push_hit(square_around(prev_c, 30.0), Action::Prev);
        icon::draw(scene, t, Icon::Prev, prev_c, 18.0, if pv { c.text } else { c.dim_strong });
        // play/pause big
        let play_c = Point::new(cxm, btn_y);
        let pl = self.push_hit(square_around(play_c, 40.0), Action::TogglePlay);
        scene.fill(Fill::NonZero, t, if pl { c.text } else { c.text.multiply_alpha(0.94) }, None, &Circle::new(play_c, 20.0));
        if self.pb.playing {
            icon::draw(scene, t, Icon::Pause, play_c, 16.0, c.panel);
        } else {
            icon::draw(scene, t, Icon::Play, Point::new(play_c.x + 1.0, play_c.y), 16.0, c.panel);
        }
        // next
        let next_c = Point::new(cxm + spacing, btn_y);
        let nx = self.push_hit(square_around(next_c, 30.0), Action::Next);
        icon::draw(scene, t, Icon::Next, next_c, 18.0, if nx { c.text } else { c.dim_strong });
        // repeat
        let rep_c = Point::new(cxm + 2.0 * spacing, btn_y);
        let rp = self.push_hit(square_around(rep_c, 30.0), Action::CycleRepeat);
        let rep_icon = if self.pb.repeat == RepeatMode::One { Icon::RepeatOne } else { Icon::Repeat };
        let rep_col = if self.pb.repeat == RepeatMode::Off { if rp { c.text } else { c.dim } } else { c.accent };
        icon::draw(scene, t, rep_icon, rep_c, 18.0, rep_col);

        // Scrubber.
        let dur = self.current_duration_ms();
        let pos = self.playhead_ms.min(dur as f64).max(0.0);
        let frac = if dur > 0 { pos / dur as f64 } else { 0.0 };
        let bar_y = r.y0 + 64.0;
        let elapsed = fmt_duration((pos / 1000.0) as u64);
        let total = fmt_duration(dur / 1000);
        let (ew, _) = tr.measure_weighted(&elapsed, 11.0, None, TextWeight::Regular);
        let bar_x0 = center.x0 + 48.0;
        let bar_x1 = center.x1 - 48.0;
        draw_text(tr, scene, t, &elapsed, bar_x0 - ew as f64 - 10.0, bar_y - 6.0, 11.0, c.dim, TextWeight::Regular, None);
        draw_text(tr, scene, t, &total, bar_x1 + 10.0, bar_y - 6.0, 11.0, c.dim, TextWeight::Regular, None);
        let track = Rect::new(bar_x0, bar_y, bar_x1, bar_y + 4.0);
        self.scrubber_rect = Rect::new(bar_x0, bar_y - 8.0, bar_x1, bar_y + 12.0);
        fill_rrect(scene, t, track, 2.0, c.white10);
        let fillx = bar_x0 + (bar_x1 - bar_x0) * frac;
        if fillx > bar_x0 {
            fill_rrect(scene, t, Rect::new(bar_x0, bar_y, fillx, bar_y + 4.0), 2.0, c.accent);
        }
        let knob_hover = self.scrubber_rect.contains(self.mouse) || self.drag == Some(Drag::Scrubber);
        if knob_hover {
            scene.fill(Fill::NonZero, t, c.text, None, &Circle::new(Point::new(fillx, bar_y + 2.0), 6.0));
        }

        // Right: queue + volume.
        let q_c = Point::new(r.x1 - right_w + 28.0, r.y0 + NOWPLAYING_H / 2.0);
        let qh = self.push_hit(square_around(q_c, 30.0), Action::Nav(View::Queue));
        icon::draw(scene, t, Icon::Queue, q_c, 18.0, if self.view == View::Queue { c.accent } else if qh { c.text } else { c.dim });

        let vol_icon_c = Point::new(r.x1 - right_w + 64.0, r.y0 + NOWPLAYING_H / 2.0);
        let vol = self.pb.volume as f64 / 100.0;
        icon::draw(scene, t, if vol <= 0.001 { Icon::VolumeMute } else { Icon::VolumeHigh }, vol_icon_c, 18.0, c.dim);
        let vbar_x0 = vol_icon_c.x + 18.0;
        let vbar_x1 = r.x1 - 24.0;
        let vbar_y = r.y0 + NOWPLAYING_H / 2.0;
        self.volume_rect = Rect::new(vbar_x0, vbar_y - 8.0, vbar_x1, vbar_y + 8.0);
        let vtrack = Rect::new(vbar_x0, vbar_y - 2.0, vbar_x1, vbar_y + 2.0);
        fill_rrect(scene, t, vtrack, 2.0, c.white10);
        let vfill = vbar_x0 + (vbar_x1 - vbar_x0) * vol;
        if vfill > vbar_x0 {
            fill_rrect(scene, t, Rect::new(vbar_x0, vbar_y - 2.0, vfill, vbar_y + 2.0), 2.0, c.accent);
        }
        scene.fill(Fill::NonZero, t, c.text, None, &Circle::new(Point::new(vfill, vbar_y), 5.0));
    }

    // ---- toast ------------------------------------------------------------

    fn draw_toast(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, c: &Palette) {
        if let Some((msg, _)) = self.toast.clone() {
            let (tw, _) = tr.measure_weighted(&msg, 13.0, None, TextWeight::Medium);
            let w = tw as f64 + 40.0;
            let x = (self.rect.x1 + self.rect.x0) / 2.0 - w / 2.0;
            let yb = self.rect.y1 - NOWPLAYING_H - 56.0;
            let rect = Rect::new(x, yb, x + w, yb + 38.0);
            fill_rrect(scene, t, rect, 19.0, c.toast_bg);
            scene.stroke(&Stroke::new(1.0), t, c.border, None, &rrect(rect, 19.0));
            draw_text(tr, scene, t, &msg, x + 20.0, vy(yb, 38.0, 13.0), 13.0, c.text, TextWeight::Medium, None);
        }
    }

    // ---- inline name prompt (create / rename playlist) --------------------

    fn draw_name_prompt(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, c: &Palette) {
        let (title, commit): (&str, Action) = match self.name_prompt.clone() {
            Some(NamePrompt::NewPlaylist) => ("New playlist", Action::CreatePlaylist),
            Some(NamePrompt::Rename(id)) => ("Rename playlist", Action::CommitRename(id)),
            None => return,
        };
        // Dim the app behind the dialog; a click on the scrim cancels.
        scene.fill(Fill::NonZero, t, c.scrim.multiply_alpha(0.5), None, &self.rect);
        self.push_hit(self.rect, Action::CloseNamePrompt);

        let w = 420.0_f64.min(self.rect.width() - 48.0);
        let h = 168.0;
        let x0 = (self.rect.x0 + self.rect.x1) / 2.0 - w / 2.0;
        let y0 = (self.rect.y0 + self.rect.y1) / 2.0 - h / 2.0;
        let dialog = Rect::new(x0, y0, x0 + w, y0 + h);
        fill_rrect(scene, t, dialog, 16.0, c.card);
        scene.stroke(&Stroke::new(1.0), t, c.border, None, &rrect(dialog, 16.0));
        // Block clicks inside the dialog from hitting the scrim.
        self.push_hit(dialog, Action::None);

        draw_text(tr, scene, t, title, x0 + 24.0, y0 + 22.0, 18.0, c.text, TextWeight::Bold, None);

        // Text field.
        let field = Rect::new(x0 + 24.0, y0 + 56.0, x0 + w - 24.0, y0 + 92.0);
        fill_rrect(scene, t, field, 10.0, c.panel);
        scene.stroke(&Stroke::new(1.5), t, c.accent, None, &rrect(field, 10.0));
        let shown = elide(tr, &self.name_input, 15.0, TextWeight::Regular, field.width() - 28.0);
        let col = if self.name_input.is_empty() { c.dim } else { c.text };
        let label = if self.name_input.is_empty() { "Playlist name" } else { shown.as_str() };
        draw_text(tr, scene, t, label, field.x0 + 14.0, vy(field.y0, 36.0, 15.0), 15.0, col, TextWeight::Regular, None);
        // Caret.
        let cw = tr.measure_weighted(&self.name_input, 15.0, None, TextWeight::Regular).0 as f64;
        let cx = (field.x0 + 14.0 + cw).min(field.x1 - 12.0);
        scene.fill(Fill::NonZero, t, c.accent, None, &Rect::new(cx, field.y0 + 9.0, cx + 1.5, field.y1 - 9.0));

        // Buttons.
        let by = y0 + h - 48.0;
        let cancel = Rect::new(x0 + w - 24.0 - 200.0, by, x0 + w - 24.0 - 108.0, by + 36.0);
        let ch = self.push_hit(cancel, Action::CloseNamePrompt);
        scene.stroke(&Stroke::new(1.0), t, if ch { c.text } else { c.border }, None, &rrect(cancel, 18.0));
        let (caw, _) = tr.measure_weighted("Cancel", 14.0, None, TextWeight::Semibold);
        draw_text(tr, scene, t, "Cancel", cancel.center().x - caw as f64 / 2.0, vy(by, 36.0, 14.0), 14.0, if ch { c.text } else { c.dim }, TextWeight::Semibold, None);
        let ok = Rect::new(x0 + w - 24.0 - 96.0, by, x0 + w - 24.0, by + 36.0);
        let oh = self.push_hit(ok, commit);
        fill_rrect(scene, t, ok, 18.0, if oh { c.accent_hover } else { c.accent });
        let ok_label = if matches!(self.name_prompt, Some(NamePrompt::Rename(_))) { "Save" } else { "Create" };
        let (ow, _) = tr.measure_weighted(ok_label, 14.0, None, TextWeight::Semibold);
        draw_text(tr, scene, t, ok_label, ok.center().x - ow as f64 / 2.0, vy(by, 36.0, 14.0), 14.0, c.bg, TextWeight::Semibold, None);
    }

    // ---- gather helpers (own data so draw can borrow &mut self) -----------

    fn gather_tracks(&self, ids: &[String]) -> Vec<TrackDisp> {
        ids.iter()
            .filter_map(|id| self.lib.track(id))
            .map(|tk| TrackDisp {
                id: tk.id.clone(),
                title: tk.title.clone(),
                artist: self.lib.artist_name(&tk.artist_id).to_string(),
                album: self.lib.album_title(&tk.album_id).to_string(),
                album_id: tk.album_id.clone(),
                dur: tk.duration_s,
                cache: self.lib.cache_state(&tk.id),
                no: tk.track_no,
            })
            .collect()
    }

    fn gather_albums(&self, limit: Option<usize>) -> Vec<AlbumDisp> {
        let mut v: Vec<AlbumDisp> = self
            .lib
            .albums_sorted()
            .iter()
            .map(|a| AlbumDisp {
                id: a.id.clone(),
                title: a.title.clone(),
                artist: self.lib.artist_name(&a.artist_id).to_string(),
                year: a.year,
                queue: self.lib.album_tracks(&a.id).iter().map(|tk| tk.id.clone()).collect(),
            })
            .collect();
        if let Some(n) = limit {
            v.truncate(n);
        }
        v
    }

    fn gather_albums_for_artist(&self, artist: &str) -> Vec<AlbumDisp> {
        self.lib
            .artist_albums(artist)
            .iter()
            .map(|a| AlbumDisp {
                id: a.id.clone(),
                title: a.title.clone(),
                artist: self.lib.artist_name(&a.artist_id).to_string(),
                year: a.year,
                queue: self.lib.album_tracks(&a.id).iter().map(|tk| tk.id.clone()).collect(),
            })
            .collect()
    }

    fn gather_artists(&self) -> Vec<ArtistDisp> {
        self.lib
            .artists_sorted()
            .iter()
            .map(|a| ArtistDisp {
                id: a.id.clone(),
                name: a.name.clone(),
                genre: a.genre.clone(),
                listeners: a.monthly_listeners,
            })
            .collect()
    }

    fn gather_playlists(&self) -> Vec<PlaylistDisp> {
        self.lib
            .playlists
            .iter()
            .map(|p| PlaylistDisp {
                id: p.id.clone(),
                name: p.name.clone(),
                count: p.track_ids.len(),
            })
            .collect()
    }

    /// Register a click target that only counts when visible inside the body
    /// viewport (so scrolled-away rows don't catch clicks under the chrome).
    fn body_hit(&mut self, rect: Rect, action: Action) -> bool {
        let cy = (rect.y0 + rect.y1) / 2.0;
        if cy < self.body_rect.y0 || cy > self.body_rect.y1 {
            return false;
        }
        self.push_hit(rect, action)
    }
}

impl TrackDisp {
    /// Seed for procedural art — keyed to the album id so an album's tracks
    /// share a cover with the grid/detail art.
    fn album_seed(&self) -> String {
        self.album_id.clone()
    }
}

fn square_around(c: Point, size: f64) -> Rect {
    Rect::new(c.x - size / 2.0, c.y - size / 2.0, c.x + size / 2.0, c.y + size / 2.0)
}

/// A small up/down chevron for reorder controls.
fn draw_chevron(scene: &mut Scene, t: Affine, center: Point, up: bool, color: Color) {
    let s = 5.0;
    let dy = if up { -1.0 } else { 1.0 };
    let mut p = vello::kurbo::BezPath::new();
    p.move_to((center.x - s, center.y + dy * s * 0.5));
    p.line_to((center.x, center.y - dy * s * 0.5));
    p.line_to((center.x + s, center.y + dy * s * 0.5));
    scene.stroke(&Stroke::new(1.6).with_caps(vello::kurbo::Cap::Round), t, color, None, &p);
}

/// A small "x" (remove) glyph.
fn draw_x(scene: &mut Scene, t: Affine, center: Point, color: Color) {
    let s = 5.0;
    let a = vello::kurbo::Line::new((center.x - s, center.y - s), (center.x + s, center.y + s));
    let b = vello::kurbo::Line::new((center.x + s, center.y - s), (center.x - s, center.y + s));
    scene.stroke(&Stroke::new(1.6).with_caps(vello::kurbo::Cap::Round), t, color, None, &a);
    scene.stroke(&Stroke::new(1.6).with_caps(vello::kurbo::Cap::Round), t, color, None, &b);
}

/// Theme-derived palette resolved once per frame.
struct Palette {
    bg: Color,
    panel: Color,
    card: Color,
    card_hover: Color,
    text: Color,
    dim: Color,
    dim_strong: Color,
    accent: Color,
    accent_hover: Color,
    success: Color,
    border: Color,
    white5: Color,
    white10: Color,
    toast_bg: Color,
    scrim: Color,
}

impl Palette {
    fn from(theme: &Theme) -> Self {
        let col = &theme.tokens.colors;
        Self {
            bg: col.neutral[0],
            panel: col.surfaces.base,
            card: col.surfaces.raised,
            card_hover: col.neutral[3].multiply_alpha(0.6),
            text: theme.text_primary,
            dim: theme.text_dim,
            dim_strong: col.neutral[7],
            accent: theme.accent_primary,
            accent_hover: theme.accent_hover,
            success: theme.status_success,
            border: theme.border_standard,
            white5: col.surface_white_5,
            white10: col.surface_white_10,
            toast_bg: col.surfaces.raised,
            scrim: col.surfaces.overlay,
        }
    }
}
