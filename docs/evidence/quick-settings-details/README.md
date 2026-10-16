# Quick Settings radio-details doors

A pointer could open Wi-Fi networks or Bluetooth devices from the root
chevron, but keyboard navigation and the root accessibility tree offered only
the radio toggle. The production baseline at
9fcf336cff512a5ff6f7c98a0a9777ed9d65d425 fails both focused regressions;
eight baseline captures retain the old body focus.

The chevrons now use the toolkit ButtonFace, with independent focus stops,
localized names and stable details keys. The existing pointer geometry is
also the published accessible target. Tab/Enter and accessibility focus
open the details without toggling a radio. The actions provider exposes
quick-settings.network and quick-settings.bluetooth as ordinary view
operations, alongside the existing Quick Settings destinations.

The native pointer journey exposed a second defect: navigation shrank the
popover before the matching release, allowing that release into underlying
content. Shell now retains the navigation release across the page change
and Escape, then lets the next desktop click through. Existing slider
capture and modal priority keep their ordering. Tiny/unlaid-out details
targets no longer clamp against an inverted range.

Evidence is fixture-only production QuickSettingsRoot painting, with
synthetic radio availability and no seeded networks or paired devices:

- Eight before/after pairs: dark, light with 200% text, high contrast with
  200% text and a 320 px panel with 200% text; both details buttons.
- Two additional after captures: 150% UI with 130% text.
- Native shell event routing covers keyboard, pointer, accessibility focus,
  provider invocation, Escape, repeated entry and resize. A recording device
  backend must receive no writes.
- An interrupted pointer journey closes the panel before release, requires
  that release to stay out of content, and requires the next ordinary
  desktop click to remain deliverable.

Shareable images and qualification are attached to the ready PR. Source
checks and the final image manifest identify their exact revision. This
does not claim installed-device or QA VM acceptance. Narrow enlarged-text
Devices/Clipboard summaries still elide in the baseline and after images;
their presentation remains a separate inventory item.

The navigation-release follow-up reproduces an absent release after focus
loss: the next content press arrived but its release was swallowed at
9da8791c351223159734ebdbecb9d29ba122cd37. The existing initial pointer
observation stage now retires the flag on focus loss or a fresh left press.
Four native cases cover both radios with and without focus loss, while the
original page-change/Escape release-ownership regression remains required.
This routing-only correction changes no painted component or screenshot.
