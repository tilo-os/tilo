//! On-write setup collected on the Confirm step.
//!
//! Installable media embeds a secret-free media profile on Customize. Separately,
//! this machine can join the target to Wi-Fi and certify a new device for the
//! person already here. The passphrase stays host-local; the collaborative
//! profile only records the network name, device label, and whether to enroll.

use automerge::AutoCommit;
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{ButtonFace, ControlSize, TextInput, TextInputKind, Toggle, ToggleFace, Widget};

use crate::Gaps;
use crate::actions::{IMAGER_ACTION_TYPE, ImagerAction};
use crate::model::InstallProfile;

/// The section's title.
const SECTION_TITLE: &str = "Network and identity";
/// What enrolling does, beside its switch.
const ENROLL_NOTE: &str = "Certifies the new device. The root secret stays on this computer.";

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
            Self::Enroll => "Enroll as a new device",
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
    metrics: SetupMetrics,
}

/// What the host measures for the panel from its renderer and theme.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SetupMetrics {
    section_h: f64,
    label_h: f64,
    field_h: f64,
    /// The switch: a small control's height, a pointer target wide.
    toggle: (f64, f64),
    gaps: Gaps,
}

impl Default for SetupMetrics {
    /// The unmeasured estimate on the unscaled scale.
    fn default() -> Self {
        let gaps = Gaps::default();
        Self {
            section_h: gaps.group,
            label_h: gaps.section,
            field_h: gaps.region,
            toggle: (gaps.group + gaps.group, gaps.group + gaps.tight),
            gaps,
        }
    }
}

impl Default for WriteSetupPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl WriteSetupPanel {
    pub fn new() -> Self {
        Self {
            wifi_ssid: TextInput::local(),
            wifi_psk: TextInput::local().with_secret(true),
            device_label: TextInput::local().with_kind(TextInputKind::Name),
            focused: None,
            last_mouse: Point::ZERO,
            metrics: SetupMetrics::default(),
        }
    }

    pub fn clear_focus(&mut self) {
        self.blur();
    }

    pub fn is_focused(&self) -> bool {
        self.focused.is_some()
    }

    pub fn measure_layout(&mut self, tr: &mut TextRenderer, theme: &Theme) -> bool {
        let line = |tr: &mut TextRenderer, role: TextRole| {
            f64::from(tr.measure_style("Ag", None, &theme.text(role)).1)
        };
        let metrics = &theme.tokens.metrics;
        let next = SetupMetrics {
            section_h: line(tr, TextRole::Subheading),
            label_h: line(tr, TextRole::Label),
            field_h: ButtonFace::height(ControlSize::Regular, theme),
            toggle: (
                f64::from(metrics.bar_control_height + metrics.icon_lg),
                f64::from(metrics.bar_control_height),
            ),
            gaps: Gaps::of(&theme.tokens.spacing),
        };
        let changed = next != self.metrics;
        self.metrics = next;
        changed
    }

    /// Height occupied below `top` inside `content`.
    pub fn extent(&self, content: Rect, top: f64) -> f64 {
        self.geom(content, top).enroll.y1 - top + self.metrics.gaps.group
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
        let m = &self.metrics;
        let g = m.gaps;
        let fh = m.field_h;
        let caption = m.label_h + g.tight;
        let row = fh + g.section + caption;
        let section_y = top;
        let ssid_y = section_y + m.section_h + g.related + caption;
        let wifi_ssid = Rect::new(x0, ssid_y, x1, ssid_y + fh);
        let psk_y = ssid_y + row;
        let wifi_psk = Rect::new(x0, psk_y, x1, psk_y + fh);
        let label_y = psk_y + row;
        let device_label = Rect::new(x0, label_y, x1, label_y + fh);
        let enroll_y = label_y + row;
        let enroll = Rect::new(x0, enroll_y, x1, enroll_y + fh);
        let (toggle_w, toggle_h) = m.toggle;
        let enroll_toggle = Rect::new(
            x1 - toggle_w,
            enroll_y + (fh - toggle_h) / 2.0,
            x1,
            enroll_y + (fh - toggle_h) / 2.0 + toggle_h,
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

    fn dispatch(
        doc: &mut AutoCommit,
        host: &mut crate::resource::ActionSink,
        action: &ImagerAction,
    ) {
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
        let ink = ctx.theme.text_primary;
        ctx.text_role(
            SECTION_TITLE,
            TextRole::Subheading,
            ink,
            Point::new(content.x0, g.section_y),
            None,
        );
        for field in [
            SetupField::WifiSsid,
            SetupField::WifiPsk,
            SetupField::DeviceLabel,
        ] {
            let r = g.field(field);
            self.draw_caption(ctx, field.label(), r);
            if let Some(input) = self.input(field) {
                input.apply_layout_external(r);
                Widget::draw(
                    input,
                    ctx.scene,
                    ctx.transform,
                    ctx.text,
                    ctx.theme,
                    presence,
                );
            }
        }
        self.draw_enroll(ctx, &g, profile.enroll_local_identity);
    }

    /// A field's caption on its own line box a `tight` step above it.
    fn draw_caption(&self, ctx: &mut PaintCtx<'_>, label: &str, field: Rect) {
        let m = &self.metrics;
        let dim = ctx.theme.text_dim;
        ctx.text_role(
            label,
            TextRole::Label,
            dim,
            Point::new(field.x0, field.y0 - m.gaps.tight - m.label_h),
            None,
        );
    }

    fn draw_enroll(&self, ctx: &mut PaintCtx<'_>, g: &SetupGeom, on: bool) {
        let r = g.enroll;
        self.draw_caption(ctx, SetupField::Enroll.label(), r);
        let room = (g.enroll_toggle.x0 - self.metrics.gaps.section - r.x0).max(1.0);
        let dim = ctx.theme.text_dim;
        ctx.text_role_in_band(
            ENROLL_NOTE,
            TextRole::Caption,
            dim,
            r.x0,
            r,
            Some(room as f32),
        );
        Toggle::paint(
            ctx.scene,
            ctx.transform,
            ctx.theme,
            &ToggleFace::new(g.enroll_toggle, on)
                .hovered(g.enroll_toggle.contains(self.last_mouse))
                .focused(self.focused == Some(SetupField::Enroll)),
        );
    }

    /// Route a winit event. `top` must match the draw/extent origin.
    pub(crate) fn handle_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        host: &mut crate::resource::ActionSink,
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
                    let inner = g.field(field);
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
        host: &mut crate::resource::ActionSink,
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
        host: &mut crate::resource::ActionSink,
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
        let inner = rect;
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
        let inner = rect;
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
                .with_description(ENROLL_NOTE)
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
