//! The app's own vello pipelines, kept across launches.
//!
//! [`crate::pipeline_seed`] hands every app the pipelines the session's
//! compositor compiled. That covers an app only when the compositor runs the
//! same vello on the same GPU and driver; a compositor from another build, a
//! desktop without one, or a shader the compositor never builds leaves the
//! app compiling from nothing on every launch. So an app also keeps what it
//! compiled, in a `wgpu::PipelineCache` blob under its **own** cache
//! directory (`$XDG_CACHE_HOME`, `/run/tilo/app/.cache` in a sandbox), and
//! starts from that on its next launch.
//!
//! The file is private to the app: only this app writes it (mode 0600 in a
//! 0700 directory) and only this app reads it, so nothing crosses a sandbox.
//! It is keyed by the adapter, the driver and the shaders' identity
//! ([`vello::SHADER_FINGERPRINT`]): the shaders live in the shared runtime,
//! not the app binary, so they are the version of what is cached. A file for
//! another key, a truncated one or one whose checksum fails is ignored, and
//! the app falls back to the session's seed, else to compiling. wgpu and the
//! driver check their own headers again before using the blob.

use std::path::{Path, PathBuf};
use tilo_lock::MutexRecover as _;

/// The file format: [`MAGIC`], the 32-byte identity, the BLAKE3 hash of the
/// blob, then the blob `wgpu::PipelineCache::get_data` returned.
const MAGIC: &[u8; 8] = b"TILOPLC1";
const HEADER_LEN: usize = MAGIC.len() + 32 + 32;
/// Far above vello's few dozen pipelines on any driver; a larger file is not
/// one this module wrote, and is never read whole.
const LIMIT: usize = 64 * 1024 * 1024;
/// Bump whenever the meaning of the identity or the blob changes.
const ABI: &[u8] = b"tilo-pipeline-cache-v1/wgpu-29";

/// Which cache the renderer started from; a startup mark records it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    /// This app's own file from an earlier launch.
    Own,
    /// The session's read-only seed.
    Seed,
    /// Nothing usable: every pipeline compiles.
    Empty,
}

/// The identity a cache file must carry to be used on this adapter, driver
/// and shader set.
pub(crate) fn identity(adapter: &wgpu::AdapterInfo, shaders: u64) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    let mut part = |bytes: &[u8]| {
        hash.update(&(bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    };
    part(ABI);
    part(format!("{:?}", adapter.backend).as_bytes());
    part(&adapter.vendor.to_le_bytes());
    part(&adapter.device.to_le_bytes());
    part(adapter.name.as_bytes());
    part(adapter.driver.as_bytes());
    part(adapter.driver_info.as_bytes());
    part(&shaders.to_le_bytes());
    *hash.finalize().as_bytes()
}

/// The file bytes for `blob`, or `None` when it is too large to keep.
pub(crate) fn encode(identity: &[u8; 32], blob: &[u8]) -> Option<Vec<u8>> {
    if blob.is_empty() || blob.len() > LIMIT - HEADER_LEN {
        return None;
    }
    let mut bytes = Vec::with_capacity(HEADER_LEN + blob.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(identity);
    bytes.extend_from_slice(blake3::hash(blob).as_bytes());
    bytes.extend_from_slice(blob);
    Some(bytes)
}

/// The blob in `bytes`, if they are a whole file of this format for
/// `identity`.
pub(crate) fn decode<'a>(bytes: &'a [u8], identity: &[u8; 32]) -> Option<&'a [u8]> {
    if bytes.len() > LIMIT {
        return None;
    }
    let (magic, rest) = bytes.split_at_checked(MAGIC.len())?;
    let (key, rest) = rest.split_at_checked(32)?;
    let (checksum, blob) = rest.split_at_checked(32)?;
    (magic == MAGIC
        && key == identity
        && !blob.is_empty()
        && checksum == blake3::hash(blob).as_bytes())
    .then_some(blob)
}

/// Caches whose renderer is still building pipelines (see
/// [`AppPipelineCache::keep_until_built`]).
static UNSAVED: std::sync::Mutex<Vec<AppPipelineCache>> = std::sync::Mutex::new(Vec::new());

