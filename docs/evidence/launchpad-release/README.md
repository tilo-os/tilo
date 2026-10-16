# Launchpad release and interruption handling

Base: 97f53ccd5bd2e018d794fe1592b06cb2c65012e4.

The actual native empty-workspace host opened Search on pointer-down. Its new
regression failed at this base after capturing both idle and pointer-down states.
The desktop widget regression and four SDK interruption/repeat regressions also
failed on this same base, before the production fix.

The launchpad uses the UI library's PaintedTargets to own press/release,
cancel drag-off, consume interrupted releases and suppress held Enter/Space.
The library also cancels presses when registered geometry moves, becomes
disabled, loses focus or receives Escape. A fresh gesture remains usable.
Target registration is cached by geometry; existing typed semantic actions,
localized accessibility routes, card layout, theme/material hierarchy and grid
navigation remain in place. Shared selection and Button faces show press state.

The native test drives ShellOverlayHost::handle_event and settle, paints the
actual native host, and asserts Search stays closed during press/cancellation,
opens on completed release and closes on Escape. No app launch, microphone,
account send or grant action runs. Requested screenshots require a working
offscreen adapter; no observer failure is skipped.

Evidence is rendered remotely with Vulkan and the Mesa CPU ICD. Dark, light,
200% text and combined 150% UI/130% text cover idle, pointer-down, drag-cancel,
released Search and Search closed by Escape. PNG originals stay outside Git;
the artifact manifest records sizes and SHA-256 checksums; PR attachments provide public originals.
The same candidate's headless compositor also passes live tilo-control
pointer press/drag-off/release, held press/Escape/release cancellation and three
fresh click/Search/Escape cycles. The inspected live 1600x900 originals show
the focused/pressed button and Search opening only on completed release.
The canonical scratch-profile launcher isolates all profile and filesystem
handoff paths; its process, socket and temporary profile were cleaned up.
This does not claim headful VM/device coverage.

Focused SDK and launchpad regressions, native Search journeys, shell test
compilation, all-target Clippy, staged hook and affected-package plan qualify
the candidate. Exact-head CI, normal protected landing and merged-main
postchecks remain separate gates.
