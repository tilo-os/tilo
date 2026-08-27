//! Which GPU the compositor tells `zwp_linux_dmabuf_v1` clients to
//! allocate on.
//!
//! # Why a compositor has to answer this at all
//!
//! Mesa's EGL and Vulkan Wayland backends no longer have a way to guess
//! the GPU. `wl_drm` — the legacy global that used to hand a client the
//! device node — was removed, so the *only* remaining channel is
//! `zwp_linux_dmabuf_v1` version 4's `dmabuf_feedback.main_device` event.
//! A compositor that creates the dmabuf global without feedback offers
//! clients a protocol they cannot bootstrap: EGL asks for the device,
//! gets nothing, and reports
//!
//! ```text
//! libEGL warning: failed to get driver name for fd -1
//! libEGL warning: MESA-LOADER: failed to retrieve device information
//! ```
//!
//! after which the client's window creation fails and no toplevel is ever
//! mapped. Advertising the main device is therefore not an optimisation —
//! it is the difference between GPU clients working and not starting.
//!
//! # Which node
//!
//! The compositor imports a client's dmabuf by **CPU-mapping its plane
//! fd** (see [`crate::compositor::foreign::import_dmabuf_rgba`]). That
//! only works on a driver whose dma-buf exporter implements `mmap`.
//! Mesa/DRM drivers (amdgpu, i915, xe, nouveau, …) do; NVIDIA's
//! proprietary `nvidia` module does not — its dma-buf fds are
//! export-only, so a client steered onto it would produce buffers the
//! compositor could not read, and every tile would show an import error
//! instead of the client.
//!
//! So the probe:
//!
//! 1. honours `QOS_DMABUF_DEVICE` (an explicit render-node path) if set,
//! 2. otherwise walks `/dev/dri/renderD*` in numeric order and picks the
//!    first node that is openable **and** bound to a driver known to
//!    support dma-buf CPU mapping,
//! 3. and reports `None` when nothing qualifies — in which case the
//!    caller must not advertise dmabuf at all, so clients fall back to
//!    `wl_shm` (slower, but it renders) rather than negotiating a buffer
//!    the compositor would have to reject.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Env override: absolute path to the DRM render node to advertise as
/// the dmabuf main device. Set this on a multi-GPU box to pin clients to
/// a specific GPU, or to force a node the driver allow-list below
/// declines.
pub const DEVICE_ENV: &str = "QOS_DMABUF_DEVICE";

/// Kernel DRM drivers whose exported dma-bufs can be CPU-mapped.
///
/// Every entry here is a driver that routes `dma_buf_ops->mmap` through
/// the DRM GEM prime helpers, which is what
/// [`crate::compositor::foreign::import_dmabuf_rgba`] relies on. The
/// list is an allow-list rather than a deny-list on purpose: an unknown
/// driver silently producing unreadable buffers would show up as a wall
/// of import errors on every tile, whereas an unknown driver that falls
/// back to `wl_shm` merely renders more slowly.
const MAPPABLE_DRIVERS: &[&str] = &[
    "amdgpu",
    "i915",
    "xe",
    "radeon",
    "nouveau",
    "msm",
    "v3d",
    "vc4",
    "lima",
    "panfrost",
    "panthor",
    "etnaviv",
    "virtio_gpu",
    "vgem",
    "vkms",
];

/// A DRM render node the compositor is willing to advertise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderNode {
    /// e.g. `/dev/dri/renderD128`.
    pub path: PathBuf,
    /// `st_rdev` of `path` — the value that goes into
    /// `zwp_linux_dmabuf_feedback_v1.main_device`.
    pub dev: libc::dev_t,
    /// Kernel driver bound to the node's PCI/platform device, as read
    /// from sysfs. `None` when sysfs didn't answer (an explicitly
    /// overridden node is accepted regardless).
    pub driver: Option<String>,
}

