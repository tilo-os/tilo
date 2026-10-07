# Native multi-device continuity: first executable slice

The first slice shares logical screen geometry between Displays and native input
routing. It does not enable real-device input or enroll devices.

`ui::widget::display_arrangement` is the existing Displays canvas promoted into
the shared toolkit. Dragging emits a supplied semantic move binding; accessibility
names include display identity and logical placement. Device-qualified target keys
can distinguish two devices that both report `eDP-1`. The existing Settings pane
uses this shared control with its existing staged/confirmed output transactions.
`settings_store::display_placement` is the common snapping, collision, and shared-edge
geometry; dimensions are logical units after DPI scale and rotation.

`network::device_topology::NativeScreenRouter` validates an observed topology and
routes boundary crossing to an already-granted exact peer/content surface. It
checks the actual shared edge intersection, finite coordinates, online state and
current grant. Keyboard/button routing follows its ephemeral focus lease. Sensitive
source focus cancels that lease. Revocation and every topology replacement cancel
focus; reconnect does not resume typing automatically. The receiver must still
call `State::apply_remote_input`, which enforces its existing share registry,
lock, physical priority, visibility, focus, reserved chords, and rate limits.
The router neither creates a grant nor substitutes for certified peer admission.

The `peer_sync_test::native_screen_crossing_reaches_the_two_peer_seat_without_video`
fixture uses two real Iroh endpoints with in-memory certificates from one synthetic
user root, exact synthetic surface grants, postcard
input serialization, the production transport and the receiving Wayland seat.
Its two screens have different logical sizes, share the same connector name, and
represent 2x DPI panels. It checks actual pointer motion, no-grant denial,
sensitive-source suppression, read-only revocation, receiver lock, offline and
reconnect focus cancellation. It emits no video stream. This is a headless source
fixture, not evidence of a real desktop-to-desktop handoff.

`tilo-ui --example native-displays` captures the same logical rectangles using
production retained layout/rendering in dark, light and high-contrast themes.
These device labels are explicitly synthetic example data, never seeded live
Settings data.

## State boundaries

Existing workspace CRDTs replicate workspace structure and shared semantic app
resource mutations. Local rendering remains on each machine. Caret, selection,
scroll, view navigation, hardware modes/DPI and seat focus remain device-local;
app resources decide which content state is shared. This slice does not claim
that every process, private app credential, or ephemeral view is replicated.
It adds no new app-state transport.

Files already uses ACL-filtered predictive candidates, bounded predictive workers,
single-flight hydration, verified byte-range fetching and the existing eviction
coordinator. Safe eviction requires durable remote evidence and retained-history
checks. No durable user resource is deleted by this slice.

## Pending usable integration

The router is not yet wired into physical input dispatch. Authenticated native
screen advertisements, peer-local surface targeting, topology persistence with
concurrent edit resolution, an exact device/input consent UX, held-key release on
handoff, source and destination sensitive-field observations, and live keyboard
focus reconciliation are still required. Displays currently shows local attached
outputs; the two-device example demonstrates the shared component, not live peer
discovery. No real input permission, capture, pairing, device restart or privileged
service has been added. Do not describe this slice as complete device continuity.

## Focused runner verification

```sh
rcargo test --locked -p settings-store display_placement
rcargo check --locked -p shell -p settings --tests
rcargo test --locked -p shell --test it peer_sync_test::native_
rcargo test --locked -p tilo-ui --lib widget::display_arrangement
rcargo --on-builder --fetch native-displays run --locked -p tilo-ui --example native-displays -- target/native-displays
```
