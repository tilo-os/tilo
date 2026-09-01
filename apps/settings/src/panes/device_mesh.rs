//! Devices & Sync — real configuration for the qOS device mesh.
//!
//! The pane reads and writes the same `RelayConfig` consumed by `qos-relay`.
//! It does not invent discovered peers or claim a route is online: until the
//! running service exposes reachability over IPC, runtime status is explicitly
//! reported as unavailable. Shared `Dropdown` and `Toggle` widgets provide
//! pointer, keyboard, focus and assistive semantics.

use std::collections::HashMap;
use std::path::PathBuf;

use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use qos_relay::{DeviceRole, RelayConfig, ReplicationIntent};
use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{Dropdown, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::widgets::{
    caption, card, cards, choice_row_described, rows, setting_row_described, value_row,
};

const DEFAULT_CONFIG: &str = "/etc/qos/relay.toml";
const ROLE_KEY: &str = "device_mesh.role";
const REPLICATION_KEY: &str = "device_mesh.replication";
const INBOUND_RELAY_KEY: &str = "device_mesh.inbound_relay";
const REMOTE_RELAY_KEY: &str = "device_mesh.remote_control_relay";
const APP_SERVING_KEY: &str = "device_mesh.app_serving";
const COMPUTE_KEY: &str = "device_mesh.compute";

const ROLE_OPTIONS: &[&str] = &[
    "Personal device",
    "Available while active",
    "Always-on server",
];
const REPLICATION_OPTIONS: &[&str] = &["On demand", "Workspace state", "Full workspace copy"];

fn role_label(role: DeviceRole) -> &'static str {
    match role {
        DeviceRole::Interactive => ROLE_OPTIONS[0],
        DeviceRole::Hybrid => ROLE_OPTIONS[1],
        DeviceRole::AlwaysOnServer => ROLE_OPTIONS[2],
    }
}

fn role_from_label(label: &str) -> Option<DeviceRole> {
    match label {
        "Personal device" => Some(DeviceRole::Interactive),
        "Available while active" => Some(DeviceRole::Hybrid),
        "Always-on server" => Some(DeviceRole::AlwaysOnServer),
        _ => None,
    }
}

fn replication_label(intent: &ReplicationIntent) -> &'static str {
    match intent {
        ReplicationIntent::WorkspaceOnly => REPLICATION_OPTIONS[1],
        ReplicationIntent::FullWorkspace => REPLICATION_OPTIONS[2],
        ReplicationIntent::OnDemand | ReplicationIntent::Resources(_) => REPLICATION_OPTIONS[0],
    }
}

fn replication_from_label(label: &str) -> Option<ReplicationIntent> {
    match label {
        "On demand" => Some(ReplicationIntent::OnDemand),
        "Workspace state" => Some(ReplicationIntent::WorkspaceOnly),
        "Full workspace copy" => Some(ReplicationIntent::FullWorkspace),
        _ => None,
    }
}