/// Why no render node could be advertised. Carried into the boot log so
/// "clients are on shm" is always explained rather than inferred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    /// `/dev/dri` has no `renderD*` entries (headless container, no GPU).
    NoRenderNodes,
    /// Nodes exist but none passed. Carries `(path, reason)` per node.
    NoneUsable(Vec<(PathBuf, String)>),
    /// `QOS_DMABUF_DEVICE` was set but unusable.
    OverrideUnusable { path: PathBuf, reason: String },
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProbeError::NoRenderNodes => {
                write!(f, "no /dev/dri/renderD* nodes exist on this system")
            }
            ProbeError::NoneUsable(rejected) => {
                write!(f, "no usable render node (")?;
                for (i, (path, why)) in rejected.iter().enumerate() {
                    if i > 0 {
                        write!(f, "; ")?;
                    }
                    write!(f, "{}: {why}", path.display())?;
                }
                write!(f, ")")
            }
            ProbeError::OverrideUnusable { path, reason } => {
                write!(f, "{DEVICE_ENV}={} is unusable: {reason}", path.display())
            }
        }
    }
}

impl std::error::Error for ProbeError {}

/// Driver name for a render node, read out of sysfs.
///
/// `/dev/dri/renderD128` → `/sys/class/drm/renderD128/device/driver` →
/// symlink basename. Returns `None` when the node isn't a `renderD*`
/// name or sysfs has no such link (e.g. inside a container that bind-
/// mounted `/dev/dri` without `/sys`).
///
/// Delegates to [`launcher::gpu::driver_for`]: the launcher answers the
/// same question when it pins an app's ICDs, and two copies of this
/// lookup could disagree about which driver a node has — which is
/// exactly how a client ends up on a different GPU than the compositor.
pub fn driver_for(path: &Path) -> Option<String> {
    launcher::gpu::driver_for(path)
}

/// Whether `driver` is known to support CPU-mapping its exported
/// dma-bufs. Public so the boot log (and tests) can explain a rejection
/// without duplicating the table.
pub fn driver_supports_cpu_map(driver: &str) -> bool {
    MAPPABLE_DRIVERS.contains(&driver)
}

fn dev_of(path: &Path) -> std::io::Result<libc::dev_t> {
    use std::os::unix::fs::MetadataExt as _;
    // `metadata` follows symlinks, which is what we want: a caller may
    // point `QOS_DMABUF_DEVICE` at `/dev/dri/by-path/...`.
    Ok(std::fs::metadata(path)?.rdev())
}

/// Can the current process actually open the node? A node we can't open
/// is a node the *client* probably can't open either (they share the
/// `render` group), and advertising it would strand the client exactly
/// the way no feedback at all does.
fn opens(path: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    Ok(())
}

/// Every `/dev/dri/renderD*` on the system, sorted by name so the
/// selection is deterministic across boots.
fn render_nodes() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir("/dev/dri") else {
        return Vec::new();
    };
    let mut nodes: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("renderD"))
        })
        .collect();
    nodes.sort();
    nodes
}

/// Pick the render node to advertise as the dmabuf main device.
///
/// See the module docs for the selection rules. This is called once at
/// [`crate::compositor::state::State::new`]; it does a handful of
/// `stat`/`open` calls and no allocation-heavy work.
pub fn probe() -> Result<RenderNode, ProbeError> {
    if let Some(raw) = std::env::var_os(DEVICE_ENV) {
        let path = PathBuf::from(raw);
        if path.as_os_str().is_empty() {
            // An empty override is the documented way to say "never
            // advertise dmabuf" — treat it as an explicit opt-out.
            return Err(ProbeError::OverrideUnusable {
                path,
                reason: "set but empty (dmabuf advertisement disabled)".to_string(),
            });
        }
        return match (dev_of(&path), opens(&path)) {
            (Ok(dev), Ok(())) => Ok(RenderNode {
                driver: driver_for(&path),
                path,
                dev,
            }),
            (Err(e), _) | (_, Err(e)) => Err(ProbeError::OverrideUnusable {
                path,
                reason: e.to_string(),
            }),
        };
    }

    let nodes = render_nodes();
    if nodes.is_empty() {
        return Err(ProbeError::NoRenderNodes);
    }
    let mut rejected = Vec::new();
    for path in nodes {
        let driver = driver_for(&path);
        match driver.as_deref() {
            Some(name) if !driver_supports_cpu_map(name) => {
                rejected.push((
                    path,
                    format!("driver `{name}` does not support CPU-mapping its dma-bufs"),
                ));
                continue;
            }
            None => {
                rejected.push((path, "no driver reported in sysfs".to_string()));
                continue;
            }
            Some(_) => {}
        }
        if let Err(e) = opens(&path) {
            rejected.push((path, format!("cannot open: {e}")));
            continue;
        }
        match dev_of(&path) {
            Ok(dev) => return Ok(RenderNode { path, dev, driver }),
            Err(e) => rejected.push((path, format!("stat failed: {e}"))),
        }
    }
    Err(ProbeError::NoneUsable(rejected))
}

