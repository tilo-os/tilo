//! A pipeline cache every app starts from, written by the session.
//!
//! Each app has its own home, so each has its own Mesa shader cache, and the
//! first launch of every app on a machine compiles vello's ~30 compute
//! pipelines from scratch: ~300 ms on a Snapdragon X with turnip, seconds on
//! lavapipe. The compositor renders with the same vello on the same device,
//! so it already holds every one of them compiled. It publishes them as a
//! `wgpu::PipelineCache` blob in `$XDG_RUNTIME_DIR/tilo/pipeline-seed/`, and
//! the launcher binds that directory **read-only** into every sandbox and
//! names it in `TILO_PIPELINE_SEED_DIR` (the host environment's `paths.pipeline_seed`).
//!
//! Only the session writes the seed. Apps read it, and never write anything
//! another app reads: a cache another app could write would let it plant
//! binaries in its neighbours' GPU pipelines. The blob is keyed by
//! [`wgpu::util::pipeline_cache_key`] (vendor and device), and both wgpu and
//! the driver validate its header against the device, so a seed from another
//! GPU or driver version is ignored rather than trusted.

use std::path::{Path, PathBuf};

/// The seed blob's file under `dir` for this adapter, if its backend has
/// pipeline caches at all.
#[must_use]
pub fn seed_file(dir: &Path, adapter: &wgpu::AdapterInfo) -> Option<PathBuf> {
    wgpu::util::pipeline_cache_key(adapter).map(|key| dir.join(key))
}

fn supported(device: &crate::gpu::DeviceHandle) -> bool {
    device
        .device
        .features()
        .contains(wgpu::Features::PIPELINE_CACHE)
}

#[allow(
    unsafe_code,
    reason = "wgpu marks pipeline cache creation unsafe; see the SAFETY note"
)]
fn create(device: &crate::gpu::DeviceHandle, data: Option<&[u8]>) -> wgpu::PipelineCache {
    // SAFETY: `data` is either absent or a blob `publish` wrote from
    // `PipelineCache::get_data` for an adapter with this cache key. wgpu
    // checks its own header (adapter key, driver, checksum) before the
    // driver sees it and `fallback` starts empty on any mismatch; the
    // driver validates its header again.
    unsafe {
        device
            .device
            .create_pipeline_cache(&wgpu::PipelineCacheDescriptor {
                label: Some("tilo-pipeline-seed"),
                data,
                fallback: true,
            })
    }
}

/// A pipeline cache started from the session's seed, when the launcher
/// provided one for this device. `None` leaves vello compiling without one.
#[must_use]
pub fn seeded(device: &crate::gpu::DeviceHandle) -> Option<wgpu::PipelineCache> {
    if !supported(device) {
        return None;
    }
    let dir = crate::host::environment().paths.pipeline_seed.clone()?;
    let file = seed_file(&dir, &device.adapter.get_info())?;
    let data = std::fs::read(file).ok()?;
    Some(create(device, Some(&data)))
}

/// An empty pipeline cache for the session to fill and then [`publish`].
#[must_use]
pub fn empty(device: &crate::gpu::DeviceHandle) -> Option<wgpu::PipelineCache> {
    supported(device).then(|| create(device, None))
}

/// Write `cache` as the seed for `device` under `dir`, replacing any older
/// one atomically, readable by everyone and writable by its owner only.
///
/// # Errors
///
/// The I/O error that kept the seed from being written, or `Ok(false)` when
/// the backend has no cache data to share.
pub fn publish(
    device: &crate::gpu::DeviceHandle,
    cache: &wgpu::PipelineCache,
    dir: &Path,
) -> std::io::Result<bool> {
    let Some(file) = seed_file(dir, &device.adapter.get_info()) else {
        return Ok(false);
    };
    let Some(data) = cache.get_data() else {
        return Ok(false);
    };
    std::fs::create_dir_all(dir)?;
    let staging = file.with_extension("tmp");
    std::fs::write(&staging, &data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o644))?;
    }
    std::fs::rename(&staging, &file)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_seed_is_keyed_by_adapter_and_round_trips() {
        let info = |vendor, device| wgpu::AdapterInfo {
            name: "test".into(),
            vendor,
            device,
            device_type: wgpu::DeviceType::IntegratedGpu,
            device_pci_bus_id: String::new(),
            driver: "turnip".into(),
            driver_info: String::new(),
            backend: wgpu::Backend::Vulkan,
            subgroup_min_size: 4,
            subgroup_max_size: 128,
            transient_saves_memory: false,
        };
        let dir = Path::new("/run/tilo/pipeline-seed");
        let adreno = seed_file(dir, &info(0x5143, 0x4350_0100)).expect("vulkan has a key");
        let other = seed_file(dir, &info(0x1002, 0x15bf)).expect("vulkan has a key");
        assert!(adreno.starts_with(dir));
        assert_ne!(
            adreno, other,
            "a seed for one GPU is never offered to another"
        );
        let mut gl = info(0x5143, 0x4350_0100);
        gl.backend = wgpu::Backend::Gl;
        assert_eq!(seed_file(dir, &gl), None);

        // The real thing, where a GPU with pipeline caches is available.
        let mut context = crate::gpu::RenderContext::new();
        let Some(index) = pollster::block_on(context.device(None)) else {
            return;
        };
        let device = &context.devices[index];
        let Some(cache) = empty(device) else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let _ = vello::Renderer::new(
            &device.device,
            vello::RendererOptions {
                use_cpu: false,
                antialiasing_support: vello::AaSupport::area_only(),
                num_init_threads: None,
                pipeline_cache: Some(cache.clone()),
            },
        )
        .unwrap();
        if publish(device, &cache, tmp.path()).unwrap() {
            let file = seed_file(tmp.path(), &device.adapter.get_info()).unwrap();
            assert!(std::fs::metadata(file).unwrap().len() > 0);
        }
    }
}
