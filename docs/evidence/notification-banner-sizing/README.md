# Measured notification banners

At 200% text, a Calendar banner painted its body over Dismiss and Snooze. The
production stack reserved 88 px above the controls for 98.6 px of text, before
padding. The focused regression fails on base 9c6fa609 and passes after the fix.

The UI library now exposes `toast::minimum_card_height`, measured from the same
normalized text, Caption/Label roles, padding and shared controls as its card
painter. The desktop stack uses it before placing cards and deciding viewport
capacity. This also sizes ordinary attributed error banners without action rows.
Single-line elision, materials, reduced motion, animation deadlines and host-owned
action effects retain their existing behavior.

All eighteen original PNGs were inspected: six before/after pairs cover dark,
light, high contrast, 200% text, combined 150% UI/130% text, and a 320 px output.
Six further captures show the same variants with a fixture-only application error.
[The artifact index](artifacts.json) records original bytes, dimensions, SHA-256
and production source blobs. PNGs stay outside Git; public PR attachments carry
the shareable originals.

Capture on the SSH runner:

```sh
WGPU_BACKEND=vulkan VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json   cargo run --locked -p tilo-desktop-surface --example notification_banner_shots -- target/banner-shots
WGPU_BACKEND=vulkan VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json   cargo run --locked -p tilo-desktop-surface --example notification_banner_shots -- target/error-shots error
```

Qualification includes eight banner tests (the new measured production regression,
painted choice routing/stale geometry, capacity, compact edges, animation and
accessibility), three shared toast cases, native notification journeys, shell
integration-target compilation and all-target Clippy. Actual CI and protected
landing remain separate gates.

Remaining banner work includes hover/keyboard affordances and full live-device
notification journeys. The runner's existing VM/KVM blocker remains; these are
production offscreen captures, not a VM or physical-device pass. No real calendar,
account, microphone, credential, Files transfer or device operation ran.
