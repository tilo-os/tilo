# Displays and Tilo's infinite workspace plane

Every display shows the same workspace, and each display holds its own
windows. They are not assigned independent numbered workspace stacks:
switching workspace switches it on every display at once, and connecting a
monitor never duplicates workspace content.

## Physical topology

`display_profiles` in the per-user settings document remembers one
`settings_store::hardware::DisplayConfig` per set of attached monitors
(`settings_store::hardware::profiles`), keyed by each monitor's EDID make,
model and serial rather than by port, so a dock that enumerates its ports in
another order still restores the arrangement. Within a layout an output is
keyed by its DRM connector name (`eDP-1`, `HDMI-A-1`), card-qualified only
when two cards expose the same name. Its `x`/`y` is the viewport's
top-left in the physical monitor island; negative authoring coordinates are
translated as a group for the renderer without changing adjacency.

The primary output determines where login and system prompts first appear, and
carries the workspace wheel and the full top bar; every other display gets a
compact top bar along its own top edge. It does **not** own a workspace.

## Each display holds its own windows

With more than one display, every window group belongs to exactly one of them
(`tilo_desktop_model::OutputPartition`). A strip becomes one sub-row per
display: each display lays out, pans and clips only its own groups, so a window
never straddles a seam, panning one display never reveals another display's
windows, and no window is drawn twice. Order within a display is the strip's
own order.

Every display stacks all of the workspace's strips in its own height, the way
a single display does. The strips' rows are laid out down one column across
the whole desktop, and each display takes every strip's share of that column
— the same proportions and gaps — within its own work area below its top bar.
A display therefore shows every strip's groups that belong to it, whatever
part of the column happens to pass over it: on a monitor stacked above a
laptop, the first strip is no longer only on the monitor and the second only
on the laptop.

- New windows open on the display with focus. Focus moves between displays
  with the pointer, and with Super+Left/Right past a display's first or last
  window.
- Super+Ctrl+Shift+Left/Right moves the focused group to the neighbouring
  display (Super+Shift+Left/Right keeps reordering within a display). Both
  are rebindable in Settings → Keyboard shortcuts.
- Dragging a group by its tab bar, with the pointer or a finger, reorders it
  among its display's groups; dropping it on another display moves it there,
  at the gap it was dropped in. Like the chord, that is a move the group
  keeps: a display coming back no longer reclaims it.
- Unplugging a display moves its groups to the nearest remaining display.
  Each remembers the display it came from and returns when that display is
  plugged back in, unless it was moved to another display in the meantime.
- A display shows a strip's groups in the strip's order, which is shared
  with every collaborator. Unplugging, replugging or the move chord only
  changes which display a group is on, never that order, so they never
  reorder a collaborator's strip. A moved group takes its place in the
  strip's order among its new display's groups. Only a deliberate reorder
  (the reorder chord, or dropping a group at a gap) rewrites the order.

Groups belong to the physical monitor, not to the connector or output number
it happens to have. The partition keys everything it remembers by the same
identity token display layouts are saved under
(`settings_store::hardware::DisplaySet`): the EDID make, model and serial,
with the connector appended only to tell identical monitors apart, or the
connector alone for a sink without a readable EDID. A restart, or moving the
cables to other ports, renumbers the compositor's output ids but brings every
group back to the monitor it was on. `OutputId(0)` is the primary.

Mirroring makes two connectors observe the same viewport origin. It is output
duplication, not a second copy of the workspace. Settings must choose a mode
advertised by both sinks; removing the mirror restores the saved independent
origin rather than inventing a conventional workspace.

## Collaboration

The workspace's strips, groups and tabs are shared document state; monitor
topology and the partition of groups across displays are private device state,
kept in the device's own `state.json` and never written to a room document or
the device's desktop document. A collaborator can therefore view the same
workspace through one ultrawide, two displays or a tablet without changing
anyone else's layout. Presence and focus refer to workspace
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
support. An output driven in HDR scans out a 10-bit (or half-float) PQ
BT.2020 signal: vello draws the desktop in 8-bit sRGB, and a final pass places
it at the user's SDR white and composites HDR client surfaces and peer streams
at their native precision in linear light (`shell::hdr_surface`). Anything
that renders the scene without that pass — screenshots, screencasts, SDR
outputs — shows those surfaces tone-mapped.

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
