//! Which GPU the compositor tells `zwp_linux_dmabuf_v1` clients to allocate on.
//!
//! Mesa's EGL/Vulkan Wayland backends can no longer guess the GPU — `wl_drm` was removed, so
//! the only remaining channel is `dmabuf_feedback.main_device`. Without it, EGL asks for the
//! device, gets nothing, and reports:
//!
//! ```text
//! libEGL warning: failed to get driver name for fd -1
//! libEGL warning: MESA-LOADER: failed to retrieve device information
//! ```
//!
//! after which window creation fails and no toplevel ever maps — advertising the main device is
//! the difference between GPU clients working and not starting.
//!
//! The compositor reads a client's dmabuf back either by **CPU-mapping its plane fd** (see
//! [`crate::compositor::foreign::import_dmabuf_rgba`]) or, where the CPU can't map it, through a GL
//! import ([`crate::compositor::gpu_import`]). Whether a node's buffers survive that trip is asked
//! of the node itself, not looked up by driver name: GBM allocates a small linear buffer there,
//! exports it as a dmabuf, and the compositor imports it the way it would a client's. A name
//! table missed `msm_dpu` — the platform driver a Snapdragon X render node reports — and every
//! app on that laptop rendered on lavapipe instead of its GPU; a GPU nobody listed now works the
//! moment its buffers do.
//!
//! The probe: (1) honours `TILO_DMABUF_DEVICE` if set, (2) otherwise walks `/dev/dri/renderD*` in
//! name order for the first node whose buffers can be exchanged, (3) reports every node's reason
//! when nothing qualifies, so the caller falls back to `wl_shm` loudly rather than negotiating a
//! buffer it would have to reject. Buffer exchange loads the GPU driver, so it runs where
//! [`launcher::gpu::in_isolation`] puts it: a forked child while the compositor is still
//! single-threaded.
//!
//! A render node is not always a GPU. A virtio GPU without 3D acceleration (QEMU's plain
//! `virtio-vga`, a cloud VM's display) still registers `renderD128`, but nothing renders behind
//! it: Mesa falls back to dumb buffers, which the kernel refuses on render nodes with `EACCES`.
//! Such a machine has no GPU to miss, so those nodes are told apart
//! ([`ProbeError::NoRenderer`]) from a GPU whose buffers fail to exchange, which is a fault.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Env override: absolute path to the DRM render node to advertise as the dmabuf main device — pin clients to a specific GPU on a multi-GPU box, or force a node whose buffer exchange check fails.
pub const DEVICE_ENV: &str = "TILO_DMABUF_DEVICE";

/// A DRM render node the compositor is willing to advertise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderNode {
    /// e.g. `/dev/dri/renderD128`.
    pub path: PathBuf,
    /// `st_rdev` of `path` — the value that goes into `zwp_linux_dmabuf_feedback_v1.main_device`.
    pub dev: libc::dev_t,
    /// Kernel driver bound to the node's PCI/platform device, read from sysfs. `None` when sysfs didn't answer (an explicitly overridden node is accepted regardless).
    pub driver: Option<String>,
}

/// Why no render node could be advertised, carried into the boot log so "clients are on shm" is always explained rather than inferred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    /// `/dev/dri` has no `renderD*` entries (headless container, no GPU).
    NoRenderNodes,
    /// Nodes exist but none passed. Carries `(path, reason)` per node.
    NoneUsable(Vec<(PathBuf, String)>),
    /// Nodes exist but none has a GPU behind it (a virtio GPU without 3D acceleration), so
    /// software rendering is all this machine has. Carries `(path, reason)` per node.
    NoRenderer(Vec<(PathBuf, String)>),
    /// `TILO_DMABUF_DEVICE` was set but unusable.
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
                write_rejected(f, rejected)?;
                write!(f, ")")
            }
            ProbeError::NoRenderer(rejected) => {
                write!(f, "no render node has a GPU behind it (")?;
                write_rejected(f, rejected)?;
                write!(f, ")")
            }
            ProbeError::OverrideUnusable { path, reason } => {
                write!(f, "{DEVICE_ENV}={} is unusable: {reason}", path.display())
            }
        }
    }
}

impl std::error::Error for ProbeError {}

