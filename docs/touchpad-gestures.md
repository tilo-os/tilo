# Touchpad gestures and settings

The chain is touchpad → libinput → compositor → Wayland → toolkit (winit) → app. Every link
is in place; this page says who owns what.

## Who gets a gesture

libinput reports swipe, pinch and hold. The compositor decides ownership once, on the
gesture's `begin`, by finger count (`crates/session/shell/src/compositor/backend/drm/gestures.rs`):

| Fingers | Owner | What happens |
|---|---|---|
| 1–2 | Focused client | Forwarded over `zwp_pointer_gestures_v1` (swipe, pinch, hold) |
| 3 swipe left / right | Shell | Pan the active strip directly |
| 3 swipe up / down | Shell | Focus the strip above / below, as Super+↑ / ↓ |
| 4 swipe up / down | Shell | Switch workspace, as Super+Page Up / Page Down |
| 4 swipe left / right | Nobody | Deliberately unassigned |
| 3+ pinch in / out | Shell | Open / close the overview |

Three fingers work the plane you see and four go one level out, the split Super and
Ctrl+Super make on the keyboard ([window-management.md](architecture/window-management.md)).
A touchscreen swipe of three or four fingers means the same.

A gesture is never delivered to both. Turning off **Touchpad gestures** in Settings hands
every gesture to the client. Swipe direction follows the touchpad's natural-scrolling
setting.

## Two-finger sideways scrolling

Two-finger scrolling is no gesture to libinput but a scroll, and it goes to the window under
the pointer first. A sideways one moves the strip when that window has no use for it
(`crates/session/shell/src/compositor/scroll_handback.rs`):

- An app built on Tilo's toolkit hands back, over the private `tilo_scroll_v1` protocol
  (`vendor/winit/src/platform_impl/linux/wayland/types/tilo_scroll.xml`), the first event of a
  mostly sideways scroll that nothing in it used and that would not navigate it back or
  forward (`ui::gesture::SidewaysScroll`). The compositor ends the scroll on the window with
  `wl_pointer.axis_stop` and pans the strip with all of it, as three fingers do. A table, map
  or editor that scrolls sideways keeps it.
- A sideways wheel notch (a tilting wheel) the app hands back steps along the strip a window,
  as Alt+Shift+wheel does.
- Any other Wayland client keeps every horizontal scroll: nothing on the wire says whether it
  used one, and browsers, spreadsheets and editors do. Three fingers pan the strip over it.
- Over the shell's own chrome (the tab row, the gaps between windows), a sideways scroll pans
  the strip directly.

A scroll keeps the owner its first event chose until the fingers lift, so the window and the
strip never move together.

## Pinch-to-zoom in apps

The compositor advertises `zwp_pointer_gestures_v1`, and Tilo's vendored winit
(`vendor/winit/src/platform_impl/linux/wayland/seat/pointer/gestures.rs`) binds it and emits
`WindowEvent::PinchGesture` with the same incremental delta as the macOS backend. Toolkit
widgets that zoom (plots, documents) handle that event, so a two-finger pinch zooms in any
Tilo app and in third-party clients that speak the protocol. Ctrl + scroll still zooms too.
Touchscreen pinch goes through the toolkit's own touch handling, not this protocol.

## Touchpad settings

Settings → Mouse & Keyboard writes the `input` section of `settings.json`; the compositor
re-reads it within a second and reapplies it to every attached device. Each libinput option
is only requested from a device that reports it
(`crates/session/shell/src/compositor/backend/drm/input_config.rs`), so a choice a device
lacks is left at that device's default.

- Tap to click, tap-and-drag, disable while typing, natural scrolling, pointer speed,
  acceleration profile, middle-button emulation, left-handed buttons.
- **Touchpad click**: device default, button areas (bottom-right corner right-clicks), or
  finger count (two-finger press right-clicks, three middle-clicks).
- **Touchpad scrolling**: two fingers or edge.
- **Scroll speed**: a multiplier (¼× to 4×) the compositor applies to wheel and finger
  scrolling alike; libinput has no such option.
