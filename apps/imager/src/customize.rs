//! The **shared guided-customization panel** — the one component that collects
//! a qOS install profile (identity, region, options) for *both* imager flows:
//! writing a pre-configured image to a stick, and installing qOS onto a disk.
//!
//! ## Why a self-contained component
//!
//! The imager and the installer both need the same "who is this machine, and
//! who logs in" step, over the same [`InstallProfile`]. Rather than each app
//! hand-rolling its own form (with subtly different validation, focus, and
//! a11y), this panel owns the whole thing once:
//!
//! * every text field is a real shared [`TextInput`] — caret, selection,
//!   clipboard, IME and a11y come for free. Secrets stay off the host and are
//!   collected only on the target machine (HIG §3: never hand-roll editing);
//! * every edit dispatches the matching [`ImagerAction`] through the app's
//!   [`ui::ActionHost`], so a change is one undo boundary, replicates to
//!   collaborators, and is byte-identical to the action the Ctrl+K palette
//!   would dispatch — the single source of truth in [`crate::actions`].
//!
//! The panel holds no profile of its own: it renders from an [`InstallProfile`]
//! snapshot each frame and writes changes back through actions, so the document
//! stays authoritative. It is deliberately free of any imager-wizard coupling
//! (no step/`Hit` types leak in) so the installer can embed the same panel.

use automerge::AutoCommit;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{TextInput, Widget};

use crate::actions::{IMAGER_ACTION_TYPE, ImagerAction};
use crate::model::InstallProfile;

/// One editable text field of the profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CzField {
    Hostname,
    FullName,
    Username,
    Locale,
    Timezone,
    Keyboard,
}

impl CzField {
    const ALL: [CzField; 6] = [
        CzField::Hostname,
        CzField::FullName,
        CzField::Username,
        CzField::Locale,
        CzField::Timezone,
        CzField::Keyboard,
    ];
}

/// The exact rects of the panel for one frame — computed once from the content
/// rect so drawing and hit-testing can never drift apart.
struct CzGeom {
    identity_label_y: f64,
    hostname: Rect,
    full_name: Rect,
    username: Rect,
    locale: Rect,
    timezone: Rect,
    region_label_y: f64,
    keyboard: Rect,
    export: Rect,
}

impl CzGeom {
    fn field(&self, f: CzField) -> Rect {
        match f {
            CzField::Hostname => self.hostname,
            CzField::FullName => self.full_name,
            CzField::Username => self.username,
            CzField::Locale => self.locale,
            CzField::Timezone => self.timezone,
            CzField::Keyboard => self.keyboard,
        }
    }
}

/// What the host app should do after handing an event to the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CzResponse {
    /// The panel consumed the event.
    pub handled: bool,
    /// The user asked to export the unattended-install config.
    pub export_requested: bool,
}

/// The shared guided-customization panel. Embeds one [`TextInput`] per
/// non-secret field and renders the media-profile preview around them.
pub struct CustomizePanel {
    hostname: TextInput,
    full_name: TextInput,
    username: TextInput,
    locale: TextInput,
    timezone: TextInput,
    keyboard: TextInput,
    focused: Option<CzField>,
    last_mouse: Point,
}

impl Default for CustomizePanel {
    fn default() -> Self {
        Self::new()
    }
}

impl CustomizePanel {
    pub fn new() -> Self {
        let field = || {
            let mut t = TextInput::local();
            t.set_plain_chrome(true); // the panel draws the field box itself
            t
        };
        Self {
            hostname: field(),
            full_name: field(),
            username: field(),
            locale: field(),
            timezone: field(),
            keyboard: field(),
            focused: None,
            last_mouse: Point::ZERO,
        }
    }

    fn input(&mut self, f: CzField) -> &mut TextInput {
        match f {
            CzField::Hostname => &mut self.hostname,
            CzField::FullName => &mut self.full_name,
            CzField::Username => &mut self.username,
            CzField::Locale => &mut self.locale,
            CzField::Timezone => &mut self.timezone,
            CzField::Keyboard => &mut self.keyboard,
        }
    }

