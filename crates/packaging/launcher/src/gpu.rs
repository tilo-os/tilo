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
//! [`select_for_driver`] uses explicit driver/search inputs on every host.
//! Sysfs discovery and default host search paths are Linux-only.
//!
//! # Derivation, not a hardcoded vendor
//!
//! The input is a render node: its kernel driver comes from sysfs, the
//! driver maps to the implementation library its userspace ICD loads
//! (`amdgpu` → `libvulkan_radeon`, …), and installed ICD JSONs are
//! scanned for the one whose `library_path` names that library. Nothing
//! here assumes AMD or a Fedora path layout.
//!
//! If exactly one ICD matches, it's pinned. If none does, the variable
//! is **not set** and the reason is recorded in
//! [`GpuSelection::notes`] — a wrong pin strands the app on a driver
//! that can't drive its GPU, while no pin merely leaves the loader's
//! own default in place, the strictly smaller failure.

use std::path::{Path, PathBuf};

/// GLVND's EGL vendor selection: a colon-separated list of vendor ICD
/// JSONs; when set, GLVND loads only these and skips the directory scan.
pub const EGL_VENDOR_ENV: &str = "__EGL_VENDOR_LIBRARY_FILENAMES";

/// Vulkan loader driver selection (loader 1.3.207+).
pub const VK_DRIVER_FILES_ENV: &str = "VK_DRIVER_FILES";

/// Pre-1.3.207 spelling of [`VK_DRIVER_FILES_ENV`], set in lock-step so
/// an older loader honours the same choice.
pub const VK_ICD_FILENAMES_ENV: &str = "VK_ICD_FILENAMES";

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
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuSelection {
    /// The DRM render node the compositor resolved, e.g.
    /// `/dev/dri/renderD128`.
    pub node: PathBuf,
    /// Kernel driver bound to that node, from sysfs. `None` when sysfs had
    /// no answer, in which case nothing can be derived and every field
    /// below is `None`.
    pub driver: Option<String>,
    /// GLVND EGL vendor manifest matching `driver`, if exactly one is
    /// installed.
    pub egl_vendor_json: Option<PathBuf>,
    /// Vulkan ICD manifest matching `driver`, if exactly one is
    /// installed.
    pub vulkan_icd_json: Option<PathBuf>,
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

/// The EGL vendor library a kernel driver's userspace is implemented
/// by. Everything Mesa drives shares one GLVND vendor; NVIDIA ships its
/// own.
fn egl_vendor_library(driver: &str) -> Option<&'static str> {
    match driver {
        "nvidia" | "nvidia-drm" => Some("libEGL_nvidia"),
        // Every in-tree DRM driver is served by Mesa's GLVND vendor, listed
        // explicitly so an unrecognised driver falls through to "don't pin"
        // instead of being pointed at Mesa on a hunch.
        "amdgpu" | "radeon" | "i915" | "xe" | "nouveau" | "msm" | "v3d" | "vc4" | "lima"
        | "panfrost" | "panthor" | "etnaviv" | "virtio_gpu" | "vgem" | "vkms" | "asahi"
        | "powervr" | "komeda" | "tegra" => Some("libEGL_mesa"),
        _ => None,
    }
}

/// The Vulkan driver library that serves a kernel driver.
///
/// One kernel driver, one Vulkan implementation. Drivers with no
/// Vulkan implementation map to `None`, correctly leaving
/// `VK_DRIVER_FILES` unset.
fn vulkan_driver_library(driver: &str) -> Option<&'static str> {
    match driver {
        "amdgpu" | "radeon" => Some("libvulkan_radeon"),
        "i915" | "xe" => Some("libvulkan_intel"),
        "nouveau" => Some("libvulkan_nouveau"),
        "msm" => Some("libvulkan_freedreno"),
        "v3d" => Some("libvulkan_broadcom"),
        "panfrost" | "panthor" => Some("libvulkan_panfrost"),
        "asahi" => Some("libvulkan_asahi"),
        "powervr" => Some("libvulkan_powervr"),
        "virtio_gpu" => Some("libvulkan_virtio"),
        // NVIDIA's Vulkan ICD is implemented inside libGLX_nvidia.
        "nvidia" | "nvidia-drm" => Some("libGLX_nvidia"),
        _ => None,
    }
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

