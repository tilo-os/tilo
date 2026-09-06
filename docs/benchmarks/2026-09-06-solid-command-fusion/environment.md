# Environment checked before build

Host architecture: aarch64. Online processors:8.
No inherited RUSTFLAGS, CARGO_ENCODED_RUSTFLAGS, CARGO_PROFILE_RELEASE_*,
CARGO_BUILD_TARGET/JOBS, RUSTC_WORKSPACE_WRAPPER/RUSTC_WRAPPER, MESA_*,
GALLIUM_*, LP_*, VK_ICD_FILENAMES or WGPU_* variables matched the explicit
environment check. No profile, driver or CPU-target override was introduced.

Workspace release: opt-level3, fatLTO, codegen-units1, panicabort, strip symbols,
overflow-checksfalse, line-tables-only debug, incrementalfalse. Cargo config
uses tools/rustc-wrapper with ordinary configured build jobs4. Frozen hashes
will include both Cargo manifests, configuration and compiler wrapper.

The prior adapter is Vulkan llvmpipe LLVM22.1.8 (CPU); each new benchmark must
print its actual adapter and keep that evidence. This is not a physicalGPU or
2010 laptop. The workspace x86 floor is x86-64-v3 (2013+); this ARM-host result
cannot certify that the OS runs on or meets latency targets on a2010 x86 CPU.
No physicalKMS/presentation/GPU-bus or constant60FPS claim follows from this
benchmark. The active acceptance goal is the specified native4K/1000-action
ordinary-release latency gates.

Post-batch finding: the v3 UI executable did not actually print adapter metadata.
All shell logs identify the CPU llvmpipe adapter, but the per-process UI adapter
identity is missing and must not be filled in from shell logs. The separate v4
untimed reporting correction passed its debug smoke and printed llvmpipe; this
is evidence only for that new process, not the previous UI measurements.
