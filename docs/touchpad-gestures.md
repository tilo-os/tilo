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
| 3 swipe up / down | Shell | Move the strip deck with the fingers; the release settles on the strip above / below, as Ctrl+Super+↑ / ↓ goes there |
| 4 swipe up / down | Shell | Switch workspace, as Super+Page Up / Page Down |
| 4 swipe left / right | Nobody | Deliberately unassigned |
| 3+ pinch in / out | Shell | Open / close the overview |

Three fingers work the plane you see and four go one level out, the split Ctrl+Super+arrows
and Super+Page Up / Page Down make on the keyboard ([window-management.md](architecture/window-management.md)).
A touchscreen swipe of three or four fingers means the same.

A three-finger pan follows the fingers 1:1. Each frame reads the finger path at the frame's
own time from libinput's event timestamps, interpolating between reports, so a steady swipe
moves the strip the same distance every frame whatever the touchpad's report rate. Fingers
lifted while still moving throw the strip: it carries on at their speed and slows to a stop
over the gesture coast time (`Gesture::fling_projection_ms`), and a throw or release past
either end settles on that end.

Three fingers up or down move the strip deck the same way: every strip is a
full-height screen, the deck follows the fingers 1:1 (against a rubber band
past the first and last strip), and the release settles it with the fingers'
momentum on the strip it carried past a quarter of the screen, or back.

A gesture is never delivered to both. Turning off **Touchpad gestures** in Settings hands
every gesture to the client. Swipe direction follows the touchpad's natural-scrolling
setting.

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
