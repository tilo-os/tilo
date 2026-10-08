//! Current device-owner diagnostics from authenticated Wayland connections.
//! No PID comes from an application string, environment or historical receipt.
use super::state::{ClientState, State};
use crate::shell::state::TileContent;
use smithay::reexports::wayland_server::Resource;

const MAX_WINDOWS: usize = 128;

fn current_peer(client: &ClientState, room: &str, package: &str) -> Option<(u32, u64)> {
    if !client.enforce_package_id
        || client.privileged
        || client.package_id.as_deref() != Some(package)
        || client.workspace_id.as_deref() != Some(room)
    {
        return None;
    }
    let pid = client.pid?;
    let start = client.pid_start_time?;
    (crate::shell::launcher::proc_stat(pid)?.1 == start).then_some((pid, start))
}

fn host_peer_is_current(
    custody: &crate::shell::launch_readiness::HostLaunchCustody,
    client: &ClientState,
    room: &str,
) -> bool {
    !client.privileged
        && client.package_id.as_deref() == Some(custody.desktop_id.as_str())
        && client.workspace_id.as_deref() == Some(custody.room.as_str())
        && client.pid == Some(custody.peer_pid)
        && client.pid_start_time == Some(custody.peer_start)
        && custody.room == room
        && crate::shell::launcher::proc_stat(custody.peer_pid)
            .is_some_and(|(_, start)| start == custody.peer_start)
        && crate::shell::launcher::proc_stat(custody.launcher_pid)
            .is_some_and(|(_, start)| start == custody.launcher_start)
}

struct Window {
    room: crate::shell::state::RoomId,
    tab: crate::shell::state::TabId,
    surface: u64,
    package: String,
    record: u64,
}

impl State {
    fn current_window_candidates(&self) -> (Vec<Window>, u64) {
        let tree = self.workspace_tree.read();
        let mut candidates = Vec::new();
        let mut omitted = 0_u64;
        for workspace in &tree.workspaces {
            for tab in workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
            {
                let TileContent::Foreign {
                    surface_id, app_id, ..
                } = &tab.tile
                else {
                    continue;
                };
                let Some(record) = tree.local_app_record(tab.id, workspace.room, app_id) else {
                    continue;
                };
                if candidates.len() == MAX_WINDOWS {
                    omitted = omitted.saturating_add(1);
                    continue;
                }
                candidates.push(Window {
                    room: workspace.room,
                    tab: tab.id,
                    surface: *surface_id,
                    package: app_id.clone(),
                    record,
                });
            }
        }
        drop(tree);
        (candidates, omitted)
    }