/// Derive the loader pinning for `node`, reading its driver from sysfs.
#[cfg(target_os = "linux")]
pub fn select_for_node(node: &Path, search: &IcdSearch) -> GpuSelection {
    let driver = driver_for(node);
    match driver {
        Some(d) => select_for_driver(node, &d, search),
        None => GpuSelection {
            node: node.to_path_buf(),
            driver: None,
            egl_vendor_json: None,
            vulkan_icd_json: None,
            notes: vec![format!(
                "no kernel driver reported for {} in /sys/class/drm; \
                 leaving the EGL/Vulkan loaders at their own defaults",
                node.display()
            )],
        },
    }
}

/// Derive the loader pinning for a node whose driver is already known.
/// The function the tests drive: taking driver name and search paths
/// as data lets a fixture tree stand in for the host's `/usr/share`.
pub fn select_for_driver(node: &Path, driver: &str, search: &IcdSearch) -> GpuSelection {
    let mut notes = Vec::new();

    let egl_vendor_json = if let Some(lib) = egl_vendor_library(driver) {
        match unique_manifest_for(&search.egl_vendor_dirs, lib, "EGL vendor") {
            Ok(p) => Some(p),
            Err(why) => {
                notes.push(format!("{EGL_VENDOR_ENV} not set: {why}"));
                None
            }
        }
    } else {
        notes.push(format!(
            "{EGL_VENDOR_ENV} not set: no known EGL vendor for driver `{driver}`"
        ));
        None
    };

    let vulkan_icd_json = if let Some(lib) = vulkan_driver_library(driver) {
        match unique_manifest_for(&search.vulkan_icd_dirs, lib, "Vulkan ICD") {
            Ok(p) => Some(p),
            Err(why) => {
                notes.push(format!("{VK_DRIVER_FILES_ENV} not set: {why}"));
                None
            }
        }
    } else {
        notes.push(format!(
            "{VK_DRIVER_FILES_ENV} not set: no known Vulkan driver for `{driver}`"
        ));
        None
    };

    GpuSelection {
        node: node.to_path_buf(),
        driver: Some(driver.to_string()),
        egl_vendor_json,
        vulkan_icd_json,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn loader_manifests_pin_the_selected_render_nodes_driver() {
        // Phase `amdgpu_node_pins_mesa_and_radv_on_a_hybrid_host`.
        // A hybrid box with both stacks installed: asking for the amdgpu node
        // must pin Mesa's EGL vendor and RADV, never NVIDIA's.
        {
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
                    (&host_icd_name("intel"), "/usr/lib64/libvulkan_intel.so"),
                ],
            );
            let sel = select_for_driver(Path::new("/dev/dri/renderD128"), "amdgpu", &search);
            assert_eq!(
                sel.egl_vendor_json.as_deref().and_then(|p| p.file_name()),
                Some("50_mesa.json".as_ref()),
                "notes: {:?}",
                sel.notes
            );
            assert_eq!(
                sel.vulkan_icd_json.as_deref().and_then(|p| p.file_name()),
                Some(host_icd_name("radeon").as_str().as_ref()),
                "notes: {:?}",
                sel.notes
            );
            // The env the app runs with names those files and nothing else.
            let env = sel.env();
            let vk = env
                .iter()
                .find(|(k, _)| *k == VK_DRIVER_FILES_ENV)
                .expect("VK_DRIVER_FILES derived");
            assert!(vk.1.ends_with(&host_icd_name("radeon")), "{vk:?}");
            assert!(
                env.iter()
                    .any(|(k, v)| *k == VK_ICD_FILENAMES_ENV && *v == vk.1),
                "the pre-1.3.207 spelling must carry the same choice: {env:?}"
            );
            assert!(
                !env.iter().any(|(_, v)| v.contains("nvidia")),
                "an amdgpu node must never hand the app an NVIDIA manifest: {env:?}"
            );
        }
        // Phase `nvidia_node_pins_the_nvidia_manifests`.
        // Same tree, NVIDIA node: the derivation is device-driven, not a
        // hardcoded preference for Mesa.
        {
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
                ],
            );
            let sel = select_for_driver(Path::new("/dev/dri/renderD129"), "nvidia", &search);
            assert_eq!(
                sel.egl_vendor_json.as_deref().and_then(|p| p.file_name()),
                Some("10_nvidia.json".as_ref())
            );
            assert_eq!(
                sel.vulkan_icd_json.as_deref().and_then(|p| p.file_name()),
                Some(host_icd_name("nvidia").as_str().as_ref())
            );
        }
        // Phase `single_gpu_host_still_pins_its_only_driver`.
        // The common case: one GPU, one Mesa install. Must still produce a
        // pin rather than bailing because there's nothing to disambiguate.
        {
            let tmp = tempfile::tempdir().unwrap();
            let search = fixture(
                tmp.path(),
                &[("50_mesa.json", "libEGL_mesa.so.0")],
                &[(&host_icd_name("intel"), "/usr/lib64/libvulkan_intel.so")],
            );
            let sel = select_for_driver(Path::new("/dev/dri/renderD128"), "i915", &search);
            assert_eq!(
                sel.egl_vendor_json.as_deref().and_then(|p| p.file_name()),
                Some("50_mesa.json".as_ref())
            );
            assert_eq!(
                sel.vulkan_icd_json.as_deref().and_then(|p| p.file_name()),
                Some(host_icd_name("intel").as_str().as_ref())
            );
            assert!(!sel.is_empty());
        }
        // Phase `every_pinned_file_is_offered_for_binding`.
        // Every file the env names must be bindable — the two lists derive
        // from the same fields, so they cannot drift.
        {
            let tmp = tempfile::tempdir().unwrap();
            let search = fixture(
                tmp.path(),
                &[("50_mesa.json", "libEGL_mesa.so.0")],
                &[(&host_icd_name("radeon"), "/usr/lib64/libvulkan_radeon.so")],
            );
            let sel = select_for_driver(Path::new("/dev/dri/renderD128"), "amdgpu", &search);
            let binds = sel.manifest_files();
            assert_eq!(
                binds,
                vec![
                    search.egl_vendor_dirs[0].join("50_mesa.json"),
                    search.vulkan_icd_dirs[0].join(host_icd_name("radeon")),
                ],
                "both loader manifests must be selected before checking their binds: {:?}",
                sel.notes
            );
            let env = sel.env();
            assert_eq!(
                env.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
                vec![EGL_VENDOR_ENV, VK_DRIVER_FILES_ENV, VK_ICD_FILENAMES_ENV]
            );
            for (key, value) in env {
                assert!(
                    binds.iter().any(|b| b.display().to_string() == value),
                    "{key} points at {value}, which is not bound into the sandbox"
                );
            }
        }
    }

    #[test]
    fn unmatched_or_foreign_loader_manifests_pin_nothing() {
        // Phase `an_unmatched_driver_sets_nothing_and_says_why`.
        // Nothing installed for this driver → nothing set, reason on the
        // record. A guess here would strand the app on a driver that can't
        // drive its GPU.
        {
            let tmp = tempfile::tempdir().unwrap();
            let search = fixture(
                tmp.path(),
                &[("50_mesa.json", "libEGL_mesa.so.0")],
                &[(&host_icd_name("radeon"), "/usr/lib64/libvulkan_radeon.so")],
            );
            // `lima` is a Mesa driver with no Vulkan implementation: EGL
            // pins, Vulkan does not.
            let sel = select_for_driver(Path::new("/dev/dri/renderD128"), "lima", &search);
            assert!(sel.egl_vendor_json.is_some());
            assert_eq!(sel.vulkan_icd_json, None);
            assert!(
                sel.notes.iter().any(|n| n.contains(VK_DRIVER_FILES_ENV)),
                "the un-pinned loader must be explained: {:?}",
                sel.notes
            );
            assert!(
                !sel.env().iter().any(|(k, _)| *k == VK_DRIVER_FILES_ENV),
                "an underived variable must be absent, not empty: {:?}",
                sel.env()
            );

            // A driver nothing knows about pins neither loader.
            let unknown =
                select_for_driver(Path::new("/dev/dri/renderD128"), "some_future_gpu", &search);
            assert!(unknown.is_empty());
            assert!(unknown.env().is_empty());
            assert_eq!(unknown.notes.len(), 2, "{:?}", unknown.notes);
        }
        // Phase `foreign_architecture_manifests_are_not_candidates`.
        // Manifests for the host and a different recognized architecture name
        // the same library — only the host file is eligible, or selection is ambiguous.
        {
            let foreign_arch = ARCH_TOKENS
                .iter()
                .copied()
                .find(|arch| *arch != host_arch_token())
                .expect("a recognized architecture different from this host");
            let foreign_name = format!("radeon_icd.{foreign_arch}.json");
            let tmp = tempfile::tempdir().unwrap();
            let search = fixture(
                tmp.path(),
                &[("50_mesa.json", "libEGL_mesa.so.0")],
                &[
                    (&foreign_name, "/usr/lib/libvulkan_radeon.so"),
                    (&host_icd_name("radeon"), "/usr/lib64/libvulkan_radeon.so"),
                ],
            );
            let sel = select_for_driver(Path::new("/dev/dri/renderD128"), "amdgpu", &search);
            let picked = sel.vulkan_icd_json.expect("exactly one arch is eligible");
            assert_eq!(
                picked,
                search.vulkan_icd_dirs[0].join(host_icd_name("radeon")),
                "the foreign manifest must not replace or make the host manifest ambiguous"
            );
            assert!(
                picked.to_string_lossy().contains(host_arch_token()),
                "picked {picked:?} on {}",
                host_arch_token()
            );
        }
        // Phase `arch_token_uses_the_manifest_spelling_not_rusts`.
        // The architecture token has to be the one the manifest *files* are
        // named with, not always Rust's spelling — getting it wrong silently
        // unpins the driver.
        {
            assert!(
                ARCH_TOKENS.contains(&host_arch_token()),
                "`{}` is not a token any ICD file name uses",
                host_arch_token()
            );
            assert!(!is_foreign_arch("50_mesa.json"), "no token means eligible");
            assert!(
                !is_foreign_arch(&format!("radeon_icd.{}.json", host_arch_token())),
                "the host's own architecture must always be eligible"
            );
        }
        // Phase `a_host_with_no_icd_directories_pins_nothing`.
        // No ICD directories at all (a stripped container image) is not an
        // error — the loaders keep their defaults and the app still runs.
        {
            let search = IcdSearch {
                egl_vendor_dirs: vec![PathBuf::from("/nonexistent/egl")],
                vulkan_icd_dirs: vec![PathBuf::from("/nonexistent/vulkan")],
            };
            let sel = select_for_driver(Path::new("/dev/dri/renderD128"), "amdgpu", &search);
            assert!(sel.is_empty());
            assert!(sel.env().is_empty());
            assert!(sel.manifest_files().is_empty());
            assert!(
                sel.notes.iter().all(|n| n.contains("no ")),
                "{:?}",
                sel.notes
            );
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn host_render_node_driver_lookup() {
        // Phase `host_render_node_resolves_to_files_that_exist`.
        // On a machine with a real render node, the whole chain — sysfs
        // driver → installed ICDs → env — has to produce a file that exists.
        // This is the path the compositor actually calls.
        fn host_render_node_resolves_to_files_that_exist() {
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
                    "{} was selected for driver {:?} but does not exist",
                    path.display(),
                    sel.driver
                );
            }
        }
        // Phase `driver_lookup_rejects_non_drm_paths`.
        // The sysfs lookup must not invent a driver for a path that isn't
        // a DRM node.
        {
            assert_eq!(driver_for(Path::new("/dev/null")), None);
            assert_eq!(driver_for(Path::new("/dev/dri/renderD9999")), None);
        }
        // Phase `host_render_node_resolves_to_files_that_exist` (defined above).
        host_render_node_resolves_to_files_that_exist();
    }
}
