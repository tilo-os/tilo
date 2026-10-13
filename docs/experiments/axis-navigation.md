# Axis navigation experiment

This is an opt-in, session-only adaptation of the axis navigation and focus-gather
ideas in Clay's [Spear launch](https://www.youtube.com/watch?v=7eGS54ScY0g), linked
from [Spear](https://spearui.com/). The supplied verified launch transcript is the
reference; the video animation has not been visually verified. No reference code,
artwork or animation assets are copied.

Tilo already has persistent workspaces and non-overlapping horizontal strips.
This experiment keeps that model and its workspace permissions. It does not add
floating windows or change collaboration state, app storage or window sizes.
The ordinary overview remains the way to zoom out and reposition across contexts;
this patch does not reimplement it or add keyboard bindings.

Use the existing action door (provider `actions`):

- `navigation.axis.enable`: opt in for this running session.
- `window.focus-next` / `window.focus-previous`: step through spatial groups in
  strip order, keeping each group's active tab. Existing keyboard, horizontal
  wheel and swipe routes use the same implementation. Ends clamp without wrapping.
- `navigation.axis.gather`: reveal the already-focused group after manual panning.
- `navigation.axis.disable`: restore ordinary navigation through individual tabs.

For example, `tilo-control actions invoke actions navigation.axis.enable` enables
it. The palette and agents expose the same actions with localized labels. Tab
cycling retains its ordinary behavior. Nothing persists the opt-in or replicates
it to another collaborator. Each session starts with the ordinary behavior.

Gather uses the existing retained row's laid-out geometry, including per-display
lanes, gutters and logical coordinates. It cancels a stale snap and uses an
instant reveal, so reduced motion needs no new animation policy. Windows wider
than the available viewport retain the existing framing policy; the experiment
never resizes them to manufacture a fit. On multiple displays navigation stays
inside the focused display. Gather requests are discarded if workspace, strip,
focus or display changes, on lock/fullscreen, or when a pointer drag interrupts.

Validation uses the production `ShellOverlayHost`, real retained layout and
keyboard/wheel event routing in `strip_navigation`, plus provider invocation in
the shell-action tests. Native `FrameBuilder` regressions also dispatch AccessKit
tab activation, asserting selection survives model resync, an already-selected
tab focuses its group, and accessibility focus follows the tab bar. A mixed-scale
case activates a tab on the other display and gathers only that display's lane.
The redesigned tab bar's separate media controls keep their media intent without
moving spatial/native focus or requesting gather. Tab-selection activation alone
uses the focus-gather path. Semantic activation commits through the same writeback boundary as pointer input;
it cannot be overwritten by the next model sync.

Offscreen journeys exercise keyboard and repeated wheel navigation, native
assistive activation and tree exports in light, dark and high-contrast themes.
Before broader UX review, verify real touchpad and AT-SPI screen-reader journeys,
mixed-density output hotplug, oversized windows and performance on hardware.
This is a prototype, not a claim of visual equivalence or hardware measurements.
