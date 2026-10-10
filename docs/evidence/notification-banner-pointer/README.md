# Notification banner completed clicks

At production main 251c4ff21156cc697e01e7a381600c9d6681a2e9, pressing a
banner immediately activates/retires it; dragging away or Escape cannot cancel
that operation. The close face also receives no hover state from native
pointer routing. Both focused native regressions fail at that base, with all
production files unchanged and twelve baseline PNGs recorded.

NotificationStack now uses shared PaintedTargets on the geometry returned by
ToastCard. The toolkit's existing widget event handler and its small pointer
adapter use the same controller, including geometry cancellation and retained
release ownership. The adapter lets canvas hosts preserve application keyboard
focus. ToastCard forwards hover/pressed state into its existing ButtonFace
controls; no new face, material or layout is introduced.

Both shell and portable NotificationLayer forward the pointer stream. Only a
completed click executes an existing host operation. Drag-out, Escape, focus
loss, resize and an independently retired notice leave no accidental action;
the matching release stays with the banner, and the next desktop click still
arrives. Dispatch checks that the notice and chosen key are still live.
Foreground history keeps its own presses and cancels an armed banner.

Evidence is fixture-only production painting after actual native WindowEvents,
with synthetic error notices and a recording content widget. No real sender,
account, device or microphone action is part of these journeys.

- Twelve before/after pairs: idle and close-hover in dark, light, 200% text,
  high contrast with 200% text, 150% UI with 130% text, and a narrow 320 px host.
- Six additional after captures show the close face while pressed; Escape
  and release then retain the same error notice.
- All six idle captures remain byte-identical to the baseline.
- Native routing tests cover five interruption cases and completed/repeated
  clicks. Portable host tests cover default action delivery and cancellation
  when history opens. Existing shared-widget keyboard/accessibility tests
  protect retained focus and activation behavior.

Inspected source-only images and qualification are attached to the ready PR.
Installed-device/VM acceptance is separate. Explicit keyboard entry into the
live banner stack, quiet/locked presentation and installed-device coverage
remain in the desktop surface inventory.

The adopted controller's missing-release sequence is also reproduced in a
native fixture. A fresh press now resets abandoned press/cancelled-release
state even when it lands outside all banner targets. Both focus-loss and
no-focus-loss cases require delivery of the next content press and release.
Cancellation still owns an arriving old release; the retained widget's
existing Escape, focus-loss and moving-geometry regressions stay required.
