# Native multi-device continuity: first executable slice

The first slice shares logical screen geometry between Displays and the existing
personal-device fabric. It does not enable real-device input or enroll devices.

`ui::widget::display_arrangement` is the existing Displays canvas promoted into
the shared toolkit. Dragging emits a supplied semantic move binding; accessibility
names include display identity and logical placement. Device-qualified target keys
can distinguish two devices with the same connector name. Settings uses this
shared control with its existing staged/confirmed output transactions.
`settings_store::display_placement` supplies common snapping, collision and shared
edge geometry. The existing fabric's `DisplayRect::logical_placement` validates
logical extents and reuses that geometry for its boundary crossing. An offline
source and malformed/overflowing bounds cannot produce a crossing.

The existing `DeviceFabric` owns self-authored screen placements and revision
ordering; `device_fabric_runtime` owns target-issued input leases, revocation,
expiry and disconnect cleanup. `permits_spatial_input` factors the existing
retained-lease gate out of its inbound action handler for direct verification.
It checks the exact controller, target, nonce, expiry and input capabilities.
The native keycode injector now converts Linux evdev to XKB on both press and
release; keysym lookup returns evdev so the same conversion is applied exactly
once. No second router, permission registry, transport or privileged service is
added.

The two-peer fixture uses the existing personal-fabric ALPN over real Iroh
endpoints, in-memory certificates from one synthetic user root, production
checkpoint encoding/decoding, input-envelope validation, the runtime's lease gate,
and `State::apply_personal_fabric_input`. Both peers exchange their own snapshots
and converge on identical logical topology. The fixture verifies native pointer
motion and keyboard pressed state, controller-specific held-key release, lock,
kill switch, missing/wrong/revoked/expired leases, offline/reconnect, rearrangement
and conflicting revisions. It needs no surface share or video stream. This is a
headless source fixture, not evidence of a real desktop-to-desktop handoff.

`tilo-ui --example native-displays` captures the same logical rectangles using
production retained layout/rendering in dark, light and high-contrast themes.
These labels are explicitly synthetic example data, never seeded live Settings
content. Captures show the shared control rather than live peer discovery.

## State boundaries

This slice verifies replication of the personal fabric's logical device topology.
Existing workspace CRDTs replicate workspace structure and shared semantic app
resource mutations; each machine renders locally. Current caret, selection, scroll
and view navigation are participant view state;
that is a current implementation boundary, not the intended same-session UX.
The integration target is for the owner’s devices to follow the active session’s
view and hand it off, while collaborators keep separate views. Hardware modes/DPI,
physical input ownership and private credentials remain device-local.
This slice does not prove complete native app-state replication or add a new
app-state transport.

Files already uses ACL-filtered predictive candidates, bounded predictive workers,
single-flight hydration, verified byte-range fetching and the existing eviction
coordinator. Safe eviction requires durable remote evidence and retained-history
checks. No durable user resource is deleted by this slice.

## Pending usable integration

Live Displays still enumerates local attached outputs. Showing and dragging
certified peers there needs a host view of existing fabric placements, bindings
to its existing placement actions, and connector-level treatment of multiple
outputs per device. The existing native input lifecycle must be exercised with
actual hosted app focus and sensitive-field observations, differing-DPI physical
outputs, rearrangement during held input, disconnect/reconnect and concurrent
app resource mutations. Those are pending integration rather than behaviors
claimed by the synthetic captures. No real input permission, capture, pairing,
device restart or security-setting change is performed by this slice.

## Focused runner verification

```sh
cargo test --locked -p settings-store display_placement
cargo check --locked -p shell -p settings --tests
cargo test --locked -p shell --test it peer_sync_test::native_
cargo test --locked -p tilo-ui --lib widget::display_arrangement
cargo run --locked -p tilo-ui --example native-displays -- target/native-displays
```
