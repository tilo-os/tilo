# Displays and Tilo's infinite workspace plane

Displays are viewports onto one continuous, infinitely scrolling workspace
plane. They are not assigned independent numbered workspace stacks and
connecting a monitor never duplicates or moves workspace content.

## Physical topology

`display_outputs` in the per-user settings document is typed by
`settings_store::hardware::DisplayConfig`. A connector keeps its stable DRM
name (`eDP-1`, `HDMI-A-1`) across unplug/replug. Its `x`/`y` is the viewport's
top-left in the physical monitor island; negative authoring coordinates are
translated as a group for the renderer without changing adjacency.

The primary output determines where login, new local-only windows and system
prompts first appear. It does **not** own a workspace. A window has one
position in the infinite plane and may cross output edges. New windows prefer
the viewport containing the pointer, falling back to primary.

Mirroring makes two connectors observe the same viewport origin. It is output
duplication, not a second copy of the workspace. Settings must choose a mode
advertised by both sinks; removing the mirror restores the saved independent
origin rather than inventing a conventional workspace.

## Collaboration

Workspace coordinates are shared document state; monitor topology is private
device state and is never synced to peers. A collaborator can therefore view
the same workspace through one ultrawide, two displays or a tablet without
changing anyone else's layout. Presence and focus refer to workspace
coordinates and stable surface IDs, never `monitor 1`.

## Applying changes safely

Settings never probes DRM. The compositor writes an atomic capability snapshot
to `$XDG_RUNTIME_DIR/tilo/display-state.json`; the snapshot is tied to the
publisher's PID **and process start time**, so a file from a crashed session or
reused PID is rejected. It contains exact KMS mode timings, usable output
features, and the bounded ICC profile set. If no live compositor snapshot is
available, Settings shows capability data as unavailable rather than deriving a
plausible-looking mode from sysfs.

Editing a control only stages a typed candidate. **Apply** creates a transaction
containing both the candidate and last-known-good configuration. The compositor
validates that candidate against a fresh descriptor set, applies it between
frames, and asks for confirmation. Settings shows **Keep** and **Revert now**
only during that state and includes the remaining time. If Settings crashes,
closes, or cannot be seen, the compositor's independent watchdog restores the
previous configuration after 15 seconds. Only Keep commits the candidate.

Validation forbids zero enabled outputs, multiple primaries, stale modes,
invalid scales, unsupported features/profiles, unknown mirror targets, mirror
cycles, and mirror pairs with no common pixel extent. A mirrored pair is
assigned the best resolution both sinks advertise; each connector keeps its
best timing for that resolution, so nominal 60 Hz panels can mirror when one
reports 60.000 Hz and the other 59.940 Hz. Sharing an origin with unrelated
extents is not called mirroring.

USB-C DisplayPort Alt Mode needs no private display protocol in Tilo: once the
kernel exposes the negotiated link as a connected KMS `DP-N` connector, it
uses the same discovery, hotplug, mode, mirror and transaction paths as a
full-size DisplayPort connector. USB display adapters which expose a KMS
`USB-N` connector are admitted as scanout targets as well; DRM writeback is
the only connected interface deliberately excluded.

Rotation is one end-to-end setting. The renderer samples the logical viewport
into the rotated physical KMS buffer, hardware and software cursor coordinates
use the same transform, absolute input lands in that logical plane, and
`wl_output` advertises the matching Wayland transform. 90/270 degree outputs
swap their logical extents before topology and combined-workspace geometry are
computed.

HDR, VRR and ICC controls are capability gated. EDID HDR support and the kernel
`vrr_capable` property are reported separately from *usable compositor*
support. The current DRM presenter uses XRGB8888 buffers (GBM render targets
or dumb buffers) and sets neither HDR metadata/colorspace nor `VRR_ENABLED`,
so Settings explains that limitation and provides no inert HDR/VRR toggle even
when the sink supports it.

ICC profiles are discovered from colord/XDG locations (`/var/lib/colord/icc`,
system color directories, and the user's XDG data directory), bounded to 16 MiB
and signature checked. A bounded background worker parses each candidate and
proves that qcms can construct the RGBA8 transform before it enters the live
descriptor; profile filesystem I/O never runs on the Settings or compositor
thread. The selected canonical path must still be present in that descriptor.
qcms applies the sRGB-to-display transform to that output's viewport before
rotation and XRGB swizzle. Removed or invalid profiles fail closed. Night Light
is composed before the output profile, so the comfort transform and calibration
are not competing post-processes.

## Hardware verification boundary

Pure tests cover transaction expiry/state, mirror and capability validation,
logical/physical rotation, cursor coordinates, transformed sub-rectangle
sampling, profile viewport extraction, output-global changes, and topology
geometry. A release image still needs the live matrix in `docs/testing-live.md`:
Intel/AMD KMS, a rotated touchscreen, mixed-DPI dual monitors, dock unplug while
confirmation is open, a deliberately bad mode followed by timeout recovery,
and a measured ICC test image. HDR and VRR must remain unavailable until a live
test proves both the DRM property handshake and output encoding path.
