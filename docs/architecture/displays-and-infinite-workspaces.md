# Displays and qOS's infinite workspace plane

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

Settings writes intent atomically. The compositor re-reads it, probes current
DRM capabilities, rejects stale modes/capabilities, then re-modesets between
frames. At least one connected output must remain enabled. A future graphical
arrangement editor should stage a topology and show a timed Keep/Revert
confirmation because an apparently valid KMS change can still produce a blank
sink. The typed validation contract already keeps unsupported modes, HDR, VRR,
profiles, mirror cycles and multiple-primary configurations out of the live
plan.

HDR, VRR and ICC controls are capability gated. An unavailable kernel/DRM
property is shown as unsupported, never as a toggle whose value is merely
remembered. Rotation likewise must not be exposed until the renderer and
cursor/touch transforms consume the same transform.