    fn profile_value(profile: &InstallProfile, f: CzField) -> &str {
        match f {
            CzField::Hostname => &profile.hostname,
            CzField::FullName => &profile.user.full_name,
            CzField::Username => &profile.user.username,
            CzField::Locale => &profile.locale,
            CzField::Timezone => &profile.timezone,
            CzField::Keyboard => &profile.keyboard,
        }
    }

    /// The [`ImagerAction`] that sets `f` to `value`. The single source of truth
    /// shared with the Ctrl+K field specs in [`crate::actions`].
    fn field_action(f: CzField, value: String) -> ImagerAction {
        match f {
            CzField::Hostname => ImagerAction::SetHostname { value },
            CzField::FullName => ImagerAction::SetFullName { value },
            CzField::Username => ImagerAction::SetUsername { value },
            CzField::Locale => ImagerAction::SetLocale { value },
            CzField::Timezone => ImagerAction::SetTimezone { value },
            CzField::Keyboard => ImagerAction::SetKeyboard { value },
        }
    }

    /// Whether the panel currently owns keyboard focus (a field is editing).
    pub fn is_focused(&self) -> bool {
        self.focused.is_some()
    }

    /// Drop keyboard focus from whichever field held it (the host calls this on
    /// a step change so a stale caret does not linger).
    pub fn clear_focus(&mut self) {
        self.blur();
    }

    /// Pull each unfocused field's text from the profile so a loaded / peer-
    /// edited value shows. While a field is focused its [`TextInput`] is the
    /// source of truth and writes back through an action, so it is left alone.
    pub fn sync(&mut self, profile: &InstallProfile) {
        for f in CzField::ALL {
            let focused = self.focused == Some(f);
            let value = Self::profile_value(profile, f).to_string();
            let input = self.input(f);
            input.set_focus(focused);
            if !focused {
                input.set_text(&value);
            }
        }
    }

    // ---- geometry ---------------------------------------------------------

    /// Shared vertical rhythm so section labels, field labels and inputs never
    /// overlap. Pure function of `content`, so draw + hit-test agree.
    fn geom(content: Rect) -> CzGeom {
        let x0 = content.x0;
        let x1 = content.x1;
        let compact = content.width() < 520.0;
        let fw = (content.width() - 16.0) / 2.0;
        let fh = 38.0;
        let row_pitch = fh + if compact { 32.0 } else { 40.0 }; // input + label + gap
        let identity_label_y = content.y0 + if compact { 72.0 } else { 52.0 };
        let host_y = content.y0 + if compact { 104.0 } else { 92.0 };
        let hostname = Rect::new(x0, host_y, x1, host_y + fh);
        let r2 = host_y + row_pitch;
        let full_name = Rect::new(x0, r2, x0 + fw, r2 + fh);
        let username = Rect::new(x0 + fw + 16.0, r2, x1, r2 + fh);
        let r3 = r2 + row_pitch;
        let timezone = Rect::new(x0, r3, x0 + fw, r3 + fh);
        let locale = Rect::new(x0 + fw + 16.0, r3, x1, r3 + fh);
        let region_label_y = r3 + fh + 26.0;
        let kb_y = region_label_y + 40.0;
        let keyboard = Rect::new(x0, kb_y, x1, kb_y + fh);
        let export = Rect::new(x1 - 250.0, kb_y + fh + 30.0, x1, kb_y + fh + 62.0);
        CzGeom {
            identity_label_y,
            hostname,
            full_name,
            username,
            locale,
            timezone,
            region_label_y,
            keyboard,
            export,
        }
    }