/// Editor for the daemon's real device-mesh configuration.
pub struct DeviceMeshPane {
    config_path: PathBuf,
    config: RelayConfig,
    load_status: String,
    needs_seed: bool,
    commit_context_pending: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl DeviceMeshPane {
    /// Load the system relay configuration. Missing configuration is a normal
    /// first-run state and starts from the daemon's defaults.
    pub fn new() -> Self {
        Self::from_path(PathBuf::from(DEFAULT_CONFIG))
    }

    fn from_path(config_path: PathBuf) -> Self {
        let (config, load_status) = match RelayConfig::load(&config_path) {
            Ok(config) => (config, "Configuration loaded".to_string()),
            Err(qos_relay::config::ConfigError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                (
                    RelayConfig::default(),
                    "Not configured on this device".to_string(),
                )
            }
            Err(error) => (
                RelayConfig::default(),
                format!("Configuration could not be read: {error}"),
            ),
        };
        Self {
            config_path,
            config,
            load_status,
            needs_seed: true,
            commit_context_pending: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    #[cfg(test)]
    fn with_path(path: &std::path::Path) -> Self {
        Self::from_path(path.to_path_buf())
    }

    fn common_replication(&self) -> ReplicationIntent {
        self.config
            .workspaces
            .first()
            .map_or(ReplicationIntent::FullWorkspace, |workspace| {
                workspace.replication.clone()
            })
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let role = Dropdown::new(
            ROLE_OPTIONS
                .iter()
                .map(|label| (*label).to_string())
                .collect(),
            ROLE_KEY,
        )
        .with_id("device_mesh.role")
        .with_placeholder(role_label(self.config.device_role));

        let mut residency_rows = vec![value_row(
            "Hosted workspaces",
            &self.config.workspaces.len().to_string(),
        )];
        if self.config.workspaces.is_empty() {
            residency_rows.push(caption(
                "No workspace is assigned to this server yet. Settings will not fabricate one.",
            ));
        } else {
            let replication = Dropdown::new(
                REPLICATION_OPTIONS
                    .iter()
                    .map(|label| (*label).to_string())
                    .collect(),
                REPLICATION_KEY,
            )
            .with_id("device_mesh.replication")
            .with_placeholder(replication_label(&self.common_replication()));
            residency_rows.push(choice_row_described(
                "Copies on this device",
                "A full copy retains collaboration records and file, ebook, and media bytes for every assigned workspace.",
                Box::new(replication),
            ));
        }
        residency_rows.push(caption(
            "Device sync uses the same signed collaboration and file replication as sharing with another person. Same identity changes permission, not validation. A copied file cannot be cryptographically taken back from a device that already received it.",
        ));

        let status = card(
            "This device",
            rows(vec![
                choice_row_described(
                    "Role",
                    "Personal devices do not host. Hybrid devices serve while available; always-on servers are expected to stay connected.",
                    Box::new(role),
                ),
                value_row("Configuration", &self.load_status),
                value_row("File", &self.config_path.display().to_string()),
                value_row(
                    "Runtime reachability",
                    "Not reported by the running service",
                ),
                caption(
                    "Availability is intentionally not guessed from configuration. Changes take effect when the device service reloads this file.",
                ),
            ]),
        );

        let relays = card(
            "Reachability",
            rows(vec![
                setting_row_described(
                    "Collaboration and chat relay",
                    "Accept inbound encrypted workspace and chat traffic when this server is reachable",
                    Box::new(
                        Toggle::new("settings.device_mesh.inbound_relay", INBOUND_RELAY_KEY)
                            .with_id("device_mesh.inbound_relay"),
                    ),
                ),
                setting_row_described(
                    "Remote-control relay",
                    "Try the target directly first; if that fails, tunnel through this device",
                    Box::new(
                        Toggle::new("settings.device_mesh.remote_relay", REMOTE_RELAY_KEY)
                            .with_id("device_mesh.remote_relay"),
                    ),
                ),
                value_row("Current remote-control route", "No active route reported"),
                caption(
                    "A relay only carries the encrypted session. The target device still shows consent, chooses shared surfaces, and filters every pointer or keyboard event.",
                ),
            ]),
        );

        let serving = card(
            "Apps and distributed work",
            rows(vec![
                setting_row_described(
                    "Serve app backends",
                    "Allow compatible apps to place their backend on this device",
                    Box::new(
                        Toggle::new("settings.device_mesh.app_serving", APP_SERVING_KEY)
                            .with_id("device_mesh.app_serving"),
                    ),
                ),
                setting_row_described(
                    "Accept distributed jobs",
                    "Advertise capacity for future sandboxed app jobs; this does not permit arbitrary host execution",
                    Box::new(
                        Toggle::new("settings.device_mesh.compute", COMPUTE_KEY)
                            .with_id("device_mesh.compute"),
                    ),
                ),
                caption(
                    "Multiple servers divide clients and apps with stable, capacity-aware placement. Certificate, workspace, and app scope remain hard eligibility checks.",
                ),
            ]),
        );

        cards(vec![
            status,
            card("Workspace residency", rows(residency_rows)),
            relays,
            serving,
        ])
    }

    fn read_str(state: &AutoCommit, root: &ObjId, key: &str) -> Option<String> {
        match state.get(root, key) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                automerge::ScalarValue::Str(text) => Some(text.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> Option<bool> {
        match state.get(root, key) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                automerge::ScalarValue::Boolean(value) => Some(*value),
                _ => None,
            },
            _ => None,
        }
    }

    fn apply_bool(state: &AutoCommit, root: &ObjId, key: &str, current: &mut bool) -> bool {
        if let Some(value) = Self::read_bool(state, root, key)
            && value != *current
        {
            *current = value;
            return true;
        }
        false
    }

    fn persist(&mut self) {
        self.load_status = match self.config.save(&self.config_path) {
            Ok(()) => "Saved; waiting for the device service to reload".to_string(),
            Err(error) => format!("Not saved: {error}"),
        };
    }
}

impl Default for DeviceMeshPane {
    fn default() -> Self {
        Self::new()
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(DeviceMeshPane::new())
}

impl Widget for DeviceMeshPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.needs_seed {
            let _ = state.put(root, ROLE_KEY, role_label(self.config.device_role));
            let _ = state.put(
                root,
                REPLICATION_KEY,
                replication_label(&self.common_replication()),
            );
            for (key, value) in [
                (INBOUND_RELAY_KEY, self.config.services.inbound_relay),
                (REMOTE_RELAY_KEY, self.config.services.remote_control_relay),
                (APP_SERVING_KEY, self.config.services.app_serving),
                (COMPUTE_KEY, self.config.services.compute),
            ] {
                let _ = state.put(root, key, value);
            }
            self.needs_seed = false;
        }

        let mut changed = false;
        if let Some(role) = Self::read_str(state, root, ROLE_KEY)
            .as_deref()
            .and_then(role_from_label)
            && role != self.config.device_role
        {
            self.config.device_role = role;
            changed = true;
        }
        if let Some(intent) = Self::read_str(state, root, REPLICATION_KEY)
            .as_deref()
            .and_then(replication_from_label)
            && self
                .config
                .workspaces
                .iter()
                .any(|workspace| workspace.replication != intent)
        {
            for workspace in &mut self.config.workspaces {
                workspace.replication = intent.clone();
            }
            changed = true;
        }
        changed |= Self::apply_bool(
            state,
            root,
            INBOUND_RELAY_KEY,
            &mut self.config.services.inbound_relay,
        );
        changed |= Self::apply_bool(
            state,
            root,
            REMOTE_RELAY_KEY,
            &mut self.config.services.remote_control_relay,
        );
        changed |= Self::apply_bool(
            state,
            root,
            APP_SERVING_KEY,
            &mut self.config.services.app_serving,
        );
        changed |= Self::apply_bool(state, root, COMPUTE_KEY, &mut self.config.services.compute);
        if changed {
            self.persist();
        }
        if let Some(composed) = &mut self.composed {
            composed.sync(state, root);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        let node = taffy
            .new_with_children(
                taffy::Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::auto(),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x + layout.size.width),
            origin.y + f64::from(layout.location.y + layout.size.height),
        );
        if let Some(composed) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(child) = children.first()
        {
            composed.apply_layout(taffy, *child, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(composed) = &mut self.composed {
            composed.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        let was_handled = *handled;
        if let Some(composed) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(child) = children.first()
        {
            composed.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                *child,
                taffy,
            );
        }
        self.commit_context_pending |= !was_handled && *handled;
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|child| child.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|child| child.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn take_commit_context(&mut self) -> Option<ui::qos_actions::ActionContext> {
        let pending = std::mem::take(&mut self.commit_context_pending)
            || self
                .composed
                .as_ref()
                .is_some_and(|content| content.needs_commit());
        crate::panes::preference_commit_context(
            pending,
            "devices-sync",
            "Devices & Sync",
            "Change device mesh settings",
        )
    }

    fn name(&self) -> &'static str {
        "DeviceMeshPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_relay::WorkspaceBinding;

    #[test]
    fn edits_write_the_real_relay_config_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay.toml");
        let mut config = RelayConfig::default();
        config.workspaces.push(WorkspaceBinding {
            id: 7,
            pin: true,
            replication: ReplicationIntent::OnDemand,
        });
        config.save(&path).unwrap();

        let mut pane = DeviceMeshPane::with_path(&path);
        let mut doc = AutoCommit::new();
        pane.sync(&mut doc, &automerge::ROOT);
        doc.put(&automerge::ROOT, ROLE_KEY, "Available while active")
            .unwrap();
        doc.put(&automerge::ROOT, REPLICATION_KEY, "Full workspace copy")
            .unwrap();
        doc.put(&automerge::ROOT, REMOTE_RELAY_KEY, false).unwrap();
        pane.sync(&mut doc, &automerge::ROOT);

        let saved = RelayConfig::load(&path).unwrap();
        assert_eq!(saved.device_role, DeviceRole::Hybrid);
        assert_eq!(
            saved.workspaces[0].replication,
            ReplicationIntent::FullWorkspace
        );
        assert!(!saved.services.remote_control_relay);
    }

    #[test]
    fn missing_config_is_honest_and_not_created_until_an_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay.toml");
        let mut pane = DeviceMeshPane::with_path(&path);
        assert!(pane.load_status.contains("Not configured"));
        let mut doc = AutoCommit::new();
        pane.sync(&mut doc, &automerge::ROOT);
        assert!(!path.exists());
        doc.put(&automerge::ROOT, ROLE_KEY, "Available while active")
            .unwrap();
        pane.sync(&mut doc, &automerge::ROOT);
        assert!(path.exists());
    }
}
