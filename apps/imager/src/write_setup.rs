//! On-write setup collected on the Confirm step.
//!
//! Installable media embeds a secret-free media profile on Customize. Separately,
//! this machine can join the target to Wi-Fi and certify a new device for the
//! person already here. The passphrase stays host-local; the collaborative
//! profile only records the network name, device label, and whether to enroll.

use automerge::AutoCommit;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::surface;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::tokens::Metrics;
use ui::widget::{TextInput, Toggle, ToggleFace, Widget};

use crate::actions::{IMAGER_ACTION_TYPE, ImagerAction};
use crate::model::InstallProfile;
use crate::rhythm::{GROUP, LABEL, RELATED, SECTION, SIBLING};

const FIELD_H: f64 = Metrics::BASE.bar_height as f64;
const TOGGLE_H: f64 = Metrics::BASE.bar_control_height as f64;
const TOGGLE_W: f64 = Metrics::BASE.bar_control_height as f64 + Metrics::BASE.icon_lg as f64;

/// One control on the write-setup panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupField {
    WifiSsid,
    WifiPsk,
    DeviceLabel,
    Enroll,
}

impl SetupField {
    const ALL: [Self; 4] = [
        Self::WifiSsid,
        Self::WifiPsk,
        Self::DeviceLabel,
        Self::Enroll,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::WifiSsid => "Wi-Fi network",
            Self::WifiPsk => "Wi-Fi password",
            Self::DeviceLabel => "Device name",
            Self::Enroll => "Enroll this machine's identity",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::WifiSsid => "wifi-ssid",
            Self::WifiPsk => "wifi-psk",
            Self::DeviceLabel => "device-label",
            Self::Enroll => "enroll-identity",
        }
    }
}

struct SetupGeom {
    section_y: f64,
    wifi_ssid: Rect,
    wifi_psk: Rect,
    device_label: Rect,
    enroll: Rect,
    enroll_toggle: Rect,
}

impl SetupGeom {
    fn field(&self, field: SetupField) -> Rect {
        match field {
            SetupField::WifiSsid => self.wifi_ssid,
            SetupField::WifiPsk => self.wifi_psk,
            SetupField::DeviceLabel => self.device_label,
            SetupField::Enroll => self.enroll,
        }
    }
}

/// What the host should do after handling an event.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SetupResponse {
    pub handled: bool,
    /// Fresh host-local passphrase when the password field changed.
    pub wifi_psk: Option<String>,
}

/// Compact form for Wi-Fi and identity on Confirm before a write.
pub struct WriteSetupPanel {
    wifi_ssid: TextInput,
    wifi_psk: TextInput,
    device_label: TextInput,
    focused: Option<SetupField>,
    last_mouse: Point,
    label_h: f64,
}

impl Default for WriteSetupPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl WriteSetupPanel {
    pub fn new() -> Self {
        let field = || {
            let mut input = TextInput::local();
            input.set_plain_chrome(true);
            input
        };
        let mut wifi_psk = field();
        wifi_psk = wifi_psk.with_secret(true);
        Self {
            wifi_ssid: field(),
            wifi_psk,
            device_label: field(),
            focused: None,
            last_mouse: Point::ZERO,
            label_h: 14.0,
        }
    }

    pub fn clear_focus(&mut self) {
        self.blur();
    }

    pub fn is_focused(&self) -> bool {
        self.focused.is_some()
    }

    pub fn measure(&mut self, tr: &mut TextRenderer) -> bool {
        let next = f64::from(tr.measure("Hg", 11.0, None).1);
        let changed = (next - self.label_h).abs() > 0.5;
        self.label_h = next;
        changed
    }

    /// Height occupied below `top` inside `content`.
    pub fn extent(&self, content: Rect, top: f64) -> f64 {
        self.geom(content, top).enroll.y1 - top + GROUP
    }

    pub fn sync(&mut self, profile: &InstallProfile, wifi_psk: &str) {
        for field in SetupField::ALL {
            if self.focused == Some(field) {
                continue;
            }
            match field {
                SetupField::WifiSsid => self.wifi_ssid.set_text(&profile.wifi_ssid),
                SetupField::WifiPsk => self.wifi_psk.set_text(wifi_psk),
                SetupField::DeviceLabel => self.device_label.set_text(&profile.device_label),
                SetupField::Enroll => {}
            }
        }
    }