/// Keep every cache held by [`AppPipelineCache::keep_until_built`]: a
/// renderer has built all it will. Each is written on a background thread.
pub(crate) fn persist_unsaved() {
    let unsaved = std::mem::take(&mut *UNSAVED.lock_recover());
    for cache in unsaved {
        cache.persist();
    }
}

fn read_bounded(file: &Path) -> Option<Vec<u8>> {
    let (dir, name) = tilo_files::file_at(file).ok()?;
    tilo_files::blocking::read_bounded(&dir, &name, LIMIT).ok()?
}

/// The pipeline cache a window's renderer builds with, and where to keep it.
pub(crate) struct AppPipelineCache {
    cache: wgpu::PipelineCache,
    file: PathBuf,
    identity: [u8; 32],
    origin: Origin,
    /// The length of the blob this launch read from its own file: a cache
    /// that did not grow compiled nothing new and is not written again.
    loaded_len: Option<usize>,
}

impl AppPipelineCache {
    /// This app's cache file if it is valid for the device, else the
    /// session's seed, else an empty cache. `None` when the backend has no
    /// pipeline caches or the process has no cache directory.
    pub(crate) fn open(device: &crate::gpu::DeviceHandle) -> Option<Self> {
        if !crate::pipeline_seed::supported(device) {
            return None;
        }
        let dir = crate::host::environment().paths.cache_dir()?;
        Self::open_in(
            device,
            &dir.join("tilo").join("pipelines"),
            crate::pipeline_seed::seed_data,
        )
    }

    /// [`Self::open`] with the cache directory and the seed supplied.
    pub(crate) fn open_in(
        device: &crate::gpu::DeviceHandle,
        dir: &Path,
        seed: impl FnOnce(&crate::gpu::DeviceHandle) -> Option<Vec<u8>>,
    ) -> Option<Self> {
        let info = device.adapter.get_info();
        let file = dir.join(format!("{}.bin", wgpu::util::pipeline_cache_key(&info)?));
        let identity = identity(&info, vello::SHADER_FINGERPRINT);
        let own =
            read_bounded(&file).and_then(|bytes| decode(&bytes, &identity).map(<[u8]>::to_vec));
        let loaded_len = own.as_ref().map(Vec::len);
        let (data, origin) = match own {
            Some(own) => (Some(own), Origin::Own),
            None => match seed(device) {
                Some(seed) => (Some(seed), Origin::Seed),
                None => (None, Origin::Empty),
            },
        };
        tilo_startup_metrics::mark(match origin {
            Origin::Own => "pipeline-cache-own",
            Origin::Seed => "pipeline-cache-seed",
            Origin::Empty => "pipeline-cache-empty",
        });
        Some(Self {
            cache: crate::pipeline_seed::create(device, data.as_deref()),
            file,
            identity,
            origin,
            loaded_len,
        })
    }

    /// The cache to hand vello.
    pub(crate) fn cache(&self) -> &wgpu::PipelineCache {
        &self.cache
    }

    #[cfg(test)]
    pub(crate) fn origin(&self) -> Origin {
        self.origin
    }

    /// Hold this cache until the renderer it feeds has built the pipelines
    /// it deferred past the first frame; [`persist_unsaved`] then keeps it.
    pub(crate) fn keep_until_built(self) {
        UNSAVED.lock_recover().push(self);
    }

    /// Keep what this launch compiled, off the first frame's path: on a
    /// background thread, and only when the cache did not come whole from
    /// this app's own file.
    pub(crate) fn persist(self) {
        let spawned = crate::background::Background::current().spawn("pipeline-cache-save", move || {
            if let Err(error) = self.save() {
                tracing::debug!(%error, file = %self.file.display(), "could not keep the app's pipeline cache");
            }
        });
        if spawned.is_err() {
            tracing::debug!("no background executor to keep the app's pipeline cache");
        }
    }

    /// Write the cache to its file. `Ok(false)` when there was nothing new to
    /// keep.
    pub(crate) fn save(&self) -> std::io::Result<bool> {
        let Some(blob) = self.cache.get_data() else {
            return Ok(false);
        };
        if self.origin == Origin::Own && self.loaded_len == Some(blob.len()) {
            return Ok(false);
        }
        let Some(bytes) = encode(&self.identity, &blob) else {
            return Ok(false);
        };
        tilo_files::native::atomic::write_private(&self.file, &bytes)?;
        Ok(true)
    }
}

