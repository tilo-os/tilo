//! What the session knows about its GPU, kept where `tilo system health --gpu` reads it.
//!
//! A session that renders on the CPU looks like a slow session, not a broken one: every app
//! still draws, just seconds late. Nothing used to say so — the reason sat in one log line among
//! thousands. This record collects the decisions that put the session and its apps on a GPU or
//! off it, so a health check can call a software fallback what it is:
//!
//! - the render node the compositor advertised to clients, or why none could be;
//! - the render nodes with no GPU behind them (a virtio GPU without 3D acceleration), which
//!   make a machine without a GPU rather than a session that misses one;
//! - the Vulkan driver the session pinned to it, or why none was;
//! - the adapter every app window reported (`tilo-gpu:` on its stderr), and every window that
//!   fell back to a CPU adapter although a GPU is present.
//!
//! The file is `$XDG_RUNTIME_DIR/tilo/gpu-health.json`, replaced whole on every change.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use tilo_lock::MutexRecover as _;

/// Name of the record under `$XDG_RUNTIME_DIR/tilo/`.
pub const FILE_NAME: &str = "gpu-health.json";

/// The record as written. Field names are the file's contract with `tilo system health`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct GpuHealth {
    /// The render node the session runs on, e.g. `/dev/dri/renderD128`.
    pub node: Option<String>,
    /// Its kernel driver, as sysfs names it.
    pub driver: Option<String>,
    /// The Vulkan device the session's driver reported for the node.
    pub vulkan_device: Option<String>,
    /// The Vulkan ICD manifest pinned for apps.
    pub vulkan_icd: Option<String>,
    /// The node advertised as `zwp_linux_dmabuf_v1`'s main device; `None` while not advertised.
    pub dmabuf_device: Option<String>,
    /// Render nodes with no GPU behind them, with the reason, by path. When every node on the
    /// machine is listed here there is no GPU to render on.
    pub nodes_without_gpu: BTreeMap<String, String>,
    /// Every reason the session or an app is not on the GPU. Empty is healthy.
    pub problems: Vec<String>,
    /// The adapter line each app last reported, by package id.
    pub app_adapters: BTreeMap<String, String>,
}

static RECORD: Mutex<Option<GpuHealth>> = Mutex::new(None);

fn update(change: impl FnOnce(&mut GpuHealth)) {
    let mut guard = RECORD.lock_recover();
    let record = guard.get_or_insert_with(GpuHealth::default);
    let before = record.clone();
    change(record);
    if *record == before {
        return;
    }
    let snapshot = record.clone();
    drop(guard);
    write(&snapshot);
}

fn path() -> PathBuf {
    crate::shell::launcher::runtime_dir()
        .join("tilo")
        .join(FILE_NAME)
}

fn write(record: &GpuHealth) {
    // Unit tests exercise the record, never the session's real runtime directory.
    if cfg!(test) {
        return;
    }
    let path = path();
    let Ok(bytes) = serde_json::to_vec_pretty(record) else {
        return;
    };
    let result = tilo_files::native::atomic::write_creating_parent(&path, &bytes);
    if let Err(error) = result {
        tracing::warn!(
            target: "shell::compositor::gpu",
            path = %path.display(),
            %error,
            "could not write the GPU health record"
        );
    }
}

/// The session's own driver choice, from [`crate::compositor::render_node::pin_process`].
pub fn record_selection(selection: &launcher::gpu::GpuSelection) {
    update(|record| {
        record.node = Some(selection.node.display().to_string());
        record.driver.clone_from(&selection.driver);
        record.vulkan_device.clone_from(&selection.vulkan_device);
        record.vulkan_icd = selection
            .vulkan_icd_json
            .as_ref()
            .map(|path| path.display().to_string());
        if selection.vulkan_icd_json.is_none() {
            record.problems.extend(selection.notes.iter().cloned());
        }
    });
}

/// A render node exists but none can serve clients.
pub fn record_probe_failure(reason: &str) {
    update(|record| {
        record.problems.push(format!(
            "no render node can exchange buffers with the compositor, so GPU clients cannot \
             present: {reason}"
        ));
    });
}

/// Render nodes exist, but none has a GPU behind it.
pub fn record_no_renderer(nodes: &[(std::path::PathBuf, String)]) {
    update(|record| {
        record.nodes_without_gpu.extend(
            nodes
                .iter()
                .map(|(path, why)| (path.display().to_string(), why.clone())),
        );
    });
}

/// Whether the dmabuf global went out, and on which node.
pub fn record_dmabuf(advertised: Result<&std::path::Path, &str>) {
    update(|record| match advertised {
        Ok(node) => record.dmabuf_device = Some(node.display().to_string()),
        Err(reason) => {
            record.dmabuf_device = None;
            record.problems.push(format!(
                "zwp_linux_dmabuf_v1 is not advertised, so apps cannot present from a GPU: \
                 {reason}"
            ));
        }
    });
}

/// Watch one line of an app's stderr for the adapter it reports.
pub fn observe_app_line(package_id: &str, line: &str) {
    let Some(report) = line.strip_prefix(ui::gpu::ADAPTER_LINE_PREFIX) else {
        return;
    };
    let report = report.trim();
    update(|record| {
        if report.starts_with(ui::gpu::SOFTWARE_FALLBACK_MESSAGE) {
            let problem = format!("{package_id}: {report}");
            if !record.problems.contains(&problem) {
                record.problems.push(problem);
            }
        } else {
            record
                .app_adapters
                .insert(package_id.to_owned(), report.to_owned());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodes_without_a_gpu_are_recorded_with_their_reason() {
        record_no_renderer(&[(
            std::path::PathBuf::from("/dev/dri/renderD128"),
            "virtio GPU without 3D acceleration".to_string(),
        )]);
        let record = RECORD.lock_recover().clone().unwrap_or_default();
        assert_eq!(
            record.nodes_without_gpu["/dev/dri/renderD128"],
            "virtio GPU without 3D acceleration"
        );
        let json = serde_json::to_value(&record).expect("the record serializes");
        assert!(json["nodes_without_gpu"].is_object(), "{json}");
    }

    #[test]
    fn app_adapter_lines_and_fallbacks_are_recorded() {
        observe_app_line("so.tilo.notes", "an ordinary line");
        observe_app_line(
            "so.tilo.file-explorer",
            &format!(
                "{} window adapter \"llvmpipe\" type=Cpu backend=Vulkan driver=\"llvmpipe\"",
                ui::gpu::ADAPTER_LINE_PREFIX
            ),
        );
        observe_app_line(
            "so.tilo.file-explorer",
            &format!(
                "{} {}: \"llvmpipe\" chosen over Turnip",
                ui::gpu::ADAPTER_LINE_PREFIX,
                ui::gpu::SOFTWARE_FALLBACK_MESSAGE
            ),
        );
        let record = RECORD.lock_recover().clone().unwrap_or_default();
        assert!(!record.app_adapters.contains_key("so.tilo.notes"));
        assert!(
            record.app_adapters["so.tilo.file-explorer"].contains("type=Cpu"),
            "{record:?}"
        );
        assert!(
            record
                .problems
                .iter()
                .any(|p| p.starts_with("so.tilo.file-explorer: ")
                    && p.contains(ui::gpu::SOFTWARE_FALLBACK_MESSAGE)),
            "{record:?}"
        );
    }
}