    fn geom(&self, content: Rect, top: f64) -> SetupGeom {
        let x0 = content.x0;
        let x1 = content.x1;
        let caption = self.label_h + LABEL;
        let row = FIELD_H + SECTION + caption;
        let section_y = top;
        let ssid_y = section_y + self.label_h + RELATED + caption;
        let wifi_ssid = Rect::new(x0, ssid_y, x1, ssid_y + FIELD_H);
        let psk_y = ssid_y + row;
        let wifi_psk = Rect::new(x0, psk_y, x1, psk_y + FIELD_H);
        let label_y = psk_y + row;
        let device_label = Rect::new(x0, label_y, x1, label_y + FIELD_H);
        let enroll_y = label_y + row;
        let enroll = Rect::new(x0, enroll_y, x1, enroll_y + FIELD_H);
        let enroll_toggle = Rect::new(
            x1 - TOGGLE_W,
            enroll_y + (FIELD_H - TOGGLE_H) / 2.0,
            x1,
            enroll_y + (FIELD_H - TOGGLE_H) / 2.0 + TOGGLE_H,
        );
        SetupGeom {
            section_y,
            wifi_ssid,
            wifi_psk,
            device_label,
            enroll,
            enroll_toggle,
        }
    }

    fn input(&mut self, field: SetupField) -> Option<&mut TextInput> {
        match field {
            SetupField::WifiSsid => Some(&mut self.wifi_ssid),
            SetupField::WifiPsk => Some(&mut self.wifi_psk),
            SetupField::DeviceLabel => Some(&mut self.device_label),
            SetupField::Enroll => None,
        }
    }

    fn input_ref(&self, field: SetupField) -> Option<&TextInput> {
        match field {
            SetupField::WifiSsid => Some(&self.wifi_ssid),
            SetupField::WifiPsk => Some(&self.wifi_psk),
            SetupField::DeviceLabel => Some(&self.device_label),
            SetupField::Enroll => None,
        }
    }

    fn field_action(field: SetupField, value: String) -> Option<ImagerAction> {
        match field {
            SetupField::WifiSsid => Some(ImagerAction::SetWifiSsid { value }),
            SetupField::DeviceLabel => Some(ImagerAction::SetDeviceLabel { value }),
            SetupField::WifiPsk | SetupField::Enroll => None,
        }
    }

    fn dispatch(doc: &mut AutoCommit, host: &mut ui::ActionHost, action: &ImagerAction) {
        host.dispatch_with_context(
            doc,
            IMAGER_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.imager", "Imager", action),
        );
    }

