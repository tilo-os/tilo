# Keyboard input sources

Keyboard layouts are per-user session state. Personal Settings owns the durable
preference and the compositor is the only actuator; Tilo does not call
`localectl`, write `/etc/vconsole.conf`, or require an administrator merely to
change the language used by one logged-in user.

## Durable contract

The `input` object in `~/.config/tilo/session/settings.json` carries:

```json
{
  "keyboard_model": "pc105",
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

## Visual layout preview

Personal Settings' Keyboard page previews every installed XKB layout and variant
without changing the active source. The explicit physical model is ANSI pc104 or
ISO pc105, Japanese jp106 or Korean kr106; the map does not claim to detect a laptop's geometry. `keyboard_model`
selects the session model used by the compositor and Xwayland; missing or invalid
legacy values retain pc105. **Use this layout** applies the preview through the
same typed input preference action as the ordinary active-source chooser.

`tilo-keyboard-map` owns native compilation from the fixed, public, read-only XKB
installation. It ignores environment and user include paths, rejects path
arguments, and returns bounded native physical identities and symbol levels.
Settings owns licensed Open Group model geometry, presentation and the declared
Preview layout, Keyboard model, Additional levels and Use this layout actions.
The shared KeyboardMap widget provides a selected native level, with every
compiled level available through the declared level selector, shared tooltip
and accessibility metadata. Narrow maps keep the selected symbol dominant.
Show live keyboard follows immutable configuration emitted by the ordinary
focused Wayland keymap/modifier events, including raw masks and current group;
explicit previews never relabel themselves as the live seat. Physical presses
remain private and foreground-only, including while page search has focus.
Page replacement retains only configuration; actual blur revokes it. Framework
modals suppress key feedback and resume from the real configuration cache.
Catalogue entries require a symbols file in the compiler's same fixed root,
so an uninstalled custom template is not offered. Installed custom files remain
valid sources. Unsupported metadata is reported explicitly rather
than replacing it with a sample QWERTY map.

While this page is visible in the focused Settings window, ordinary physical
press/release events highlight positions even when the layout picker or search
field owns editing focus. The toolkit strips logical text from a physical-input
observation before its own shortcuts (including Undo) consume the event. The
observer never consumes input or changes focus, and synthesized window key
notifications do not become physical feedback.
Logical text, pressed positions and modifier state are not written to resources,
CRDT history, accessibility values or logs. Focus loss and page exit clear held
positions. A single bounded compiled preview survives search recomposition.
No global input subscription, device access or writable XKB metadata is granted.

## Compositor application

The bare-TTY DRM input path compiles all configured groups with libxkbcommon,
selects the saved group, and translates physical libinput keycodes through that
state. The Wayland seat publishes the same keymap to clients and virtual
keyboards. The existing settings-file watch recompiles both paths in the live
session. If compilation fails (for example, an XKB data package was removed),
hot reload keeps the previous working keymap; failure never leaves a running
session without keyboard input.

Switching sources is the compositor's rebindable **Next keyboard layout**
shortcut, Super+Alt+Space by default (Super+Space is global search). It writes
the new active source to the settings document, switches the Wayland seat and
the DRM keyboard at once — keeping held modifiers — and shows the layout's XKB
name in the on-screen readout. A settings reload that changes only the active
source switches groups rather than recompiling the keymap. No XKB group toggle
option is compiled in: `grp:alt_shift_toggle` switched silently, was forgotten
on reload and collided with Alt+Shift+Tab.

Compose-key selections and the Caps Lock remaps (Control `ctrl:nocaps`, Escape
`caps:escape`, Backspace `caps:backspace`) map to standard XKB options; Compose
on Caps Lock wins over a Caps Lock remap.

## Input methods (Chinese, Japanese, Korean)

Composing input methods are input sources in the same ordered list, with ids
`ime/rime`, `ime/anthy` and `ime/hangul` (`/` never occurs in an XKB name).
They are not XKB groups: the four-group bound counts layouts only, and an
engine types on the primary (first) layout, so the XKB group index is the
active layout's position among layouts, or 0 while an engine is active
(`RichInputConfig::active_layout_index`). Settings offers an engine only when
the image ships it.

Fcitx5 provides the engines: Rime (Simplified Chinese through its
`luna_pinyin_simp` schema, set in the image's
`/usr/share/rime-data/default.custom.yaml`), Anthy and Hangul, none of which
links a UI toolkit. The compositor starts it — never XDG autostart —
as `/usr/bin/fcitx5` on its own private Wayland display, exactly as the
on-screen keyboard gets its own: the display admits only the process group the
compositor started, running as the same user, and for Fcitx5 only a process
whose `/proc/<pid>/exe` is that executable. Connections on each private display
carry an immutable `TrustedInputMethod` credential; public app sockets never
see `zwp_input_method_manager_v2` or `zwp_virtual_keyboard_manager_v1`. Its
configuration is regenerated in the runtime directory on every start (the
engine as its default, `ActiveByDefault`, every enumerate chord cleared so it
cannot take Super+Space or Ctrl+Space, and the unused addons disabled); user dictionaries stay in its ordinary data directory.

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

Text the shell inserts on the user's behalf — a pick from the emoji picker
(Super+.) — takes the same road, never synthesised key events
(`compositor::text_commit`). When the OSK or Fcitx5 owns the seat, the commit
is delivered inside that owner's session: on the text input its activation
enabled, with the session's serial, and the owner keeps the seat. When nobody
owns it, the arbiter lends the idle seat to the compositor for one commit: an
in-process connection with its own credential binds the input method, waits
for the field to activate it, commits, and destroys the input method again.
The picker holds the keyboard while it is open, so the commit waits (two
seconds at most) for the field to get the keyboard back and re-enable. A field
that declares a password, PIN or hidden/sensitive content still receives the
emoji, but it is not added to the picker's recents.

The picker opens at the field's caret the way candidate windows do: below
the cursor rectangle the field last committed with `set_cursor_rectangle`,
flipped above it at the bottom of the output, and kept on that output. A
field reports its caret only while its text input is enabled, which happens
only while an input method owns the seat; without one, or when the field
reports no caret, the picker is centred on the display with focus.

Apps receive composed text through `zwp_text_input_v3` (GTK, Qt, Chromium and
Tilo apps through winit's IME). Candidate windows are `zwp_input_popup_surface_v2`
surfaces placed directly below the field's caret rectangle, flipped above it
at the bottom of an output, and kept on that output.

X11 apps compose through XIM on their workspace's own X display. X11 host
launches get `XMODIFIERS=@im=fcitx`. Once a workspace's X server has run for
a real client for a second, the compositor asks the running Fcitx5 over the
session bus (`org.fcitx.Fcitx.Controller1.OpenX11Connection`) to serve that
display. First it refreshes the composer's `XAUTHORITY` file with every
workspace display's cookie. Each server and composer pair is asked once. The
composer's connection never keeps a server alive: the 60-second idle stop
counts clients by their peer socket inode (`sock_diag`) and excludes inodes
held by the composer's process group. When the server stops, the connection
dies with it.

Hosted development mode receives already-translated physical key events from
the host compositor, so Tilo cannot override that host keyboard. Its embedded
Wayland seat still publishes the configured Tilo map. Production DRM sessions
own raw input and apply the preference end to end.
