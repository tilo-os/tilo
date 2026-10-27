//! Which GPU a launched application renders on.
//!
//! # Why the launcher has to answer this
//!
//! The compositor picks one DRM render node and advertises it as
//! `zwp_linux_dmabuf_v1`'s `main_device`. Every client has to end up on
//! **that** device — one that renders on a different GPU produces
//! dma-bufs the compositor can't import, and the tile shows an import
//! error instead of the app.
//!
//! Advertising `main_device` isn't enough, because the device is chosen
//! by the client's *loader*, one layer below Wayland:
//!
//! - **Vulkan**: the loader reads every `*.json` in the ICD search path
//!   and exposes one `VkPhysicalDevice` per driver — an app asking for a
//!   high-performance adapter gets whichever ranked first, frequently
//!   the discrete GPU, not the one the compositor renders on.
//! - **EGL**: GLVND reads `/usr/share/glvnd/egl_vendor.d/*.json` in name
//!   order and loads the first vendor that answers — `10_nvidia` sorts
//!   before `50_mesa`, so a box with a broken NVIDIA userspace fails EGL
//!   before Mesa is ever asked.
//!
//! Both are selected by environment variable, and both must name a file
//! that exists **inside the sandbox** — this module derives the paths
//! and the Linux sandbox launcher binds them.
//!
//! [`select_for_device`] takes the node's identity, the search paths and
//! the way ICDs are asked as inputs, so it runs against fixtures on every
//! host. Sysfs discovery, default search paths and actually loading an ICD
//! ([`select_for_node`]) are Linux-only.
//!
//! # Asked, not looked up
//!
//! The input is a render node and its `(major, minor)`. Every installed
//! Vulkan ICD for this architecture is loaded and asked for its physical
//! devices; the one whose `VK_EXT_physical_device_drm` render node is this
//! node is the driver for it. No table maps kernel driver names to
//! libraries: sysfs names the *platform* driver the node hangs off, which
//! for a Snapdragon X display controller is `msm_dpu` rather than `msm`,
//! and a table keyed on names silently pinned nothing there. A GPU nobody
//! listed works the moment its ICD can see it.
//!
//! The EGL vendor follows the Vulkan driver's vendor — Mesa's ICDs and
//! Mesa's GLVND vendor ship together, NVIDIA's likewise.
//!
//! If exactly one ICD claims the node, it's pinned. If none does, the
//! variable is **not set** and every ICD's answer is recorded in
//! [`GpuSelection::notes`] — a wrong pin strands the app on a driver
//! that can't drive its GPU, while no pin merely leaves the loader's
//! own default in place, the strictly smaller failure.

use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use tilo_lock::MutexRecover as _;

/// The capability an app declares to reach the GPU itself
/// (`[capabilities]` in its manifest).
pub const CAPABILITY: &str = "gpu";

/// Whether `manifest`'s app opens the GPU itself.
///
/// A toolkit app does not: the render service of its room draws its frames
/// (`tilo-render-service`), so its sandbox has no GPU node, no loader pin and
/// no shader-cache seed, and the launcher spends nothing preparing them.
/// Apps that do (a web engine, video, games, 3D, ML) declare `gpu`.
///
/// A permission, decided like `io-uring` (`crate::seccomp::syscall_grants`):
/// the signed manifest is the ceiling (an app that does not declare `gpu`
/// never gets it); an app the image ships (`image_shipped`) is granted what
/// it declares; any other app only what the room's capabilities name
/// (`Scope::Device("gpu")`), which the person gives through the permission
/// prompt.
pub fn granted(
    manifest: &crate::manifest::ManifestView,
    plan: &crate::capabilities::CapabilityPlan,
    image_shipped: bool,
) -> bool {
    manifest.declares_capability(CAPABILITY)
        && (image_shipped || plan.devices.iter().any(|device| device == CAPABILITY))
}

/// GLVND's EGL vendor selection: a colon-separated list of vendor ICD
/// JSONs; when set, GLVND loads only these and skips the directory scan.
pub const EGL_VENDOR_ENV: &str = "__EGL_VENDOR_LIBRARY_FILENAMES";

/// Name of the session's pipeline cache directory under
/// `$XDG_RUNTIME_DIR/tilo/` (see `ui::pipeline_seed`).
pub const PIPELINE_SEED_DIR_NAME: &str = "pipeline-seed";

/// Where that directory appears inside a sandbox, read-only.
pub const SANDBOX_PIPELINE_SEED_DIR: &str = "/run/tilo/pipeline-seed";

/// Names [`SANDBOX_PIPELINE_SEED_DIR`] to the app; `ui::pipeline_seed`
/// reads it.
pub const PIPELINE_SEED_ENV: &str = "TILO_PIPELINE_SEED_DIR";

/// The session's pipeline cache directory on this host, if one was
/// published.
#[must_use]
pub fn host_pipeline_seed() -> Option<PathBuf> {
    let dir = tilo_paths::xdg::runtime_dir()?
        .join("tilo")
        .join(PIPELINE_SEED_DIR_NAME);
    dir.is_dir().then_some(dir)
}

/// Vulkan loader driver selection (loader 1.3.207+).
pub const VK_DRIVER_FILES_ENV: &str = "VK_DRIVER_FILES";

/// Pre-1.3.207 spelling of [`VK_DRIVER_FILES_ENV`], set in lock-step so
/// an older loader honours the same choice.
pub const VK_ICD_FILENAMES_ENV: &str = "VK_ICD_FILENAMES";

/// Turns off Mesa's implicit device-selection layer
/// (`VK_LAYER_MESA_device_select`). It exists to pick one GPU among several
/// and reorders the physical devices for that, connecting to the display
/// server and enumerating every device on each `vkCreateInstance`. With the
/// driver pinned to the one ICD that drives the session's render node there
/// is exactly one device to pick, so the layer only costs time: ~4 ms of
/// adapter and device creation per app start on the Snapdragon X here.
pub const NO_DEVICE_SELECT_ENV: &str = "NODEVICE_SELECT";

/// Mesa's Windows-compatibility switch ([`SiteIntegration`]).
pub const SITE_INTEGRATION_ENV: &str = "mesa_vk_map_memory_placed";

/// What a driver can do for Windows code running under a translation layer,
/// named by the value [`SITE_INTEGRATION_ENV`] takes.
///
/// A D3D translation layer has to hand a guest program memory at an address
/// the program picked. Two extensions provide it:
///
/// - `VK_EXT_map_memory_placed` (turnip, Honeykrisp) places a host
///   allocation with **target buffers**, and the Vulkan memory model gives
///   that placement the `bufferDeviceAddress` guarantees a guest heap needs.
/// - `VK_KHR_map_memory2` places an allocation without target buffers, but
///   the memory then carries no device address of its own.
///
/// Mesa gates both behind this one switch, so the launcher sets it only
/// when the driver itself advertises the feature. Which driver has which
/// extension is the driver's business: the launcher asks, never assumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SiteIntegration {
    /// Placement with target buffers (`VK_EXT_map_memory_placed`).
    #[serde(rename = "target-buffers")]
    TargetBuffers,
    /// Placement without target buffers.
    #[serde(rename = "placed")]
    Placed,
}

impl SiteIntegration {
    /// The value [`SITE_INTEGRATION_ENV`] takes.
    #[must_use]
    pub fn value(self) -> String {
        match self {
            Self::TargetBuffers => "target-buffers".to_owned(),
            Self::Placed => "placed".to_owned(),
        }
    }
}

/// One feature field a driver must report `VK_TRUE`.
///
/// Asking a device several features at once means chaining extension
/// structures onto `VkPhysicalDeviceFeatures2` and reading each field back.
/// Naming the field keeps that query in one place and lets the tests and the
/// selection reason about features without touching Vulkan pointers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Feature {
    /// `VkPhysicalDeviceBufferDeviceAddressFeatures::bufferDeviceAddress`.
    BufferDeviceAddress,
    /// `VkPhysicalDeviceMapMemoryPlacedFeaturesEXT::memoryMapPlaced`.
    MemoryMapPlaced,
    /// `VkPhysicalDeviceMapMemoryPlacedFeaturesEXT::memoryMapRangePlaced`.
    MemoryMapRangePlaced,
    /// `VkPhysicalDeviceMapMemoryPlacedFeaturesEXT::memoryUnmapReserve`.
    MemoryUnmapReserve,
}

