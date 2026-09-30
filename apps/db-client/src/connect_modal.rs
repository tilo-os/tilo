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
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::model::{ConnectionProfile, Engine};
use crate::theme_ext::ThemeExt;
use ui::PresenceCtx;
use ui::a11y::{AccessNode, AccessState, Action as A11yAction, Role};
use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, EventCtx, TextInput, Widget};

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

impl Field {
    fn key(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Host => "host",
            Self::Port => "port",
            Self::Database => "database",
            Self::User => "user",
            Self::Password => "password",
            Self::FilePath => "file-path",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "name" => Self::Name,
            "host" => Self::Host,
            "port" => Self::Port,
            "database" => Self::Database,
            "user" => Self::User,
            "password" => Self::Password,
            "file-path" => Self::FilePath,
            _ => return None,
        })
    }

    fn label(self) -> &'static str {
        match self {
            Self::Name => "Connection name",
            Self::Host => "Host",
            Self::Port => "Port",
            Self::Database => "Database",
            Self::User => "User",
            Self::Password => "Password",
            Self::FilePath => "SQLite file path",
        }
    }
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

    /// The hand-laid connection form has no retained container widget, so it
    /// publishes its controls as a synthetic dialog tree. Password text is
    /// deliberately absent from the exported value.
    pub fn accessibility(&self) -> Option<AccessNode> {
        if !self.open {
            return None;
        }
        let mut children = vec![
            AccessNode::new(Role::Group)
                .with_name("Database engine")
                .with_children(
                    [Engine::Postgres, Engine::MySql, Engine::Sqlite]
                        .into_iter()
                        .map(|engine| {
                            AccessNode::new(Role::RadioButton)
                                .with_name(engine.label())
                                .with_action_target(format!("db.modal.engine:{}", engine.as_key()))
                                .with_state(
                                    AccessState::new()
                                        .focusable(true)
                                        .checked(self.engine == engine),
                                )
                        })
                        .collect(),
                ),
        ];
        children.extend(self.visible_fields().iter().copied().map(|field| {
            let input = self.input(field);
            let mut node = AccessNode::new(Role::TextInput)
                .with_name(field.label())
                .with_action_target(format!("db.modal.field:{}", field.key()))
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .protected(field == Field::Password),
                );
            if field != Field::Password {
                node = node.with_value(input.value());
            }
            node
        }));
        children.extend([
            AccessNode::new(Role::Button)
                .with_name("Cancel")
                .with_action_target("db.modal.cancel")
                .with_state(AccessState::new().focusable(true)),
            AccessNode::new(Role::Button)
                .with_name("Save connection")
                .with_action_target("db.modal.save")
                .with_state(AccessState::new().focusable(true)),
        ]);
        Some(
            AccessNode::new(Role::Dialog)
                .with_name("New database connection")
                .with_description("Enter connection details. Passwords stay on this device.")
                .with_state(AccessState::new().modal(true))
                .with_children(children),
        )
    }

    /// Route assistive-technology edits and activation through the same local
    /// fields and save/cancel flow as pointer and keyboard input.
    pub fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &A11yAction,
    ) -> Option<ConnectionProfile> {
        if !self.open {
            return None;
        }
        if target == "db.modal.cancel" && matches!(action, A11yAction::Activate) {
            self.open = false;
            self.focus_field(None);
            return None;
        }
        if target == "db.modal.save" && matches!(action, A11yAction::Activate) {
            let profile = self.build_profile();
            self.open = false;
            self.focus_field(None);
            return Some(profile);
        }
        if let Some(key) = target.strip_prefix("db.modal.engine:")
            && matches!(action, A11yAction::Activate | A11yAction::ToggleCheck)
            && matches!(key, "postgres" | "mysql" | "sqlite")
        {
            self.set_engine(Engine::from_key(key));
            return None;
        }
        if let Some(key) = target.strip_prefix("db.modal.field:")
            && let Some(field) = Field::from_key(key)
        {
            match action {
                A11yAction::Focus | A11yAction::Activate => self.focus_field(Some(field)),
                A11yAction::SetValue(value) => {
                    self.input_mut(field).set_text(value);
                    self.focus_field(Some(field));
                }
                _ => return None,
            }
        }
        None
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

    fn input(&self, f: Field) -> &TextInput {
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
        ctx: &mut EventCtx<'_>,
        out: &mut Option<ConnectionProfile>,
    ) {
        let mut editor = EditorEvent {
            event,
            scale_factor: ctx.scale_factor,
            modifiers: *ctx.modifiers,
            state: &mut *ctx.state,
            root: ctx.path,
            node_id: ctx.node_id,
            taffy: ctx.taffy,
        };
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
                *ctx.handled = true;
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
                *ctx.handled = true;
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
                *ctx.handled = true;
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
            &mut EventCtx {
                scale_factor: event.scale_factor,
                modifiers: &event.modifiers,
                state: event.state,
                path: event.root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: event.node_id,
                taffy: event.taffy,
            },
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
        // Scrim: the theme's own dimming tier.
        scene.fill(
            Fill::NonZero,
            t,
            theme.tokens.colors.surfaces.scrim,
            None,
            &root,
        );

        let pw = 460.0;
        // The sheet is as tall as its content (title band, engine selector,
        // 56 px per field row, the password note, the action row), so the
        // actions sit one `s6` group step under the last field instead of
        // floating at the bottom of a fixed-height panel (HIG §7b).
        let sp = &theme.tokens.spacing;
        let field_rows = if self.engine == Engine::Sqlite {
            2.0
        } else {
            4.0
        };
        let note_h = if self.engine == Engine::Sqlite {
            0.0
        } else {
            f64::from(sp.s6)
        };
        let ph = 20.0
            + 36.0
            + 48.0
            + field_rows * 56.0
            + note_h
            + f64::from(sp.s6)
            + 32.0
            + f64::from(sp.s4);
        let px = root.x0 + (root.width() - pw) / 2.0;
        let py = root.y0 + (root.height() - ph) / 2.0;
        let panel = Rect::new(px, py, px + pw, py + ph);
        self.panel = panel;
        // A floating dialog is a Panel: the shell's panel ground and its
        // derived border, so the sheet has an edge on the dark preset too.
        scene.fill(
            Fill::NonZero,
            t,
            ui::chrome::panel_ground(theme),
            None,
            &panel.to_rounded_rect(12.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            t,
            ui::chrome::panel_border(theme),
            None,
            &ui::surface::edge_outline(panel, 12.0),
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
        y = self.draw_fields(&mut PaintCtx::new(scene, t, tr, theme), y);

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
            if active {
                scene.fill(Fill::NonZero, t, bg, None, &seg.to_rounded_rect(6.0));
            } else {
                ui::surface::card(scene, t, seg, 6.0, bg);
            }
            let fg = if active {
                theme.tokens.colors.on_surface(theme.accent_primary).ink
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
    fn draw_fields(&mut self, ctx: &mut PaintCtx<'_>, y: f64) -> f64 {
        self.field_rects.clear();
        let mut y = y;
        if self.engine == Engine::Sqlite {
            y = self.field(ctx, y, ("Name", Field::Name));
            y = self.field(ctx, y, ("SQLite file path", Field::FilePath));
        } else {
            y = self.field(ctx, y, ("Name", Field::Name));
            // Host + Port on one row.
            y = self.field_pair(ctx, y, ("Host", Field::Host), ("Port", Field::Port));
            y = self.field(ctx, y, ("Database", Field::Database));
            y = self.field_pair(ctx, y, ("User", Field::User), ("Password", Field::Password));
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

    /// One labelled input spanning the panel's content width at `y`.
    /// Returns the Y position beneath it.
    fn field(&mut self, ctx: &mut PaintCtx<'_>, y: f64, (label, field): (&str, Field)) -> f64 {
        let (px, pw) = (self.panel.x0, self.panel.width());
        ctx.text(
            label,
            &TextStyle::new(11.0),
            ctx.theme.text_dim,
            Point::new(px + 24.0, y),
            None,
        );
        let r = Rect::new(px + 24.0, y + 16.0, px + pw - 24.0, y + 44.0);
        self.draw_input(ctx, r, field);
        self.field_rects.push((field, r));
        y + 56.0
    }

    /// Two labelled inputs sharing one row at `y`, the first taking most of
    /// the width. Returns the Y position beneath them.
    fn field_pair(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        y: f64,
        (l1, f1): (&str, Field),
        (l2, f2): (&str, Field),
    ) -> f64 {
        let (px, pw) = (self.panel.x0, self.panel.width());
        let total = pw - 48.0;
        let w1 = total * 0.68;
        let label = TextStyle::new(11.0);
        ctx.text(
            l1,
            &label,
            ctx.theme.text_dim,
            Point::new(px + 24.0, y),
            None,
        );
        ctx.text(
            l2,
            &label,
            ctx.theme.text_dim,
            Point::new(px + 24.0 + w1 + 12.0, y),
            None,
        );
        let r1 = Rect::new(px + 24.0, y + 16.0, px + 24.0 + w1, y + 44.0);
        let r2 = Rect::new(px + 24.0 + w1 + 12.0, y + 16.0, px + pw - 24.0, y + 44.0);
        self.draw_input(ctx, r1, f1);
        self.draw_input(ctx, r2, f2);
        self.field_rects.push((f1, r1));
        self.field_rects.push((f2, r2));
        y + 56.0
    }

    fn draw_input(&mut self, ctx: &mut PaintCtx<'_>, r: Rect, field: Field) {
        let PaintCtx {
            scene,
            transform: t,
            text: tr,
            theme,
        } = ctx.reborrow();
        let focused = self.focus == Some(field);
        if focused {
            scene.fill(
                Fill::NonZero,
                t,
                theme.bg_app,
                None,
                &r.to_rounded_rect(6.0),
            );
            scene.stroke(
                &Stroke::new(1.5),
                t,
                theme.border_focus,
                None,
                &r.to_rounded_rect(6.0),
            );
        } else {
            // Rest: the field's derived edge (a declared border vanished on
            // the dark panel ground).
            ui::surface::card(scene, t, r, 6.0, theme.bg_app);
        }
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
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        let mut focus_target = None;
        m.handle_event(
            event,
            &mut EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: &mut state,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
            &mut out,
        );
        assert!(handled, "an open modal consumes its editor event");
        assert!(out.is_none(), "editor event does not submit the form");
    }

    /// Opening resets the form, profiles parse the port, focus wraps for `SQLite` and engine switches blur hidden fields.
    #[test]
    fn connect_form_resets_parses_and_navigates() {
        // --- phase: open_resets_form_and_focus ---
        {
            let mut m = ConnectModal::new();
            m.open_new();
            assert!(m.is_open());
            assert_eq!(m.engine, Engine::Postgres);
        }

        // --- phase: build_profile_parses_port ---
        {
            let mut m = ConnectModal::new();
            m.open_new();
            m.host.set_text("db.example");
            m.port.set_text("6543");
            m.database.set_text("app");
            let p = m.build_profile();
            assert_eq!(p.port, 6543);
            assert_eq!(p.host, "db.example");
        }

        // --- phase: next_field_wraps_for_sqlite ---
        {
            let mut m = ConnectModal::new();
            m.open_new();
            m.set_engine(Engine::Sqlite);
            m.focus = Some(Field::FilePath);
            // Only Name + FilePath in the SQLite order, so it wraps to Name.
            assert!(matches!(m.next_field(), Field::Name));
        }

        // --- phase: switching_engine_blurs_a_field_that_becomes_hidden ---
        {
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

    /// Fields replace keyboard selections and commit IME through the shared input, and the password is a protected secret.
    #[test]
    fn connect_form_inputs_use_shared_secret_safe_editing() {
        // --- phase: shared_input_replaces_a_keyboard_selection ---
        {
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

        // --- phase: ime_commit_uses_the_same_focused_input_path ---
        {
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

        // --- phase: password_is_a_protected_secret_input ---
        {
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
    }
}
