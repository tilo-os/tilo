# Notification center: shared controls and usable enlarged text

Before: main c3a50df. After: this branch's production NotificationCenter.
Generated remotely with the fixture-only notification_center_shots example;
both sets use the same notices, viewport, type scales and action focus.
The panel is rendered offscreen with the production painter and toolkit text
renderer. These captures do not record the user's desktop or a VM session.

| State | Before | After |
| --- | --- | --- |
| Dark: Clear all focus and an application error | [Before](https://code.tilo.so/tilo/tilo/raw/commit/62d251fbc81b088c81b7a3138ee1d1083019ddcd/docs/evidence/notification-center/before-dark-clear-focused.png) | [After](https://code.tilo.so/tilo/tilo/raw/commit/62d251fbc81b088c81b7a3138ee1d1083019ddcd/docs/evidence/notification-center/after-dark-clear-focused.png) |
| Light: dismiss-action focus | [Before](https://code.tilo.so/tilo/tilo/raw/commit/62d251fbc81b088c81b7a3138ee1d1083019ddcd/docs/evidence/notification-center/before-light-dismiss-focused.png) | [After](https://code.tilo.so/tilo/tilo/raw/commit/62d251fbc81b088c81b7a3138ee1d1083019ddcd/docs/evidence/notification-center/after-light-dismiss-focused.png) |
| 200% text: dismiss focus and long recovery text | [Before](https://code.tilo.so/tilo/tilo/raw/commit/62d251fbc81b088c81b7a3138ee1d1083019ddcd/docs/evidence/notification-center/before-large-text-dismiss-focused.png) | [After](https://code.tilo.so/tilo/tilo/raw/commit/62d251fbc81b088c81b7a3138ee1d1083019ddcd/docs/evidence/notification-center/after-large-text-dismiss-focused.png) |
| 200% text: Undo focus | [Before](https://code.tilo.so/tilo/tilo/raw/commit/62d251fbc81b088c81b7a3138ee1d1083019ddcd/docs/evidence/notification-center/before-large-text-undo-focused.png) | [After](https://code.tilo.so/tilo/tilo/raw/commit/62d251fbc81b088c81b7a3138ee1d1083019ddcd/docs/evidence/notification-center/after-large-text-undo-focused.png) |

## Representative comparison

### 200% text, long recovery preview

![Before: recovery text paints below its notification card](https://code.tilo.so/tilo/tilo/raw/commit/62d251fbc81b088c81b7a3138ee1d1083019ddcd/docs/evidence/notification-center/before-large-text-dismiss-focused.png)

![After: toolkit elision keeps recovery text in its row and the action has a scaled target](https://code.tilo.so/tilo/tilo/raw/commit/62d251fbc81b088c81b7a3138ee1d1083019ddcd/docs/evidence/notification-center/after-large-text-dismiss-focused.png)

### 200% text, Undo

![Before: Undo has no indication of accessibility focus](https://code.tilo.so/tilo/tilo/raw/commit/62d251fbc81b088c81b7a3138ee1d1083019ddcd/docs/evidence/notification-center/before-large-text-undo-focused.png)

![After: shared Undo face has a focus ring and measured bounds](https://code.tilo.so/tilo/tilo/raw/commit/62d251fbc81b088c81b7a3138ee1d1083019ddcd/docs/evidence/notification-center/after-large-text-undo-focused.png)

Full notification text remains in the accessible description. Preview elision
does not modify notification content or its action.

The production overlay test covers pointer and keyboard paging, Back, Clear all,
Undo and accessibility activation. Additional center regressions cover stale
Clear all focus after a pointer group action, interrupted/repeated Clear/Undo,
and preserving the focused row/control across resize with enlarged text.

No real notification sender, account, agent task or microphone is used by the
capture fixture. Workspace grants and platform security policy are unchanged.

## Production shell overlay matrix

The existing opt-in shell render test also passed for newest/middle/oldest
positions and after Clear all on a compact viewport, enlarged text and high
contrast with UI scaling. Representative production overlay captures:

- [Compact, enlarged text, oldest page](https://code.tilo.so/tilo/tilo/raw/commit/62d251fbc81b088c81b7a3138ee1d1083019ddcd/docs/evidence/notification-center/native-960x540-large-text-oldest.png)
- [High contrast at 125% UI scale, after Clear all](https://code.tilo.so/tilo/tilo/raw/commit/62d251fbc81b088c81b7a3138ee1d1083019ddcd/docs/evidence/notification-center/native-1280x720-high-contrast-after-clear.png)

The native fixtures render the shell overlay on a quiet black canvas; they do
not depict a booted VM desktop. The center is above the existing live banner
stack in this fixture. Reviewing how active banners and retained history are
presented together is a remaining notification UX increment.

PNG binaries are stored in ChatGPT Library, with native file identities, dimensions,
source commits and SHA-256 checksums in [artifacts.json](artifacts.json). The paired
image links above retain the already-published capture revision as a browser-viewable
fallback. PNG binaries are removed from the current source tree.