/// The site integration the claiming driver advertises, if it can place
/// memory at all.
///
/// A driver that will not say, or says only some of it, is left alone: the
/// switch is an opt-in whose failure mode is a graphics stack that stops
/// working, so only every feature of one path turns it on.
fn site_integrator(claim: Option<&Claim>, probe: &impl IcdProbe) -> Option<SiteIntegration> {
    let claim = claim?;
    let has = |features: &[Feature]| probe.advertises(&claim.manifest, &claim.device, features);
    // Target buffers are the stronger of the two, and a driver with both
    // takes the same path either way, so they are asked for first.
    if has(&[
        Feature::BufferDeviceAddress,
        Feature::MemoryMapPlaced,
        Feature::MemoryMapRangePlaced,
        Feature::MemoryUnmapReserve,
    ]) == Ok(true)
    {
        return Some(SiteIntegration::TargetBuffers);
    }
    has(&[Feature::MemoryMapPlaced])
        .eq(&Ok(true))
        .then_some(SiteIntegration::Placed)
}

/// Directories the EGL vendor JSONs are installed in, most-specific
/// first. `/etc` overrides `/usr` by GLVND's own convention.
#[cfg(target_os = "linux")]
const EGL_VENDOR_DIRS: &[&str] = &[
    "/etc/glvnd/egl_vendor.d",
    "/usr/share/glvnd/egl_vendor.d",
    "/usr/local/share/glvnd/egl_vendor.d",
];

/// Directories the Vulkan ICD manifests are installed in.
#[cfg(target_os = "linux")]
const VULKAN_ICD_DIRS: &[&str] = &[
    "/etc/vulkan/icd.d",
    "/usr/share/vulkan/icd.d",
    "/usr/local/share/vulkan/icd.d",
];

/// Architecture tokens in ICD manifest file names
/// (`radeon_icd.x86_64.json`, `radeon_icd.i686.json`). A file naming a
/// different arch is skipped, or a multilib host offers two "matching"
/// candidates for no reason.
const ARCH_TOKENS: &[&str] = &[
    "i686", "x86_64", "aarch64", "armv7l", "armv7hl", "riscv64", "ppc64le", "s390x",
];

/// Where to look for ICD manifests. Split out so tests can point the
/// *production* selection code at a fixture tree instead of the host's
/// real `/usr/share`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IcdSearch {
    pub egl_vendor_dirs: Vec<PathBuf>,
    pub vulkan_icd_dirs: Vec<PathBuf>,
}

#[cfg(target_os = "linux")]
impl Default for IcdSearch {
    fn default() -> Self {
        Self {
            egl_vendor_dirs: EGL_VENDOR_DIRS.iter().map(PathBuf::from).collect(),
            vulkan_icd_dirs: VULKAN_ICD_DIRS.iter().map(PathBuf::from).collect(),
        }
    }
}

/// The GPU an app should be pinned to, and the loader manifests that
/// pin it there.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GpuSelection {
    /// The DRM render node the compositor resolved, e.g.
    /// `/dev/dri/renderD128`.
    pub node: PathBuf,
    /// Kernel driver bound to that node, from sysfs. `None` when sysfs had
    /// no answer, in which case nothing can be derived and every field
    /// below is `None`.
    pub driver: Option<String>,
    /// GLVND EGL vendor manifest of the Vulkan driver's vendor, if exactly
    /// one is installed.
    pub egl_vendor_json: Option<PathBuf>,
    /// The one Vulkan ICD manifest whose driver reports this node.
    pub vulkan_icd_json: Option<PathBuf>,
    /// The physical device that ICD reported for the node, e.g.
    /// `Turnip Adreno (TM) X1-85`.
    pub vulkan_device: Option<String>,
    /// The session's published pipeline cache directory on the host, bound
    /// read-only at [`SANDBOX_PIPELINE_SEED_DIR`] when it exists.
    #[serde(default)]
    pub pipeline_seed: Option<PathBuf>,
    /// The Windows-compatibility site integration this driver supports
    /// ([`SiteIntegration`]). `None` for a driver that cannot place memory,
    /// which is every native GPU, so the environment is unchanged there.
    #[serde(default)]
    pub site_integration: Option<SiteIntegration>,
    /// Human-readable record of everything that was *not* pinned and why —
    /// otherwise invisible until a tile renders black.
    pub notes: Vec<String>,
}

impl GpuSelection {
    /// The environment the app must run with. Only variables this
    /// selection could derive appear — an unmatched loader is left at its
    /// own default rather than pinned to a guess.
    pub fn env(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if let Some(p) = &self.egl_vendor_json {
            out.push((EGL_VENDOR_ENV, p.display().to_string()));
        }
        if let Some(p) = &self.vulkan_icd_json {
            out.push((VK_DRIVER_FILES_ENV, p.display().to_string()));
            out.push((VK_ICD_FILENAMES_ENV, p.display().to_string()));
            out.push((NO_DEVICE_SELECT_ENV, "1".to_owned()));
        }
        if let Some(site) = &self.site_integration {
            out.push((SITE_INTEGRATION_ENV, site.value()));
        }
        out
    }

    /// Files that must be readable *inside the sandbox* for [`Self::env`]
    /// to mean anything: GLVND and the Vulkan loader both treat "set but
    /// unreadable" as "no drivers", so a bad bind is worse than no variable.
    pub fn manifest_files(&self) -> Vec<PathBuf> {
        self.egl_vendor_json
            .iter()
            .chain(self.vulkan_icd_json.iter())
            .cloned()
            .collect()
    }

    /// Did anything at all get pinned? Used by callers to decide
    /// whether to log a selection line or a "left to the loader" line.
    pub fn is_empty(&self) -> bool {
        self.egl_vendor_json.is_none() && self.vulkan_icd_json.is_none()
    }
}

/// Kernel driver bound to a render node, read out of sysfs.
///
/// `/dev/dri/renderD128` → `/sys/class/drm/renderD128/device/driver` →
/// symlink basename. `None` when sysfs has no such link.
#[cfg(target_os = "linux")]
pub fn driver_for(node: &Path) -> Option<String> {
    let name = node.file_name()?.to_str()?;
    let link = Path::new("/sys/class/drm").join(name).join("device/driver");
    let target = std::fs::read_link(link).ok()?;
    Some(target.file_name()?.to_str()?.to_string())
}

/// `library_path` out of a loader manifest, as a bare file name.
///
/// Both manifest flavours use the same shape —
/// `{"ICD": {"library_path": "..."}}` — and the value may be a bare
/// soname or an absolute path, so comparing on the file name matches
/// both forms.
fn manifest_library_name(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let raw = value.get("ICD")?.get("library_path")?.as_str()?;
    Some(
        Path::new(raw)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(raw)
            .to_string(),
    )
}

/// The architecture token this build's ICDs are named with.
///
/// `std::env::consts::ARCH` and the manifest file names agree on
/// `x86_64`/`aarch64` but not on 32-bit x86 (`x86` vs `i686`) — getting
/// this wrong would filter out the only manifest that fits.
fn host_arch_token() -> &'static str {
    match std::env::consts::ARCH {
        "x86" => "i686",
        "arm" => "armv7l",
        other => other,
    }
}

/// Is this manifest file name for an architecture we can't load?
/// `radeon_icd.i686.json` on an `x86_64` host describes a 32-bit library
/// this process could never dlopen. Files with no arch token are
/// always eligible.
fn is_foreign_arch(file_name: &str) -> bool {
    let host = host_arch_token();
    file_name
        .split('.')
        .any(|part| ARCH_TOKENS.contains(&part) && part != host)
}

/// Every `*.json` under `dirs`, in directory order then name order.
fn manifests_in(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut found: Vec<PathBuf> = entries
            .filter_map(std::result::Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        found.sort();
        out.extend(found);
    }
    out
}