/// Whether this app's first frame draws a backdrop blur, as its last launch
/// found, kept beside its pipeline cache under the same rules (its own cache
/// directory, written only by this app, mode 0600).
///
/// On a software adapter every frame is rasterized on the CPU except a
/// backdrop blur, which vello draws, so only an app whose first frame blurs
/// a backdrop waits on vello's pipelines. Such an app starts building them as
/// soon as it has a device; any other app leaves that build until after its
/// first present, so its startup does not share the CPU with LLVM compiling
/// pipelines it is not about to use. A wrong or missing hint costs time,
/// never a pipeline: a frame that needs one still builds or waits for it.
pub(crate) mod first_frame_backdrop {
    use std::path::{Path, PathBuf};

    const FILE: &str = "first-frame-backdrop";

    fn dir() -> Option<PathBuf> {
        Some(
            crate::host::environment()
                .paths
                .cache_dir()?
                .join("tilo")
                .join("pipelines"),
        )
    }

    /// What the last launch recorded; `false` when it recorded nothing.
    pub(crate) fn expected() -> bool {
        dir().is_some_and(|dir| expected_in(&dir))
    }

    pub(crate) fn expected_in(dir: &Path) -> bool {
        recorded(dir) == Some(true)
    }

    /// What the file says, `None` when there is none.
    fn recorded(dir: &Path) -> Option<bool> {
        tilo_files::file_at(&dir.join(FILE))
            .and_then(|(dir, name)| tilo_files::blocking::read_prefix(&dir, &name, 1))
            .ok()
            .map(|bytes| bytes == b"1")
    }

    /// Record what this launch's first frame did, on a background thread and
    /// only when it differs from what the last launch recorded.
    pub(crate) fn record(drew: bool) {
        let Some(dir) = dir() else {
            return;
        };
        let spawned = crate::background::Background::current().spawn(
            "first-frame-backdrop-save",
            move || {
                if let Err(error) = record_in(&dir, drew) {
                    tracing::debug!(%error, "could not keep the first-frame backdrop hint");
                }
            },
        );
        if spawned.is_err() {
            tracing::debug!("no background executor to keep the first-frame backdrop hint");
        }
    }

