# Constrained emoji picker and shared scroll lifecycle

The picker composes the existing SearchField and ToggleGroup with the UI library's
ScrollRegion. Short outputs retain measured emoji targets and a fixed search
field while the categories, grid and footer share a clipped, scrollable body.
Skin-tone choices wrap into measured rows and use the same library for overflow,
keyboard reveal, pointer capture, fade scheduling and the native semantic scroll
door. Materials, typography, spacing, control metrics and focus ink remain themed.

## Independent reproductions

On exact base bf051e3162e8a14cacc45dbd8c1bf8535f9166e0, three new
portable regressions fail: the short card/grid is unavailable, narrow tones leave
the card, and Down does not follow the painted tone rows. Two shared scrollbar
regressions also fail on that unchanged production source: focus loss and Escape
retain capture.

Two additional production-path regressions reproduce stale scrollbar capture
when viewport or content metrics change. Identical layout calls retain an ongoing
drag; changed metrics retire it, late move/release cannot continue it, and a fresh
drag uses the new geometry.

Actual intermediate capture inspection exposed a further hidden no-results
message on a short output. Its separate keyboard/paint regression fails before
the empty-result reveal correction. The intermediate PNG remains at its original
capture path and checksum; it is not relabelled as an exact-base capture.

Original inline shared-scroll and picker test bytes, and the original native
overlay test file after removing only the new module declaration, remain
byte-identical to the exact base. Separate test modules retain existing assertions.

## Journeys and qualification

Focused tests exercise measured target bounds, spatial tone navigation, named
pointer insertion, semantic scrolling, stale press/release, query correction,
nested Escape, close/reopen, resize, focus loss and reduced-motion scheduling.
Native journeys dispatch through ShellOverlayHost and its existing public Widget
scroll surface; they assert that behind-content input is not delivered.

Remote qualification runs the focused shared-scroll, picker and native modules;
strict all-target UI/desktop/shell Clippy; and cargo check --locked -p shell --tests.
Final receipts, source/tree identities and checksums accompany the ready PR.

## Actual screenshots

The fixture-only emoji_picker_constrained example calls production keyboard and
paint paths and renders through the remote Mesa CPU Vulkan backend. The exact-base
matrix covers light 240x360, dark 200% text 320x240, combined 150% UI/130% text
320x240 and a normal dark 800x700 control. Added final states include high contrast,
the keyboard-focused last wrapped tone and an empty search. Screenshot PNGs stay
outside Git; representative PR attachments are anonymously downloaded and checked
against SHA-256 before they are reported as shareable.

Normal-size geometry and aesthetics are retained. Pixel comparisons record small
shadow antialiasing differences explicitly rather than claiming byte identity.

## Remaining acceptance and boundaries

Installed-device input, physical display hotplug and compositor/VM captures remain
separate acceptance gates; these receipts prove production offscreen rendering
and native Widget input routing, not DRM or KVM operation. No credentials, grants,
trust, signing, sandbox policy, microphone or account sends are changed.

Quick Settings #3710 shares overlay_host/mod.rs at file level; its inspected hunks
do not change the existing primary-scroll route. Scope notice:
https://code.tilo.so/tilo/tilo/pulls/3710#issuecomment-50549.
Messages #2881 and Dictation #2974 share that host file in separate existing hunks.
Files #3009 broker, transfer and undo paths are excluded.

The broad remaining-surface inventory stays active in
../../desktop-surface-adoption.md. This increment does not close unrelated
launcher, notification, window chrome, status, workspace or installed-device work.