    fn window_is_current(&self, window: &Window) -> bool {
        let tree = self.workspace_tree.read();
        let current = tree.local_app_record(window.tab, window.room, &window.package) == Some(window.record)
            && tree.workspaces.iter().filter(|workspace| workspace.room == window.room)
                .flat_map(|workspace| &workspace.strips).flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs).any(|tab| tab.id == window.tab && matches!(&tab.tile, TileContent::Foreign {surface_id, ..} if *surface_id == window.surface));
        drop(tree);
        current
    }

    /// Actual observed host launches, through the same owner-only state door.
    /// Client app IDs are labels; only a surface-local kernel ancestry witness
    /// can produce a row. No native package or SDK authority is inferred.
    pub fn host_window_provenance_json(&self) -> serde_json::Value {
        let tree = self.workspace_tree.read();
        let mut windows = Vec::new();
        let mut omitted = 0_u64;
        for workspace in &tree.workspaces {
            for tab in workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
            {
                let TileContent::Foreign { surface_id, .. } = &tab.tile else {
                    continue;
                };
                let Some(surface) = self
                    .client_input
                    .surface(*surface_id)
                    .filter(|surface| surface.is_alive())
                else {
                    continue;
                };
                let Some(client) = surface.client() else {
                    continue;
                };
                let Some(identity) = client.get_data::<ClientState>() else {
                    continue;
                };
                let custody = smithay::wayland::compositor::with_states(surface, |states| {
                    states
                        .data_map
                        .get::<crate::shell::launch_readiness::HostLaunchCustody>()
                        .cloned()
                });
                let Some(custody) = custody else { continue };
                if windows.len() == MAX_WINDOWS {
                    omitted = omitted.saturating_add(1);
                    continue;
                }
                if !host_peer_is_current(&custody, identity, &workspace.room.to_string()) {
                    continue;
                }
                let Some(adoption_id) = super::state::surface_adoption_id(surface) else {
                    continue;
                };
                windows.push(serde_json::json!({
                    "attempt_id": custody.attempt, "invocation": custody.invocation,
                    "adoption_id": adoption_id,
                    "room": custody.room, "tab_id": tab.id, "surface_id": surface_id,
                    "desktop_id": custody.desktop_id, "peer_pid": custody.peer_pid,
                    "peer_start_time_ticks": custody.peer_start.to_string(),
                    "launcher_pid": custody.launcher_pid,
                    "launcher_start_time_ticks": custody.launcher_start.to_string(),
                }));
            }
        }
        drop(tree);
        serde_json::json!({"method": "observed-host-launch-wayland-peer", "windows": windows, "omitted": omitted})
    }

    /// Read through owner-only `state_dump`, never agent-control or app RPC.
    /// Add the exact current input-route geometry to already authenticated workers.
    pub fn native_window_input_provenance_json(
        &self,
        frame: &super::frame::FrameBuilder,
    ) -> serde_json::Value {
        self.native_window_input_provenance_with_broker(frame, None)
    }

    /// Keep current certified checkout and measured input geometry in one projection.
    pub fn native_window_input_provenance_with_broker(
        &self,
        frame: &super::frame::FrameBuilder,
        broker: Option<&tilo_action_broker::collab_broker::AppActionBroker>,
    ) -> serde_json::Value {
        let mut proof = self.native_window_provenance_with_broker(broker);
        let tree = self.workspace_tree.read();
        let Some(fields) = proof.as_object_mut() else {
            return proof;
        };
        fields.insert(
            "input_outputs".into(),
            serde_json::json!(
                frame
                    .host()
                    .output_geometry()
                    .iter()
                    .map(|output| {
                        let rect = output.rect();
                        [rect.x0, rect.y0, rect.x1, rect.y1]
                    })
                    .collect::<Vec<_>>()
            ),
        );
        if let Some(windows) = fields
            .get_mut("windows")
            .and_then(serde_json::Value::as_array_mut)
        {
            for window in windows {
                let surface = window.get("surface_id").and_then(serde_json::Value::as_u64);
                let hit = surface.and_then(|surface| {
                    crate::shell::input_policy::foreign_tile_for_surface(
                        &tree,
                        frame.host(),
                        surface,
                    )
                });
                if let Some(fields) = window.as_object_mut() {
                    fields.insert(
                        "input_rect".into(),
                        hit.map_or(serde_json::Value::Null, |hit| {
                            serde_json::json!([hit.rect.x0, hit.rect.y0, hit.rect.x1, hit.rect.y1])
                        }),
                    );
                }
            }
        }
        proof
    }

    pub fn native_window_provenance_json(&self) -> serde_json::Value {
        self.native_window_provenance_with_broker(None)
    }

    /// Join current Wayland custody to the current certified broker checkout.
    pub fn native_window_provenance_with_broker(
        &self,
        broker: Option<&tilo_action_broker::collab_broker::AppActionBroker>,
    ) -> serde_json::Value {
        let (candidates, omitted) = self.current_window_candidates();
        let mut windows = Vec::new();
        for window in candidates {
            let Some(surface) = self
                .client_input
                .surface(window.surface)
                .filter(|surface| surface.is_alive())
            else {
                continue;
            };
            let Some(client) = surface.client() else {
                continue;
            };
            let Some(identity) = client.get_data::<ClientState>() else {
                continue;
            };
            let Some((pid, start)) =
                current_peer(identity, &window.room.to_string(), &window.package)
            else {
                continue;
            };
            let Some(adoption_id) = super::state::surface_adoption_id(surface) else {
                continue;
            };
            if !self.window_is_current(&window) {
                continue;
            }
            let checkout = broker.and_then(|broker| {
                broker.native_checkout(
                    &window.package,
                    &window.room.to_string(),
                    window.record,
                    pid,
                    start,
                )
            });
            if !self.window_is_current(&window)
                || current_peer(identity, &window.room.to_string(), &window.package)
                    != Some((pid, start))
            {
                continue;
            }
            windows.push(serde_json::json!({
                "certified_checkout": checkout.map(|route| serde_json::json!({
                    "application": route.app_id, "room": route.cluster_id, "record": route.record_id,
                })),
                "adoption_id": adoption_id, "room": window.room.to_string(), "tab_id": window.tab,
                "surface_id": window.surface, "package_id": window.package,
                "record_instance": window.record, "peer_pid": pid,
                "peer_start_time_ticks": start.to_string(),
            }));
        }
        serde_json::json!({"method": "authenticated-wayland-peer", "windows": windows, "omitted": omitted})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_custody_rejects_wrong_room_peer_and_reused_processes() {
        let pid = std::process::id();
        let start = crate::shell::launcher::proc_stat(pid).unwrap().1;
        let mut custody = crate::shell::launch_readiness::HostLaunchCustody {
            attempt: uuid::Uuid::new_v4(),
            invocation: None,
            desktop_id: "org.gnome.TextEditor.desktop".into(),
            room: "room".into(),
            launcher_pid: pid,
            launcher_start: start,
            peer_pid: pid,
            peer_start: start,
        };
        let mut client = ClientState {
            pid: Some(pid),
            pid_start_time: Some(start),
            package_id: Some(custody.desktop_id.clone()),
            workspace_id: Some(custody.room.clone()),
            ..ClientState::default()
        };
        assert!(host_peer_is_current(&custody, &client, "room"));
        assert!(!host_peer_is_current(&custody, &client, "foreign"));
        client.privileged = true;
        assert!(!host_peer_is_current(&custody, &client, "room"));
        client.privileged = false;
        client.pid = None;
        assert!(!host_peer_is_current(&custody, &client, "room"));
        client.pid = Some(pid);
        client.package_id = Some("foreign.desktop".into());
        assert!(!host_peer_is_current(&custody, &client, "room"));
        client.package_id = Some(custody.desktop_id.clone());
        client.workspace_id = Some("foreign".into());
        assert!(!host_peer_is_current(&custody, &client, "room"));
        client.workspace_id = Some(custody.room.clone());
        custody.launcher_start = start.checked_add(1).unwrap();
        assert!(!host_peer_is_current(&custody, &client, "room"));
        custody.launcher_start = start;
        custody.peer_start = start.checked_add(1).unwrap();
        assert!(!host_peer_is_current(&custody, &client, "room"));
    }

    #[test]
    fn current_authenticated_peer_rejects_foreign_custody_and_pid_reuse() {
        let room = "authenticated-room";
        let package = "so.tilo.calculator";
        let pid = std::process::id();
        let (_, start) = crate::shell::launcher::proc_stat(pid).expect("actual kernel process");
        let make = || ClientState {
            package_id: Some(package.into()),
            workspace_id: Some(room.into()),
            enforce_package_id: true,
            pid: Some(pid),
            pid_start_time: Some(start),
            ..ClientState::default()
        };
        assert_eq!(current_peer(&make(), room, package), Some((pid, start)));
        assert_eq!(current_peer(&make(), "foreign-room", package), None);
        assert_eq!(current_peer(&make(), room, "foreign-package"), None);
        let mut reused = make();
        reused.pid_start_time = start.checked_add(1);
        assert_eq!(current_peer(&reused, room, package), None);
        let mut missing = make();
        missing.pid_start_time = None;
        assert_eq!(current_peer(&missing, room, package), None);
        let mut unconfined = make();
        unconfined.enforce_package_id = false;
        assert_eq!(current_peer(&unconfined, room, package), None);
        let mut privileged = make();
        privileged.privileged = true;
        assert_eq!(current_peer(&privileged, room, package), None);
    }
}
