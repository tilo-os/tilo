# Tilo DX12 compiled-shader cache prototype

Source: crates.io `wgpu-hal` 29.0.3, upstream commit
`4cbe6232b2d7c289b6e1a38416a6ae1461a22e81` (wgpu-hal directory).
Original MIT/Apache licenses and normalized manifest are retained. This is a
pinned dependency patch, not an edition or dependency upgrade.

The public wgpu PipelineCache currently works only with Vulkan. This patch
does not advertise DX12 PIPELINE_CACHE support. It exposes a narrow DX12-only
Device import/export API for the existing process-local DXC shader cache.

Behavioral changes are limited to `src/dx12/{mod,device,shader_cache,shader_cache_key,
shader_compilation}.rs` and the Windows LibraryLoader feature. The imported source
also follows the workspace rustfmt style and records reasons for retained upstream
lint exceptions, as required by the repository lint gate:

- Runtime WGSL validation, specialization and layout-specific HLSL generation
  remain unchanged. Only the following compiler invocation can be skipped.
- Cache keys contain length-delimited exact HLSL, entry point, shader stage,
  shader model, source label, and dynamic DXC debug/optimization/F16 options.
- Import is unsafe: the caller must verify compiler identity and provenance.
  The toolkit fingerprints the DLL before loading it, holding a replacement-
  preventing handle for the device lifetime, and owns bounded private storage.
- Import rejects duplicate/empty/oversized records before mutating cache state.
- Export returns used compute shaders only; render shaders follow the original
  path. A rejected cached compute PSO evicts imported bytecode and retries the
  same validated shader through DXC, bounded to one retry per pipeline.
- Compiler identity uses the kernel module path of the actual DxcCreateInstance
  symbol. The toolkit loads the final path from its held fingerprinted file
  handle, then compares the loaded module's volume/file ID with that held file.
- Stats report hits, misses, rejections, summed DXC compile elapsed durations and
  summed compute PSO creation elapsed durations. These include descheduling and
  are neither measured CPU time nor startup wall-clock critical path.

On an upstream upgrade, review `compile_dxc` arguments and runtime HLSL options,
update the toolkit cache ABI, and rerun cold/hit/corrupt-cache native rendering
tests. No API here should be generalized to arbitrary untrusted shader blobs.

Native ARM64 release Calculator validation (2026-09-18) passed: checksum
corruption fell back to compilation; checksum-valid invalid DXIL reached the
real driver rejection path, recompiled, presented and subsequently repaired the
cache. Native accessibility/keyboard `40+2` returned `42`; dark/light offscreen
1280x800 output matched the pre-patch binary pixel-for-pixel.

On the measured unplugged, Energy-Saver host, alternating runs of the same
installed binary had median first-present 1,589ms with caching disabled (4 runs)
and 1,111ms with caching enabled (5 runs, range 569--1,467ms). Median Vello
initialization fell from 1,216ms to 34ms; normal hits reused 24 compute shaders.
This is a material shader-stage reduction, not near-immediate app startup.
Fonts and surface setup still dominated some runs. An empty shader cache took
1,772ms; this was not a controlled cold OS boot. Power and runtime scheduling
were not controlled, and the measurements must not be generalized to all hosts.

The patch does not accelerate first cache misses, Naga translation, fonts,
adapter selection or driver-native pipeline creation. Linux rendering is unchanged.

## Simulated device loss (Vulkan)

`vulkan::Device::simulate_device_loss` makes every later fence wait on that
device return `DeviceError::Lost`, which is what the driver's
`VK_ERROR_DEVICE_LOST` becomes when the GPU is reset under in-flight work. wgpu-core then does what it
does for a real reset: marks the device invalid and runs its device-lost
callback. The compositor's tests reach it through `wgpu::Device::as_hal` to
prove it survives a reset. It is a per-device flag that only that method sets,
so it is inert in every other build; the change is `DeviceShared::simulated_loss`
and an early return in `Device::wait` (`src/vulkan/device.rs`).

## Vulkan presentation damage

`vulkan::Surface::set_next_present_damage` supplies bounded framebuffer
rectangles for one native swapchain presentation. The adapter enables
`VK_KHR_incremental_present` only when the physical device supports it. The
setter accepts only configured, single-layer surfaces with identity transform
and 1–128 positive rectangles inside the framebuffer extent. Unsupported or
invalid hints clear previous hints and preserve ordinary full presentation.

The native present path consumes the hint once, including on present failure,
and chains `VkPresentRegionsKHR` alongside existing display timing. Queue,
semaphore and swapchain image ownership remain unchanged. The caller must
cover every pixel changed since the previous successful presentation; it must
still initialize every acquired image. The toolkit retains its full blit into
each acquired swapchain image and uses full presentation after redraw, resize,
skipped frames or effects without bounded coverage.

This follows the optional hint contract in the
[Vulkan specification](https://docs.vulkan.org/refpages/latest/refpages/source/VkPresentRegionKHR.html).
The change is in `src/vulkan/{adapter,mod,swapchain/native}.rs`. Pixel coverage
and fallback tests exercise the toolkit's hint conversion; native WSI transport
and Yoga typing latency still require verification.

## IEEE-safe Metal shaders

`metal::Device::load_shader` compiles naga's MSL with `MTLMathMode::Safe`
(`fastMathEnabled = NO` before macOS 15/iOS 18) instead of Metal's default
fast math. Fast math lets the compiler reassociate floating-point
expressions, which undoes Vello's translation-stable area coverage
(`vendor/PATCHES.md`): on an Apple M1 a damage-region redraw differed from a
full render of the same scene by one channel step at a few edge pixels, so
partially redrawn frames drifted from fresh ones. With safe math the two agree
pixel for pixel. Passthrough MSL keeps the options its author compiles with.
The change is in `src/metal/device.rs`.

## Vulkan loader device lookups ordered against its list changes

The Vulkan loader (1.4.341 as Fedora 44 ships it) resolves a `VkDevice` to
its driver for `vkSetDebugUtilsObjectNameEXT`, `vkCreateSwapchainKHR` and
`vkGetDeviceProcAddr` by walking every instance's drivers and their devices
under one mutex, while `vkDestroyDevice` frees device nodes under another
and `vkCreateInstance` and `vkEnumeratePhysicalDevices` free the driver
nodes of an already listed instance whose driver fails or has no physical
device. A process that names objects on one device (every resource, once
`InstanceFlags::DEBUG` is on, as in debug builds) while another thread does
any of these reads a freed node and crashes. The shell's parallel GPU tests
did, in `loader_get_icd_and_device`. `vulkan::LOADER_DEVICE_LISTS`, a
process-wide re-entrant mutex, is held around those lookups, instance and
device creation (through loading the device's functions) and destruction,
and physical device enumeration. Drop this once the loaders Tilo runs on
order their own lists. The change is in
`src/vulkan/{mod,adapter,device,drm,instance,swapchain/native}.rs`.
