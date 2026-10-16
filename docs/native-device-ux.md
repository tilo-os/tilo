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

The `native-displays` toolkit example captures the same logical rectangles using
production retained layout/rendering in dark, light and high-contrast themes.
These labels are explicitly synthetic example data, never seeded live Settings
content. Captures show the shared control rather than live peer discovery.

## State boundaries

This slice verifies replication of the personal fabric's logical device topology.
Existing workspace CRDTs replicate workspace structure and shared semantic app
resource mutations; each machine renders locally. Current caret, selection, scroll
and view navigation are participant view state. That is a current implementation
boundary, not the intended same-session UX.
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

Code Editor's typed continuation projects its active durable buffer identity,
CRDT-anchored carets and selections, top visible character, fractional line offset
and horizontal columns. It includes no file path or text. Resolution happens
against the existing hydrated record after merge; unavailable or foreign-object
anchors are refused. Pixel dimensions are measured on the receiving device.
Restoring this presentation neither edits text nor grants keyboard focus.
The text facet is available only with an active bound buffer. An empty editor
retains the framework's presentation-only continuation.

The headless two-app fixture uses the existing checked storage installer, real
editor dispatchers and the production record store to prove concurrent offline
edits, anchored selection and shifted viewport with different measured cell
widths. A separate production-launch fixture starts two fresh profiles without
prehydration. Modern replicas importing identical file bytes construct the same
Text object, import those bytes once and retain both private writers' offline
edits after merge. Existing legacy records still require checked hydration;
mixed legacy/new cold bootstrap and divergent initial versions remain pending.
The cursor API rejects an operation from another text sequence rather than
asserting. Continuous owner-session following and physical handoff are pending.


Live Displays now observes certified peers' connector topology through the shared
host projection described below. Peer dragging now invokes exported screen
resources through the authenticated Settings host route. Screens belonging to a
single device translate together; rearranging its individual connectors remains
its local Displays transaction. The existing native input lifecycle must be
exercised with actual hosted app focus, sensitive fields, differing-DPI physical
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

## Live connector discovery follow-up

The compositor now publishes its actual logical connector rectangles and each
connector's scale over the existing certified personal fabric. The optional
connector message leaves schema-6 checkpoints and existing payload tags intact;
older peers discard that message and continue using aggregate screen bounds.
There is no additional discovery daemon or transport. Connector observations are
bounded to 32 outputs, device qualified and accepted only for the current
owner-authored display revision. They do not create an input lease.

Displays reads these observations under the compositor's existing private runtime
lease and shows real device names, connectors, logical positions and offline
status. If the compositor exits, the reader rejects the stale snapshot. The shell
view consumes the same projection. Crossing now uses actual connector edges and
maps into the destination seat's logical coordinates, including stacked outputs;
empty space inside an aggregate bounding rectangle cannot act as a screen edge.

The card exports a revision-bound move operation on each online peer screen.
Dragging and automation use the same operation. The existing owner fabric action
receives a compare-and-set request, validates current dimensions and occupied
screen interiors again, and refuses a second request before the first is
published. The native app connection admits Settings only while the local
workspace owner is unlocked. Neither this route nor the observation issues an
input lease. Offline and legacy observations stay read only. A transport receipt
means requested, not applied: the owner-authored projection confirms the result.
Settings reports disconnect, a competing layout revision or a ten-second missing
confirmation rather than leaving a gesture indefinitely pending. The checked
wire action also expires after ten seconds and is rechecked on the target pump,
so a delayed gesture cannot apply after an extended disconnection. An old durable
checkpoint alone cannot admit a move before live connectors are rediscovered.

A device's screens translate together, preserving native connector geometry and
DPI; each machine keeps rendering locally. The owner restores the origin from
its existing fabric checkpoint, using live output dimensions after reconnect or
mode changes. The fingerprint used to detect changes now normalizes a copy,
so publication retains real owner revision stamps. Unplugged outputs are never
resurrected by stored placement. Host disconnect refuses pending gestures, and
correlated receipts cannot revive old requests.

Physical pointer/keyboard handoff, hosted native application focus, sensitive
fields and continuous owner-session following still require qualification.
Mixed legacy/new bootstrap and divergent initial file versions remain pending. Connector observations are rediscovered
after compositor restart; durable user resources are unaffected. A connector message that arrives
before its matching checkpoint is discarded and retried at the presence heartbeat.

`settings --shots PATH --scenario device-displays --theme dark|light|high-contrast`
uses explicit two-device capture fixtures in the actual Displays pane. They are
never seeded into the live application. The transport fixture separately exchanges
real authenticated connector messages and verifies both peers' geometry.

## Revision-bound peer dragging verification

Run these focused checks with plain cargo or any cargo drop-in:

```sh
cargo test --locked -p shell --lib display_drag_
cargo test --locked -p shell --lib display_move_requires_settings_owner_and_an_unlocked_session
cargo test --locked -p settings --lib peer_drag_
cargo test --locked -p tilo-ui --lib widget::display_arrangement::tests
cargo test --locked -p tilo-ui --lib display_move_reply_matches_exact_request_and_disconnect_never_replays_it
cargo check --locked -p shell -p settings --tests
```

These synthetic peers and mounted apps exercise source/UI contracts without
making persistent enrollment or input grants to real devices. They qualify
whole-device translation; they do not claim individual remote connector mode
changes or live physical keyboard/pointer focus validation.