    fn blur(&mut self) {
        if let Some(field) = self.focused.take()
            && let Some(input) = self.input(field)
        {
            input.set_focus(false);
        }
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
            &RoundedRect::from_rect(r.inset(-w / 2.0), (radius - w / 2.0).max(0.0)),
        );
    }

    fn txt(
        tr: &mut TextRenderer,
        scene: &mut Scene,
        s: &str,
        size: f32,
        color: Color,
        x: f64,
        top: f64,
        weight: TextWeight,
    ) {
        tr.draw_text_weighted(
            scene,
            s,
            size,
            color,
            Affine::translate((x, top)),
            None,
            weight,
        );
    }

    /// Draw the panel starting at `top` inside `content`.
    pub fn draw(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        presence: &PresenceCtx,
        content: Rect,
        top: f64,
        profile: &InstallProfile,
    ) {
        let g = self.geom(content, top);
        let c = &ctx.theme.tokens.colors;
        Self::txt(
            ctx.text,
            ctx.scene,
            "ON THIS WRITE",
            11.0,
            c.text_dim,
            content.x0,
            g.section_y,
            TextWeight::Semibold,
        );
        for field in [
            SetupField::WifiSsid,
            SetupField::WifiPsk,
            SetupField::DeviceLabel,
        ] {
            self.draw_text_field(
                ctx.scene,
                ctx.text,
                ctx.theme,
                ctx.transform,
                presence,
                field,
                g.field(field),
            );
        }
        self.draw_enroll(
            ctx.scene,
            ctx.text,
            ctx.theme,
            ctx.transform,
            &g,
            profile.enroll_local_identity,
        );
    }

    fn draw_text_field(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        presence: &PresenceCtx,
        field: SetupField,
        r: Rect,
    ) {
        let c = &theme.tokens.colors;
        let label_top = r.y0 - LABEL - self.label_h;
        Self::txt(
            tr,
            scene,
            field.label(),
            11.0,
            c.text_dim,
            r.x0,
            label_top,
            TextWeight::Semibold,
        );
        let focused = self.focused == Some(field);
        let radius = theme.radius_standard;
        if focused {
            Self::fill_rr(scene, t, r, radius, theme.bg_active);
            Self::stroke_rr(scene, t, r, radius, c.border_focus, 2.0);
        } else {
            surface::card(scene, t, r, radius, c.surfaces.raised);
        }
        let inner = Rect::new(r.x0 + SIBLING, r.y0, r.x1 - SIBLING, r.y1);
        if let Some(input) = self.input(field) {
            input.apply_layout_external(inner);
            Widget::draw(input, scene, t, tr, theme, presence);
        }
    }

    fn draw_enroll(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        g: &SetupGeom,
        on: bool,
    ) {
        let c = &theme.tokens.colors;
        let r = g.enroll;
        let label_top = r.y0 - LABEL - self.label_h;
        Self::txt(
            tr,
            scene,
            SetupField::Enroll.label(),
            11.0,
            c.text_dim,
            r.x0,
            label_top,
            TextWeight::Semibold,
        );
        Self::txt(
            tr,
            scene,
            "Certify a new device; the root secret stays on this machine.",
            11.0,
            c.text_dim,
            r.x0,
            r.y0 + (FIELD_H - self.label_h) / 2.0,
            TextWeight::Regular,
        );
        Toggle::paint(
            scene,
            t,
            theme,
            &ToggleFace::new(g.enroll_toggle, on)
                .hovered(g.enroll_toggle.contains(self.last_mouse))
                .focused(self.focused == Some(SetupField::Enroll)),
        );
    }

    /// Route a winit event. `top` must match the draw/extent origin.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        host: &mut ui::ActionHost,
        profile: &InstallProfile,
        content: Rect,
        top: f64,
    ) -> SetupResponse {
        let g = self.geom(content, top);
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                if let Some(field) = self.focused
                    && let Some(input) = self.input(field)
                {
                    let inner = Rect::new(
                        g.field(field).x0 + SIBLING,
                        g.field(field).y0,
                        g.field(field).x1 - SIBLING,
                        g.field(field).y1,
                    );
                    input.apply_layout_external(inner);
                    let (mut handled, mut focus_target) = (false, None);
                    input.handle_event(
                        event,
                        &mut ui::widget::EventCtx {
                            handled: &mut handled,
                            focus_target: &mut focus_target,
                            ..ctx.reborrow(ctx.path, ctx.node_id)
                        },
                    );
                }
                SetupResponse::default()
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.handle_click(event, ctx, host, profile, &g),
            WindowEvent::KeyboardInput { event: key_ev, .. }
                if key_ev.state == ElementState::Pressed =>
            {
                self.handle_key(event, ctx, key_ev, host, profile, &g)
            }
            _ => SetupResponse::default(),
        }
    }

    fn handle_click(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        host: &mut ui::ActionHost,
        profile: &InstallProfile,
        g: &SetupGeom,
    ) -> SetupResponse {
        let p = self.last_mouse;
        if g.enroll.contains(p) || g.enroll_toggle.contains(p) {
            self.blur();
            self.focused = Some(SetupField::Enroll);
            Self::dispatch(
                ctx.state,
                host,
                &ImagerAction::SetEnrollLocalIdentity {
                    on: !profile.enroll_local_identity,
                },
            );
            return SetupResponse {
                handled: true,
                wifi_psk: None,
            };
        }
        for field in [
            SetupField::WifiSsid,
            SetupField::WifiPsk,
            SetupField::DeviceLabel,
        ] {
            if g.field(field).contains(p) {
                self.focus_field(event, ctx, field, g.field(field));
                return SetupResponse {
                    handled: true,
                    wifi_psk: None,
                };
            }
        }
        self.blur();
        SetupResponse::default()
    }

    fn handle_key(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        key_ev: &winit::event::KeyEvent,
        host: &mut ui::ActionHost,
        profile: &InstallProfile,
        g: &SetupGeom,
    ) -> SetupResponse {
        let Some(field) = self.focused else {
            return SetupResponse::default();
        };
        if field == SetupField::Enroll {
            if matches!(
                &key_ev.logical_key,
                Key::Named(NamedKey::Enter | NamedKey::Space)
            ) {
                Self::dispatch(
                    ctx.state,
                    host,
                    &ImagerAction::SetEnrollLocalIdentity {
                        on: !profile.enroll_local_identity,
                    },
                );
                return SetupResponse {
                    handled: true,
                    wifi_psk: None,
                };
            }
            if matches!(&key_ev.logical_key, Key::Named(NamedKey::Escape)) {
                self.blur();
                return SetupResponse {
                    handled: true,
                    wifi_psk: None,
                };
            }
            return SetupResponse::default();
        }
        if let Key::Named(NamedKey::Enter | NamedKey::Escape) = &key_ev.logical_key {
            self.blur();
            return SetupResponse {
                handled: true,
                wifi_psk: None,
            };
        }
        let rect = g.field(field);
        let inner = Rect::new(rect.x0 + SIBLING, rect.y0, rect.x1 - SIBLING, rect.y1);
        let Some(input) = self.input(field) else {
            return SetupResponse::default();
        };
        input.set_focus(true);
        input.apply_layout_external(inner);
        let (mut handled, mut focus_target) = (false, None);
        input.handle_event(
            event,
            &mut ui::widget::EventCtx {
                handled: &mut handled,
                focus_target: &mut focus_target,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        let value = self
            .input_ref(field)
            .map(|i| i.value().to_string())
            .unwrap_or_default();
        if field == SetupField::WifiPsk {
            return SetupResponse {
                handled: true,
                wifi_psk: Some(value),
            };
        }
        if let Some(action) = Self::field_action(field, value) {
            Self::dispatch(ctx.state, host, &action);
        }
        SetupResponse {
            handled: true,
            wifi_psk: None,
        }
    }

    fn focus_field(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        field: SetupField,
        rect: Rect,
    ) {
        self.blur();
        self.focused = Some(field);
        let Some(input) = self.input(field) else {
            return;
        };
        let inner = Rect::new(rect.x0 + SIBLING, rect.y0, rect.x1 - SIBLING, rect.y1);
        input.set_focus(true);
        input.apply_layout_external(inner);
        let (mut handled, mut focus_target) = (false, None);
        input.handle_event(
            event,
            &mut ui::widget::EventCtx {
                handled: &mut handled,
                focus_target: &mut focus_target,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
    }

    pub fn accessibility(
        &self,
        content: Rect,
        top: f64,
        enroll_on: bool,
    ) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};

        let g = self.geom(content, top);
        let mut nodes = Vec::new();
        for field in [
            SetupField::WifiSsid,
            SetupField::WifiPsk,
            SetupField::DeviceLabel,
        ] {
            let rect = g.field(field);
            let node = self
                .input_ref(field)
                .and_then(Widget::accessibility)
                .unwrap_or_else(|| AccessNode::new(Role::TextInput));
            nodes.push(
                node.with_name(field.label())
                    .with_state(AccessState {
                        focused: self.focused == Some(field),
                        focusable: true,
                        protected: field == SetupField::WifiPsk,
                        ..Default::default()
                    })
                    .with_bounds(rect)
                    .with_action_target(format!("imager:write-setup:{}", field.key())),
            );
        }
        nodes.push(
            AccessNode::new(Role::Switch)
                .with_name(SetupField::Enroll.label())
                .with_description("Certify a new device; the root secret stays on this machine.")
                .with_state(AccessState {
                    focused: self.focused == Some(SetupField::Enroll),
                    focusable: true,
                    checked: Some(enroll_on),
                    ..Default::default()
                })
                .with_bounds(g.enroll)
                .with_action_target("imager:write-setup:enroll-identity"),
        );
        nodes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_setup_stacks_fields_below_the_section_caption() {
        let panel = WriteSetupPanel::new();
        let content = Rect::new(24.0, 0.0, 400.0, 800.0);
        let g = panel.geom(content, 100.0);
        assert!(g.wifi_ssid.y0 > g.section_y);
        assert!(g.wifi_psk.y0 > g.wifi_ssid.y1);
        assert!(g.device_label.y0 > g.wifi_psk.y1);
        assert!(g.enroll.y0 > g.device_label.y1);
        assert!(g.enroll_toggle.x1 <= content.x1 + 1e-6);
    }
}
