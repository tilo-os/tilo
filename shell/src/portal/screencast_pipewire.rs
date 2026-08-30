//! Pipewire stream provisioning for the `ScreenCast` portal backend.
//!
//! [`ScreenCastStreamProvisioner`] is the surface the portal calls into to
//! allocate a Pipewire node and return its id + properties to the
//! frontend. [`MockScreenCastProvisioner`] is the default implementor,
//! used by tests and the `pipewire`-feature-disabled build, returning a
//! synthetic node id without touching the system daemon.
//! `RealPipewireProvisioner` (behind the `pipewire` Cargo feature) opens
//! a connection to the system daemon, creates a video stream of the
//! workspace's composed scene at the negotiated dimensions, and returns
//! the real node id, which the compositor's frame loop pushes buffers into.
//!
//! Pipewire is a C-library system dep not guaranteed on every build host
//! (CI, distrobox, dev laptops), so the crate is optional and the mock
//! ships by default; production OS builds turn the feature on.
//!
//! The current stream carries only the workspace owner's own composed
//! scene; compositing in peers or the overlay layer is future work.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::zvariant::{OwnedValue, Value};

use super::PortalCaller;

/// What the provisioner needs to spin up a stream.
#[derive(Clone, Debug)]
pub struct PipewireStreamRequest {
    /// Identity of the requesting app — used for `pw.client.app.name` on the stream's property dict.
    pub caller: PortalCaller,
    /// Width in pixels of the stream's video frames, negotiated with the frontend during `SelectSources`.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Target framerate; v1 hard-codes to 30 unless the frontend asked for a specific rate.
    pub framerate: u32,
    /// The source type this stream captures: `1` monitor, `2` window, `4`
    /// virtual (see `ScreenCast.AvailableSourceTypes`), chosen from the
    /// user's `SelectSources` selection and echoed back on [`PipewireStreamInfo`].
    pub source_type: u32,
    /// Cursor embedding mode: `1` hidden, `2` embedded, `4` metadata (see
    /// `ScreenCast.AvailableCursorModes`) — the real provisioner uses this to decide whether to composite the cursor or ship it as metadata.
    pub cursor_mode: u32,
}

impl Default for PipewireStreamRequest {
    fn default() -> Self {
        Self {
            caller: PortalCaller::new(crate::shell::state::WorkspaceId(0), "unknown"),
            width: 1920,
            height: 1080,
            framerate: 30,
            source_type: 1,
            cursor_mode: 2,
        }
    }
}

/// Production-shape description of a provisioned Pipewire stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipewireStreamInfo {
    /// Pipewire node id (uint32) — the frontend uses this with `pipewire-stream-1.0` to attach to the producer side.
    pub node_id: u32,
    /// Pipewire `source_type`: 1 = monitor, 2 = window, 4 = virtual. v1
    /// always emits 1 — sharing the workspace's rendered output as a single "monitor" surface.
    pub source_type: u32,
    /// `id` field — opaque, disambiguates multiple streams from the same node for the frontend. v1 always 0.
    pub id: u32,
    /// `(width, height)` of the stream's video frames.
    pub size: (u32, u32),
    /// `(x, y)` position in the compositor coordinate space; per the
    /// `ScreenCast` spec **only** meaningful for monitor sources — [`Self::into_stream_tuple`] omits it otherwise.
    pub position: (i32, i32),
}

impl PipewireStreamInfo {
    /// Serialise into the `(node_id, properties_dict)` pair
    /// `ScreenCast.Start`'s `streams` field expects: `id`, `size`,
    /// `source_type`, and — for monitor sources — `position`.
    pub fn into_stream_tuple(self) -> (u32, HashMap<String, OwnedValue>) {
        let mut props: HashMap<String, OwnedValue> = HashMap::new();
        if let Ok(v) = OwnedValue::try_from(Value::U32(self.id)) {
            props.insert("id".to_string(), v);
        }
        if let Ok(v) = OwnedValue::try_from(Value::U32(self.source_type)) {
            props.insert("source_type".to_string(), v);
        }
        let size = (self.size.0, self.size.1);
        if let Ok(v) = OwnedValue::try_from(Value::new(size)) {
            props.insert("size".to_string(), v);
        }
        // `position` is only defined for monitor sources (source_type 1).
        if self.source_type == 1 {
            let pos = (self.position.0, self.position.1);
            if let Ok(v) = OwnedValue::try_from(Value::new(pos)) {
                props.insert("position".to_string(), v);
            }
        }
        (self.node_id, props)
    }
}

