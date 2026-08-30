# Keyboard input sources

Keyboard layouts are per-user session state. Personal Settings owns the durable
preference and the compositor is the only actuator; qOS does not call
`localectl`, write `/etc/vconsole.conf`, or require an administrator merely to
change the language used by one logged-in user.

## Durable contract

The `input` object in `~/.config/qos/settings.json` carries:

```json
{
  "keyboard_layout": "de:nodeadkeys",
  "keyboard_layouts": ["us", "de:nodeadkeys"],
  "compose_key": "ralt"
}
```

A source id is `layout` or `layout:variant`. The ordered list is bounded to four
unique, validated XKB groups. `keyboard_layout` is the active source and remains
under its historic key for compatibility. A document containing only the old
`keyboard_layout` field migrates to a one-item list; it does not silently add US.
Malformed ids, path punctuation, commas, duplicates, and entries beyond the
bound are ignored by the shared settings contract before libxkbcommon sees them.

Settings reads layouts and variants from the image's standard
`/usr/share/X11/xkb/rules/evdev.xml` catalogue. `base.xml` and a small built-in
catalogue are availability fallbacks, not competing stores. The UI presents a
compact active-source chooser first; installed variants, adding, ordering and
removal live in the secondary Input sources card.

## Compositor application

The bare-TTY DRM input path compiles all configured groups with libxkbcommon,
selects the saved group, and translates physical libinput keycodes through that
state. The Wayland seat publishes the same keymap to clients and virtual
keyboards. The existing settings-file watch recompiles both paths in the live
session. If compilation fails (for example, an XKB data package was removed),
hot reload keeps the previous working keymap; failure never leaves a running
session without keyboard input.

Alt+Shift uses the standard `grp:alt_shift_toggle` option when more than one
source is configured. Super+Space is deliberately not claimed because it is the
qOS global-search shortcut. Compose-key selections map to standard XKB options.
The saved active source is the session-start preference; shortcut cycling is
temporary and does not rewrite settings on every chord.

Hosted development mode receives already-translated physical key events from
the host compositor, so qOS cannot override that host keyboard. Its embedded
Wayland seat still publishes the configured qOS map. Production DRM sessions
own raw input and apply the preference end to end.