fn write_rejected(
    f: &mut std::fmt::Formatter<'_>,
    rejected: &[(PathBuf, String)],
) -> std::fmt::Result {
    for (i, (path, why)) in rejected.iter().enumerate() {
        if i > 0 {
            write!(f, "; ")?;
        }
        write!(f, "{}: {why}", path.display())?;
    }
    Ok(())
}

/// Why [`choose`] passed over a node.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Rejection {
    /// A GPU whose buffers cannot reach the compositor: a fault worth reporting.
    Unusable(String),
    /// No GPU behind the node at all.
    NoRenderer(String),
}

impl From<String> for Rejection {
    fn from(why: String) -> Self {
        Rejection::Unusable(why)
    }
}

/// Driver name for a render node, read out of sysfs: `/dev/dri/renderD128` →
/// `/sys/class/drm/renderD128/device/driver` → symlink basename. `None` when the node isn't a
/// `renderD*` name or sysfs has no such link (e.g. a container that bind-mounted `/dev/dri` without `/sys`).
///
/// Delegates to [`launcher::gpu::driver_for`]: the launcher answers the same question when
/// pinning an app's ICDs, and two copies of this lookup disagreeing is exactly how a client ends up on a different GPU than the compositor.
#[cfg(target_os = "linux")]
pub fn driver_for(path: &Path) -> Option<String> {
    launcher::gpu::driver_for(path)
}

#[cfg(target_os = "linux")]
fn dev_of(path: &Path) -> std::io::Result<libc::dev_t> {
    use std::os::unix::fs::MetadataExt as _;
    // `metadata` follows symlinks, which is what we want: a caller may point `TILO_DMABUF_DEVICE` at `/dev/dri/by-path/...`.
    Ok(std::fs::metadata(path)?.rdev())
}

/// Can the current process actually open the node? A node we can't open is one the *client* probably can't either (they share the `render` group) — advertising it would strand it as no feedback would.
#[cfg(target_os = "linux")]
fn opens(path: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    Ok(())
}

/// Every `/dev/dri/renderD*` on the system, sorted by name so the selection is deterministic across boots.
#[cfg(target_os = "linux")]
fn render_nodes() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir("/dev/dri") else {
        return Vec::new();
    };
    let mut nodes: Vec<PathBuf> = entries
        .filter_map(std::result::Result::ok)
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

/// Pick the render node to advertise as the dmabuf main device; see the module docs for the selection rules.
///
/// The first answer is kept for the life of the process: it is asked from `main` (see
/// [`pin_process`]) and again by [`crate::compositor::state::State::new`], and both must name the
/// same node.
///
/// # Errors
///
/// Returns [`ProbeError::OverrideUnusable`] when `TILO_DMABUF_DEVICE` is set
/// but the node it names can't be stat'd or opened; [`ProbeError::NoRenderNodes`]
/// when `/dev/dri` has no `renderD*` entries; [`ProbeError::NoneUsable`] when
/// nodes exist but none can exchange a buffer with the compositor, each with its reason.
#[cfg(target_os = "linux")]
pub fn probe() -> Result<RenderNode, ProbeError> {
    static PROBED: OnceLock<Result<RenderNode, ProbeError>> = OnceLock::new();
    PROBED.get_or_init(probe_uncached).clone()
}