/// Recoverable errors from the provisioner. The portal layer turns these into `response=2` (other error) per the freedesktop.org portal contract.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ProvisioningError {
    /// The Pipewire daemon couldn't be reached. Production logs and falls
    /// through to deny — the consent dialog succeeds but streaming fails with a clear error.
    #[error("pipewire daemon unreachable: {0}")]
    DaemonUnreachable(String),
    /// Stream creation failed mid-handshake — usually a format rejection by the daemon.
    #[error("pipewire stream setup failed: {0}")]
    StreamSetup(String),
    /// Generic "provisioner not available" — used when the `pipewire`
    /// feature isn't compiled in; falls through to a synthetic id from the mock provisioner.
    #[error("provisioner not implemented")]
    NotImplemented,
}

/// Host-side contract: provision a Pipewire stream the frontend can
/// attach to. Synchronous because the Pipewire C API is; production runs this from a tokio-blocking task or the compositor thread.
pub trait ScreenCastStreamProvisioner: Send + Sync {
    fn provision_stream(
        &self,
        req: &PipewireStreamRequest,
    ) -> Result<PipewireStreamInfo, ProvisioningError>;

    /// Open a `PipeWire` remote connection and hand back its fd.
    ///
    /// This is what `ScreenCast.OpenPipeWireRemote` returns to the app: a fd
    /// passed to `pw_context_connect_fd` to attach to the nodes provisioned
    /// by [`Self::provision_stream`]. The real provisioner scopes it to the
    /// compositor's own connection; the mock returns a real but inert OS fd
    /// so the D-Bus fd-passing path is exercised without a daemon.
    fn open_remote_fd(&self) -> Result<std::os::fd::OwnedFd, ProvisioningError>;
}

/// Cheap-clone wrapper around `Arc<dyn ScreenCastStreamProvisioner>`.
#[derive(Clone)]
pub struct ScreenCastStreamProvisionerHandle {
    inner: Arc<dyn ScreenCastStreamProvisioner>,
}

impl ScreenCastStreamProvisionerHandle {
    pub fn new(inner: Arc<dyn ScreenCastStreamProvisioner>) -> Self {
        Self { inner }
    }

    pub fn provision(
        &self,
        req: &PipewireStreamRequest,
    ) -> Result<PipewireStreamInfo, ProvisioningError> {
        self.inner.provision_stream(req)
    }

    /// Open a `PipeWire` remote fd for `ScreenCast.OpenPipeWireRemote`.
    pub fn open_remote_fd(&self) -> Result<std::os::fd::OwnedFd, ProvisioningError> {
        self.inner.open_remote_fd()
    }
}

/// Mock provisioner — returns a fixed node id, used by the `pipewire`-disabled build and by all tests.
pub struct MockScreenCastProvisioner {
    next_node: Mutex<u32>,
    /// Tracks the requests seen so tests can assert on the caller + dimensions the backend forwarded.
    seen: Mutex<Vec<PipewireStreamRequest>>,
    name_hint: String,
}

impl MockScreenCastProvisioner {
    /// Build a mock whose first response carries `start_node_id`; subsequent responses increment by 1.
    pub fn new_with_node(start_node_id: u32, name_hint: &str) -> Self {
        Self {
            next_node: Mutex::new(start_node_id),
            seen: Mutex::new(Vec::new()),
            name_hint: name_hint.to_string(),
        }
    }

    /// Read-only access to the recorded request log.
    pub fn requests(&self) -> Vec<PipewireStreamRequest> {
        self.seen.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Read-only access to the descriptive label set at construction time —
    /// used by tests asserting the mock's wiring without duplicating the name.
    pub fn name_hint(&self) -> &str {
        &self.name_hint
    }
}

impl ScreenCastStreamProvisioner for MockScreenCastProvisioner {
    fn provision_stream(
        &self,
        req: &PipewireStreamRequest,
    ) -> Result<PipewireStreamInfo, ProvisioningError> {
        if let Ok(mut s) = self.seen.lock() {
            s.push(req.clone());
        }
        let node_id = {
            let mut n = self.next_node.lock().unwrap();
            let id = *n;
            *n = n.wrapping_add(1);
            id
        };
        Ok(PipewireStreamInfo {
            node_id,
            // Echo the requested source type so a window/virtual selection
            // round-trips through the stream props instead of being silently downgraded to monitor.
            source_type: req.source_type,
            id: 0,
            size: (req.width, req.height),
            position: (0, 0),
        })
    }

