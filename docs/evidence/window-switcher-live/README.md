# Alt+Tab live candidate evidence

The shared desktop switcher kept its opening snapshot after windows closed or
moved. Closing another card left a stale preview and accessibility entry; closing
the armed card could commit its dead address on Alt release. The current surface
preserves the opening order and stable selection while pruning closed windows,
refreshing moved addresses and cancelling an interrupted selected window.

The existing theme materials, selection-tier painter and native app-mark painter
remain in use. Changed geometry consumes old-position pointer presses until a
new frame is painted. No thumbnail/privacy or workspace permission policy changes.

## Verification

- Three production regressions fail on baseline e2a31864; all 11 focused desktop
  cases pass, including stale-card pointer input and the production capture matrix.
- Seven native shell integration cases pass, including real close-tab routing
  during Alt+Tab, Alt release, cancellation and reopening.
- Shell tests compile and desktop/shell all-target Clippy with warnings denied pass.
- Six before and six after production captures cover dark/light/high contrast,
  200% text, 150% UI plus 130% text, and 320×480. Eight inspected representatives
  are Library image files; artifacts.json records original IDs and checksums.
- Capture source: b0cff0c012271f537ad86b90bbdaae7251d39200; its source is unchanged
  by the clean rebase onto current main. The fixture deliberately uses withheld
  previews and generic foreign-app marks, not real application screenshots.
- Remote Vulkan lavapipe only. These are production offscreen captures and native
  input journeys; live VM and physical-device coverage remain separate.

## Visual findings

Before: the closed Browser card remains beside selected Files. After: Browser is
removed and the shared Files focus ring stays attached to Files. Caption and
panel bounds remain legible across the inspected theme and scaling variants.

Normal protected CI, landing and merged-main postchecks remain completion gates.
New PNGs are external artifacts; this directory contains text only.
