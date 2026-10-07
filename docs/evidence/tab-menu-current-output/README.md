# Tab menu current-output evidence

Resizing an output leaves the open tab menu constrained to its opening viewport.
At a 640px width the old shared-menu box starts at x620 and ends at x780, so
only a clipped sliver remains visible. Removing its display leaves it at
x1100..1260 instead of moving it onto the remaining focused output.

The shell now calls the shared ContextMenu.constrain_to API with current
LayerGeometry before its existing draw_overlay. The toolkit owns placement,
measurement, scrolling, focus and material painting. Opening anchor, selected
action, resource target and permission/action dispatch stay intact.

## Verification

- Both production-layer regressions fail on baseline 87ca95d77f35a81d1ea9a304f5fb621633334d92.
- All three focused current-output cases pass with the fix, including the
  production capture matrix, retained semantic focus and dismissal.
- The existing production host keyboard journey passes: open at focused tab,
  navigation, held-chord repeat, Esc/cancel, reopen and split.
- Shell test targets compile; shell all-target Clippy with warnings denied passes.
- Six before/six after CPU production captures cover dark/light/high contrast,
  200% text, 150% UI plus 130% text, and a 320x480 output. Eight inspected
  representatives are native Library image files; artifacts.json records
  original Library IDs, dimensions, source revisions and checksums.
- Verified source: a94a43ff72c5aef0746592c85df3b6752d607a9f. Explicit Vulkan lavapipe
  rendering; no physical DRM retry. PNGs remain outside Git.

Before: every action label is clipped by the resized right edge. After: all
three labels are readable, Split Right remains armed, and the shared panel
fits the current output in every inspected theme/scale variant.

This is production layer rendering and host input verification. Live VM/device
coverage remains separate. A broader live headless Super+K journey currently
times out after a workspace-wheel click, including the documented real-modifier
variant; its unchanged strict assertions, actual state and failure capture are
retained for a separate diagnosis, not counted as a passing journey here.

Normal required CI, protected landing and merged-main postchecks remain gates.

## Separate live pointer journey

A private CPU headless compositor built at 34e3267a passed clicking the named launchpad:open-app target, global palette visibility/scope and its semantic input, keyboard typing, Esc removal from state and actual pixels, then repeated open/type/cancel in the same session. The original Super+K route remains failing, including Esc recovery; no bypass or policy change was made. Two separately inspected live PNGs are also recorded in artifacts.json with original Library IDs and the older compositor source revision. They are additional journey evidence, not menu before/after captures. The private process, scratch profile and control socket were cleaned up.

Current-main rebase verification at 24064dc39ead080a7bcf606c4b7b7820daa59496 passed all three current-output cases, the production keyboard journey and shell test compilation. All six newly rendered PNGs are byte-for-byte identical to the inspected a94a43ff captures, including the SHA-256 values recorded here; original Library identities and capture source attribution are preserved. Final Clippy and normal CI/landing/main postchecks remain gates.

After the real inventory conflict from Alt+Tab landing, the menu implementation rebased without a source conflict onto 6476c63a. All three current-output cases, keyboard journey, production pointer open/dismiss case and action-parity case passed. All six rerendered PNGs remain byte-identical to the original inspected images. Compile/Clippy completion and normal CI/landing/main postchecks remain gates.