#[cfg(target_os = "linux")]
fn probe_uncached() -> Result<RenderNode, ProbeError> {
    if let Some(raw) = std::env::var_os(DEVICE_ENV) {
        let path = PathBuf::from(raw);
        if path.as_os_str().is_empty() {
            // An empty override is the documented way to say "never advertise dmabuf" — treat it as an explicit opt-out.
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
    choose(render_nodes(), |path| {
        opens(path).map_err(|e| format!("cannot open: {e}"))?;
        let dev = dev_of(path).map_err(|e| format!("stat failed: {e}"))?;
        if let Some(why) = renderer::missing(path) {
            return Err(Rejection::NoRenderer(why));
        }
        let exchange = launcher::gpu::in_isolation(|| exchange::check(path))
            .and_then(std::convert::identity)?;
        tracing::info!(
            target: "shell::compositor::dmabuf",
            node = %path.display(),
            driver = driver_for(path).as_deref().unwrap_or("<unknown>"),
            "{exchange}"
        );
        Ok(dev)
    })
}

/// The first of `nodes` that `usable` accepts, or every node's reason. Nodes are a GPU's
/// fault ([`ProbeError::NoneUsable`]) unless every one of them has no GPU behind it.
/// Split from the host so tests can stand in for `/dev/dri`.
fn choose(
    nodes: Vec<PathBuf>,
    usable: impl Fn(&Path) -> Result<libc::dev_t, Rejection>,
) -> Result<RenderNode, ProbeError> {
    if nodes.is_empty() {
        return Err(ProbeError::NoRenderNodes);
    }
    let mut rejected = Vec::new();
    let mut every_node_lacks_a_renderer = true;
    for path in nodes {
        match usable(&path) {
            Ok(dev) => {
                return Ok(RenderNode {
                    driver: driver_name(&path),
                    path,
                    dev,
                });
            }
            Err(rejection) => {
                let why = match rejection {
                    Rejection::Unusable(why) => {
                        every_node_lacks_a_renderer = false;
                        why
                    }
                    Rejection::NoRenderer(why) => why,
                };
                let why = match driver_name(&path) {
                    Some(driver) => format!("{why} (driver `{driver}`)"),
                    None => why,
                };
                rejected.push((path, why));
            }
        }
    }
    if every_node_lacks_a_renderer {
        Err(ProbeError::NoRenderer(rejected))
    } else {
        Err(ProbeError::NoneUsable(rejected))
    }
}

#[cfg(target_os = "linux")]
fn driver_name(path: &Path) -> Option<String> {
    driver_for(path)
}

#[cfg(not(target_os = "linux"))]
fn driver_name(_path: &Path) -> Option<String> {
    None
}

/// Is there a GPU behind a render node at all?
///
/// Only virtio GPUs register a render node with nothing behind it: the kernel exposes
/// `renderD*` whether or not the host offers 3D acceleration, and says which through
/// `DRM_IOCTL_VIRTGPU_GETPARAM`. That ioctl number means something else on every other
/// driver, so it is asked only of a node whose DRM driver says it is `virtio_gpu`.
#[cfg(target_os = "linux")]
mod renderer {
    use smithay::reexports::drm::Device as _;
    use std::os::fd::{AsFd, AsRawFd as _, BorrowedFd};
    use std::path::Path;

    /// `DRM_IOWR(DRM_COMMAND_BASE + DRM_VIRTGPU_GETPARAM, struct drm_virtgpu_getparam)`.
    const DRM_IOCTL_VIRTGPU_GETPARAM: libc::c_ulong = 0xc010_6443;
    /// The host renders virgl (OpenGL) command streams.
    const VIRTGPU_PARAM_3D_FEATURES: u64 = 1;
    /// The host offers context types beyond virgl (Venus, gfxstream, cross-domain).
    const VIRTGPU_PARAM_CONTEXT_INIT: u64 = 6;

    #[repr(C)]
    struct VirtgpuGetparam {
        param: u64,
        /// User pointer the kernel writes an `int` through.
        value: u64,
    }

    struct Node(std::fs::File);

    impl AsFd for Node {
        fn as_fd(&self) -> BorrowedFd<'_> {
            self.0.as_fd()
        }
    }

    impl smithay::reexports::drm::Device for Node {}

    /// `Some(reason)` when `path` has no GPU behind it; `None` when it has one, or when the
    /// kernel would not say (the buffer exchange then judges the node as a GPU).
    pub(super) fn missing(path: &Path) -> Option<String> {
        let node = Node(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .ok()?,
        );
        let driver = node.get_driver().ok()?;
        if driver.name() != "virtio_gpu" {
            return None;
        }
        let renders = |param| getparam(&node, param).map(|value| value != 0);
        match (
            renders(VIRTGPU_PARAM_3D_FEATURES),
            renders(VIRTGPU_PARAM_CONTEXT_INIT),
        ) {
            (Ok(false), Ok(false) | Err(_)) => Some(
                "virtio GPU without 3D acceleration: the host renders nothing for it (boot                  the VM with virgl or Venus for a GPU)"
                    .to_string(),
            ),
            _ => None,
        }
    }

    fn getparam(node: &Node, param: u64) -> std::io::Result<libc::c_int> {
        let mut value: libc::c_int = 0;
        let mut request = VirtgpuGetparam {
            param,
            value: (&raw mut value).expose_provenance() as u64,
        };
        // SAFETY: `request` is the `struct drm_virtgpu_getparam` this ioctl number encodes
        // (two u64s, 16 bytes), and its `value` points at an `int` that outlives the call.
        let result = unsafe {
            libc::ioctl(
                node.0.as_raw_fd(),
                DRM_IOCTL_VIRTGPU_GETPARAM,
                &raw mut request,
            )
        };
        if result == 0 {
            Ok(value)
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

/// Can a buffer this node's driver allocates reach the compositor's pixels?
#[cfg(target_os = "linux")]
mod exchange {
    use smithay::backend::allocator::dmabuf::AsDmabuf as _;
    use smithay::backend::allocator::gbm::{GbmBuffer, GbmBufferFlags, GbmDevice};
    use smithay::backend::allocator::{Fourcc, Modifier};
    use std::path::Path;

    /// Allocate a small linear ARGB buffer on `path`, export it as a dmabuf and import it the
    /// way a client's would be: CPU-mapped first, then through the GL importer. `Ok` names the
    /// path that worked; `Err` says why neither did.
    pub(super) fn check(path: &Path) -> Result<String, String> {
        const SIDE: u32 = 16;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| format!("cannot open: {e}"))?;
        let device = GbmDevice::new(file).map_err(|e| format!("gbm_create_device: {e}"))?;
        let usage = GbmBufferFlags::RENDERING;
        let (bo, implicit) = match device.create_buffer_object_with_modifiers2::<()>(
            SIDE,
            SIDE,
            Fourcc::Argb8888,
            std::iter::once(Modifier::Linear),
            usage,
        ) {
            Ok(bo) => (bo, false),
            Err(explicit) => (
                device
                    .create_buffer_object::<()>(
                        SIDE,
                        SIDE,
                        Fourcc::Argb8888,
                        usage | GbmBufferFlags::LINEAR,
                    )
                    .map_err(|implicit| {
                        format!(
                            "cannot allocate a linear ARGB8888 buffer (explicit modifier: \
                             {explicit}; implicit: {implicit})"
                        )
                    })?,
                true,
            ),
        };
        let dmabuf = GbmBuffer::from_bo(bo, implicit)
            .export()
            .map_err(|e| format!("cannot export the buffer as a dmabuf: {e}"))?;
        let mut pixels = Vec::new();
        let cpu = match crate::compositor::foreign::import_dmabuf_rgba(
            &dmabuf,
            &mut pixels,
            None,
            &mut None,
        ) {
            Ok(_) => return Ok("dmabuf exchange works: buffers are CPU-mappable".to_string()),
            Err(e) => e.to_string(),
        };
        let gl = crate::compositor::gpu_import::GpuImporter::new(path)
            .map_err(|e| format!("{e:?}"))
            .and_then(|mut importer| {
                importer
                    .import(&dmabuf, &mut pixels)
                    .map_err(|e| format!("{e:?}"))
            });
        match gl {
            Ok(_) => Ok("dmabuf exchange works: buffers import through GL".to_string()),
            Err(gl) => Err(format!(
                "its buffers reach the compositor neither by CPU map ({cpu}) nor by GL import ({gl})"
            )),
        }
    }
}

/// The node the running compositor advertised, published by [`crate::compositor::state::State::new`].
///
/// The buffer-import side ([`crate::compositor::foreign`]) needs the same node the protocol side
/// advertised, reached from a process-wide store with no `State` in scope — published here once rather than threaded through every call site.
static ADVERTISED: OnceLock<PathBuf> = OnceLock::new();

/// Record the node advertised as `dmabuf_feedback.main_device`. Idempotent: the first caller wins, so a second `State` in the same process (tests) cannot repoint a live importer at another GPU.
pub fn set_advertised(path: PathBuf) {
    let _ = ADVERTISED.set(path);
}

/// The node recorded by [`set_advertised`], if a dmabuf global was advertised at all.
pub fn advertised() -> Option<&'static Path> {
    ADVERTISED.get().map(PathBuf::as_path)
}

/// The node [`pin_process`] resolved at startup, before anything in this process touched a GPU.
static PINNED: OnceLock<PathBuf> = OnceLock::new();

/// The render node this compositor settled on, whatever stage of boot the caller is at.
///
/// [`advertised`] is authoritative once the dmabuf global exists, but it's only set when
/// `dmabuf_feedback` could be built — a compositor that fell back to `wl_shm` still renders on a
/// GPU its clients still have to match, so [`pin_process`] (which runs before either) is the fallback.
///
/// This is the value the app launcher passes to `tilo-launcher --render-node`, so apps follow the compositor's actual choice rather than re-deriving one of their own.
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
    /// Lines explaining anything that was *not* applied: a loader with no matching manifest, or a variable the operator had already set.
    pub notes: Vec<String>,
}

/// Point this process's EGL and Vulkan loaders at the GPU the compositor is going to render on, before either loader has run.
///
/// The compositor picks its render node from the DRM device list (see [`probe`]), but its own
/// renderer goes through wgpu, which asks the Vulkan loader and GLVND — neither knows about that
/// choice, and on a hybrid machine they answer with whatever driver ranks first, so the
/// compositor can end up on one GPU while telling clients to allocate on another.
///
/// Must be called from `main` before any thread is spawned and before creating a wgpu instance:
/// `set_var` is only sound while the process is single-threaded, and loaders read these
/// variables once, at first use. A variable already set in the caller's environment is left
/// alone and recorded in [`ProcessPin::notes`] — combined with [`DEVICE_ENV`], that's a complete manual escape hatch.
#[cfg(target_os = "linux")]
#[allow(
    clippy::disallowed_methods,
    reason = "called from `main` before the warm-up thread or any other thread is spawned, and the EGL/Vulkan loaders of this process read these variables at first use"
)]
pub fn pin_process() -> Option<ProcessPin> {
    let node = match probe() {
        Ok(node) => node,
        Err(e) => {
            // Nothing to pin to. Not fatal: `State::new` logs the same rejection in prose and the compositor falls back to shm.
            tracing::debug!(target: "shell::compositor::gpu", "no render node to pin to: {e}");
            match &e {
                ProbeError::NoneUsable(_) => {
                    crate::compositor::gpu_health::record_probe_failure(&e.to_string());
                }
                ProbeError::NoRenderer(nodes) => {
                    crate::compositor::gpu_health::record_no_renderer(nodes);
                    pin_software();
                }
                ProbeError::NoRenderNodes | ProbeError::OverrideUnusable { .. } => {}
            }
            return None;
        }
    };
    let _ = PINNED.set(node.path.clone());
    let selection =
        launcher::gpu::select_for_node(&node.path, &launcher::gpu::IcdSearch::default());
    crate::compositor::gpu_health::record_selection(&selection);
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

/// A machine whose every render node has no GPU behind it renders with
/// lavapipe: pin this process's loaders to it, so neither the compositor nor
/// the apps it launches (which inherit the pin, `launcher::gpu::
/// inherited_software_pin`) load every installed driver, and apps ask for
/// Vulkan alone instead of also starting OpenGL for a GPU that is not there.
/// Same contract as [`pin_process`]: called from `main` before any thread
/// exists, and a variable the operator set is left alone.
#[cfg(target_os = "linux")]
#[allow(
    clippy::disallowed_methods,
    reason = "called from `pin_process` in `main` before any thread is spawned, and the EGL/Vulkan loaders of this process read these variables at first use"
)]
fn pin_software() {
    let pin = match launcher::gpu::software_pin(&launcher::gpu::IcdSearch::default()) {
        Ok(pin) => pin,
        Err(why) => {
            tracing::info!(target: "shell::compositor::gpu", "no software renderer to pin to: {why}");
            return;
        }
    };
    for (key, value) in pin.env() {
        if std::env::var_os(key).is_some() {
            continue;
        }
        // SAFETY: called from `main` before any thread is spawned, so no
        // other thread can be reading the environment concurrently.
        unsafe { std::env::set_var(key, &value) };
    }
    tracing::info!(
        target: "shell::compositor::gpu",
        vulkan = %pin.vulkan_icd_json.display(),
        "no GPU: pinned this session's loaders to the software renderer"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_node_driver_advertisement_is_self_describing() {
        // Phase `the_first_node_whose_buffers_exchange_is_chosen`.
        // A node is judged by what its buffers do, whatever its driver is
        // called: here a display-only node fails the exchange and the GPU
        // behind it is chosen, as on a Snapdragon X laptop.
        {
            let nodes = vec![
                PathBuf::from("/fake/dri/renderD128"),
                PathBuf::from("/fake/dri/renderD129"),
                PathBuf::from("/fake/dri/renderD130"),
            ];
            let chosen = choose(nodes.clone(), |path| match path.file_name() {
                Some(name) if name == "renderD128" => Err(Rejection::Unusable(
                    "its buffers reach the compositor neither by CPU map nor by GL import"
                        .to_string(),
                )),
                Some(name) if name == "renderD129" => Ok(0xe281),
                _ => panic!("the probe must stop at the first usable node"),
            })
            .expect("renderD129 exchanges buffers");
            assert_eq!(chosen.path, nodes[1]);
            assert_eq!(chosen.dev, 0xe281);
        }
        {
            let nodes = vec![
                PathBuf::from("/fake/dri/renderD128"),
                PathBuf::from("/fake/dri/renderD129"),
            ];
            let Err(ProbeError::NoneUsable(rejected)) = choose(nodes, |path| {
                Err(format!("no exchange on {}", path.display()).into())
            }) else {
                panic!("no usable node must be an error");
            };
            assert_eq!(rejected.len(), 2);
            assert!(rejected[1].1.contains("renderD129"), "{rejected:?}");
            assert_eq!(
                choose(Vec::new(), |_| Ok(0)),
                Err(ProbeError::NoRenderNodes)
            );
        }
        // Phase `a_machine_whose_nodes_have_no_gpu_behind_them_has_no_gpu`.
        // A virtio GPU without 3D acceleration registers a render node all the same; that
        // is a machine without a GPU, not a GPU that fails. One real GPU among such nodes
        // that cannot exchange buffers is still that GPU's fault.
        {
            let display = PathBuf::from("/fake/dri/renderD128");
            let gpu = PathBuf::from("/fake/dri/renderD129");
            let no_renderer = |path: &Path| {
                Err(Rejection::NoRenderer(format!(
                    "virtio GPU without 3D acceleration at {}",
                    path.display()
                )))
            };
            let Err(ProbeError::NoRenderer(rejected)) = choose(vec![display.clone()], no_renderer)
            else {
                panic!("a lone display-only node is no GPU");
            };
            assert_eq!(rejected.len(), 1);
            assert!(rejected[0].1.contains("without 3D"), "{rejected:?}");
            let mixed = choose(vec![display.clone(), gpu], |path| {
                if path == display {
                    no_renderer(path)
                } else {
                    Err(Rejection::Unusable("cannot export the buffer".to_string()))
                }
            });
            let Err(ProbeError::NoneUsable(rejected)) = mixed else {
                panic!("a GPU that cannot exchange buffers is a fault: {mixed:?}");
            };
            assert_eq!(rejected.len(), 2);
            let text = ProbeError::NoRenderer(vec![(display, "no 3D".to_string())]).to_string();
            assert!(text.contains("renderD128: no 3D"), "{text}");
        }
        {
            // Every rejection path has to say *why* in the boot log — "clients silently fell back to shm" is exactly the failure mode this module exists to prevent.
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
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn render_node_probe_reads_sysfs_consistently() {
        // Phase `probe_reports_a_stat_consistent_device_number`.
        // On a machine with a Mesa GPU the probe must succeed and hand back a node whose `dev` matches `stat(2)` — that number goes on the wire as `main_device`, and a wrong one strands the client.
        fn probe_reports_a_stat_consistent_device_number() {
            // Env-independent: skip when the box genuinely has no mappable GPU rather than asserting hardware into existence.
            let Ok(node) = probe() else {
                eprintln!("skipping: no render node exchanges buffers on this machine");
                return;
            };
            let expected = dev_of(&node.path).expect("probe returned a node that no longer stats");
            assert_eq!(
                node.dev, expected,
                "main_device must be the node's st_rdev; clients resolve the GPU from it"
            );
            assert_eq!(probe(), Ok(node), "the probe answers once per process");
        }
        assert_eq!(driver_for(Path::new("/dev/null")), None);
        assert_eq!(driver_for(Path::new("/dev/dri/renderD9999")), None);
        // Phase `probe_reports_a_stat_consistent_device_number` (defined above).
        probe_reports_a_stat_consistent_device_number();
    }
}