/// The single manifest under `dirs` whose `library_path` names
/// `library`, or `Err(reason)` describing why there wasn't exactly one.
fn unique_manifest_for(dirs: &[PathBuf], library: &str, kind: &str) -> Result<PathBuf, String> {
    let mut matches: Vec<PathBuf> = Vec::new();
    let mut scanned = 0usize;
    for path in manifests_in(dirs) {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if is_foreign_arch(name) {
            continue;
        }
        scanned = scanned.saturating_add(1);
        let Some(lib) = manifest_library_name(&path) else {
            continue;
        };
        // Compare on the stem so a soname version bump doesn't unpin the driver.
        if lib.split(".so").next().is_some_and(|stem| stem == library) {
            matches.push(path);
        }
    }
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(format!(
            "no {kind} manifest for `{library}` among {scanned} candidate(s) in {}",
            render_dirs(dirs)
        )),
        n => Err(format!(
            "{n} {kind} manifests claim `{library}` ({}) — refusing to guess",
            matches
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn render_dirs(dirs: &[PathBuf]) -> String {
    dirs.iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `library_path` out of a loader manifest, as something `dlopen` can
/// load: absolute and bare sonames as written, a relative path resolved
/// against the manifest's directory the way the Vulkan loader does.
fn manifest_library_path(path: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let raw = Path::new(value.get("ICD")?.get("library_path")?.as_str()?);
    if raw.is_absolute() || raw.components().count() == 1 {
        Some(raw.to_path_buf())
    } else {
        Some(path.parent()?.join(raw))
    }
}

/// One physical device an ICD exposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VulkanDevice {
    /// `VkPhysicalDeviceProperties::deviceName`.
    pub name: String,
    /// `(major, minor)` of its DRM render node, from
    /// `VK_EXT_physical_device_drm`. `None` for a device with no render
    /// node (lavapipe) or a driver without the extension.
    pub render_node: Option<(u64, u64)>,
}

/// Asks one installed ICD which devices it drives.
pub trait IcdProbe {
    /// The physical devices the ICD at `library` (named by `manifest`)
    /// reports, or why it could not be asked.
    ///
    /// # Errors
    ///
    /// A reason for the selection's notes when the library cannot be
    /// loaded or its instance cannot be created.
    fn devices(&self, manifest: &Path, library: &Path) -> Result<Vec<VulkanDevice>, String>;

    /// Whether the physical device the ICD at `manifest` names `device`
    /// advertises every feature in `features` (`VK_TRUE` for each).
    ///
    /// The default answers `false`: a probe that scripts devices describes
    /// none, and "not advertised" is the safe answer everywhere this is
    /// read — it leaves the environment alone rather than switching on
    /// something the driver cannot do.
    ///
    /// # Errors
    ///
    /// A reason the question could not be put to that device.
    fn advertises(
        &self,
        _manifest: &Path,
        _device: &str,
        _features: &[Feature],
    ) -> Result<bool, String> {
        Ok(false)
    }
}

/// `(major, minor)` of a device node.
#[cfg(target_os = "linux")]
fn node_numbers(node: &Path) -> std::io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    let rdev = std::fs::metadata(node)?.rdev();
    Ok((u64::from(libc::major(rdev)), u64::from(libc::minor(rdev))))
}

/// Derive the loader pinning for `node` by asking every installed ICD.
///
/// Loading an ICD is not free (lavapipe initialises LLVM), so the answer
/// is kept for the life of the process: a node's driver does not change
/// under a running session. Callers that launch many apps should still
/// resolve once and pass the result on, as the compositor does.
#[cfg(target_os = "linux")]
pub fn select_for_node(node: &Path, search: &IcdSearch) -> GpuSelection {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static CACHE: Mutex<Option<HashMap<(PathBuf, IcdSearch), GpuSelection>>> = Mutex::new(None);
    let key = (node.to_path_buf(), search.clone());
    if let Some(found) = CACHE
        .lock_recover()
        .as_ref()
        .and_then(|cache| cache.get(&key))
    {
        return found.clone();
    }
    let selection = match node_numbers(node) {
        Ok(numbers) => in_isolation(|| {
            select_for_device(
                node,
                numbers,
                driver_for(node),
                search,
                &loaded::LoadedIcdProbe,
            )
        })
        .unwrap_or_else(|why| GpuSelection {
            node: node.to_path_buf(),
            driver: driver_for(node),
            egl_vendor_json: None,
            vulkan_icd_json: None,
            vulkan_device: None,
            pipeline_seed: None,
            site_integration: None,
            notes: vec![format!(
                "asking the installed Vulkan drivers about {} failed: {why}; leaving the \
                 EGL/Vulkan loaders at their own defaults",
                node.display()
            )],
        }),
        Err(error) => GpuSelection {
            node: node.to_path_buf(),
            driver: driver_for(node),
            egl_vendor_json: None,
            vulkan_icd_json: None,
            vulkan_device: None,
            pipeline_seed: None,
            site_integration: None,
            notes: vec![format!(
                "cannot stat {}: {error}; leaving the EGL/Vulkan loaders at their own defaults",
                node.display()
            )],
        },
    };
    CACHE
        .lock_recover()
        .get_or_insert_with(HashMap::new)
        .insert(key, selection.clone());
    selection
}

/// Run `probe` where the drivers it loads cannot affect the caller.
///
/// Loading a GPU driver runs its initialisers, and some start threads. The
/// compositor resolves its GPU from `main` while it is still
/// single-threaded, because it then pins the loaders with `set_var`, which
/// is only sound with no other thread alive. So a single-threaded caller
/// runs the probe in a forked child and reads the answer back over a pipe:
/// no driver is ever loaded into it, and a driver that crashes takes only
/// the child down. A caller that already has threads cannot fork safely;
/// it probes in-process, which is fine because it has no environment left
/// to pin.
///
/// # Errors
///
/// Why the child produced no answer: it crashed, or the pipe failed.
#[cfg(target_os = "linux")]
pub fn in_isolation<T: serde::Serialize + serde::de::DeserializeOwned>(
    probe: impl FnOnce() -> T,
) -> Result<T, String> {
    if !std::fs::read_dir("/proc/self/task").is_ok_and(|tasks| tasks.count() == 1) {
        return Ok(probe());
    }
    isolated::in_child(probe)
}

#[cfg(target_os = "linux")]
mod isolated {
    use std::io::{Read as _, Write as _};
    use std::os::fd::{FromRawFd as _, OwnedFd};

    #[allow(
        unsafe_code,
        reason = "fork/pipe/_exit isolate driver initialisers from a single-threaded caller"
    )]
    pub(super) fn in_child<T: serde::Serialize + serde::de::DeserializeOwned>(
        probe: impl FnOnce() -> T,
    ) -> Result<T, String> {
        let mut fds = [0; 2];
        // SAFETY: `fds` has room for the two descriptors pipe2 writes.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(format!("pipe: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: pipe2 succeeded, so both are fresh descriptors we own.
        let (read_end, write_end) =
            unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        // SAFETY: the caller is single-threaded (checked by the caller), so
        // the child is a complete copy and may do anything the parent could.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(format!("fork: {}", std::io::Error::last_os_error()));
        }
        if pid == 0 {
            drop(read_end);
            let answer = serde_json::to_vec(&probe()).unwrap_or_default();
            let mut out = std::fs::File::from(write_end);
            let code = i32::from(out.write_all(&answer).is_err());
            // SAFETY: leave without running the parent's exit handlers or
            // the drivers' destructors.
            unsafe { libc::_exit(code) };
        }
        drop(write_end);
        let mut answer = Vec::new();
        let read = std::fs::File::from(read_end).read_to_end(&mut answer);
        let mut status = 0;
        // SAFETY: `pid` is our own child.
        unsafe { libc::waitpid(pid, &raw mut status, 0) };
        read.map_err(|error| format!("reading the answer: {error}"))?;
        if libc::WIFSIGNALED(status) {
            return Err(format!(
                "a driver crashed the probe (signal {})",
                libc::WTERMSIG(status)
            ));
        }
        if !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0 {
            return Err(format!("the probe exited with status {status}"));
        }
        serde_json::from_slice(&answer).map_err(|error| format!("unreadable answer: {error}"))
    }
}

