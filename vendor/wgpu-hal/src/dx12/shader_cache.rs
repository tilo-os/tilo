//! Tilo's narrow DXC cache extension. Does not advertise PIPELINE_CACHE.
//! File storage and compiler-binary identity belong to the caller.

use super::{shader_compilation::CompilerContainer, Device, ShaderCacheKey};
use alloc::vec::Vec;

/// Opaque key and compiler output. Only persist entries returned by this device.
pub struct ShaderCacheEntry {
    pub key: Vec<u8>,
    pub shader: Vec<u8>,
}

/// Elapsed durations summed across initialization threads, including any
/// descheduling. These are neither CPU time nor wall-clock critical path.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShaderCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub rejections: u64,
    pub compile_ns: u128,
    /// Compute pipeline creation only; excludes shader compilation.
    pub pso_ns: u128,
}

impl ShaderCacheKey {
    pub(super) fn persistent_key(&self) -> Vec<u8> {
        super::shader_cache_key::encode(
            self.compiler_flags,
            [
                self.source.as_bytes(),
                self.entry_point.as_bytes(),
                self.stage.to_hlsl_str().as_bytes(),
                self.shader_model.to_str().as_bytes(),
                self.source_name.as_slice(),
            ],
        )
    }
}

impl Device {
    pub(super) fn shader_cache_flags(&self) -> u8 {
        // Must track every dynamic argument in compile_dxc. HLSL version,
        // strictness and other fixed options are covered by the cache ABI.
        u8::from(
            self.shared
                .private_caps
                .instance_flags
                .contains(wgt::InstanceFlags::DEBUG)
                && !self.shared.private_caps.workarounds.avoid_shader_debug_info,
        ) | (u8::from(self.features.contains(wgt::Features::SHADER_F16)) << 1)
    }

    /// Absolute compiler path, only when this device actually uses dynamic DXC.
    /// The caller must fingerprint the binary, not just its filename/version.
    pub fn shader_cache_compiler_path(&self) -> Option<std::path::PathBuf> {
        if !matches!(&*self.compiler_container, CompilerContainer::DynamicDxc(_)) {
            return None;
        }
        self.compiler_container.loaded_library_path()
    }

    pub(super) fn reject_imported_shader_cache(&self) {
        let mut cache = self.shader_cache.lock();
        cache.imported.clear();
        cache
            .entries
            .retain(|_, value| !matches!(&value.shader, super::CompiledShader::Cached(_)));
        cache.stats.rejections += 1;
    }

    /// Seed the process-local cache. Malformed/oversized input is rejected whole.
    ///
    /// # Safety
    /// Entries must be unmodified output of `export_shader_cache` from this HAL
    /// revision and the exact same compiler binary. They must be protected from
    /// untrusted modification; the backend cannot validate compiled DXIL.
    pub unsafe fn import_shader_cache(&self, entries: Vec<ShaderCacheEntry>) -> bool {
        if self.shader_cache_compiler_path().is_none() || entries.len() > 256 {
            return false;
        }
        let mut imported = super::HashMap::default();
        let mut size = 0usize;
        for entry in entries {
            size = size
                .saturating_add(entry.key.len())
                .saturating_add(entry.shader.len());
            if entry.key.is_empty()
                || entry.key.len() > 1024 * 1024
                || entry.shader.is_empty()
                || entry.shader.len() > 1024 * 1024
                || size > 32 * 1024 * 1024
                || imported.insert(entry.key, entry.shader).is_some()
            {
                return false;
            }
        }
        self.shader_cache.lock().imported = imported;
        true
    }

    /// Snapshot only shaders actually used by this process. No disk I/O.
    pub fn export_shader_cache(&self) -> Vec<ShaderCacheEntry> {
        if self.shader_cache_compiler_path().is_none() {
            return Vec::new();
        }
        let cache = self.shader_cache.lock();
        let mut output = Vec::new();
        let mut total = 0usize;
        for (key, value) in &cache.entries {
            if key.stage != naga::ShaderStage::Compute {
                continue;
            }
            // Bound before allocating/copying either the source or bytecode.
            let input_len = key
                .source
                .len()
                .saturating_add(key.entry_point.len())
                .saturating_add(key.source_name.len());
            if input_len > 1024 * 1024 || output.len() >= 256 {
                return Vec::new();
            }
            let bytecode = value.shader.create_native_shader();
            if bytecode.BytecodeLength == 0 || bytecode.BytecodeLength > 1024 * 1024 {
                return Vec::new();
            }
            let key = key.persistent_key();
            total = total
                .saturating_add(key.len())
                .saturating_add(bytecode.BytecodeLength);
            if key.len() > 1024 * 1024 || total > 32 * 1024 * 1024 {
                return Vec::new();
            }
            // SAFETY: the locked entry owns the live DXC/Vec allocation for
            // the whole copy. BytecodeLength comes from that same allocation.
            let bytes = unsafe {
                core::slice::from_raw_parts(
                    bytecode.pShaderBytecode.cast::<u8>(),
                    bytecode.BytecodeLength,
                )
            };
            output.push(ShaderCacheEntry {
                key,
                shader: bytes.to_vec(),
            });
        }
        output
    }

    pub fn shader_cache_stats(&self) -> ShaderCacheStats {
        self.shader_cache.lock().stats
    }
}
