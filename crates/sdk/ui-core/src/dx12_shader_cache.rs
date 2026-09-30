//! Private, bounded persistence for the pinned HAL's DXC shader cache.
//! Only compiled shaders are reused: normal WGSL/Naga validation and layout
//! translation still happen, and native PSOs are still created by the driver.
#![allow(
    clippy::disallowed_methods,
    reason = "native host runner: shader-cache persistence runs on its own writer thread"
)]

// Bump whenever HAL cache semantics, compiler arguments or the codec change.
#[cfg(all(windows, feature = "dx12-shader-cache"))]
const ABI: &[u8] = b"tilo-dxc-cache-v1/wgpu-hal-29.0.3/naga-29.0.3";

mod codec;
#[cfg(all(windows, feature = "dx12-shader-cache"))]
use codec::{Entry, LIMIT, decode, encode};

#[cfg(all(windows, feature = "dx12-shader-cache"))]
pub(crate) use platform::{CacheSession, CompilerIdentity, report};

#[cfg(all(windows, feature = "dx12-shader-cache"))]
mod platform {
    use super::*;
    use std::os::windows::{ffi::OsStringExt, fs::OpenOptionsExt, io::AsRawHandle};
    use std::{
        fs::File,
        io::Read,
        path::{Path, PathBuf},
        sync::Arc,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle, GetFinalPathNameByHandleW,
    };

