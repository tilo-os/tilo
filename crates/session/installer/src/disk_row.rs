//! [`DiskRow`] — a full-width selectable disk row.
//!
//! `ui::Button` is fixed at 120×40 px, far too narrow for a disk label
//! like `/dev/sda — 238.5G — Samsung SSD 860 EVO`. `DiskRow` is a leaf
//! widget that fills the card width, shows the device on one line and the
//! model on a dimmer second line, and is **focusable** (so keyboard
//! arrow-nav + startup auto-focus reach it). Selecting it — by click or
//! by pressing Space/Enter while focused, or by assistive activation — fires
//! its [`Control`]: the disk's published `select` action, which the
//! [`crate::installer_app::InstallerApp`] runs through its action door.

use std::collections::HashMap;
use ui::widget::ModelPath;
use ui::widget::TaffyTreeExt;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::control::Control;
use crate::disks::Disk;

const ROW_HEIGHT: f32 = 54.0;
const PAD: f64 = 12.0;
const NAME_SIZE: f32 = 16.0;
const MODEL_SIZE: f32 = 12.0;

pub struct DiskRow {
    disk: Disk,
    /// The published action choosing this disk.
    control: Control,
    /// Whether this row is the currently-selected disk (drives the
    /// highlight). Set by the app each time it rebuilds the composition.
    selected: bool,
    rect: Rect,
    is_hovered: bool,
    is_focused: bool,
}

impl DiskRow {
    pub fn new(disk: Disk, control: Control, selected: bool) -> Self {
        Self {
            disk,
            control,
            selected,
            rect: Rect::ZERO,
            is_hovered: false,
            is_focused: false,
        }
    }

    /// Choose this disk.
    fn fire(&self) {
        self.control.fire();
    }

    /// The stable control identity: the action it runs.
    fn control_id(&self) -> String {
        self.control.address().encode()
    }

    fn disabled(&self) -> bool {
        self.disk.blocks_install() || self.disk.read_only || !self.disk.identity_ready()
    }

    /// The short, spoken identity of this candidate. Keep the hardware model,
    /// human-formatted capacity, and currently displayed device path, but do
    /// not expose serial numbers, WWNs, or kernel device numbers to assistive
    /// technology. Those identifiers are useful for the backend's safety
    /// binding, not for choosing a disk.
    fn accessible_name(&self) -> String {
        let model = self.disk.model.trim();
        if model.is_empty() {
            format!(
                "{} storage device, device {}",
                self.disk.size.trim(),
                self.disk.name.trim()
            )
        } else {
            format!(
                "{model}, {}, device {}",
                self.disk.size.trim(),
                self.disk.name.trim()
            )
        }
    }

    fn accessible_description(&self) -> String {
        let mut sentences = vec![format!("{}.", self.disk.contents_summary())];
        if self.disk.hosts_staged_installer() {
            sentences.push(
                "This disk also holds the running installer and your existing system. Tilo is added only in the space prepared for it.".to_string(),
            );
        } else if self.disk.is_system {
            sentences.push(
                "Unavailable because this is the disk the live system is running from.".to_string(),
            );
        }
        if self.disk.read_only {
            sentences.push("Unavailable because this disk is read-only.".to_string());
        }
        if !self.disk.identity_ready() {
            sentences.push(
                "Unavailable because the installer could not verify which physical disk this is. Refresh the disk list or reconnect it."
                    .to_string(),
            );
        }
        sentences.join(" ")
    }
}