/// The node the running compositor advertised, published by
/// [`crate::compositor::state::State::new`].
///
/// The buffer-import side ([`crate::compositor::foreign`]) needs the
/// same node the protocol side advertised, and it is reached from a
/// process-wide store with no `State` in scope — so the decision is
/// published here once rather than threaded through every call site.
static ADVERTISED: OnceLock<PathBuf> = OnceLock::new();

/// Record the node advertised as `dmabuf_feedback.main_device`.
/// Idempotent: the first caller wins, so a second `State` in the same
/// process (tests) cannot repoint a live importer at another GPU.
pub fn set_advertised(path: PathBuf) {
    let _ = ADVERTISED.set(path);
}

/// The node recorded by [`set_advertised`], if a dmabuf global was
/// advertised at all.
pub fn advertised() -> Option<&'static Path> {
    ADVERTISED.get().map(PathBuf::as_path)
}

/// The node [`pin_process`] resolved at startup, before anything in
/// this process touched a GPU.
static PINNED: OnceLock<PathBuf> = OnceLock::new();

/// The render node this compositor settled on, whatever stage of boot
/// the caller is at.
///
/// [`advertised`] is the authoritative answer once the dmabuf global
/// exists, but it is only set when `dmabuf_feedback` could be built —
/// and a compositor that fell back to `wl_shm` still renders on a GPU,
/// which its clients still have to match. [`pin_process`] runs before
/// either, so it is the fallback.
///
/// This is the value the app launcher passes to `qos-launcher
/// --render-node`: the apps follow the compositor's actual choice
/// rather than re-deriving one of their own.
pub fn resolved() -> Option<&'static Path> {
    advertised().or_else(|| PINNED.get().map(PathBuf::as_path))
}

/// What [`pin_process`] did, for the boot log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessPin {
    /// The node this process (and everything it launches) is on.
    pub node: PathBuf,
    /// Kernel driver bound to it.
    pub driver: Option<String>,
    /// `(variable, value)` pairs actually written into the environment.
    pub applied: Vec<(String, String)>,
    /// Lines explaining anything that was *not* applied: a loader with
    /// no matching manifest, or a variable the operator had already set.
    pub notes: Vec<String>,
}

