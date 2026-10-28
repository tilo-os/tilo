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
    (crate::shell::host_runtime::proc_stat(pid)?.1 == start).then_some((pid, start))
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
                let Some(record) = tree.local_app_record(tab.id, workspace.room(), app_id) else {
                    continue;
                };
                if candidates.len() == MAX_WINDOWS {
                    omitted = omitted.saturating_add(1);
                    continue;
                }
                candidates.push(Window {
                    room: workspace.room(),
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
            && tree.workspaces.iter().filter(|workspace| workspace.room() == window.room)
                .flat_map(|workspace| &workspace.strips).flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs).any(|tab| tab.id == window.tab && matches!(&tab.tile, TileContent::Foreign {surface_id, ..} if *surface_id == window.surface));
        drop(tree);
        current
    }

    /// Read through owner-only `state_dump`, never agent-control or app RPC.
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
    fn current_authenticated_peer_rejects_foreign_custody_and_pid_reuse() {
        let room = "authenticated-room";
        let package = "so.tilo.calculator";
        let pid = std::process::id();
        let (_, start) = crate::shell::host_runtime::proc_stat(pid).expect("actual kernel process");
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
