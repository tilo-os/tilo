# Tilo: a lost device is reported, not fatal

Source: crates.io `wgpu` 29.0.3, upstream commit
`4cbe6232b2d7c289b6e1a38416a6ae1461a22e81` (wgpu directory), carried as it
ships apart from this change.

`Device::poll` and `Instance::poll_all_devices` panicked on any error that is
not a timeout or a wrong submission index, and a lost device is one: after a GPU
reset (`radv/amdgpu: The CS has been cancelled because the context is lost`)
the next poll panicked with `Parent device is lost`, and the compositor, built
with `panic = "abort"`, took the whole session down to a black screen.

A lost device is an environment event the caller can recover from by opening a
new one, and wgpu-core has already reported it through the device-lost callback
before the poll returns. So in `src/backend/wgpu_core.rs`:

- `Device::poll` returns `PollError::Timeout` for a device error: the work it
  waited for will never finish. The callback carries the reason.
- `Instance::poll_all_devices` returns `false` (queues not empty).

Every other fatal site is unchanged. Remove this patch once upstream reports a
lost device from `poll` (`PollError` has no variant for it in 29.0).

Workspace CI compiles `vendor/wgpu` with `-D warnings` for
`wasm32-unknown-unknown`. Upstream 29.0.3 leaves the HAL documentation
macros, two `Deref` imports and `BufferMappedRangeInterface::len` unused on
that target, so `lib.rs` allows those lints there. Remove the allow when
upstream stops emitting them.