    // ---- drawing ----------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn txt(
        tr: &mut TextRenderer,
        scene: &mut Scene,
        s: &str,
        size: f32,
        color: Color,
        x: f64,
        top: f64,
        max_w: Option<f32>,
        weight: TextWeight,
    ) {
        tr.draw_text_weighted(
            scene,
            s,
            size,
            color,
            Affine::translate((x, top)),
            max_w,
            weight,
        );
    }

    fn fill_rr(scene: &mut Scene, t: Affine, r: Rect, radius: f64, c: Color) {
        scene.fill(
            Fill::NonZero,
            t,
            c,
            None,
            &RoundedRect::from_rect(r, radius),
        );
    }
    fn stroke_rr(scene: &mut Scene, t: Affine, r: Rect, radius: f64, c: Color, w: f64) {
        scene.stroke(
            &Stroke::new(w),
            t,
            c,
            None,
            &RoundedRect::from_rect(r, radius),
        );
    }

    fn section_label(
        tr: &mut TextRenderer,
        scene: &mut Scene,
        theme: &Theme,
        s: &str,
        x: f64,
        y: f64,
    ) {
        Self::txt(
            tr,
            scene,
            s,
            11.0,
            theme.tokens.colors.text_dim,
            x,
            y,
            None,
            TextWeight::Semibold,
        );
    }

    /// Draw + hit-forward one text field (box + label + the shared input glyphs).
    #[allow(clippy::too_many_arguments)]
    fn draw_field(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        presence: &PresenceCtx,
        f: CzField,
        r: Rect,
        label: &str,
    ) {
        let c = &theme.tokens.colors;
        Self::txt(
            tr,
            scene,
            label,
            11.0,
            c.text_dim,
            r.x0 + 2.0,
            r.y0 - 17.0,
            None,
            TextWeight::Semibold,
        );
        let focused = self.focused == Some(f);
        Self::fill_rr(scene, t, r, 9.0, c.neutral[2]);
        Self::stroke_rr(
            scene,
            t,
            r,
            9.0,
            if focused {
                c.border_focus
            } else {
                c.border_standard
            },
            if focused { 1.6 } else { 1.0 },
        );
        // The plain-chrome input paints its own glyphs + caret inside the box.
        let inner = Rect::new(r.x0 + 12.0, r.y0, r.x1 - 12.0, r.y1);
        let input = self.input(f);
        input.apply_layout_external(inner);
        Widget::draw(input, scene, t, tr, theme, presence);
    }

    /// Render the whole panel into `content` for the given profile snapshot.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        content: Rect,
        profile: &InstallProfile,
        target_removable: bool,
    ) {
        let g = Self::geom(content);
        self.draw_identity_fields(scene, tr, theme, t, presence, content.x0, &g);
        self.draw_region_and_options(
            scene,
            tr,
            theme,
            t,
            presence,
            content.x0,
            &g,
            profile,
            target_removable,
        );
        self.draw_export_button(scene, tr, theme, t, &g);
    }

    /// The IDENTITY section: hostname, name, username and time zone.
    #[allow(clippy::too_many_arguments)]
    fn draw_identity_fields(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        presence: &PresenceCtx,
        label_x: f64,
        g: &CzGeom,
    ) {
        Self::section_label(tr, scene, theme, "IDENTITY", label_x, g.identity_label_y);
        self.draw_field(
            scene,
            tr,
            theme,
            t,
            presence,
            CzField::Hostname,
            g.hostname,
            "Hostname",
        );
        self.draw_field(
            scene,
            tr,
            theme,
            t,
            presence,
            CzField::FullName,
            g.full_name,
            "Full name",
        );
        self.draw_field(
            scene,
            tr,
            theme,
            t,
            presence,
            CzField::Username,
            g.username,
            "Username",
        );
        self.draw_field(
            scene,
            tr,
            theme,
            t,
            presence,
            CzField::Timezone,
            g.timezone,
            "Time zone",
        );
        self.draw_field(
            scene,
            tr,
            theme,
            t,
            presence,
            CzField::Locale,
            g.locale,
            "Language & locale",
        );
    }

    /// The REGION section contains the last host-side preference. Security,
    /// storage target and destructive choices are completed on the device.
    #[allow(clippy::too_many_arguments)]
    fn draw_region_and_options(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        presence: &PresenceCtx,
        label_x: f64,
        g: &CzGeom,
        _profile: &InstallProfile,
        _target_removable: bool,
    ) {
        Self::section_label(
            tr,
            scene,
            theme,
            "REGION & OPTIONS",
            label_x,
            g.region_label_y,
        );
        self.draw_field(
            scene,
            tr,
            theme,
            t,
            presence,
            CzField::Keyboard,
            g.keyboard,
            "Keyboard",
        );
    }

    /// Preview the exact profile which will be embedded into the medium.
    fn draw_export_button(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        g: &CzGeom,
    ) {
        let c = &theme.tokens.colors;
        let hov = g.export.contains(self.last_mouse);
        Self::fill_rr(
            scene,
            t,
            g.export,
            9.0,
            if hov { c.neutral[3] } else { c.neutral[2] },
        );
        Self::stroke_rr(scene, t, g.export, 9.0, c.accent[6], 1.0);
        let (ew, _) =
            tr.measure_weighted("Preview embedded setup", 12.0, None, TextWeight::Semibold);
        Self::txt(
            tr,
            scene,
            "Preview embedded setup",
            12.0,
            c.accent[7],
            f64::midpoint(g.export.x0, g.export.x1) - f64::from(ew) / 2.0,
            g.export.y0 + 8.0,
            None,
            TextWeight::Semibold,
        );
    }

    // ---- interaction ------------------------------------------------------

    /// Route a winit event into the panel. Text edits and toggles dispatch
    /// their [`ImagerAction`] through `host`; the returned [`CzResponse`] tells
    /// the host whether the event was consumed and whether export was asked for.
    #[allow(clippy::too_many_arguments)]
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: &Modifiers,
        doc: &mut AutoCommit,
        path: &automerge::ObjId,
        host: &mut ui::ActionHost,
        profile: &InstallProfile,
        target_removable: bool,
        content: Rect,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) -> CzResponse {
        let g = Self::geom(content);
        match event {
            WindowEvent::CursorMoved { position, .. } => self.handle_cursor_moved(
                event, position.x, position.y, scale, *modifiers, doc, path, &g, node_id, taffy,
            ),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.handle_left_click(
                event,
                scale,
                *modifiers,
                doc,
                path,
                host,
                profile,
                target_removable,
                &g,
                node_id,
                taffy,
            ),
            WindowEvent::KeyboardInput { event: key_ev, .. }
                if key_ev.state == ElementState::Pressed =>
            {
                self.handle_key_event(
                    event, key_ev, scale, *modifiers, doc, path, host, &g, node_id, taffy,
                )
            }
            _ => CzResponse {
                handled: false,
                export_requested: false,
            },
        }
    }

    /// Track the pointer and forward it to a focused field so drag-selection
    /// tracks along.
    #[allow(clippy::too_many_arguments)]
    fn handle_cursor_moved(
        &mut self,
        event: &WindowEvent,
        px: f64,
        py: f64,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut AutoCommit,
        path: &automerge::ObjId,
        g: &CzGeom,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) -> CzResponse {
        self.last_mouse = Point::new(px / scale, py / scale);
        if let Some(f) = self.focused {
            let inner = Rect::new(
                g.field(f).x0 + 12.0,
                g.field(f).y0,
                g.field(f).x1 - 12.0,
                g.field(f).y1,
            );
            let input = self.input(f);
            input.apply_layout_external(inner);
            let (mut h, mut ft) = (false, None);
            input.handle_event(
                event, scale, &modifiers, doc, path, &mut h, &mut ft, node_id, taffy,
            );
        }
        CzResponse {
            handled: false,
            export_requested: false,
        }
    }

    /// A left click: focus a field and forward the press for caret placement,
    /// dispatch a toggle, request export, or (on empty space) drop focus.
    #[allow(clippy::too_many_arguments)]
    fn handle_left_click(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut AutoCommit,
        path: &automerge::ObjId,
        _host: &mut ui::ActionHost,
        _profile: &InstallProfile,
        _target_removable: bool,
        g: &CzGeom,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) -> CzResponse {
        let p = self.last_mouse;
        for f in CzField::ALL {
            if g.field(f).contains(p) {
                self.focus_field(
                    f,
                    g.field(f),
                    event,
                    scale,
                    modifiers,
                    doc,
                    path,
                    node_id,
                    taffy,
                );
                return CzResponse {
                    handled: true,
                    export_requested: false,
                };
            }
        }
        if g.export.contains(p) {
            return CzResponse {
                handled: true,
                export_requested: true,
            };
        }
        // A click elsewhere drops field focus.
        self.blur();
        CzResponse {
            handled: false,
            export_requested: false,
        }
    }

    /// A key press while a field is focused: Enter/Escape blur it, anything
    /// else routes to the input and dispatches its fresh value.
    #[allow(clippy::too_many_arguments)]
    fn handle_key_event(
        &mut self,
        event: &WindowEvent,
        key_ev: &winit::event::KeyEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut AutoCommit,
        path: &automerge::ObjId,
        host: &mut ui::ActionHost,
        g: &CzGeom,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) -> CzResponse {
        let Some(f) = self.focused else {
            return CzResponse {
                handled: false,
                export_requested: false,
            };
        };
        if let Key::Named(NamedKey::Enter | NamedKey::Escape) = &key_ev.logical_key {
            self.blur();
            return CzResponse {
                handled: true,
                export_requested: false,
            };
        }
        // Route the keystroke to the focused input, then dispatch the field's
        // absolute-set action with its fresh value.
        let field_rect = g.field(f);
        let inner = Rect::new(
            field_rect.x0 + 12.0,
            field_rect.y0,
            field_rect.x1 - 12.0,
            field_rect.y1,
        );
        let input = self.input(f);
        input.set_focus(true);
        input.apply_layout_external(inner);
        let (mut h, mut ft) = (false, None);
        input.handle_event(
            event, scale, &modifiers, doc, path, &mut h, &mut ft, node_id, taffy,
        );
        let value = self.input(f).value().to_string();
        Self::dispatch(doc, host, &Self::field_action(f, value));
        CzResponse {
            handled: true,
            export_requested: false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn focus_field(
        &mut self,
        f: CzField,
        field_rect: Rect,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut AutoCommit,
        path: &automerge::ObjId,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) {
        self.blur();
        self.focused = Some(f);
        let inner = Rect::new(
            field_rect.x0 + 12.0,
            field_rect.y0,
            field_rect.x1 - 12.0,
            field_rect.y1,
        );
        let input = self.input(f);
        input.set_focus(true);
        input.apply_layout_external(inner);
        let (mut h, mut ft) = (false, None);
        input.handle_event(
            event, scale, &modifiers, doc, path, &mut h, &mut ft, node_id, taffy,
        );
    }

    fn blur(&mut self) {
        if let Some(f) = self.focused.take() {
            self.input(f).set_focus(false);
        }
    }

    fn dispatch(doc: &mut AutoCommit, host: &mut ui::ActionHost, action: &ImagerAction) {
        host.dispatch_with_context(
            doc,
            IMAGER_ACTION_TYPE,
            action,
            qos_actions::ActionContext::for_action("org.qos.Imager", "Imager", action),
        );
    }

    /// Focus a field programmatically (test/host helper).
    #[cfg(test)]
    pub fn focus_for_test(&mut self, f: CzField) {
        self.blur();
        self.focused = Some(f);
        self.input(f).set_focus(true);
    }

    #[cfg(test)]
    pub fn field_value(&mut self, f: CzField) -> String {
        self.input(f).value().to_string()
    }
}
