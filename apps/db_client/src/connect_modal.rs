//! The "New connection" modal: an engine selector plus the host/port/db/user
//! fields (or a SQLite file path), with Cancel / Save actions. Captures a
//! [`ConnectionProfile`] the app appends to the connection list.
//!
//! Keeping this app-local (rather than the toolkit `ModalDialog`) lets the
//! field set switch dynamically with the selected engine — SQLite shows a
//! file path; the network engines show host/port/db/user. It is a focused
//! form widget that paints over a scrim. Each field is a shared
//! [`ui::widget::TextInput`] (so caret, selection, clipboard, and — for the
//! password — a real masked/`secret` mode all come from the toolkit rather
//! than a hand-rolled `String`).

use vello::kurbo::{Affine, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};
use vello::Scene;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::model::{ConnectionProfile, Engine};
use crate::theme_ext::ThemeExt;
use ui::automerge::AutoCommit;
use ui::presence::PresenceCtx;
use ui::taffy::{NodeId, TaffyTree};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, TextInput, Widget};

/// Which text field currently has focus.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Host,
    Port,
    Database,
    User,
    Password,
    FilePath,
}

pub struct ConnectModal {
    open: bool,
    engine: Engine,
    name: TextInput,
    host: TextInput,
    port: TextInput,
    database: TextInput,
    user: TextInput,
    /// Masked: a `secret` [`TextInput`], so the plaintext never reaches the
    /// a11y tree and the on-screen glyphs are bullets — the toolkit owns the
    /// masking (no hand-rolled `"•".repeat(...)`).
    password: TextInput,
    file_path: TextInput,
    focus: Option<Field>,
    cursor: Point,

    // Hit rects, set during draw.
    panel: Rect,
    engine_btns: Vec<(Engine, Rect)>,
    field_rects: Vec<(Field, Rect)>,
    cancel_btn: Rect,
    save_btn: Rect,
}

