//! The "New connection" modal: an engine selector plus the host/port/db/user
//! fields (or a `SQLite` file path), with Cancel / Save actions. Captures a
//! [`ConnectionProfile`] the app appends to the connection list.
//!
//! Keeping this app-local (rather than the toolkit `ModalDialog`) lets the
//! field set switch dynamically with the selected engine — `SQLite` shows a
//! file path; the network engines show host/port/db/user. It is a focused
//! form widget that paints over a scrim.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::model::{ConnectionProfile, Engine};
use crate::theme_ext::ThemeExt;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace};

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
    name: String,
    host: String,
    port: String,
    database: String,
    user: String,
    password: String,
    file_path: String,
    focus: Option<Field>,
    cursor: Point,

    // Hit rects, set during draw.
    panel: Rect,
    engine_btns: Vec<(Engine, Rect)>,
    field_rects: Vec<(Field, Rect)>,
    cancel_btn: Rect,
    save_btn: Rect,
}

impl Default for ConnectModal {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectModal {
    pub fn new() -> Self {
        Self {
            open: false,
            engine: Engine::Postgres,
            name: String::new(),
            host: "localhost".into(),
            port: "5432".into(),
            database: String::new(),
            user: String::new(),
            password: String::new(),
            file_path: String::new(),
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
        self.name = "New connection".into();
        self.host = "localhost".into();
        self.port = "5432".into();
        self.database = String::new();
        self.user = String::new();
        self.password = String::new();
        self.file_path = String::new();
        self.focus = Some(Field::Name);
    }

    fn set_engine(&mut self, e: Engine) {
        self.engine = e;
        if e != Engine::Sqlite && self.port.is_empty() {
            self.port = e.default_port().to_string();
        }
    }

    fn field_mut(&mut self, f: Field) -> &mut String {
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

    fn field_value(&self, f: Field) -> &str {
        match f {
            Field::Name => &self.name,
            Field::Host => &self.host,
            Field::Port => &self.port,
            Field::Database => &self.database,
            Field::User => &self.user,
            Field::Password => &self.password,
            Field::FilePath => &self.file_path,
        }
    }

    fn build_profile(&self) -> ConnectionProfile {
        let mut p = ConnectionProfile::new(
            if self.name.trim().is_empty() {
                "Untitled"
            } else {
                self.name.trim()
            },
            self.engine,
        );
        p.host.clone_from(&self.host);
        p.port = self
            .port
            .parse()
            .unwrap_or_else(|_| self.engine.default_port());
        p.database.clone_from(&self.database);
        p.user.clone_from(&self.user);
        p.password.clone_from(&self.password);
        p.file_path.clone_from(&self.file_path);
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
                        return;
                    }
                }
                // Click outside the panel dismisses.
                if !self.panel.contains(c) {
                    self.open = false;
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        text,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                *handled = true;
                let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
                match logical_key {
                    Key::Named(NamedKey::Escape) => self.open = false,
                    Key::Named(NamedKey::Enter) => {
                        *out = Some(self.build_profile());
                        self.open = false;
                    }
                    Key::Named(NamedKey::Tab) => {
                        self.focus = Some(self.next_field());
                    }
                    Key::Named(NamedKey::Backspace) => {
                        if let Some(f) = self.focus {
                            self.field_mut(f).pop();
                        }
                    }
                    Key::Named(NamedKey::Space) => {
                        if let Some(f) = self.focus {
                            self.field_mut(f).push(' ');
                        }
                    }
                    _ => {
                        if let (Some(f), Some(t)) = (self.focus, text)
                            && !ctrl
                        {
                            self.field_mut(f).push_str(t.as_str());
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
        // `order` always has at least one entry (the SQLite and network
        // field sets both start with `Field::Name`), so this only ever
        // falls back on an impossible empty list.
        match self.focus {
            None => order.first().copied().unwrap_or(Field::Name),
            Some(cur) => {
                let i = order.iter().position(|f| *f == cur).unwrap_or(0);
                let next = (i + 1) % order.len().max(1);
                order.get(next).copied().unwrap_or(Field::Name)
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
    ) {
        // Scrim.
        scene.fill(
            Fill::NonZero,
            t,
            Color::from_rgba8(0, 0, 0, 150),
            None,
            &root,
        );

        let pw = 460.0;
        let ph = if self.engine == Engine::Sqlite {
            320.0
        } else {
            512.0
        };
        let px = root.x0 + (root.width() - pw) / 2.0;
        let py = root.y0 + (root.height() - ph) / 2.0;
        let panel = Rect::new(px, py, px + pw, py + ph);
        self.panel = panel;
        scene.fill(
            Fill::NonZero,
            t,
            theme.bg_primary,
            None,
            &panel.to_rounded_rect(12.0),
        );
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

        y = self.draw_engine_selector(scene, t, tr, theme, px, pw, y);
        y = self.draw_fields(scene, t, tr, theme, px, pw, y);

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

        self.draw_buttons(scene, t, tr, theme);
    }

    /// Engine selector segmented control. Returns the Y position beneath it.
    fn draw_engine_selector(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        px: f64,
        pw: f64,
        y: f64,
    ) -> f64 {
        let p = theme.db();
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
                scene.stroke(
                    &Stroke::new(1.0),
                    t,
                    theme.border_standard,
                    None,
                    &seg.to_rounded_rect(6.0),
                );
            }
            let fg = if active {
                Color::WHITE
            } else {
                theme.text_primary
            };
            let (lw, _) = tr.measure(e.label(), 12.5, None);
            tr.draw_text(
                scene,
                e.label(),
                12.5,
                fg,
                t * Affine::translate((seg.x0 + (seg.width() - f64::from(lw)) / 2.0, y + 8.0)),
                None,
            );
            sx += seg_w;
        }
        y + 48.0
    }

    /// The engine-dependent field set (`SQLite` file path, or the network
    /// host/port/db/user/password rows). Returns the Y position beneath it.
    fn draw_fields(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        px: f64,
        pw: f64,
        y: f64,
    ) -> f64 {
        self.field_rects.clear();
        let mut y = y;
        if self.engine == Engine::Sqlite {
            y = self.field(scene, t, tr, theme, px, pw, y, "Name", Field::Name);
            y = self.field(
                scene,
                t,
                tr,
                theme,
                px,
                pw,
                y,
                "SQLite file path",
                Field::FilePath,
            );
        } else {
            y = self.field(scene, t, tr, theme, px, pw, y, "Name", Field::Name);
            // Host + Port on one row.
            let (hx, ()) = self.field_pair(
                scene,
                t,
                tr,
                theme,
                px,
                pw,
                y,
                "Host",
                Field::Host,
                "Port",
                Field::Port,
            );
            y = hx;
            y = self.field(scene, t, tr, theme, px, pw, y, "Database", Field::Database);
            let (ux, ()) = self.field_pair(
                scene,
                t,
                tr,
                theme,
                px,
                pw,
                y,
                "User",
                Field::User,
                "Password",
                Field::Password,
            );
            y = ux;
        }
        y
    }

    /// Cancel/Save buttons anchored to the bottom-right of `self.panel`
    /// (already positioned earlier in `draw`).
    fn draw_buttons(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let panel = self.panel;
        let cancel = Rect::new(
            panel.x1 - 200.0,
            panel.y1 - 48.0,
            panel.x1 - 110.0,
            panel.y1 - 16.0,
        );
        let save = Rect::new(
            panel.x1 - 100.0,
            panel.y1 - 48.0,
            panel.x1 - 24.0,
            panel.y1 - 16.0,
        );
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

    #[allow(
        clippy::too_many_arguments,
        reason = "shared draw context (canvas/theme/panel geometry) plus this field's label and slot"
    )]
    fn field(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
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
        self.draw_input(scene, t, tr, theme, r, field);
        self.field_rects.push((field, r));
        y + 56.0
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "shared draw context (canvas/theme/panel geometry) plus a pair of fields' labels and slots"
    )]
    fn field_pair(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        px: f64,
        pw: f64,
        y: f64,
        l1: &str,
        f1: Field,
        l2: &str,
        f2: Field,
    ) -> (f64, ()) {
        let total = pw - 48.0;
        let w1 = total * 0.68;
        tr.draw_text(
            scene,
            l1,
            11.0,
            theme.text_dim,
            t * Affine::translate((px + 24.0, y)),
            None,
        );
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
        self.draw_input(scene, t, tr, theme, r1, f1);
        self.draw_input(scene, t, tr, theme, r2, f2);
        self.field_rects.push((f1, r1));
        self.field_rects.push((f2, r2));
        (y + 56.0, ())
    }

    fn draw_input(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
        field: Field,
    ) {
        let focused = self.focus == Some(field);
        scene.fill(
            Fill::NonZero,
            t,
            theme.bg_app,
            None,
            &r.to_rounded_rect(6.0),
        );
        let border = if focused {
            theme.border_focus
        } else {
            theme.border_standard
        };
        scene.stroke(
            &Stroke::new(if focused { 1.5 } else { 1.0 }),
            t,
            border,
            None,
            &r.to_rounded_rect(6.0),
        );
        // Password fields render masked; every other field shows its value.
        let raw = self.field_value(field);
        let masked;
        let val: &str = if field == Field::Password {
            masked = "•".repeat(raw.chars().count());
            &masked
        } else {
            raw
        };
        tr.draw_text(
            scene,
            val,
            13.0,
            theme.text_primary,
            t * Affine::translate((r.x0 + 10.0, r.y0 + 7.0)),
            Some((r.width() - 20.0) as f32),
        );
        if focused {
            let (vw, _) = tr.measure(val, 13.0, None);
            let cx = (r.x0 + 10.0 + f64::from(vw)).min(r.x1 - 6.0);
            scene.stroke(
                &Stroke::new(1.5),
                t,
                theme.accent_primary,
                None,
                &vello::kurbo::Line::new(Point::new(cx, r.y0 + 6.0), Point::new(cx, r.y1 - 6.0)),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        m.host = "db.example".into();
        m.port = "6543".into();
        m.database = "app".into();
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
}