    fn open_remote_fd(&self) -> Result<std::os::fd::OwnedFd, ProvisioningError> {
        // A real but inert fd (`/dev/null`) so `OpenPipeWireRemote` actually
        // passes a usable descriptor; production returns the compositor's scoped daemon connection fd instead.
        let file = std::fs::File::open("/dev/null")
            .map_err(|e| ProvisioningError::DaemonUnreachable(e.to_string()))?;
        Ok(std::os::fd::OwnedFd::from(file))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::WorkspaceId;

    #[test]
    fn mock_returns_incrementing_node_ids() {
        let m = MockScreenCastProvisioner::new_with_node(7, "test");
        let req = PipewireStreamRequest {
            caller: PortalCaller::new(WorkspaceId(1), "app"),
            width: 1920,
            height: 1080,
            framerate: 30,
            source_type: 1,
            cursor_mode: 2,
        };
        let a = m.provision_stream(&req).expect("first stream");
        let b = m.provision_stream(&req).expect("second stream");
        assert_eq!(a.node_id, 7);
        assert_eq!(b.node_id, 8);
        // size matches the request.
        assert_eq!(a.size, (1920, 1080));
        // source_type 1 == monitor — echoed from the request.
        assert_eq!(a.source_type, 1);
    }

    #[test]
    fn mock_records_requests() {
        let m = Arc::new(MockScreenCastProvisioner::new_with_node(1, "test"));
        let h = ScreenCastStreamProvisionerHandle::new(m.clone());
        let req = PipewireStreamRequest {
            caller: PortalCaller::new(WorkspaceId(5), "org.example"),
            width: 1280,
            height: 720,
            framerate: 30,
            source_type: 1,
            cursor_mode: 2,
        };
        let _ = h.provision(&req).unwrap();
        let seen = m.requests();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].caller.app_id, "org.example");
        assert_eq!(seen[0].width, 1280);
    }

    #[test]
    fn stream_info_serialises_into_dict_with_required_keys() {
        let info = PipewireStreamInfo {
            node_id: 42,
            source_type: 1,
            id: 0,
            size: (640, 480),
            position: (100, 200),
        };
        let (node_id, props) = info.into_stream_tuple();
        assert_eq!(node_id, 42);
        // The freedesktop ScreenCast spec requires `size`; `id`/`source_type`
        // are optional but xdg-desktop-portal's frontends consume them.
        assert!(props.contains_key("size"));
        assert!(props.contains_key("source_type"));
        assert!(props.contains_key("id"));
        // `position` is present for monitor sources.
        assert!(props.contains_key("position"));
    }

    #[test]
    fn stream_info_omits_position_for_window_source() {
        // Window (2) / virtual (4) sources have no compositor-space position; the spec says `position` is monitor-only.
        let info = PipewireStreamInfo {
            node_id: 9,
            source_type: 2,
            id: 0,
            size: (800, 600),
            position: (5, 5),
        };
        let (_node, props) = info.into_stream_tuple();
        assert!(!props.contains_key("position"));
        assert!(props.contains_key("size"));
    }

    #[test]
    fn mock_open_remote_fd_returns_real_fd() {
        use std::os::fd::AsRawFd;
        let m = MockScreenCastProvisioner::new_with_node(1, "test");
        let fd = m.open_remote_fd().expect("mock opens a real fd");
        assert!(fd.as_raw_fd() >= 0, "must be a valid OS fd");
    }

    #[test]
    fn mock_echoes_requested_source_type() {
        let m = MockScreenCastProvisioner::new_with_node(1, "test");
        let mut req = PipewireStreamRequest {
            caller: PortalCaller::new(WorkspaceId(1), "app"),
            width: 100,
            height: 100,
            framerate: 30,
            source_type: 4, // virtual
            cursor_mode: 4,
        };
        let info = m.provision_stream(&req).unwrap();
        assert_eq!(info.source_type, 4, "virtual selection round-trips");
        req.source_type = 2; // window
        let info2 = m.provision_stream(&req).unwrap();
        assert_eq!(info2.source_type, 2);
    }
}