impl ConnectModal {
    pub fn new() -> Self {
        let field = || TextInput::local().with_plain_chrome().with_font_size(13.0);
        let mut port = field();
        port.set_text("5432");
        let mut host = field();
        host.set_text("localhost");
        Self {
            open: false,
            engine: Engine::Postgres,
            name: field(),
            host,
            port,
            database: field(),
            user: field(),
            password: field().with_secret(true),
            file_path: field(),
            focus: None,
            cursor: Point::ZERO,
            panel: Rect::ZERO,
            engine_btns: Vec::new(),
            field_rects: Vec::new(),
            cancel_btn: Rect::ZERO,
            save_btn: Rect::ZERO,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open_new(&mut self) {
        self.open = true;
        self.engine = Engine::Postgres;
        self.name.set_text("New connection");
        self.host.set_text("localhost");
        self.port.set_text("5432");
        self.database.set_text("");
        self.user.set_text("");
        self.password.set_text("");
        self.file_path.set_text("");
        self.focus = Some(Field::Name);
        self.sync_focus();
    }

    fn set_engine(&mut self, e: Engine) {
        self.engine = e;
        if e != Engine::Sqlite && self.port.value().is_empty() {
            self.port.set_text(&e.default_port().to_string());
        }
    }

    /// Mirror `focus` onto the fields so the focused one owns the caret and
    /// receives keystrokes.
    fn sync_focus(&mut self) {
        let f = self.focus;
        self.name.set_focus(f == Some(Field::Name));
        self.host.set_focus(f == Some(Field::Host));
        self.port.set_focus(f == Some(Field::Port));
        self.database.set_focus(f == Some(Field::Database));
        self.user.set_focus(f == Some(Field::User));
        self.password.set_focus(f == Some(Field::Password));
        self.file_path.set_focus(f == Some(Field::FilePath));
    }

    fn field_mut(&mut self, f: Field) -> &mut TextInput {
        match f {
            Field::Name => &mut self.name,
            Field::Host => &mut self.host,
            Field::Port => &mut self.port,
            Field::Database => &mut self.database,
            Field::User => &mut self.user,
            Field::Password => &mut self.password,
            Field::FilePath => &mut self.file_path,
        }
    }

    fn build_profile(&self) -> ConnectionProfile {
        let name = self.name.value();
        let mut p = ConnectionProfile::new(
            if name.trim().is_empty() { "Untitled" } else { name.trim() },
            self.engine,
        );
        p.host = self.host.value().to_string();
        p.port = self
            .port
            .value()
            .parse()
            .unwrap_or_else(|_| self.engine.default_port());
        p.database = self.database.value().to_string();
        p.user = self.user.value().to_string();
        p.password = self.password.value().to_string();
        p.file_path = self.file_path.value().to_string();
        p
    }

    /// Handle an event while the modal is open. On Save, writes the new
    /// profile into `out`.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        handled: &mut bool,
        out: &mut Option<ConnectionProfile>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                *handled = true;
                let c = self.cursor;
                if self.cancel_btn.contains(c) {
                    self.open = false;
                    return;
                }
                if self.save_btn.contains(c) {
                    *out = Some(self.build_profile());
                    self.open = false;
                    return;
                }
                for (e, r) in self.engine_btns.clone() {
                    if r.contains(c) {
                        self.set_engine(e);
                        return;
                    }
                }
                self.focus = None;
                for (f, r) in self.field_rects.clone() {
                    if r.contains(c) {
                        self.focus = Some(f);
                        break;
                    }
                }
                self.sync_focus();
                // Click outside the panel dismisses (only if it hit no field).
                if self.focus.is_none() && !self.panel.contains(c) {
                    self.open = false;
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                *handled = true;
                match logical_key {
                    Key::Named(NamedKey::Escape) => self.open = false,
                    Key::Named(NamedKey::Enter) => {
                        *out = Some(self.build_profile());
                        self.open = false;
                    }
                    Key::Named(NamedKey::Tab) => {
                        self.focus = Some(self.next_field());
                        self.sync_focus();
                    }
                    // Text, backspace, caret motion, selection, Ctrl+C/V/X all
                    // go to the focused shared field (which owns the real caret
                    // / selection / clipboard, and masks the password itself).
                    _ => {
                        if let Some(f) = self.focus {
                            self.sync_focus();
                            let mut scratch = AutoCommit::new();
                            let taffy: TaffyTree<()> = TaffyTree::new();
                            let node = NodeId::new(0);
                            let mut sink = None;
                            self.field_mut(f).handle_event(
                                event,
                                scale_factor,
                                modifiers,
                                &mut scratch,
                                &ui::automerge::ROOT,
                                handled,
                                &mut sink,
                                node,
                                &taffy,
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn next_field(&self) -> Field {
        let order: Vec<Field> = if self.engine == Engine::Sqlite {
            vec![Field::Name, Field::FilePath]
        } else {
            vec![
                Field::Name,
                Field::Host,
                Field::Port,
                Field::Database,
                Field::User,
                Field::Password,
            ]
        };
        match self.focus {
            None => order[0],
            Some(cur) => {
                let i = order.iter().position(|f| *f == cur).unwrap_or(0);
                order[(i + 1) % order.len()]
            }
        }
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        root: Rect,
        presence: &PresenceCtx,
    ) {
        let p = theme.db();
        // Scrim.
        scene.fill(Fill::NonZero, t, Color::from_rgba8(0, 0, 0, 150), None, &root);

        let pw = 460.0;
        let ph = if self.engine == Engine::Sqlite { 320.0 } else { 512.0 };
        let px = root.x0 + (root.width() - pw) / 2.0;
        let py = root.y0 + (root.height() - ph) / 2.0;
        let panel = Rect::new(px, py, px + pw, py + ph);
        self.panel = panel;
        scene.fill(Fill::NonZero, t, theme.bg_primary, None, &panel.to_rounded_rect(12.0));
        scene.stroke(
            &Stroke::new(1.0),
            t,
            theme.border_standard,
            None,
            &panel.to_rounded_rect(12.0),
        );

        let mut y = py + 20.0;
        tr.draw_text(
            scene,
            "New connection",
            18.0,
            theme.text_primary,
            t * Affine::translate((px + 24.0, y)),
            None,
        );
        y += 36.0;

        // Engine selector segmented control.
        self.engine_btns.clear();
        let seg_w = (pw - 48.0) / 3.0;
        let mut sx = px + 24.0;
        for e in Engine::all() {
            let seg = Rect::new(sx, y, sx + seg_w - 6.0, y + 32.0);
            self.engine_btns.push((e, seg));
            let active = e == self.engine;
            let bg = if active {
                theme.accent_primary
            } else if seg.contains(self.cursor) {
                p.hover
            } else {
                theme.bg_app
            };
            scene.fill(Fill::NonZero, t, bg, None, &seg.to_rounded_rect(6.0));
            if !active {
                scene.stroke(&Stroke::new(1.0), t, theme.border_standard, None, &seg.to_rounded_rect(6.0));
            }
            let fg = if active { Color::WHITE } else { theme.text_primary };
            let (lw, _) = tr.measure(e.label(), 12.5, None);
            tr.draw_text(
                scene,
                e.label(),
                12.5,
                fg,
                t * Affine::translate((seg.x0 + (seg.width() - lw as f64) / 2.0, y + 8.0)),
                None,
            );
            sx += seg_w;
        }
        y += 48.0;

        // Fields.
        self.field_rects.clear();
        if self.engine == Engine::Sqlite {
            y = self.field(scene, t, tr, theme, presence, px, pw, y, "Name", Field::Name);
            y = self.field(scene, t, tr, theme, presence, px, pw, y, "SQLite file path", Field::FilePath);
        } else {
            y = self.field(scene, t, tr, theme, presence, px, pw, y, "Name", Field::Name);
            // Host + Port on one row.
            let hx = self.field_pair(scene, t, tr, theme, presence, px, pw, y, "Host", Field::Host, "Port", Field::Port);
            y = hx;
            y = self.field(scene, t, tr, theme, presence, px, pw, y, "Database", Field::Database);
            let ux = self.field_pair(
                scene, t, tr, theme, presence, px, pw, y, "User", Field::User, "Password", Field::Password,
            );
            y = ux;
        }

        // Password-handling note for the network engines.
        if self.engine != Engine::Sqlite {
            tr.draw_text(
                scene,
                "The password is kept in memory for this session only and is never written to disk.",
                10.5,
                theme.text_dim,
                t * Affine::translate((px + 24.0, y + 2.0)),
                Some((pw - 48.0) as f32),
            );
        }

        // Buttons.
        let cancel = Rect::new(px + pw - 200.0, py + ph - 48.0, px + pw - 110.0, py + ph - 16.0);
        let save = Rect::new(px + pw - 100.0, py + ph - 48.0, px + pw - 24.0, py + ph - 16.0);
        self.cancel_btn = cancel;
        self.save_btn = save;

        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(cancel, "Cancel")
                .radius(6.0)
                .text_size(12.5)
                .weight(TextWeight::Regular)
                .hovered(cancel.contains(self.cursor)),
        );
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(save, "Save")
                .radius(6.0)
                .text_size(12.5)
                .weight(TextWeight::Regular)
                .hovered(save.contains(self.cursor))
                .primary(),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn field(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        px: f64,
        pw: f64,
        y: f64,
        label: &str,
        field: Field,
    ) -> f64 {
        tr.draw_text(
            scene,
            label,
            11.0,
            theme.text_dim,
            t * Affine::translate((px + 24.0, y)),
            None,
        );
        let r = Rect::new(px + 24.0, y + 16.0, px + pw - 24.0, y + 44.0);
        self.draw_input(scene, t, tr, theme, presence, r, field);
        self.field_rects.push((field, r));
        y + 56.0
    }

    #[allow(clippy::too_many_arguments)]
    fn field_pair(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        px: f64,
        pw: f64,
        y: f64,
        l1: &str,
        f1: Field,
        l2: &str,
        f2: Field,
    ) -> f64 {
        let total = pw - 48.0;
        let w1 = total * 0.68;
        tr.draw_text(scene, l1, 11.0, theme.text_dim, t * Affine::translate((px + 24.0, y)), None);
        tr.draw_text(
            scene,
            l2,
            11.0,
            theme.text_dim,
            t * Affine::translate((px + 24.0 + w1 + 12.0, y)),
            None,
        );
        let r1 = Rect::new(px + 24.0, y + 16.0, px + 24.0 + w1, y + 44.0);
        let r2 = Rect::new(px + 24.0 + w1 + 12.0, y + 16.0, px + pw - 24.0, y + 44.0);
        self.draw_input(scene, t, tr, theme, presence, r1, f1);
        self.draw_input(scene, t, tr, theme, presence, r2, f2);
        self.field_rects.push((f1, r1));
        self.field_rects.push((f2, r2));
        y + 56.0
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_input(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        r: Rect,
        field: Field,
    ) {
        let focused = self.focus == Some(field);
        scene.fill(Fill::NonZero, t, theme.bg_app, None, &r.to_rounded_rect(6.0));
        let border = if focused { theme.border_focus } else { theme.border_standard };
        scene.stroke(&Stroke::new(if focused { 1.5 } else { 1.0 }), t, border, None, &r.to_rounded_rect(6.0));
        // The value, caret, selection, and password masking all belong to the
        // shared field — disks keeps only the box + border here.
        let inner = Rect::new(r.x0 + 10.0, r.y0, r.x1 - 10.0, r.y1);
        let input = self.field_mut(field);
        input.apply_layout_external(inner);
        input.draw(scene, t, tr, theme, presence);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `KeyboardInput` winit event. `KeyEvent`'s `platform_specific`
    /// field is crate-private, so the struct is zero-initialised then the
    /// public fields the field reads are overwritten. `text` is what a focused
    /// `TextInput` actually inserts.
    fn char_event(s: &str) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(winit::keyboard::SmolStr::new(s));
        ev.physical_key =
            winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(winit::keyboard::SmolStr::new(s));
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn named_event(named: NamedKey) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(named);
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

    fn dispatch(m: &mut ConnectModal, ev: &WindowEvent) -> Option<ConnectionProfile> {
        let mods = Modifiers::default();
        let mut handled = false;
        let mut out = None;
        m.handle_event(ev, 1.0, &mods, &mut handled, &mut out);
        out
    }

    fn type_str(m: &mut ConnectModal, s: &str) {
        for ch in s.chars() {
            dispatch(m, &char_event(&ch.to_string()));
        }
    }

    #[test]
    fn open_resets_form_and_focus() {
        let mut m = ConnectModal::new();
        m.open_new();
        assert!(m.is_open());
        assert_eq!(m.engine, Engine::Postgres);
    }

    #[test]
    fn build_profile_parses_port() {
        let mut m = ConnectModal::new();
        m.open_new();
        m.host.set_text("db.example");
        m.port.set_text("6543");
        m.database.set_text("app");
        let p = m.build_profile();
        assert_eq!(p.port, 6543);
        assert_eq!(p.host, "db.example");
    }

    #[test]
    fn next_field_wraps_for_sqlite() {
        let mut m = ConnectModal::new();
        m.open_new();
        m.set_engine(Engine::Sqlite);
        m.focus = Some(Field::FilePath);
        // Only Name + FilePath in the SQLite order, so it wraps to Name.
        assert!(matches!(m.next_field(), Field::Name));
    }

    /// Typing through the real event path lands in the focused shared field,
    /// Tab moves focus, and the password field is a masked `secret` input whose
    /// real bytes still reach the built profile.
    #[test]
    fn typing_fields_through_the_shared_inputs_builds_the_profile() {
        let mut m = ConnectModal::new();
        m.open_new();
        // Name is focused on open; replace the seeded text.
        m.name.set_text("");
        m.sync_focus();
        type_str(&mut m, "prod");
        assert_eq!(m.name.value(), "prod", "keys land in the focused Name field");

        // Tab to Host and type; the seeded default is still selectable/erasable.
        dispatch(&mut m, &named_event(NamedKey::Tab));
        assert!(matches!(m.focus, Some(Field::Host)));
        m.host.set_text("");
        type_str(&mut m, "db.internal");
        assert_eq!(m.host.value(), "db.internal");

        // Jump straight to the password field and type a secret.
        m.focus = Some(Field::Password);
        m.sync_focus();
        type_str(&mut m, "s3cr3t");
        assert_eq!(m.password.value(), "s3cr3t", "the secret field keeps the real bytes");
        assert!(m.password.is_secret(), "the password field is masked");

        // Enter saves; the built profile carries every typed value.
        let profile = dispatch(&mut m, &named_event(NamedKey::Enter))
            .expect("Enter emits the profile");
        assert_eq!(profile.name, "prod");
        assert_eq!(profile.host, "db.internal");
        assert_eq!(profile.password, "s3cr3t");
        assert!(!m.is_open(), "saving closes the modal");
    }
}
