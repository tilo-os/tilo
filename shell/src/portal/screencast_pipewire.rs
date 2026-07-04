//! Pipewire stream provisioning for the `ScreenCast` portal backend.
//!
//! Sandbox Phase 4.3 follow-up: previously, `ScreenCast.Start` returned
//! `streams: vec![]` on Allow. This module supplies the abstraction
//! the portal calls into to actually allocate a Pipewire node and
//! return its id + properties to the frontend.
//!
//! ## Architecture
//!
//! The trait [`ScreenCastStreamProvisioner`] is the surface the
//! portal interacts with. Two implementors are imagined:
//!
//! - [`MockScreenCastProvisioner`] — used by tests and by the
//!   `pipewire`-feature-disabled build. Returns a synthetic node id
//!   so the portal response shape is correct without touching the
//!   system Pipewire daemon.
//! - `RealPipewireProvisioner` (gated behind the `pipewire` crate
//!   feature in `Cargo.toml`) — opens a connection to the system
//!   Pipewire daemon, creates a video stream of the workspace's
//!   composed scene at the negotiated dimensions, and returns the
//!   real node id. Stream content delivery is a v1 step: the
//!   provisioner just sets up the node; the compositor's frame loop
//!   pushes buffers into it.
//!
//! ## Why mock is the default
//!
//! Pipewire is a C-library system dep. We can't assume it's
//! available on every build host (Cargo's `cargo test -p
//! quick-desktop --lib` runs in CI, distrobox, dev laptops). The
//! `pipewire` crate is therefore an optional dep in `Cargo.toml`,
//! and the `MockScreenCastProvisioner` is the default that ships
//! when the feature is off. Production OS builds turn the feature
//! on; the resulting binary links against `libpipewire-0.3`.
//!
//! ## v1 limitation: owner's-scene-only
//!
//! The first-pass ScreenCast stream carries the workspace owner's
//! own composed scene — the same surface their wgpu renderer paints
//! into the offscreen output buffer. Multi-surface composition
//! (peers, overlay layer, etc.) is a v2 follow-up; the same shape
//! the Phase 4 peer-sync code takes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::zvariant::{OwnedValue, Value};

use super::PortalCaller;

/// What the provisioner needs to spin up a stream.
#[derive(Clone, Debug)]
pub struct PipewireStreamRequest {
    /// Identity of the requesting app — used for `pw.client.app.name`
    /// on the stream's property dict.
    pub caller: PortalCaller,
    /// Width in pixels of the stream's video frames. Negotiated with
    /// the frontend during `SelectSources`.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Target framerate. v1 hard-codes to 30 unless the frontend
    /// asked for a specific rate.
    pub framerate: u32,
}

impl Default for PipewireStreamRequest {
    fn default() -> Self {
        Self {
            caller: PortalCaller::new(crate::shell::state::WorkspaceId(0), "unknown"),
            width: 1920,
            height: 1080,
            framerate: 30,
        }
    }
}

/// Production-shape description of a provisioned Pipewire stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipewireStreamInfo {
    /// Pipewire node id (uint32) — the frontend uses this with
    /// `pipewire-stream-1.0` to attach to the producer side.
    pub node_id: u32,
    /// Pipewire `source_type`: 1 = monitor, 2 = window, 4 = virtual.
    /// v1 always emits 1 (monitor) — we're sharing the workspace's
    /// rendered output as a single "monitor" surface.
    pub source_type: u32,
    /// `id` field — opaque, used by the frontend to disambiguate
    /// multiple streams from the same node. v1 always 0.
    pub id: u32,
    /// `(width, height)` of the stream's video frames.
    pub size: (u32, u32),
}

impl PipewireStreamInfo {
    /// Serialise into the `(node_id, properties_dict)` pair the
    /// `streams` field of `ScreenCast.Start`'s response expects.
    ///
    /// `properties_dict` carries `id`, `size`, and `source_type`
    /// per the xdg-desktop-portal spec.
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
        (self.node_id, props)
    }
}

/// Recoverable errors from the provisioner. The portal layer turns
/// these into `response=2` (other error) per the freedesktop.org
/// portal contract.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ProvisioningError {
    /// The Pipewire daemon couldn't be reached. Production: log +
    /// fall through to deny; the user sees the consent dialog
    /// succeed and the streaming session fail with a clear error.
    #[error("pipewire daemon unreachable: {0}")]
    DaemonUnreachable(String),
    /// Stream creation failed mid-handshake — usually a format
    /// rejection by the daemon.
    #[error("pipewire stream setup failed: {0}")]
    StreamSetup(String),
    /// Generic "provisioner not available" — used when the
    /// `pipewire` feature isn't compiled in. Falls through to a
    /// synthetic stream id from the mock provisioner.
    #[error("provisioner not implemented")]
    NotImplemented,
}

/// Host-side contract: provision a Pipewire stream the frontend can
/// attach to. Synchronous because the Pipewire C API itself is.
/// Production callers run this from inside a tokio-blocking task or
/// on the dedicated compositor thread.
pub trait ScreenCastStreamProvisioner: Send + Sync {
    fn provision_stream(
        &self,
        req: &PipewireStreamRequest,
    ) -> Result<PipewireStreamInfo, ProvisioningError>;
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
}

/// Mock provisioner — returns a fixed node id. Used by the
/// `pipewire`-disabled build and by all tests.
pub struct MockScreenCastProvisioner {
    next_node: Mutex<u32>,
    /// Tracks the requests seen so tests can assert on the caller +
    /// dimensions the backend forwarded.
    seen: Mutex<Vec<PipewireStreamRequest>>,
    name_hint: String,
}

impl MockScreenCastProvisioner {
    /// Build a mock whose first response carries `start_node_id`.
    /// Subsequent responses increment by 1.
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

    /// Read-only access to the descriptive label set at construction
    /// time. Used by tests asserting the mock's wiring without
    /// duplicating the name in the test body.
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
            source_type: 1, // monitor
            id: 0,
            size: (req.width, req.height),
        })
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
        };
        let a = m.provision_stream(&req).expect("first stream");
        let b = m.provision_stream(&req).expect("second stream");
        assert_eq!(a.node_id, 7);
        assert_eq!(b.node_id, 8);
        // size matches the request.
        assert_eq!(a.size, (1920, 1080));
        // source_type 1 == monitor — v1 only emits monitor streams.
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
        };
        let (node_id, props) = info.into_stream_tuple();
        assert_eq!(node_id, 42);
        // The freedesktop ScreenCast spec requires `size` to be present;
        // `id` and `source_type` are optional but xdg-desktop-portal's
        // frontends consume them.
        assert!(props.contains_key("size"));
        assert!(props.contains_key("source_type"));
        assert!(props.contains_key("id"));
    }
}