    fn file_id(file: &File) -> Option<(u32, u32, u32)> {
        // SAFETY: zero is valid for this plain output structure; the live
        // file handle and writable structure remain valid for the call.
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) } == 0 {
            return None;
        }
        Some((
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        ))
    }

    fn final_path(file: &File) -> Option<PathBuf> {
        let mut buffer = vec![0u16; 32768];
        // SAFETY: live file handle and buffer of the stated length. Flags 0
        // request the normalized DOS-volume path of the opened file object.
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                u32::try_from(buffer.len()).ok()?,
                0,
            )
        } as usize;
        if length == 0 || length >= buffer.len() {
            return None;
        }
        Some(PathBuf::from(std::ffi::OsString::from_wide(
            buffer.get(..length)?,
        )))
    }

    pub(crate) struct CompilerIdentity {
        // Open before LoadLibrary and held as long as the device: the module
        // cannot be replaced between hashing and compiling/importing shaders.
        _file: File,
        path: PathBuf,
        file_id: (u32, u32, u32),
        identity: [u8; 32],
    }

    impl CompilerIdentity {
        pub(crate) fn open(compiler: &wgpu::Dx12Compiler) -> Option<Arc<Self>> {
            if std::env::var("TILO_DX12_SHADER_CACHE").is_ok_and(|value| value == "off") {
                return None;
            }
            let wgpu::Dx12Compiler::DynamicDxc { dxc_path } = compiler else {
                return None;
            };
            let path = PathBuf::from(dxc_path);
            if !path.is_absolute() {
                return None;
            }
            tilo_startup_metrics::mark("dx12-compiler-fingerprint-start");
            let result = (|| {
                let mut file = File::options().read(true).share_mode(1).open(&path).ok()?;
                let metadata = file.metadata().ok()?;
                if !metadata.is_file() || metadata.len() > 128 * 1024 * 1024 {
                    return None;
                }
                let path = final_path(&file)?;
                let file_id = file_id(&file)?;
                let mut hash = blake3::Hasher::new();
                hash.update(ABI);
                hash.update(std::env::consts::ARCH.as_bytes());
                let mut buffer = [0u8; 64 * 1024];
                loop {
                    let len = file.read(&mut buffer).ok()?;
                    if len == 0 {
                        break;
                    }
                    hash.update(buffer.get(..len)?);
                }
                Some(Arc::new(Self {
                    _file: file,
                    path,
                    file_id,
                    identity: *hash.finalize().as_bytes(),
                }))
            })();
            tilo_startup_metrics::mark("dx12-compiler-fingerprint-done");
            result
        }

        pub(crate) fn compiler(&self) -> wgpu::Dx12Compiler {
            // Load the resolved held file, independent of parent junctions in
            // the original app-local path. Its handle forbids replacement.
            wgpu::Dx12Compiler::DynamicDxc {
                dxc_path: self.path.to_string_lossy().into_owned(),
            }
        }
    }

    pub(crate) struct CacheSession {
        compiler: Arc<CompilerIdentity>,
        path: PathBuf,
    }

    impl CacheSession {
        pub(crate) fn load(
            device: &wgpu::Device,
            compiler: Option<Arc<CompilerIdentity>>,
        ) -> Option<Self> {
            let compiler = compiler?;
            // SAFETY: borrowing the HAL device only; never destroy or submit
            // work through it. Import's separate bytecode trust contract below.
            let hal = unsafe { device.as_hal::<wgpu::hal::api::Dx12>() }?;
            let loaded_file = File::options()
                .read(true)
                .share_mode(1)
                .open(hal.shader_cache_compiler_path()?)
                .ok()?;
            if file_id(&loaded_file)? != compiler.file_id {
                return None;
            }
            let base = tilo_paths::env_dir("XDG_CACHE_HOME").or_else(tilo_paths::cache_dir)?;
            let directory = base.join("tilo").join("dx12-shaders");
            private_fs::directory(&directory).ok()?;
            // One bounded file per app, so updates replace stale revisions and
            // unrelated applications cannot grow this file indefinitely.
            let exe = std::env::current_exe().ok()?;
            let name = exe.file_stem()?.to_str()?;
            let path = directory.join(format!("{name}.bin"));
            tilo_startup_metrics::mark("dx12-shader-cache-load-start");
            let entries = read_entries(&path, &compiler.identity);
            if let Some(entries) = entries {
                // SAFETY: opened handle is private/user-owned, checksum and
                // exact compiler+HAL ABI match. Entries are only our exported
                // bytecode; never accept imported document/network content.
                let loaded = unsafe {
                    hal.import_shader_cache(
                        entries
                            .into_iter()
                            .map(|entry| wgpu::hal::dx12::ShaderCacheEntry {
                                key: entry.key,
                                shader: entry.shader,
                            })
                            .collect(),
                    )
                };
                if loaded {
                    tilo_startup_metrics::mark("dx12-shader-cache-loaded");
                }
            }
            tilo_startup_metrics::mark("dx12-shader-cache-load-done");
            Some(Self { compiler, path })
        }

        pub(crate) fn finish(&self, device: &wgpu::Device) {
            let device = device.clone();
            let path = self.path.clone();
            let compiler = self.compiler.clone();
            // Snapshot/encoding and persistence never delay first present.
            // An interrupted atomic write leaves the old complete file.
            let _ = std::thread::Builder::new()
                .name("dx12-shader-cache-save".into())
                .spawn(move || {
                    // SAFETY: read-only snapshot; never destroy the borrowed HAL device.
                    let Some(hal) = (unsafe { device.as_hal::<wgpu::hal::api::Dx12>() }) else {
                        return;
                    };
                    if hal.shader_cache_stats().misses == 0 {
                        return;
                    }
                    let entries: Vec<_> = hal
                        .export_shader_cache()
                        .into_iter()
                        .map(|entry| Entry {
                            key: entry.key,
                            shader: entry.shader,
                        })
                        .collect();
                    drop(hal);
                    let Some(bytes) = encode(&compiler.identity, &entries) else {
                        return;
                    };
                    if let Err(error) = private_fs::write_atomic(&path, &bytes) {
                        tracing::debug!(%error, "DX12 shader cache persistence unavailable");
                    }
                });
        }
    }

    pub(crate) fn report(device: &wgpu::Device) {
        if std::env::var_os("TILO_STARTUP_METRICS_PATH").is_none() {
            return;
        }
        // SAFETY: only reads CPU-side statistics; never destroys the device.
        let Some(hal) = (unsafe { device.as_hal::<wgpu::hal::api::Dx12>() }) else {
            return;
        };
        let stats = hal.shader_cache_stats();
        eprintln!(
            "tilo DX12 shader cache: hits={} misses={} rejected={} compile_elapsed_us={} compute_pso_elapsed_us={}",
            stats.hits,
            stats.misses,
            stats.rejections,
            stats.compile_ns / 1000,
            stats.pso_ns / 1000
        );
    }

    fn read_entries(path: &Path, identity: &[u8; 32]) -> Option<Vec<Entry>> {
        let file = private_fs::read(path).ok()?;
        if file.metadata().ok()?.len() > LIMIT as u64 {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(LIMIT as u64 + 1).read_to_end(&mut bytes).ok()?;
        decode(&bytes, identity)
    }
}