/// The pin a compositor already applied to its own environment, when it
/// names exactly one installed manifest per loader and one is the Vulkan
/// driver's. The compositor resolves its node once at startup and exports
/// the result through these variables; every launcher it spawns reads them
/// back instead of loading every driver again per launch.
#[must_use]
pub fn inherited_selection(node: &Path) -> Option<GpuSelection> {
    let single_file = |key: &str| {
        let value = std::env::var_os(key)?;
        let path = PathBuf::from(value);
        (path.is_absolute() && path.is_file()).then_some(path)
    };
    let vulkan_icd_json = single_file(VK_DRIVER_FILES_ENV)?;
    Some(GpuSelection {
        node: node.to_path_buf(),
        driver: driver_name(node),
        egl_vendor_json: single_file(EGL_VENDOR_ENV),
        vulkan_icd_json: Some(vulkan_icd_json),
        vulkan_device: None,
        pipeline_seed: None,
        site_integration: None,
        notes: Vec::new(),
    })
}

/// The Vulkan driver of a machine with no GPU: Mesa's software rasteriser,
/// lavapipe.
const SOFTWARE_VULKAN_LIBRARY: &str = "libvulkan_lvp";

/// Mesa's GLVND vendor, which serves the software OpenGL rasteriser.
const MESA_EGL_LIBRARY: &str = "libEGL_mesa";

/// The loader pin of a machine whose compositor found no GPU at all (every
/// render node is a virtio GPU the host renders nothing for): the compositor
/// renders with lavapipe, and so must its apps.
///
/// Without a pin the Vulkan loader still loads every installed driver for
/// every app, and an app cannot tell a GPU-less render node from a GPU with
/// only an OpenGL driver, so it also initialises OpenGL (`ui::gpu`). Pinned,
/// the loader loads lavapipe alone and the app asks for Vulkan alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoftwarePin {
    /// Lavapipe's Vulkan ICD manifest.
    pub vulkan_icd_json: PathBuf,
    /// Mesa's GLVND EGL vendor manifest, if exactly one is installed.
    pub egl_vendor_json: Option<PathBuf>,
}

impl SoftwarePin {
    /// The environment the compositor and its apps run with.
    pub fn env(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if let Some(path) = &self.egl_vendor_json {
            out.push((EGL_VENDOR_ENV, path.display().to_string()));
        }
        let vulkan = self.vulkan_icd_json.display().to_string();
        out.push((VK_DRIVER_FILES_ENV, vulkan.clone()));
        out.push((VK_ICD_FILENAMES_ENV, vulkan));
        out.push((NO_DEVICE_SELECT_ENV, "1".to_owned()));
        out
    }

    /// Files that must be readable inside the sandbox for [`Self::env`].
    pub fn manifest_files(&self) -> Vec<PathBuf> {
        self.egl_vendor_json
            .iter()
            .chain(std::iter::once(&self.vulkan_icd_json))
            .cloned()
            .collect()
    }
}

/// The software pin from the manifests installed under `search`, or why
/// there is none (no lavapipe manifest, or several).
pub fn software_pin(search: &IcdSearch) -> Result<SoftwarePin, String> {
    let vulkan_icd_json = unique_manifest_for(
        &search.vulkan_icd_dirs,
        SOFTWARE_VULKAN_LIBRARY,
        "Vulkan ICD",
    )?;
    Ok(SoftwarePin {
        vulkan_icd_json,
        egl_vendor_json: unique_manifest_for(
            &search.egl_vendor_dirs,
            MESA_EGL_LIBRARY,
            "EGL vendor",
        )
        .ok(),
    })
}

/// The software pin a GPU-less compositor applied to its own environment,
/// read back by a launch it started with no render node. Only lavapipe's
/// manifest counts: an arbitrary driver someone named is no evidence that
/// the machine has no GPU.
#[must_use]
pub fn inherited_software_pin() -> Option<SoftwarePin> {
    software_pin_from(
        std::env::var_os(VK_DRIVER_FILES_ENV),
        std::env::var_os(EGL_VENDOR_ENV),
    )
}

fn software_pin_from(
    vulkan: Option<std::ffi::OsString>,
    egl: Option<std::ffi::OsString>,
) -> Option<SoftwarePin> {
    let single_file = |value: std::ffi::OsString| {
        let path = PathBuf::from(value);
        (path.is_absolute() && path.is_file()).then_some(path)
    };
    let vulkan_icd_json = single_file(vulkan?)?;
    let library = manifest_library_name(&vulkan_icd_json)?;
    if library.split(".so").next() != Some(SOFTWARE_VULKAN_LIBRARY) {
        return None;
    }
    let egl_vendor_json = egl.and_then(single_file).filter(|path| {
        manifest_library_name(path)
            .is_some_and(|name| name.split(".so").next() == Some(MESA_EGL_LIBRARY))
    });
    Some(SoftwarePin {
        vulkan_icd_json,
        egl_vendor_json,
    })
}

#[cfg(target_os = "linux")]
fn driver_name(node: &Path) -> Option<String> {
    driver_for(node)
}

#[cfg(not(target_os = "linux"))]
fn driver_name(_node: &Path) -> Option<String> {
    None
}

/// The GLVND vendor library shipped by the vendor of a Vulkan driver.
/// Every Mesa Vulkan driver is a `libvulkan_*` beside Mesa's one EGL
/// vendor; NVIDIA's Vulkan driver lives in `libGLX_nvidia`.
fn egl_vendor_for_vulkan(library: &str) -> Option<&'static str> {
    if library == "libGLX_nvidia" {
        Some("libEGL_nvidia")
    } else if library.starts_with("libvulkan_") {
        Some("libEGL_mesa")
    } else {
        None
    }
}

/// Derive the loader pinning for the node numbered `numbers`, asking each
/// installed ICD through `probe`. The function the tests drive: a fixture
/// tree and a scripted probe stand in for the host.
pub fn select_for_device(
    node: &Path,
    numbers: (u64, u64),
    driver: Option<String>,
    search: &IcdSearch,
    probe: &impl IcdProbe,
) -> GpuSelection {
    let mut notes = Vec::new();
    let claim = match claim_for(node, numbers, search, probe) {
        Ok(claim) => Some(claim),
        Err(why) => {
            notes.push(format!("{VK_DRIVER_FILES_ENV} not set: {why}"));
            None
        }
    };
    let egl_vendor_json = match egl_vendor_after(claim.as_ref(), search) {
        Ok(path) => Some(path),
        Err(why) => {
            notes.push(format!("{EGL_VENDOR_ENV} not set: {why}"));
            None
        }
    };
    let site_integration = site_integrator(claim.as_ref(), probe);
    let (vulkan_icd_json, vulkan_device) = claim.map_or((None, None), |claim| {
        (Some(claim.manifest), Some(claim.device))
    });
    GpuSelection {
        node: node.to_path_buf(),
        driver,
        egl_vendor_json,
        vulkan_icd_json,
        vulkan_device,
        pipeline_seed: None,
        site_integration,
        notes,
    }
}

/// The one ICD that reports a node as its render node.
struct Claim {
    manifest: PathBuf,
    /// Library file name, e.g. `libvulkan_freedreno.so`.
    library: String,
    device: String,
}

