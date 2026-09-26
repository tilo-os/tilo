# Keyboard input sources

Keyboard layouts are per-user session state. Personal Settings owns the durable
preference and the compositor is the only actuator; Tilo does not call
`localectl`, write `/etc/vconsole.conf`, or require an administrator merely to
change the language used by one logged-in user.

## Durable contract

The `input` object in `~/.config/tilo/settings.json` carries:

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
Tilo global-search shortcut. Compose-key selections map to standard XKB options.
The saved active source is the session-start preference; shortcut cycling is
temporary and does not rewrite settings on every chord.

## Input methods (Chinese, Japanese, Korean)

Composing input methods are input sources in the same ordered list, with ids
`ime/pinyin`, `ime/mozc` and `ime/hangul` (`/` never occurs in an XKB name).
They are not XKB groups: the four-group bound counts layouts only, and an
engine types on the primary (first) layout, so the XKB group index is the
active layout's position among layouts, or 0 while an engine is active
(`RichInputConfig::active_layout_index`). Settings offers an engine only when
the image ships it.

Fcitx5 provides the engines. The compositor starts it — never XDG autostart —
as `/usr/bin/fcitx5` on its own private Wayland display, exactly as the
on-screen keyboard gets its own: the display admits only the process group the
compositor started, running as the same user, and for Fcitx5 only a process
whose `/proc/<pid>/exe` is that executable. Connections on each private display
carry an immutable `TrustedInputMethod` credential; public app sockets never
see `zwp_input_method_manager_v2` or `zwp_virtual_keyboard_manager_v1`. Its
configuration is regenerated in the runtime directory on every start (the
engine as its default, `ActiveByDefault`, every enumerate chord cleared so it
cannot take Super+Space or Ctrl+Space, Cloud Pinyin and the other unused
addons disabled); user dictionaries stay in its ordinary data directory.

`input-method-v2` admits one input method per seat, so `compositor::input_method`
arbitrates:

| active source | OSK preference | owner |
|---|---|---|
| engine (installed) | any | Fcitx5 with that engine; OSK stopped |
| layout, or engine not installed | on | on-screen keyboard |
| layout, or engine not installed | off | nobody |

The on-screen keyboard steps aside rather than feeding Fcitx5: its keys are
committed text, not key events a composer could compose, and splitting one
field between two input methods would leave preedit owned by neither. The
decision is enforced on the wire — the globals are visible only to the current
owner's kind, and moving the seat disconnects the previous owner's
connections before its process group is terminated. Switching engines restarts
Fcitx5 with the new configuration.

Apps receive composed text through `zwp_text_input_v3` (GTK, Qt, Chromium and
Tilo apps through winit's IME). Candidate windows are `zwp_input_popup_surface_v2`
surfaces placed directly below the field's caret rectangle, flipped above it
at the bottom of an output, and kept on that output.

Hosted development mode receives already-translated physical key events from
the host compositor, so Tilo cannot override that host keyboard. Its embedded
Wayland seat still publishes the configured Tilo map. Production DRM sessions
own raw input and apply the preference end to end.