    /// `Ok(true)` when the file was written.
    pub(crate) fn record_in(dir: &Path, drew: bool) -> std::io::Result<bool> {
        if recorded(dir) == Some(drew) {
            return Ok(false);
        }
        tilo_files::native::atomic::write_private(&dir.join(FILE), if drew { b"1" } else { b"0" })?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_frame_backdrop_hint_is_kept_and_rewritten_only_on_change() {
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path().join("tilo").join("pipelines");
        assert!(
            !first_frame_backdrop::expected_in(&dir),
            "a first launch expects no backdrop"
        );
        assert!(first_frame_backdrop::record_in(&dir, true).unwrap());
        assert!(first_frame_backdrop::expected_in(&dir));
        assert!(
            !first_frame_backdrop::record_in(&dir, true).unwrap(),
            "an unchanged hint is not rewritten"
        );
        assert!(first_frame_backdrop::record_in(&dir, false).unwrap());
        assert!(!first_frame_backdrop::expected_in(&dir));
        assert!(!first_frame_backdrop::record_in(&dir, false).unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.join("first-frame-backdrop"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "the hint is private to the app");
        }
        std::fs::write(dir.join("first-frame-backdrop"), b"garbage").unwrap();
        assert!(
            !first_frame_backdrop::expected_in(&dir),
            "anything but the app's own record expects no backdrop"
        );
    }

    fn adapter(driver_info: &str) -> wgpu::AdapterInfo {
        wgpu::AdapterInfo {
            name: "llvmpipe (LLVM 20.1.8, 256 bits)".into(),
            vendor: 0x10005,
            device: 0,
            device_type: wgpu::DeviceType::Cpu,
            device_pci_bus_id: String::new(),
            driver: "llvmpipe".into(),
            driver_info: driver_info.into(),
            backend: wgpu::Backend::Vulkan,
            subgroup_min_size: 8,
            subgroup_max_size: 8,
            transient_saves_memory: false,
        }
    }

    #[test]
    fn a_cache_round_trips_through_its_file_format() {
        let key = identity(&adapter("Mesa 25.1.9"), 7);
        let blob = b"a driver's pipeline cache".to_vec();
        let bytes = encode(&key, &blob).expect("a small blob is kept");
        assert_eq!(decode(&bytes, &key), Some(blob.as_slice()));
    }

    #[test]
    fn a_corrupt_or_truncated_file_is_ignored() {
        let key = identity(&adapter("Mesa 25.1.9"), 7);
        let bytes = encode(&key, b"a driver's pipeline cache").unwrap();
        for cut in [0, 7, HEADER_LEN - 1, HEADER_LEN, bytes.len() - 1] {
            assert_eq!(decode(&bytes[..cut], &key), None, "cut at {cut}");
        }
        for flip in [
            0,
            MAGIC.len(),
            MAGIC.len() + 32,
            HEADER_LEN,
            bytes.len() - 1,
        ] {
            let mut corrupt = bytes.clone();
            corrupt[flip] ^= 0x40;
            assert_eq!(decode(&corrupt, &key), None, "byte {flip} flipped");
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert_eq!(
            decode(&longer, &key),
            None,
            "a trailing byte fails the checksum"
        );
        assert_eq!(encode(&key, &[]), None, "an empty blob is not worth a file");
    }

    #[test]
    fn a_cache_for_another_adapter_driver_or_shader_set_is_ignored() {
        let base = adapter("Mesa 25.1.9");
        let key = identity(&base, 7);
        let bytes = encode(&key, b"pipelines").unwrap();
        let mut other_device = base.clone();
        other_device.device = 1;
        let mut other_backend = base.clone();
        other_backend.backend = wgpu::Backend::Gl;
        for (what, other) in [
            ("driver update", identity(&adapter("Mesa 25.2.0"), 7)),
            ("shader set", identity(&base, 8)),
            ("device", identity(&other_device, 7)),
            ("backend", identity(&other_backend, 7)),
        ] {
            assert_ne!(other, key, "{what} must change the identity");
            assert_eq!(decode(&bytes, &other), None, "{what}");
        }
    }

    /// The real thing, where a device with pipeline caches is available:
    /// a first launch starts from nothing (no seed here) and keeps what
    /// vello compiled; the next starts from that file and writes nothing.
    #[test]
    fn a_second_launch_starts_from_the_first_launch_s_pipelines() {
        let mut context = crate::gpu::RenderContext::new();
        let Some(index) = pollster::block_on(context.device(None)) else {
            return;
        };
        let device = &context.devices[index];
        if !crate::pipeline_seed::supported(device) {
            return;
        }
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path().join("tilo").join("pipelines");
        let build = |cache: &AppPipelineCache| {
            vello::Renderer::new(
                &device.device,
                vello::RendererOptions {
                    use_cpu: false,
                    antialiasing_support: vello::AaSupport::area_only(),
                    num_init_threads: None,
                    pipeline_cache: Some(cache.cache().clone()),
                },
            )
            .unwrap()
        };

        let first = AppPipelineCache::open_in(device, &dir, |_| None).unwrap();
        assert_eq!(first.origin(), Origin::Empty);
        let _renderer = build(&first);
        assert!(
            first.save().unwrap(),
            "a first launch keeps what it compiled"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&first.file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "the cache is private to the app");
            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(dir_mode & 0o777, 0o700);
        }

        let second = AppPipelineCache::open_in(device, &dir, |_| {
            panic!("a valid own file is used before the seed")
        })
        .unwrap();
        assert_eq!(second.origin(), Origin::Own);
        let _renderer = build(&second);
        assert!(
            !second.save().unwrap(),
            "a launch that compiled nothing new does not rewrite its cache"
        );

        // A damaged file is never handed to the driver: the seed is used.
        let mut bytes = std::fs::read(&first.file).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&first.file, &bytes).unwrap();
        let third = AppPipelineCache::open_in(device, &dir, |_| None).unwrap();
        assert_eq!(third.origin(), Origin::Empty);
        let _renderer = build(&third);
    }
}