/// Point this process's EGL and Vulkan loaders at the GPU the
/// compositor is going to render on, before either loader has run.
///
/// The compositor picks its render node from the DRM device list (see
/// [`probe`]), but its *own* renderer goes through wgpu, which asks the
/// Vulkan loader and GLVND — neither of which knows about that choice.
/// On a hybrid machine they answer with whatever driver they rank
/// first, so the compositor can end up initialising on one GPU while
/// telling its clients to allocate on another, or blocking in a
/// half-installed vendor driver it was never going to use.
///
/// # Ordering
///
/// Must be called from `main` before any thread is spawned and before
/// anything creates a wgpu instance — `set_var` is only sound while the
/// process is single-threaded, and the loaders read these variables
/// once, at first use.
///
/// # Operator override
///
/// A variable the caller's environment already carries is left alone
/// and recorded in [`ProcessPin::notes`]. Someone who exported
/// `VK_DRIVER_FILES` by hand outranks a derivation, and combined with
/// [`DEVICE_ENV`] (which chooses the node itself) that gives a complete
/// manual escape hatch.
pub fn pin_process() -> Option<ProcessPin> {
    let node = match probe() {
        Ok(node) => node,
        Err(e) => {
            // Nothing to pin to. Not fatal: `State::new` logs the same
            // rejection in prose and the compositor falls back to shm.
            tracing::debug!(target: "shell::compositor::gpu", "no render node to pin to: {e}");
            return None;
        }
    };
    let _ = PINNED.set(node.path.clone());
    let selection =
        launcher::gpu::select_for_node(&node.path, &launcher::gpu::IcdSearch::default());
    let mut applied = Vec::new();
    let mut notes = selection.notes.clone();
    for (key, value) in selection.env() {
        if let Some(existing) = std::env::var_os(key) {
            notes.push(format!(
                "{key} already set to {:?} in the environment; leaving the operator's \
                 choice in place",
                existing.to_string_lossy()
            ));
            continue;
        }
        // SAFETY: called from `main` before any thread is spawned, so
        // no other thread can be reading the environment concurrently.
        unsafe { std::env::set_var(key, &value) };
        applied.push((key.to_string(), value));
    }
    Some(ProcessPin {
        node: node.path,
        driver: node.driver,
        applied,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvidia_proprietary_is_not_advertised_for_cpu_import() {
        // The whole point of the allow-list: `nvidia` exports dma-bufs
        // that cannot be mmap'd, so steering a client there would make
        // every foreign tile an import error.
        assert!(!driver_supports_cpu_map("nvidia"));
        assert!(!driver_supports_cpu_map("nvidia-drm"));
    }

    #[test]
    fn mesa_drm_drivers_are_advertised() {
        for d in ["amdgpu", "i915", "xe", "nouveau", "virtio_gpu"] {
            assert!(
                driver_supports_cpu_map(d),
                "`{d}` exports CPU-mappable dma-bufs and must be advertisable"
            );
        }
    }

    #[test]
    fn probe_result_is_self_describing() {
        // Every rejection path has to be able to say *why* in the boot
        // log — "clients silently fell back to shm" is exactly the
        // failure mode this module exists to prevent.
        let e = ProbeError::NoneUsable(vec![(
            PathBuf::from("/dev/dri/renderD129"),
            "driver `nvidia` does not support CPU-mapping its dma-bufs".into(),
        )]);
        let text = e.to_string();
        assert!(text.contains("renderD129"), "{text}");
        assert!(text.contains("nvidia"), "{text}");

        assert!(
            ProbeError::NoRenderNodes.to_string().contains("/dev/dri"),
            "the no-GPU case must name the directory it looked in"
        );
    }

    #[test]
    fn driver_lookup_rejects_paths_without_a_sysfs_entry() {
        assert_eq!(driver_for(Path::new("/dev/null")), None);
        assert_eq!(driver_for(Path::new("/dev/dri/renderD9999")), None);
    }

    /// On a machine that actually has a Mesa GPU the probe must succeed
    /// and hand back a node whose `dev` matches `stat(2)` — that number
    /// is what goes on the wire as `main_device`, and a wrong one sends
    /// the client to the wrong GPU (or nowhere).
    #[test]
    fn probe_reports_a_stat_consistent_device_number() {
        // Env-independent: skip when the box genuinely has no mappable
        // GPU rather than asserting hardware into existence.
        let Ok(node) = probe() else {
            eprintln!("skipping: no CPU-mappable render node on this machine");
            return;
        };
        let expected = dev_of(&node.path).expect("probe returned a node that no longer stats");
        assert_eq!(
            node.dev, expected,
            "main_device must be the node's st_rdev; clients resolve the GPU from it"
        );
        let driver = node.driver.as_deref().unwrap_or("<none>");
        assert!(
            driver_supports_cpu_map(driver),
            "probe returned `{driver}`, which cannot CPU-map its dma-bufs"
        );
    }
}
