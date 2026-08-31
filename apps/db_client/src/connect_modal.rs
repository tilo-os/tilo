//! The "New connection" modal: an engine selector plus the host/port/db/user
//! fields (or a `SQLite` file path), with Cancel / Save actions. Captures a
//! [`ConnectionProfile`] the app appends to the connection list.
//!
//! Keeping this app-local (rather than the toolkit `ModalDialog`) lets the
//! field set switch dynamically with the selected engine — `SQLite` shows a
//! file path; the network engines show host/port/db/user. It is a focused
//! form widget that paints over a scrim.

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::model::{ConnectionProfile, Engine};
use crate::theme_ext::ThemeExt;
use ui::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, TextInput, Widget};

/// Which text field currently has focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Name,
    Host,
    Port,
    Database,
    User,
    Password,
    FilePath,
}

/// Existing host context borrowed by the modal's local embedded inputs.
/// Keeping it together avoids allocating framework state per keystroke.
struct EditorEvent<'a> {
    event: &'a WindowEvent,
    scale_factor: f64,
    modifiers: Modifiers,
    state: &'a mut AutoCommit,
    root: &'a ObjId,
    node_id: NodeId,
    taffy: &'a TaffyTree<()>,
}

pub struct ConnectModal {
    open: bool,
    engine: Engine,
    name: TextInput,
    host: TextInput,
    port: TextInput,
    database: TextInput,
    user: TextInput,
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

impl Default for ConnectModal {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectModal {
    pub fn new() -> Self {
        let input = |id: &str| {
            TextInput::local()
                .with_id(id)
                .with_font_size(13.0)
                .with_plain_chrome()
        };
        Self {
            open: false,
            engine: Engine::Postgres,
            name: input("db_connection_name"),
            host: input("db_connection_host").with_initial_value("localhost"),
            port: input("db_connection_port").with_initial_value("5432"),
            database: input("db_connection_database"),
            user: input("db_connection_user"),
            password: input("db_connection_password").with_secret(true),
            file_path: input("db_connection_file_path"),
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
        self.focus_field(Some(Field::Name));
    }

    fn set_engine(&mut self, e: Engine) {
        self.engine = e;
        if e != Engine::Sqlite && self.port.value().is_empty() {
            self.port.set_text(&e.default_port().to_string());
        }
        if self
            .focus
            .is_some_and(|f| !self.visible_fields().contains(&f))
        {
            self.focus_field(Some(Field::Name));
        }
    }

    fn input_mut(&mut self, f: Field) -> &mut TextInput {
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

    fn visible_fields(&self) -> &'static [Field] {
        const SQLITE: &[Field] = &[Field::Name, Field::FilePath];
        const NETWORK: &[Field] = &[
            Field::Name,
            Field::Host,
            Field::Port,
            Field::Database,
            Field::User,
            Field::Password,
        ];
        if self.engine == Engine::Sqlite {
            SQLITE
        } else {
            NETWORK
        }
    }

    fn focus_field(&mut self, focus: Option<Field>) {
        self.focus = focus;
        for field in [
            Field::Name,
            Field::Host,
            Field::Port,
            Field::Database,
            Field::User,
            Field::Password,
            Field::FilePath,
        ] {
            Widget::set_focus(self.input_mut(field), focus == Some(field));
        }
    }

    fn build_profile(&self) -> ConnectionProfile {
        let mut p = ConnectionProfile::new(
            if self.name.value().trim().is_empty() {
                "Untitled"
            } else {
                self.name.value().trim()
            },
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
        state: &mut AutoCommit,
        root: &ObjId,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
        handled: &mut bool,
        out: &mut Option<ConnectionProfile>,
    ) {
        let mut editor = EditorEvent {
            event,
            scale_factor,
            modifiers: *modifiers,
            state,
            root,
            node_id,
            taffy,
        };
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
                // Every visible input observes pointer movement so a later
                // press can place its caret at the actual click position.
                for field in self.visible_fields().iter().copied() {
                    self.route_to_input(field, &mut editor);
                }
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
                    self.focus_field(None);
                    return;
                }
                if self.save_btn.contains(c) {
                    *out = Some(self.build_profile());
                    self.open = false;
                    self.focus_field(None);
                    return;
                }
                for (e, r) in self.engine_btns.clone() {
                    if r.contains(c) {
                        self.set_engine(e);
                        return;
                    }
                }
                self.focus_field(None);
                for (f, r) in self.field_rects.clone() {
                    if r.contains(c) {
                        self.focus_field(Some(f));
                        self.route_to_focused_input(&mut editor);
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
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                *handled = true;
                match logical_key {
                    Key::Named(NamedKey::Escape) => {
                        self.open = false;
                        self.focus_field(None);
                    }
                    Key::Named(NamedKey::Enter) => {
                        *out = Some(self.build_profile());
                        self.open = false;
                        self.focus_field(None);
                    }
                    Key::Named(NamedKey::Tab) => {
                        self.focus_field(Some(self.next_field()));
                    }
                    _ => self.route_to_focused_input(&mut editor),
                }
            }
            // IME preedit/commit and accessibility-driven edits must follow
            // the same shared input path as physical keyboard events.
            _ if self.focus.is_some() => {
                *handled = true;
                self.route_to_focused_input(&mut editor);
            }
            _ => {}
        }
    }

    /// Drive the focused hand-laid field through the toolkit widget event
    /// path using the host's existing document and layout context. The inputs
    /// are local-mode, so they borrow that context without writing into it.
    fn route_to_focused_input(&mut self, event: &mut EditorEvent<'_>) {
        let Some(field) = self.focus else { return };
        self.route_to_input(field, event);
    }

    fn route_to_input(&mut self, field: Field, event: &mut EditorEvent<'_>) {
        let (mut handled, mut focus_target) = (false, None);
        Widget::handle_event(
            self.input_mut(field),
            event.event,
            event.scale_factor,
            &event.modifiers,
            event.state,
            event.root,
            &mut handled,
            &mut focus_target,
            event.node_id,
            event.taffy,
        );
    }

    fn next_field(&self) -> Field {
        let order = self.visible_fields();
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
    #[allow(clippy::too_many_arguments)]
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
    #[allow(clippy::too_many_arguments)]
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
        &mut self,
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
        // The shared plain-chrome input paints text, selection and caret. Its
        // secret mode renders one bullet per character and keeps the value out
        // of accessibility and IME surrounding-text surfaces. Explicit user
        // copy/cut still follows the toolkit's normal password-field policy.
        let inner = Rect::new(r.x0 + 10.0, r.y0, r.x1 - 10.0, r.y1);
        let input = self.input_mut(field);
        input.apply_layout_external(inner);
        Widget::draw(input, scene, t, tr, theme, &PresenceCtx::empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::a11y::Role;
    use winit::event::DeviceId;
    use winit::keyboard::{KeyLocation, ModifiersState, NativeKeyCode, PhysicalKey, SmolStr};

    fn device_id() -> DeviceId {
        // Winit exposes no public constructor; events in these tests never
        // inspect the opaque platform identifier.
        unsafe { std::mem::zeroed() }
    }

    fn key_event(key: Key, text: Option<&str>) -> WindowEvent {
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.logical_key = key;
        event.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        event.location = KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.repeat = false;
        event.text = text.map(SmolStr::new);
        WindowEvent::KeyboardInput {
            device_id: device_id(),
            event,
            is_synthetic: false,
        }
    }

    fn send(m: &mut ConnectModal, event: &WindowEvent, modifiers: Modifiers) {
        let mut handled = false;
        let mut out = None;
        let mut state = AutoCommit::new();
        let mut taffy = TaffyTree::new();
        let node = taffy.new_leaf(Default::default()).unwrap();
        m.handle_event(
            event,
            1.0,
            &modifiers,
            &mut state,
            &automerge::ROOT,
            node,
            &taffy,
            &mut handled,
            &mut out,
        );
        assert!(handled, "an open modal consumes its editor event");
        assert!(out.is_none(), "editor event does not submit the form");
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

    #[test]
    fn shared_input_replaces_a_keyboard_selection() {
        let mut m = ConnectModal::new();
        m.open_new();
        let ctrl = Modifiers::from(ModifiersState::CONTROL);
        send(
            &mut m,
            &key_event(Key::Character(SmolStr::new("a")), Some("a")),
            ctrl,
        );
        assert_eq!(m.name.selection_range_public(), Some((0, 14)));

        send(
            &mut m,
            &key_event(Key::Character(SmolStr::new("P")), Some("P")),
            Modifiers::default(),
        );
        assert_eq!(m.name.value(), "P");
    }

    #[test]
    fn ime_commit_uses_the_same_focused_input_path() {
        let mut m = ConnectModal::new();
        m.open_new();
        m.name.set_text("");
        send(
            &mut m,
            &WindowEvent::Ime(winit::event::Ime::Commit("接続".into())),
            Modifiers::default(),
        );
        assert_eq!(m.name.value(), "接続");
    }

    #[test]
    fn password_is_a_protected_secret_input() {
        let mut m = ConnectModal::new();
        m.open_new();
        m.password.set_text("correct horse");
        assert!(m.password.is_secret());
        assert_eq!(
            Widget::ime_purpose(&m.password),
            Some(winit::window::ImePurpose::Password)
        );
        let node = Widget::accessibility(&m.password).expect("text input accessibility node");
        assert_eq!(node.role, Role::TextInput);
        assert!(node.state.protected);
        assert_eq!(node.value, None, "password never enters the a11y value");
        assert_eq!(m.build_profile().password, "correct horse");
    }

    #[test]
    fn switching_engine_blurs_a_field_that_becomes_hidden() {
        let mut m = ConnectModal::new();
        m.open_new();
        m.focus_field(Some(Field::Password));
        m.set_engine(Engine::Sqlite);
        assert_eq!(m.focus, Some(Field::Name));
        assert!(Widget::is_focused(&m.name));
        assert!(!Widget::is_focused(&m.password));
        assert_eq!(m.visible_fields(), &[Field::Name, Field::FilePath]);
    }
}
