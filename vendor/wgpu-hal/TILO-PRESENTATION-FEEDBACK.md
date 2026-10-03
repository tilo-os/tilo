# Optional Metal presentation observation

Source: crates.io `wgpu-hal` 29.0.3, retaining upstream Apache-2.0/MIT licenses.
Only Cargo.toml and src/metal/{mod.rs,surface.rs} change from that package.

The Metal backend exposes a one-shot observer for the next presentation on a
Metal surface (`Surface::observe_next_present`). It was a Cargo feature
(`tilo-presentation-feedback`), which a crate published to crates.io cannot
name on the upstream `wgpu-hal`; it is now part of `metal`, and the SDK's
`presentation-feedback` feature is what opts in to using it. Callers must serialize registration and
presentation, and release the wgpu HAL guard before calling present. Duplicate
registration fails. Callback registration checks runtime API availability.

The existing drawable receives an addPresentedHandler callback immediately
before the original presentation path. No extra drawable, command buffer,
fence, presentation policy, or GPU work is introduced. Without an armed
observer the presentation path only takes one uncontended lock. The callback owns only
reporting state; it must not block or panic, and must not retain its drawable.

Read presentedTime from the callback argument. Positive timestamps indicate
OS-reported presentation, zero is not presentation evidence, and missing
callbacks must remain unknown. Callback delivery for every dropped frame is
not assumed. Raw host seconds are not directly comparable to arbitrary Rust
Instant epochs. This patch is diagnostic scaffolding, not a cross-platform
presentation API; keep it narrow for eventual upstream replacement.

References:
- https://developer.apple.com/documentation/metal/mtldrawable/presentedtime
- https://developer.apple.com/documentation/metal/mtldrawable/addpresentedhandler(_:)
