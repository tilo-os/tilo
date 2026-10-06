# Session power flyout evidence

Before: c3a50df, with the same fixture matrix. After: the production PowerMenu
uses the toolkit ListRowFace. These are remote offscreen captures of the real
paint path on a quiet fixture canvas, not a booted VM or the user's desktop.

The flyout keeps its status-area placement, Panel material and quiet row floor.
The shared face supplies Body/Caption type, glyphs, elision, selected lift and
a visible keyboard focus ring. Measured localized action names widen the panel;
a narrow output with enough height switches to one column. A height-constrained
row keeps its action name and drops secondary paint, retaining its accessible
description.

| State | Before | After |
| --- | --- | --- |
| Dark, normal type | [Before](before-dark-1x-1280x800.png) | [After](after-dark-1x-1280x800.png) |
| Light, normal type | [Before](before-light-1x-1280x800.png) | [After](after-light-1x-1280x800.png) |
| Dark, 200% text | [Before](before-dark-2x-1280x800.png) | [After](after-dark-2x-1280x800.png) |
| Laptop, 200% text | [Before](before-dark-2x-800x700.png) | [After](after-dark-2x-800x700.png) |
| High contrast | [Before](before-high-contrast-1x-1280x800.png) | [After](after-high-contrast-1x-1280x800.png) |
| 150% UI, 130% text | [Before](before-dark-ui-1.5-text-1.3-1280x800.png) | [After](after-dark-ui-1.5-text-1.3-1280x800.png) |
| Narrow, 200% text | [Before](before-dark-2x-480x600.png) | [After](after-dark-2x-480x600.png) |

The selected action is Switch user. Render tests assert actual Panel and idle-row
pixels, not just file creation. Focus, label visibility and secondary-copy
containment were inspected in the generated pixels.

Verification runs remotely with plain locked Cargo:
- Focused power-menu tests cover all row activation outcomes, animated hit tests,
  supported session rows, keyboard cycling, Esc and outside cancellation.
- The new combined-scale regression draws the production menu, resizes it,
  checks retained focus and a11y/pointer bounds, cancels, reopens and checks that
  repeated input after close is ignored. The same regression fails on the base.
- Native overlay accessibility routing and quick-settings pointer entry are tested.
- cargo check --locked -p shell --tests; shell library Clippy; exact staged hooks.

Tests inspect returned action intents only. No lock/logout/power effects or
account/device actions are executed. Live compositor/VM journeys remain a
separate verification layer.