/// How a device answered, for the notes when nothing claimed the node.
fn describe(devices: &[VulkanDevice]) -> String {
    if devices.is_empty() {
        return "no devices".to_string();
    }
    devices
        .iter()
        .map(|d| match d.render_node {
            Some((major, minor)) => format!("{} on {major}:{minor}", d.name),
            None => format!("{} without a render node", d.name),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Ask every host-architecture ICD; exactly one must claim the node.
fn claim_for(
    node: &Path,
    numbers: (u64, u64),
    search: &IcdSearch,
    probe: &impl IcdProbe,
) -> Result<Claim, String> {
    let mut claims = Vec::new();
    let mut answers = Vec::new();
    for manifest in manifests_in(&search.vulkan_icd_dirs) {
        let Some(file) = manifest.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if is_foreign_arch(file) {
            continue;
        }
        let (Some(path), Some(library)) = (
            manifest_library_path(&manifest),
            manifest_library_name(&manifest),
        ) else {
            answers.push(format!("{file}: unreadable manifest"));
            continue;
        };
        match probe.devices(&manifest, &path) {
            Ok(devices) => {
                let before = claims.len();
                claims.extend(
                    devices
                        .iter()
                        .filter(|d| d.render_node == Some(numbers))
                        .map(|d| Claim {
                            manifest: manifest.clone(),
                            library: library.clone(),
                            device: d.name.clone(),
                        }),
                );
                if claims.len() == before {
                    answers.push(format!("{file}: {}", describe(&devices)));
                }
            }
            Err(why) => answers.push(format!("{file}: {why}")),
        }
    }
    match claims.len() {
        1 => Ok(claims.remove(0)),
        0 => Err(format!(
            "no Vulkan driver reports {} ({}:{}) as its render node [{}]",
            node.display(),
            numbers.0,
            numbers.1,
            answers.join("; ")
        )),
        n => Err(format!(
            "{n} Vulkan drivers claim {} ({}) — refusing to guess",
            node.display(),
            claims
                .iter()
                .map(|c| format!("{} = {}", c.manifest.display(), c.device))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The EGL vendor of the claiming Vulkan driver's vendor.
fn egl_vendor_after(claim: Option<&Claim>, search: &IcdSearch) -> Result<PathBuf, String> {
    let Some(claim) = claim else {
        return Err("the EGL vendor follows the Vulkan driver, and none was found".to_string());
    };
    let stem = claim.library.split(".so").next().unwrap_or(&claim.library);
    let Some(vendor) = egl_vendor_for_vulkan(stem) else {
        return Err(format!(
            "no known EGL vendor ships the Vulkan driver `{stem}`"
        ));
    };
    unique_manifest_for(&search.egl_vendor_dirs, vendor, "EGL vendor")
}

/// Asking a real ICD: load its library and talk to it directly, without
/// the loader, so one answer belongs to exactly one manifest.
#[cfg(target_os = "linux")]
mod loaded {
    use super::{Feature, IcdProbe, VulkanDevice};
    use ash::vk;

    use std::path::Path;

    /// The probe the compositor and launcher use.
    pub struct LoadedIcdProbe;

    type NegotiateFn = unsafe extern "system" fn(*mut u32) -> vk::Result;

    impl IcdProbe for LoadedIcdProbe {
        fn devices(&self, _manifest: &Path, library: &Path) -> Result<Vec<VulkanDevice>, String> {
            with_instance(library, vk::API_VERSION_1_1, devices_of)
        }

        fn advertises(
            &self,
            manifest: &Path,
            device: &str,
            features: &[Feature],
        ) -> Result<bool, String> {
            // The loader hands `library_path` to `dlopen` exactly as an
            // absolute path or a bare soname, so the launcher does the
            // same rather than trust the caller's spelling.
            let library = super::manifest_library_path(manifest)
                .ok_or_else(|| format!("{}: unreadable manifest", manifest.display()))?;
            with_instance(&library, vk::API_VERSION_1_2, |instance| {
                advertised(instance, device, features)
            })
        }
    }

    /// Load an ICD's library, open an instance at `api_version`, hand it to
    /// `ask`, and destroy it. The one path both probes take, so a driver
    /// that cannot be loaded is reported the same way either way.
    #[allow(
        unsafe_code,
        reason = "the Vulkan ICD interface is a C ABI reached through dlopen"
    )]
    fn with_instance<T>(
        library: &Path,
        api_version: u32,
        ask: impl FnOnce(&ash::Instance) -> Result<T, String>,
    ) -> Result<T, String> {
        // SAFETY: loading a Vulkan ICD runs its initialisers, exactly as
        // the Vulkan loader would in this process a moment later.
        let lib = unsafe { libloading::Library::new(library) }
            .map_err(|error| format!("cannot load: {error}"))?;
        // SAFETY: the symbol names and signatures are the ICD interface's,
        // fixed by the Vulkan loader-driver contract.
        let get_instance_proc_addr = unsafe {
            if let Ok(negotiate) =
                lib.get::<NegotiateFn>(b"vk_icdNegotiateLoaderICDInterfaceVersion\0")
            {
                let mut version = 5u32;
                let _ = negotiate(&raw mut version);
            }
            *lib.get::<vk::PFN_vkGetInstanceProcAddr>(b"vk_icdGetInstanceProcAddr\0")
                .map_err(|error| format!("not a Vulkan ICD: {error}"))?
        };
        // Mesa drivers register exit handlers; unloading one would run them
        // against freed code. Keep it, as the loader does.
        std::mem::forget(lib);
        // SAFETY: `get_instance_proc_addr` is the ICD's own entry point,
        // kept alive by the library forgotten above.
        let entry = unsafe { entry_of(get_instance_proc_addr) }?;
        let app = vk::ApplicationInfo::default().api_version(api_version);
        let create = vk::InstanceCreateInfo::default().application_info(&app);
        // SAFETY: a plain instance with no layers or extensions.
        let instance = unsafe { entry.create_instance(&create, None) }
            .map_err(|error| format!("vkCreateInstance: {error}"))?;
        let result = ask(&instance);
        // SAFETY: nothing created from the instance outlives this call.
        unsafe { instance.destroy_instance(None) };
        result
    }

    /// Whether the device called `target` answered `VK_TRUE` for every
    /// feature in `features`.
    ///
    /// Every structure a [`Feature`] lives in is chained onto one features
    /// query whatever was asked for; a structure the driver does not know is
    /// left `VK_FALSE`, which is the answer an absent feature should give.
    #[allow(unsafe_code, reason = "Vulkan physical-device queries are a C ABI")]
    fn advertised(
        instance: &ash::Instance,
        target: &str,
        features: &[Feature],
    ) -> Result<bool, String> {
        if features.is_empty() {
            return Ok(true);
        }
        // SAFETY: `instance` is live for the whole call.
        let physical = unsafe { instance.enumerate_physical_devices() }
            .map_err(|error| format!("vkEnumeratePhysicalDevices: {error}"))?;
        for device in physical {
            // SAFETY: `device` came from this instance.
            let properties = unsafe { instance.get_physical_device_properties(device) };
            if properties
                .device_name_as_c_str()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
                != target
            {
                continue;
            }
            let mut address = vk::PhysicalDeviceBufferDeviceAddressFeatures::default();
            let mut placed = vk::PhysicalDeviceMapMemoryPlacedFeaturesEXT::default();
            let mut asked = vk::PhysicalDeviceFeatures2::default()
                .push_next(&mut address)
                .push_next(&mut placed);
            // SAFETY: the device is live and the chained structures are
            // locals of this frame.
            unsafe { instance.get_physical_device_features2(device, &mut asked) };
            let has = |feature: Feature| match feature {
                Feature::BufferDeviceAddress => address.buffer_device_address,
                Feature::MemoryMapPlaced => placed.memory_map_placed,
                Feature::MemoryMapRangePlaced => placed.memory_map_range_placed,
                Feature::MemoryUnmapReserve => placed.memory_unmap_reserve,
            };
            return Ok(features.iter().all(|feature| has(*feature) == vk::TRUE));
        }
        Err(format!(
            "the driver no longer reports a device named `{target}`"
        ))
    }

    /// The Vulkan entry an ICD's `get_instance_proc_addr` opens, or why it
    /// opens none. A driver whose device has failed (an NVIDIA GPU that fell
    /// off the bus, say) can still load yet hand out no vkCreateInstance;
    /// ash panics on a missing entry point, and a panic in the probe cannot
    /// unwind, so it would abort the whole compositor. Such a driver counts
    /// as offering no devices instead.
    ///
    /// # Safety
    ///
    /// `get_instance_proc_addr` must be a Vulkan ICD's entry point that stays
    /// loaded for as long as the returned entry is used.
    #[allow(
        unsafe_code,
        reason = "the Vulkan ICD interface is a C ABI reached through dlopen"
    )]
    unsafe fn entry_of(
        get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    ) -> Result<ash::Entry, String> {
        // SAFETY: the caller's entry point, asked with a null instance for a
        // global command, as the loader-driver contract allows.
        let create_instance =
            unsafe { get_instance_proc_addr(vk::Instance::null(), c"vkCreateInstance".as_ptr()) };
        if create_instance.is_none() {
            return Err("the driver offers no vkCreateInstance".into());
        }
        // SAFETY: the caller guarantees a live ICD entry point.
        Ok(unsafe {
            ash::Entry::from_static_fn(ash::StaticFn {
                get_instance_proc_addr,
            })
        })
    }

    #[allow(
        unsafe_code,
        reason = "the Vulkan ICD interface is a C ABI reached through dlopen"
    )]
    fn devices_of(instance: &ash::Instance) -> Result<Vec<VulkanDevice>, String> {
        // SAFETY: `instance` is live for the whole function.
        let physical = unsafe { instance.enumerate_physical_devices() }
            .map_err(|error| format!("vkEnumeratePhysicalDevices: {error}"))?;
        let mut out = Vec::new();
        for device in physical {
            // SAFETY: `device` came from this instance.
            let properties = unsafe { instance.get_physical_device_properties(device) };
            let name = properties
                .device_name_as_c_str()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if properties.api_version < vk::API_VERSION_1_1 {
                out.push(VulkanDevice {
                    name,
                    render_node: None,
                });
                continue;
            }
            // SAFETY: as above.
            let has_drm = unsafe { instance.enumerate_device_extension_properties(device) }
                .unwrap_or_default()
                .iter()
                .any(|extension| {
                    extension.extension_name_as_c_str().ok()
                        == Some(ash::ext::physical_device_drm::NAME)
                });
            let render_node = if has_drm {
                let mut drm = vk::PhysicalDeviceDrmPropertiesEXT::default();
                let mut properties2 = vk::PhysicalDeviceProperties2::default().push_next(&mut drm);
                // SAFETY: the device is Vulkan 1.1 and advertises the
                // extension whose structure is chained.
                unsafe { instance.get_physical_device_properties2(device, &mut properties2) };
                (drm.has_render == vk::TRUE).then(|| {
                    (
                        u64::try_from(drm.render_major).unwrap_or(u64::MAX),
                        u64::try_from(drm.render_minor).unwrap_or(u64::MAX),
                    )
                })
            } else {
                None
            };
            out.push(VulkanDevice { name, render_node });
        }

        Ok(out)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// What a failed driver answers for every command: nothing.
        unsafe extern "system" fn nothing(
            _instance: vk::Instance,
            _name: *const std::ffi::c_char,
        ) -> vk::PFN_vkVoidFunction {
            None
        }

        /// A driver that loads but hands out no vkCreateInstance is refused
        /// with a reason rather than panicking inside ash.
        #[test]
        #[allow(
            unsafe_code,
            reason = "calls the unsafe probe helper with a test entry point"
        )]
        fn a_driver_without_create_instance_offers_no_entry() {
            // SAFETY: `nothing` is a static function that never dereferences.
            let refused = unsafe { entry_of(nothing) };
            assert_eq!(
                refused.err().as_deref(),
                Some("the driver offers no vkCreateInstance")
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn host_icd_name(driver: &str) -> String {
        format!("{driver}_icd.{}.json", host_arch_token())
    }

    /// Build a fixture ICD tree. `egl` and `vulkan` are
    /// `(file_name, library_path)` pairs written as real manifests, so the
    /// production scanner parses real JSON off a real filesystem.
    fn fixture(dir: &Path, egl: &[(&str, &str)], vulkan: &[(&str, &str)]) -> IcdSearch {
        let egl_dir = dir.join("glvnd/egl_vendor.d");
        let vk_dir = dir.join("vulkan/icd.d");
        std::fs::create_dir_all(&egl_dir).unwrap();
        std::fs::create_dir_all(&vk_dir).unwrap();
        for (name, lib) in egl {
            std::fs::write(
                egl_dir.join(name),
                format!(r#"{{"file_format_version":"1.0.0","ICD":{{"library_path":"{lib}"}}}}"#),
            )
            .unwrap();
        }
        for (name, lib) in vulkan {
            std::fs::write(
                vk_dir.join(name),
                format!(
                    r#"{{"file_format_version":"1.0.1","ICD":{{"library_path":"{lib}","api_version":"1.4.0"}}}}"#
                ),
            )
            .unwrap();
        }
        IcdSearch {
            egl_vendor_dirs: vec![egl_dir],
            vulkan_icd_dirs: vec![vk_dir],
        }
    }

    /// What each ICD library answers, keyed by its file name. A library
    /// with no entry fails to load.
    struct Scripted(HashMap<&'static str, Vec<VulkanDevice>>);

    impl IcdProbe for Scripted {
        fn devices(&self, _manifest: &Path, library: &Path) -> Result<Vec<VulkanDevice>, String> {
            let name = library.file_name().and_then(|n| n.to_str()).unwrap_or("");
            self.0
                .get(name)
                .cloned()
                .ok_or_else(|| format!("cannot load {name}"))
        }
    }

    /// A probe whose driver advertises exactly `features`; asking for any
    /// feature outside them is answered `false`, as an unsupported extension
    /// answers.
    struct Advertises(HashMap<&'static str, Vec<VulkanDevice>>, Vec<Feature>);

    impl IcdProbe for Advertises {
        fn devices(&self, _manifest: &Path, library: &Path) -> Result<Vec<VulkanDevice>, String> {
            let name = library.file_name().and_then(|n| n.to_str()).unwrap_or("");
            self.0
                .get(name)
                .cloned()
                .ok_or_else(|| format!("cannot load {name}"))
        }

        fn advertises(
            &self,
            _manifest: &Path,
            _device: &str,
            features: &[Feature],
        ) -> Result<bool, String> {
            Ok(features.iter().all(|feature| self.1.contains(feature)))
        }
    }

    fn device(name: &str, render_node: Option<(u64, u64)>) -> VulkanDevice {
        VulkanDevice {
            name: name.to_owned(),
            render_node,
        }
    }

    #[test]
    fn a_pinned_vulkan_driver_turns_off_device_selection_and_an_unpinned_one_does_not() {
        let pinned = GpuSelection {
            node: PathBuf::from("/dev/dri/renderD128"),
            driver: None,
            egl_vendor_json: None,
            vulkan_icd_json: Some(PathBuf::from("/usr/share/vulkan/icd.d/x.json")),
            vulkan_device: None,
            pipeline_seed: None,
            site_integration: None,
            notes: Vec::new(),
        };
        assert!(
            pinned
                .env()
                .contains(&(NO_DEVICE_SELECT_ENV, "1".to_owned()))
        );
        let unpinned = GpuSelection {
            vulkan_icd_json: None,
            ..pinned
        };
        assert!(
            !unpinned
                .env()
                .iter()
                .any(|(key, _)| *key == NO_DEVICE_SELECT_ENV),
            "without a pinned driver the layer still picks among the devices"
        );
    }

    #[test]
    fn a_gpu_less_machine_is_pinned_to_lavapipe_and_launches_inherit_only_that() {
        let tmp = tempfile::tempdir().unwrap();
        let search = fixture(
            tmp.path(),
            &[("50_mesa.json", "libEGL_mesa.so.0")],
            &[
                (&host_icd_name("radeon"), "libvulkan_radeon.so"),
                (&host_icd_name("lvp"), "libvulkan_lvp.so"),
            ],
        );
        let pin = software_pin(&search).expect("lavapipe is installed");
        assert!(pin.vulkan_icd_json.ends_with(host_icd_name("lvp")));
        assert!(
            pin.egl_vendor_json
                .as_ref()
                .is_some_and(|path| path.ends_with("50_mesa.json"))
        );
        let lavapipe = pin.vulkan_icd_json.display().to_string();
        assert!(pin.env().contains(&(VK_DRIVER_FILES_ENV, lavapipe.clone())));
        assert!(
            pin.env().contains(&(NO_DEVICE_SELECT_ENV, "1".to_owned())),
            "one pinned driver leaves the device-select layer nothing to choose"
        );
        assert!(pin.env().contains(&(VK_ICD_FILENAMES_ENV, lavapipe)));
        assert_eq!(pin.manifest_files().len(), 2);

        assert_eq!(
            software_pin_from(
                Some(pin.vulkan_icd_json.clone().into()),
                pin.egl_vendor_json.clone().map(Into::into),
            ),
            Some(pin)
        );
        let radeon = search.vulkan_icd_dirs[0].join(host_icd_name("radeon"));
        assert_eq!(
            software_pin_from(Some(radeon.into()), None),
            None,
            "a GPU's driver named in the environment is not evidence of a GPU-less machine"
        );
        assert_eq!(software_pin_from(None, None), None);
        assert_eq!(
            software_pin_from(Some("lvp_icd.json".into()), None),
            None,
            "only an absolute path to an existing manifest is inherited"
        );

        let without = tempfile::tempdir().unwrap();
        let search = fixture(
            without.path(),
            &[],
            &[(&host_icd_name("radeon"), "libvulkan_radeon.so")],
        );
        assert!(software_pin(&search).is_err(), "no lavapipe, no pin");
    }

    const RENDER_D128: (u64, u64) = (226, 128);
    const RENDER_D129: (u64, u64) = (226, 129);

    fn select(search: &IcdSearch, numbers: (u64, u64), probe: &Scripted) -> GpuSelection {
        select_for_device(
            Path::new("/dev/dri/renderD128"),
            numbers,
            Some("whatever-sysfs-says".to_owned()),
            search,
            probe,
        )
    }

    #[test]
    fn the_icd_that_reports_the_node_is_pinned_whatever_sysfs_calls_it() {
        // A hybrid box: NVIDIA on renderD129, AMD on renderD128, lavapipe
        // with no node. The AMD node pins RADV and Mesa's EGL vendor.
        let tmp = tempfile::tempdir().unwrap();
        let search = fixture(
            tmp.path(),
            &[
                ("10_nvidia.json", "libEGL_nvidia.so.0"),
                ("50_mesa.json", "libEGL_mesa.so.0"),
            ],
            &[
                (&host_icd_name("nvidia"), "/usr/lib64/libGLX_nvidia.so.0"),
                (&host_icd_name("radeon"), "/usr/lib64/libvulkan_radeon.so"),
                (&host_icd_name("lvp"), "/usr/lib64/libvulkan_lvp.so"),
            ],
        );
        let probe = Scripted(HashMap::from([
            (
                "libGLX_nvidia.so.0",
                vec![device("NVIDIA GeForce RTX 4060", Some(RENDER_D129))],
            ),
            (
                "libvulkan_radeon.so",
                vec![device("AMD Radeon 780M (RADV PHOENIX)", Some(RENDER_D128))],
            ),
            ("libvulkan_lvp.so", vec![device("llvmpipe", None)]),
        ]));
        let amd = select(&search, RENDER_D128, &probe);
        assert_eq!(
            amd.vulkan_icd_json.as_deref().and_then(|p| p.file_name()),
            Some(host_icd_name("radeon").as_str().as_ref()),
            "{:?}",
            amd.notes
        );
        assert_eq!(
            amd.egl_vendor_json.as_deref().and_then(|p| p.file_name()),
            Some("50_mesa.json".as_ref())
        );
        assert_eq!(
            amd.vulkan_device.as_deref(),
            Some("AMD Radeon 780M (RADV PHOENIX)")
        );
        let env = amd.env();
        let vk = env
            .iter()
            .find(|(k, _)| *k == VK_DRIVER_FILES_ENV)
            .expect("VK_DRIVER_FILES derived");
        assert!(
            env.iter()
                .any(|(k, v)| *k == VK_ICD_FILENAMES_ENV && *v == vk.1),
            "the pre-1.3.207 spelling must carry the same choice: {env:?}"
        );
        assert!(!env.iter().any(|(_, v)| v.contains("nvidia")), "{env:?}");
        // Every file the env names is offered for binding; the device-select
        // switch names no file.
        for (key, value) in env
            .into_iter()
            .filter(|(key, _)| *key != NO_DEVICE_SELECT_ENV)
        {
            assert!(
                amd.manifest_files()
                    .iter()
                    .any(|b| b.display().to_string() == value),
                "{key} points at {value}, which is not bound into the sandbox"
            );
        }

        // Same tree, the NVIDIA node: NVIDIA's ICD and EGL vendor.
        let nvidia = select(&search, RENDER_D129, &probe);
        assert_eq!(
            nvidia
                .vulkan_icd_json
                .as_deref()
                .and_then(|p| p.file_name()),
            Some(host_icd_name("nvidia").as_str().as_ref())
        );
        assert_eq!(
            nvidia
                .egl_vendor_json
                .as_deref()
                .and_then(|p| p.file_name()),
            Some("10_nvidia.json".as_ref())
        );
    }

    #[test]
    fn a_snapdragon_display_node_pins_turnip_over_lavapipe() {
        // The field failure: sysfs names the node's driver `msm_dpu`, which
        // no table listed. Turnip reporting the node is all it takes.
        let tmp = tempfile::tempdir().unwrap();
        let search = fixture(
            tmp.path(),
            &[("50_mesa.json", "libEGL_mesa.so.0")],
            &[
                (&host_icd_name("freedreno"), "libvulkan_freedreno.so"),
                (&host_icd_name("lvp"), "libvulkan_lvp.so"),
                (&host_icd_name("asahi"), "libvulkan_asahi.so"),
            ],
        );
        let probe = Scripted(HashMap::from([
            (
                "libvulkan_freedreno.so",
                vec![device("Turnip Adreno (TM) X1-85", Some(RENDER_D128))],
            ),
            ("libvulkan_lvp.so", vec![device("llvmpipe", None)]),
            ("libvulkan_asahi.so", Vec::new()),
        ]));
        let sel = select_for_device(
            Path::new("/dev/dri/renderD128"),
            RENDER_D128,
            Some("msm_dpu".to_owned()),
            &search,
            &probe,
        );
        assert_eq!(
            sel.vulkan_icd_json.as_deref().and_then(|p| p.file_name()),
            Some(host_icd_name("freedreno").as_str().as_ref()),
            "{:?}",
            sel.notes
        );
        assert_eq!(
            sel.vulkan_device.as_deref(),
            Some("Turnip Adreno (TM) X1-85")
        );
        assert!(sel.egl_vendor_json.is_some(), "{:?}", sel.notes);
        assert!(sel.notes.is_empty(), "{:?}", sel.notes);
    }

    #[test]
    fn nothing_is_pinned_when_no_icd_or_several_claim_the_node() {
        let tmp = tempfile::tempdir().unwrap();
        let search = fixture(
            tmp.path(),
            &[("50_mesa.json", "libEGL_mesa.so.0")],
            &[
                (&host_icd_name("radeon"), "/usr/lib64/libvulkan_radeon.so"),
                (&host_icd_name("lvp"), "/usr/lib64/libvulkan_lvp.so"),
                (&host_icd_name("broken"), "/usr/lib64/libvulkan_broken.so"),
            ],
        );
        // A GL-only GPU: nothing reports the node, so nothing is pinned
        // and every ICD's answer is on the record.
        let probe = Scripted(HashMap::from([
            (
                "libvulkan_radeon.so",
                vec![device("AMD Radeon", Some(RENDER_D129))],
            ),
            ("libvulkan_lvp.so", vec![device("llvmpipe", None)]),
        ]));
        let sel = select(&search, RENDER_D128, &probe);
        assert!(sel.is_empty());
        assert!(sel.env().is_empty());
        let notes = sel.notes.join("\n");
        assert!(notes.contains("AMD Radeon on 226:129"), "{notes}");
        assert!(notes.contains("llvmpipe without a render node"), "{notes}");
        assert!(notes.contains("cannot load libvulkan_broken.so"), "{notes}");
        assert!(notes.contains(EGL_VENDOR_ENV), "{notes}");

        // Two drivers claiming one node is ambiguous, not a choice.
        let probe = Scripted(HashMap::from([
            (
                "libvulkan_radeon.so",
                vec![device("RADV", Some(RENDER_D128))],
            ),
            (
                "libvulkan_lvp.so",
                vec![device("also me", Some(RENDER_D128))],
            ),
        ]));
        let sel = select(&search, RENDER_D128, &probe);
        assert_eq!(sel.vulkan_icd_json, None);
        assert!(
            sel.notes.iter().any(|n| n.contains("refusing to guess")),
            "{:?}",
            sel.notes
        );
    }

    #[test]
    fn foreign_architecture_manifests_are_never_asked() {
        let foreign_arch = ARCH_TOKENS
            .iter()
            .copied()
            .find(|arch| *arch != host_arch_token())
            .expect("a recognized architecture different from this host");
        let tmp = tempfile::tempdir().unwrap();
        let search = fixture(
            tmp.path(),
            &[("50_mesa.json", "libEGL_mesa.so.0")],
            &[
                (
                    &format!("radeon_icd.{foreign_arch}.json"),
                    "/usr/lib/libvulkan_radeon32.so",
                ),
                (&host_icd_name("radeon"), "/usr/lib64/libvulkan_radeon.so"),
            ],
        );
        // Both would claim the node; only the host's file may be asked, or
        // selection is ambiguous.
        let probe = Scripted(HashMap::from([
            (
                "libvulkan_radeon32.so",
                vec![device("RADV", Some(RENDER_D128))],
            ),
            (
                "libvulkan_radeon.so",
                vec![device("RADV", Some(RENDER_D128))],
            ),
        ]));
        let sel = select(&search, RENDER_D128, &probe);
        assert_eq!(
            sel.vulkan_icd_json,
            Some(search.vulkan_icd_dirs[0].join(host_icd_name("radeon"))),
            "{:?}",
            sel.notes
        );
        assert!(ARCH_TOKENS.contains(&host_arch_token()));
        assert!(!is_foreign_arch("50_mesa.json"), "no token means eligible");
    }

    #[test]
    fn relative_library_paths_resolve_against_their_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let search = fixture(
            tmp.path(),
            &[],
            &[
                ("abs.json", "/usr/lib64/libvulkan_radeon.so"),
                ("bare.json", "libvulkan_radeon.so"),
                ("rel.json", "../lib/libvulkan_radeon.so"),
            ],
        );
        let dir = &search.vulkan_icd_dirs[0];
        assert_eq!(
            manifest_library_path(&dir.join("abs.json")),
            Some(PathBuf::from("/usr/lib64/libvulkan_radeon.so"))
        );
        assert_eq!(
            manifest_library_path(&dir.join("bare.json")),
            Some(PathBuf::from("libvulkan_radeon.so"))
        );
        assert_eq!(
            manifest_library_path(&dir.join("rel.json")),
            Some(dir.join("../lib/libvulkan_radeon.so"))
        );
    }

    /// A tree where a driver reports the node, with `advertised` as the
    /// features it answers `VK_TRUE` for.
    fn placement_tree(tmp: &Path, advertised: Vec<Feature>) -> (IcdSearch, Advertises) {
        let search = fixture(
            tmp,
            &[("50_mesa.json", "libEGL_mesa.so.0")],
            &[
                (&host_icd_name("asahi"), "/usr/lib64/libvulkan_asahi.so"),
                (&host_icd_name("lvp"), "/usr/lib64/libvulkan_lvp.so"),
            ],
        );
        let probe = Advertises(
            HashMap::from([
                (
                    "libvulkan_asahi.so",
                    vec![device("Apple M1 (G13G B0)", Some(RENDER_D128))],
                ),
                ("libvulkan_lvp.so", vec![device("llvmpipe", None)]),
            ]),
            advertised,
        );
        (search, probe)
    }

    fn selection_for(spelled: &str, search: &IcdSearch, probe: &Advertises) -> GpuSelection {
        select_for_device(
            Path::new("/dev/dri/renderD128"),
            RENDER_D128,
            Some(spelled.to_owned()),
            search,
            probe,
        )
    }

    #[test]
    fn a_driver_that_places_memory_gets_the_site_integration_switch() {
        let tmp = tempfile::tempdir().unwrap();
        let (search, probe) = placement_tree(
            tmp.path(),
            vec![
                Feature::BufferDeviceAddress,
                Feature::MemoryMapPlaced,
                Feature::MemoryMapRangePlaced,
                Feature::MemoryUnmapReserve,
            ],
        );
        let sel = selection_for("asahi", &search, &probe);
        assert_eq!(sel.site_integration, Some(SiteIntegration::TargetBuffers));
        assert!(
            sel.env()
                .contains(&(SITE_INTEGRATION_ENV, "target-buffers".to_owned())),
            "{:?}",
            sel.env()
        );
    }

    #[test]
    fn placement_without_target_buffers_is_reported_as_placement_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let (search, probe) = placement_tree(
            tmp.path(),
            vec![Feature::MemoryMapPlaced, Feature::MemoryMapRangePlaced],
        );
        assert_eq!(
            selection_for("asahi", &search, &probe).site_integration,
            Some(SiteIntegration::Placed)
        );
    }

    #[test]
    fn a_partial_placement_advertisement_is_not_enough_for_target_buffers() {
        // `memoryMapPlaced` and a placed range, but no unmap reservation:
        // the range could be handed out twice, so this is placement only.
        let tmp = tempfile::tempdir().unwrap();
        let (search, probe) = placement_tree(
            tmp.path(),
            vec![
                Feature::BufferDeviceAddress,
                Feature::MemoryMapPlaced,
                Feature::MemoryMapRangePlaced,
            ],
        );
        assert_eq!(
            selection_for("asahi", &search, &probe).site_integration,
            Some(SiteIntegration::Placed)
        );
    }

    #[test]
    fn a_driver_that_cannot_place_memory_leaves_the_switch_alone() {
        // A native GPU: it claims the node and pins its loaders, and the
        // variable a translation layer reads never appears.
        let tmp = tempfile::tempdir().unwrap();
        let (search, probe) = placement_tree(tmp.path(), vec![Feature::BufferDeviceAddress]);
        let sel = selection_for("asahi", &search, &probe);
        assert!(sel.vulkan_icd_json.is_some(), "{:?}", sel.notes);
        assert_eq!(sel.site_integration, None);
        assert!(
            !sel.env()
                .iter()
                .any(|(key, _)| *key == SITE_INTEGRATION_ENV),
            "{:?}",
            sel.env()
        );
    }

    #[test]
    fn an_unpinned_session_never_sets_the_site_integration_switch() {
        // Nothing claims the node, so there is no driver to ask and the
        // environment stays exactly as the loaders leave it.
        let tmp = tempfile::tempdir().unwrap();
        let (search, probe) = placement_tree(
            tmp.path(),
            vec![
                Feature::BufferDeviceAddress,
                Feature::MemoryMapPlaced,
                Feature::MemoryMapRangePlaced,
                Feature::MemoryUnmapReserve,
            ],
        );
        let elsewhere = Advertises(
            HashMap::from([(
                "libvulkan_asahi.so",
                vec![device("Apple M1 (G13G B0)", Some((226, 129)))],
            )]),
            probe.1,
        );
        let sel = selection_for("asahi", &search, &elsewhere);
        assert!(sel.is_empty());
        assert_eq!(sel.site_integration, None);
        assert!(
            !sel.env()
                .iter()
                .any(|(key, _)| *key == SITE_INTEGRATION_ENV),
            "{:?}",
            sel.env()
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn host_render_node_resolves_to_files_that_exist() {
        // The path the compositor actually calls, against this machine's
        // real drivers: whatever it pins has to exist.
        assert_eq!(driver_for(Path::new("/dev/null")), None);
        assert_eq!(driver_for(Path::new("/dev/dri/renderD9999")), None);
        let Some(node) = std::fs::read_dir("/dev/dri")
            .ok()
            .into_iter()
            .flatten()
            .filter_map(std::result::Result::ok)
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("renderD"))
            })
        else {
            eprintln!("skipping: no /dev/dri/renderD* on this machine");
            return;
        };
        let sel = select_for_node(&node, &IcdSearch::default());
        for path in sel.manifest_files() {
            assert!(
                path.exists(),
                "{} was selected but does not exist",
                path.display()
            );
        }
        assert_eq!(
            select_for_node(&node, &IcdSearch::default()),
            sel,
            "a second ask answers from the cache"
        );
    }
}
