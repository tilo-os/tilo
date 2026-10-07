# Launcher recovery qualification

Source before: `6fc43bd89e0837e23ed951d3cd5001009018c87b`.
Source after: `6f6300af17918835e81f7087dfef17527eba02e4`.

A synchronous provider refusal previously closed search. The production host now
retains query, focus and selected result, with the toolkit's existing status pill
and accessible detail. A held Enter does not repeat the invocation. Existing
callback-owned warning/error notices are reused; progress notices cannot conceal
the error. A fresh result set clears recovery feedback.

Shared CommandPalette geometry accounts for both baked theme scales. Selected
titles and glyphs use the ink returned by the shared armed-row painter, preserving
the existing material and lift. Enlarged detail still elides visually where needed;
the accessible description retains the complete recovery message.

## Captures

The six after variants are dark, light, high contrast, 200% text at 800×700,
UI 150% plus text 130% at 1280×800, and a 480×600 narrow viewport.
The two before images deliberately show the palette disappearing after refusal.
These use a controlled application provider through production registry, input
routing, layout and draw paths. Fixture/demo backgrounds are not an installed
application or the user's desktop.

The live pair uses the real CPU-rendered headless compositor and a fresh isolated
profile: wheel pointer activation, held Super+Space, Files query, state/target
checks, actual snapshots, then Escape and pixel verification that search is gone.
The scratch broker is unavailable, as its banner honestly reports; this is not
a collaboration or successful sandboxed app-launch qualification.

All ten original PNGs were inspected. Public attachments and representative image
links are recorded in the ready pull request. `artifacts.json` records byte sizes,
dimensions, hashes and source commits. Images remain outside Git. New Library
saving was unavailable in this session; no new Library identities are claimed.

## Verification

- Before: keyboard and pointer recovery tests fail on the production host.
- 12 shared palette tests pass, including measured font heights against actual
  row bounds, grouped rows, navigation, argument prompts and query editing.
- Five native recovery tests pass: retry/Escape/reopen, repeat suppression,
  pointer retry after resize/query correction, callback-notice reuse, progress
  notice/error distinction, and the production render matrix.
- Every captured failed row must remain visibly reachable inside its actual list.
  The render test checks title pixels against painted row fill at the shared
  4.5:1 floor, rather than trusting a helper's returned color.
- Existing native palette chip/scope and launch-notice retirement/Escape cases pass.
- `cargo check --locked -p shell --tests` and
  `cargo clippy --locked -p tilo-ui -p shell --all-targets -- -D warnings` pass.
- Staged pre-commit contracts pass. The affected plan selects 97 of 273 packages,
  following shared UI/shell dependencies; it does not select the full workspace.

All Cargo, compositor and rendering work ran on the SSH builder with CPU Vulkan.
VM coverage remains blocked by refused SSH on port 2222 and denied /dev/kvm access.
The separate DRM permission denial was respected. No device grants, signing trust,
security policy, clipboard transfer/undo, or durable notification scheduler changed.

## Remaining launcher inventory

Provider-index ranking/selection stability, empty/loading states and successful
sandboxed app launch/failed asynchronous launch remain distinct work. Full device,
hotplug and VM journeys require an authorized working environment.
