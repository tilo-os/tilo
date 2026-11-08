//! What the session knows about its GPU, kept where `tilo system health --gpu` reads it.
//! A session that renders on the CPU looks like a slow session, not a broken one: every app still
//! draws, just seconds late. This record collects the one decision that puts the session and its
//! apps on a GPU or off it — the device `zwp_linux_dmabuf_v1` tells clients to allocate on — so the
//! health check can call a software fallback what it is.
//! The file is `$XDG_RUNTIME_DIR/tilo/gpu-health.json` , replaced whole when the decision changes.
//! Its field names are the file's contract with `tilo system health --gpu` .

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Name of the record under `$XDG_RUNTIME_DIR/tilo/` .
pub const FILE_NAME: &str = "gpu-health.json";

/// The record as written.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct GpuHealth {
    /// The render node the session runs on, e.g. `/dev/dri/renderD128` .
    pub node: Option<String>,
    /// Its kernel driver, as sysfs names it.
    pub driver: Option<String>,
    /// The Vulkan device reported for the node: the launcher pins drivers per app, so the session
    /// itself pins none.
    pub vulkan_device: Option<String>,
    /// The Vulkan ICD manifest pinned for apps, which the session does not do.
    pub vulkan_icd: Option<String>,
    /// The node advertised as `zwp_linux_dmabuf_v1` 's main device: where clients allocate. `None`
    /// while not advertised.
    pub dmabuf_device: Option<String>,
    /// Render nodes with no GPU behind them, with the reason, by path.
    pub nodes_without_gpu: BTreeMap<String, String>,
    /// Every reason the session is not on the GPU. Empty is healthy.
    pub problems: Vec<String>,
    /// The adapter line each app last reported, by package id.
    pub app_adapters: BTreeMap<String, String>,
}

fn path() -> PathBuf {
    tilo_paths::xdg::runtime_dir_or_default()
        .join("tilo")
        .join(FILE_NAME)
}

/// Record where clients allocate: `device` is the node the session hands them, or none when no
/// GPU-backed global went out.
pub(crate) fn record_dmabuf(device: Option<&Path>) {
    let record = GpuHealth {
        node: device.map(|node| node.display().to_string()),
        driver: device.and_then(driver_of),
        dmabuf_device: device.map(|node| node.display().to_string()),
        problems: match device {
            Some(_) => Vec::new(),
            None => vec![
                "zwp_linux_dmabuf_v1 is not advertised, so apps cannot present from a GPU"
                    .to_owned(),
            ],
        },
        ..GpuHealth::default()
    };
    write(&record);
}

/// The kernel driver behind a render node, as sysfs names it.
fn driver_of(node: &Path) -> Option<String> {
    let name = node.file_name()?;
    let driver = Path::new("/sys/class/drm")
        .join(name)
        .join("device/driver")
        .read_link()
        .ok()?;
    driver
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// Replace the record whole. A runtime directory the session cannot write says so once, in its log,
/// and the health check reports the session as having said nothing.
fn write(record: &GpuHealth) {
    let path = path();
    let Ok(bytes) = serde_json::to_vec_pretty(record) else {
        return;
    };
    if let Err(error) = tilo_files::native::atomic::write_creating_parent(&path, &bytes) {
        tracing::warn!(
            path = %path.display(),
            %error,
            "could not write the GPU health record"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_without_a_gpu_records_why() {
        let mut record = GpuHealth::default();
        record.problems.push(
            "zwp_linux_dmabuf_v1 is not advertised, so apps cannot present from a GPU".to_owned(),
        );
        let json = serde_json::to_value(&record).expect("the record serializes");
        assert!(json["dmabuf_device"].is_null(), "{json}");
        assert!(json["problems"].is_array(), "{json}");
        assert!(json["app_adapters"].is_object(), "{json}");
    }
}