impl Widget for DiskRow {
    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(crate::full_width_row(ROW_HEIGHT), node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // `layout` only errors for a `NodeId` from a different tree, which
        // never happens here since `node` is always the id this same widget
        // received from its own `build_layout` call.
        #[allow(clippy::unwrap_used, reason = "node always belongs to this tree")]
        let l = taffy.layout(node).unwrap();
        self.rect = ui::widget::placed(l, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let raised = theme.tokens.colors.surfaces.raised;
        let bg = if self.selected {
            theme.tokens.colors.lift_on(raised, 1.0)
        } else if self.is_hovered {
            theme.tokens.colors.lift_on(raised, 0.5)
        } else {
            raised
        };
        let shape = RoundedRect::from_rect(self.rect, theme.radius_standard);
        scene.fill(Fill::NonZero, transform, bg, None, &shape);

        let (stroke_w, stroke_color) = if self.selected {
            (2.0, theme.tokens.colors.intent.inverted)
        } else if self.is_focused {
            (2.0, theme.border_focus)
        } else {
            (1.0, theme.border_standard)
        };
        scene.stroke(
            &Stroke::new(stroke_w),
            transform,
            stroke_color,
            None,
            &shape,
        );

        // Line 1: device path + size. Line 2: model (dim), if present.
        let head = format!("{}    {}", self.disk.name, self.disk.size);
        let y1 = self.rect.y0 + PAD * 0.6;
        text_renderer.draw_text(
            scene,
            &head,
            NAME_SIZE,
            if self.disabled() {
                theme.text_dim
            } else {
                theme.text_primary
            },
            transform.pre_translate(Vec2::new(self.rect.x0 + PAD, y1)),
            None,
        );
        let mut details = Vec::new();
        if !self.disk.model.is_empty() {
            details.push(self.disk.model.clone());
        }
        details.push(self.disk.contents_summary());
        if self.disk.hosts_staged_installer() {
            details.push("prepared by Tilo Imager".to_string());
        } else if self.disk.is_system {
            details.push("running system disk".to_string());
            details.push("not selectable".to_string());
        }
        if self.disk.read_only {
            details.push("read-only".to_string());
            details.push("not selectable".to_string());
        }
        if !self.disk.identity_ready() {
            details.push("identity unavailable".to_string());
            details.push("not selectable".to_string());
        }
        if !details.is_empty() {
            let y2 = y1 + f64::from(NAME_SIZE) + 4.0;
            text_renderer.draw_text(
                scene,
                &details.join(" · "),
                MODEL_SIZE,
                theme.text_dim,
                transform.pre_translate(Vec2::new(self.rect.x0 + PAD, y2)),
                None,
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                let hover = self.rect.contains(logical);
                if hover != self.is_hovered {
                    self.is_hovered = hover;
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.is_hovered && !self.disabled() {
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.is_hovered && !self.disabled() {
                    self.fire();
                    *ctx.handled = true;
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
            } if self.is_focused && !self.disabled() => {
                // A focused row behaves like every other selectable control.
                // Consuming Enter here is important: otherwise the wizard's
                // app-level Enter shortcut tries to advance the page before a
                // disk has been selected.
                if matches!(logical_key, Key::Named(NamedKey::Enter | NamedKey::Space)) {
                    self.fire();
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "DiskRow"
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.disk.name.as_str())
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        !self.disabled()
    }
    fn set_focus(&mut self, focused: bool) {
        self.is_focused = focused;
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }

    fn is_enabled(&self) -> bool {
        !self.disabled()
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        _state: &mut ui::widget::Edits<'_>,
        _root: &ModelPath,
    ) -> bool {
        if self.disabled()
            || !matches!(
                action,
                ui::a11y::Action::Activate | ui::a11y::Action::ToggleCheck
            )
        {
            return false;
        }
        self.fire();
        true
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};

        Some(
            AccessNode::new(Role::RadioButton)
                .with_semantic_key(self.control_id())
                .with_name(self.accessible_name())
                .with_description(self.accessible_description())
                .with_state(AccessState {
                    focusable: self.is_focusable(),
                    focused: self.is_focused,
                    disabled: self.disabled(),
                    // For a radio button, `checked` is the platform selection
                    // state. Setting the generic `selected` state too would
                    // make screen readers announce the choice twice.
                    checked: Some(self.selected),
                    ..Default::default()
                }),
        )
    }
}

impl ui::widget::controls::ActionControls for DiskRow {
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        vec![self.control.metadata(&self.control_id())]
    }
}

#[cfg(test)]
mod tests {
    use crate::control::Presses;
    use automerge::{AutoCommit, ROOT, ReadDoc};

    use super::*;

    fn disk() -> Disk {
        Disk {
            name: "/dev/vdb".into(),
            size: "40.0 GiB".into(),
            model: String::new(),
            size_bytes: 40 * 1024 * 1024 * 1024,
            partition_table: "gpt".into(),
            partition_table_uuid: "11111111-1111-1111-1111-111111111111".into(),
            logical_sector_size: 512,
            major_minor: "252:16".into(),
            wwn: String::new(),
            serial: "tilo-test-target".into(),
            removable: false,
            read_only: false,
            is_system: false,
            live_system_nodes: Vec::new(),
            internal_plan: None,
            partitions: Vec::new(),
        }
    }

    use ui::synthetic_input::named_key;

    fn select() -> (Control, Presses) {
        Control::detached("installer.disk:/dev/vdb", "select", serde_json::Value::Null)
    }

    fn row(target: Disk, selected: bool) -> DiskRow {
        DiskRow::new(target, select().0, selected)
    }

    fn assert_key_selects(key: NamedKey) {
        let (control, presses) = select();
        let mut row = DiskRow::new(disk(), control, false);
        row.set_focus(true);
        let mut doc = AutoCommit::new();
        let mut taffy = TaffyTree::new();
        let mut node_map = HashMap::new();
        let node = row.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let handled = ui::testing::Dispatch::new()
            .doc(&mut doc)
            .layout(&taffy, node)
            .send(&mut row, &named_key(key))
            .handled;
        assert!(handled);
        assert_eq!(
            presses.take().map(|press| press.address.encode()),
            Some(tilo_actions::ActionAddress::new("installer.disk:/dev/vdb", "select").encode())
        );
        assert_eq!(doc.length(ROOT), 0);
    }

    #[test]
    fn disk_row_selection_is_one_action_for_keys_and_assistive_activation() {
        // Phase `enter_and_space_select_a_focused_disk`.
        {
            assert_key_selects(NamedKey::Enter);
            assert_key_selects(NamedKey::Space);
        }
        // Phase `disk_without_a_live_identity_is_not_selectable`.
        {
            let mut target = disk();
            target.major_minor.clear();
            let row = row(target, false);
            assert!(!row.is_focusable());
        }
        // Phase `assistive_activation_runs_the_same_selection_action`.
        {
            let (control, presses) = select();
            let mut row = DiskRow::new(disk(), control, false);
            let mut doc = AutoCommit::new();

            assert!(row.perform_action(
                &ui::a11y::Action::Activate,
                &mut ui::widget::Edits::lend(&mut doc),
                &ROOT
            ));
            assert_eq!(
                presses.take().map(|press| press.address.action),
                Some("select".to_string())
            );
            assert_eq!(doc.length(ROOT), 0);
        }
    }

    #[test]
    fn disk_row_accessibility_names_the_disk_and_every_barrier() {
        // Phase `accessibility_names_the_human_disk_without_hardware_identifiers`.
        {
            let mut target = disk();
            target.model = "QEMU Fast Storage".into();
            target.wwn = "secret-wwn".into();
            let mut row = row(target, true);
            row.set_focus(true);

            let node = row.accessibility().expect("disk selection semantics");
            assert_eq!(node.role, ui::a11y::Role::RadioButton);
            assert_eq!(
                node.name.as_deref(),
                Some("QEMU Fast Storage, 40.0 GiB, device /dev/vdb")
            );
            assert_eq!(node.description.as_deref(), Some("Empty disk."));
            assert_eq!(node.state.checked, Some(true));
            assert!(node.state.focusable);
            assert!(node.state.focused);
            assert!(!node.state.disabled);
            assert!(
                !node
                    .name
                    .as_deref()
                    .unwrap_or_default()
                    .contains("secret-wwn")
            );
        }
        // Phase `accessibility_uses_a_clear_fallback_when_model_is_missing`.
        {
            let node = row(disk(), false)
                .accessibility()
                .expect("disk selection semantics");
            assert_eq!(
                node.name.as_deref(),
                Some("40.0 GiB storage device, device /dev/vdb")
            );
            assert_eq!(node.state.checked, Some(false));
        }
        // Phase `disabled_semantics_explain_every_safety_barrier`.
        {
            let mut target = disk();
            target.is_system = true;
            target.read_only = true;
            target.major_minor.clear();
            let (control, presses) = select();
            let mut row = DiskRow::new(target, control, false);

            let node = row.accessibility().expect("disabled disk semantics");
            let description = node.description.as_deref().expect("disabled reason");
            assert!(node.state.disabled);
            assert!(!node.state.focusable);
            assert!(description.contains("live system is running from"));
            assert!(description.contains("disk is read-only"));
            assert!(description.contains("could not verify which physical disk"));

            let mut doc = AutoCommit::new();
            assert!(!row.perform_action(
                &ui::a11y::Action::Activate,
                &mut ui::widget::Edits::lend(&mut doc),
                &ROOT
            ));
            assert!(presses.take().is_none());
        }
        // Phase `staged_installer_disk_is_selectable_and_says_what_it_holds`.
        {
            let mut target = disk();
            target.is_system = true;
            target.internal_plan = Some(crate::disks::GuidedAlongsidePlan {
                free_start_bytes: 1 << 30,
                free_size_bytes: 35 << 30,
                efi_start_bytes: 1 << 30,
                efi_size_bytes: 2 << 30,
                boot_start_bytes: 3 << 30,
                boot_size_bytes: 1 << 30,
                root_start_bytes: 4 << 30,
                root_size_bytes: 32 << 30,
            });
            let row = row(target, false);
            let node = row.accessibility().expect("staged disk semantics");
            assert!(!node.state.disabled);
            assert!(node.state.focusable);
            let description = node.description.as_deref().expect("staged description");
            assert!(description.contains("also holds the running installer"));
            assert!(!description.contains("Unavailable"));
        }
    }
}
