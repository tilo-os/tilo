# Tilo shell navigation and keyboard map

Status: implemented contract, 2026-09-01. This is the compact product map for
shell navigation. Chords Settings cannot rebind live in one table,
`crates/session/shell/src/shell/fixed_chords.rs`, rendered below under "Fixed
shell chords"; a test fails when that section and the table disagree.
Editable bindings default to what this page shows and follow the user's
Settings. Overlay-specific behavior lives in
`crates/session/shell/src/shell/overlay_host/mod.rs`.

## Containment and focus

```text
Workspace
└── Strip
    └── Window group
        └── Tab (app/document; owns keyboard focus)
```

The resting desktop exposes the current workspace and local tabs. Overview is
the only surface that expands the full hierarchy. Switching a workspace or
strip changes context; switching a window or tab changes focus within it.

## Global navigation

| Intent | Keyboard | Pointer route |
|---|---|---|
| Search current app | Ctrl+K | Search |
| Search all Tilo | Super+Space | Search |
| The Super surface: search, widgets, time, status, people | Super tapped alone (stays), Super held alone (peek) | — |
| Open/close Overview | Super+Tab | Overview button |
| Preview workspace in Overview | Up/Down | Workspace rail |
| Commit preview | Enter | Select workspace |
| Previous/next workspace | Super+Page Up/Page Down | Workspace arrows/wheel |
| Maximize/restore the focused window | Super+Up/Down (Super+M toggles) | Double-click the tab bar |
| Strip above/below | Ctrl+Super+Up/Down | Strip navigation, three-finger vertical swipe |
| Go to strip N, counting from the top (the Lounge strip is 1) | Super+1 … Super+9 | Strip navigation |
| Carry the focused group to the strip above/below | Shift+Ctrl+Super+Up/Down | Drag onto another strip |
| Cycle previous/next window | Alt+Shift+Tab / Alt+Tab | Select group/tab |
| Spatial previous/next window | Super+Left/Right | Select group/tab |
| Focus the Nth window of the strip | Ctrl+Super+1 … Ctrl+Super+9 | Select group/tab |
| Summon/dismiss workspace wheel | Super+S | — |
| Quick Settings | Super+Q | Top bar |
| Keyboard guide | F1, Super+Shift+/ | — |
| Capture Studio (choose what to capture) | Print | Capture action |
| Screenshot the whole screen now | Shift+Print | — |
| Screenshot the focused window now | Alt+Print | — |
| Dismiss top transient surface | Escape | Scrim/close action |
| Clipboard history | Super+V | Quick Settings → Clipboard |
| Notifications | Super+A | Quick Settings → Notifications |
| Power menu | Ctrl+Alt+Delete | Quick Settings power button |
| Magnifier on/off | Super+Alt+8 | Settings → Accessibility |
| Magnifier zoom in / out | Super+Alt+= / Super+Alt+- | Settings → Accessibility |
| Next keyboard layout | Super+Alt+Space | Settings → Mouse & keyboard |
| Insert an emoji into the focused text field | Super+. | — |
| Ask an agent about the focused window's selection | Super+C | "Ask agent…" in a text field, list row or context menu |

Super+C (the chord Windows gives its assistant) opens the agent quick composer on
what is selected in the focused window, set to queue a task about it. A Tilo
app answers with the selection's place in its own resource, so the agent
acts on exactly that range; any other app contributes the primary selection's
text, which travels quoted in the instruction. The composer asks the agent
asked last in the workspace, else its first live one; with no live agent the
chord says so on screen.

Shift+Print and Alt+Print skip the chooser: the capture is saved to
`Pictures/Screenshots`, copied to the clipboard and announced by the same
receipt a chooser capture leaves. Alt+Print with no focused window opens the
chooser.

Ctrl+Left/Right is deliberately never captured: text fields and editors retain
word-wise movement. Ctrl+Up/Down likewise remains application-owned. Escape is
captured only while shell chrome is open. Alt+Tab is not spatial: it walks
every window of the current workspace, most recently used first (see "Overlay
behavior"). Super+Left/Right clamps because it describes a spatial direction.

## Window and tab actions

| Intent | Keyboard | Pointer route |
|---|---|---|
| Open app as tab in current group | Super+T | Tab-strip `+` |
| Open app as adjacent window | Super+N | Global search/open |
| Open recovery terminal | Super+Enter | Global search/open Terminal |
| Open a terminal | Ctrl+Alt+T | Global search/open Terminal |
| Open Files | Super+E | Global search/open Files |
| Full screen / restore focused window | Super+F | — |
| Close focused tab | Super+W | Tab close |
| Reopen last closed tab (this workspace) | Super+Shift+T | Close toast's Undo |
| Duplicate focused tab | Super+D | Tab action |
| Maximize/restore focused window group | Super+M | Frame maximize/restore |
| Move active tab into a new group | Super+Shift+Enter | Drag tab into a gap |
| Move active tab into adjacent group | Super+[/] | Drag tab onto adjacent group |
| Move focused window group left/right in the strip | Super+Shift+Left/Right | Drag the group |
| Move focused window group to the strip above/below | Super+Shift+Up/Down | — |
| Move focused window group to the previous/next display | Super+Ctrl+Shift+Left/Right | Drag the group onto the display |
| Make focused window group narrower/wider | Super+- / Super+= | — |
| Previous/next tab in current group | Ctrl+Shift+Tab / Ctrl+Tab | Tab strip |
| Choose where the desktop appears (extend, mirror, one screen, wireless) | Super+P | Search "mirror displays" / Settings › Displays |

Super tapped alone means Super pressed and released with no other key,
click or scroll in between, before the 300 ms hold; any chord disarms it.
The tap opens the Super surface and it stays until Escape or a click
outside. Held alone that long, Super opens the same surface as a peek,
which closes when Super comes up unless something was typed or clicked
in it; a chord pressed during the hold closes the peek and runs. Super+N is already the
adjacent-window chord, so notifications take Super+A (the Windows Action
Center key). Super+Up/Down maximize and restore the focused window group on
any strip, as on Windows and GNOME; pressed again they do nothing, and
Super+M toggles. Strips are one level out, on Ctrl+Super+Up/Down, and
Super+1 … 9 go straight to a strip, counting from the top, so the shared
Lounge strip is Super+1 where a workspace has one; a number past the last
strip does nothing. Shift carries the focused group in the direction
Ctrl+Super+Up/Down moves focus; carrying past the first or last strip does
nothing, and the group lands after the destination strip's focused group.
Before 2026-10-17 Super+Up/Down switched strips (maximizing only on the top
strip), Super+Shift+Up/Down carried the group and Super+1 … 9 focused a
window; a settings file that still holds one of those defaults takes the new
one, and a chord the person chose stays. Ctrl+Super is rarely bound by apps,
so these reach the desktop without taking app shortcuts. Super+F makes a Wayland client full screen through
`xdg_toplevel`; a Tilo app, which has no protocol fullscreen, maximizes its
window group instead. Each resize step is 5% of the display's width. With
one display, the move-to-display chords do nothing; displays are ordered left
to right, then top to bottom. Super+Up/Down, Ctrl+Super+Up/Down, Super+1 … 9,
Ctrl+Super+1 … 9, Super+E, Ctrl+Alt+T, Super+V, Super+A,
Super+F, Super+Alt+8, Super+Alt+=, Super+Alt+-, Super+C and Ctrl+Alt+Delete are
defaults, rebindable in Settings → Keyboard shortcuts.

The magnifier zooms from 2× to 6×; zooming out of 2× turns it off, and
Super+Alt+8 turns it back on at the zoom it had. Each change shows the zoom
on screen. It follows the pointer, or the keyboard focus when that moved
last, so tabbing through a form keeps the focused control in view. Settings
→ Accessibility chooses between a lens and full-screen magnification.

## Vim navigation

Settings → Keyboard shortcuts → "Vim navigation (H J K L)" adds a Super+Ctrl
layer alongside the arrow bindings, which keep working:

| Intent | Keyboard |
|---|---|
| Previous/next window | Super+Ctrl+H / Super+Ctrl+L |
| Previous/next strip | Super+Ctrl+K / Super+Ctrl+J |
| Move focused window group left/right | Super+Ctrl+Shift+H / Super+Ctrl+Shift+L |

The layer is off by default and never takes a bare or Ctrl-only letter, so
terminals and editors keep H/J/K/L.

Application Ctrl shortcuts stay application-owned; shell-level window actions
use Super so a browser, terminal or editor does not lose familiar bindings.

## Fixed shell chords

These never reach a focused app, cannot be rebound in Settings and cannot be
granted to an app through the GlobalShortcuts portal.

<!-- fixed-shell-chords:start -->
| Intent | Keyboard |
|---|---|
| Screenshot | Print Screen |
| Switch windows | Alt+Tab |
| Switch windows backwards | Alt+Shift+Tab |
| Switch windows of the app | Alt+` |
| Close tab | Alt+F4  /  Super+W |
| Change workspace | Super+Page Up / Down |
| New window | Super+N |
| Add tab here | Super+T |
| Duplicate tab | Super+D |
| Open terminal | Super+Enter |
| Workspace wheel | Super+S |
| Display layout | Super+P |
| Capture screenshot | Super+Shift+S |
| Picture in picture | Super+Shift+P |
| Save recent replay | Super+Shift+R |
| Workspace history | Super+H |
| Explore workspace history | Super+Shift+H |
| Keyboard guide | F1 |
| Stop all remote input | Super+Esc |
| Toggle mouse keys | Alt+Shift+Num Lock |
<!-- fixed-shell-chords:end -->

## In the browser

The browser desktop takes the Super chords above where the browser passes
Super through, and the same keys with a stand-in no browser or system binds:
Alt+Shift, or Ctrl+Option on a Mac. Search stays on Ctrl+K (Cmd+K on a Mac).

| Intent | Keyboard |
|---|---|
| Search all Tilo | Ctrl+K / Cmd+K |
| Open/close Overview | Alt+Shift+O, Super+Tab |
| Previous/next window | Alt+Shift+Left/Right |
| Previous/next strip | Alt+Shift+Up/Down |
| Previous/next workspace | Alt+Shift+Page Up/Page Down |
| Focus the Nth window of the strip | Alt+Shift+1 … Alt+Shift+9 |

## Overlay behavior

- Quick Settings root: Tab/Shift+Tab moves through every enabled control,
  Enter/Space activates it, and Left/Right adjusts a focused slider.
- Window switcher (Alt+Tab): windows of the current workspace, most recently
  used first. Hold Alt and press Tab (Shift+Tab, the arrows) to move; release
  Alt to switch, Escape to stay. A quick Alt+Tab goes straight back to the
  previous window without showing anything; the switcher appears once Alt has
  been held for 120 ms. Hovering a window selects it, clicking or tapping one
  switches to it. Alt+` does the same across the focused app's windows only.
- Overview: Up/Down changes only the preview; Enter commits and closes.
- Display layout (Super+P): Super+P again moves the highlight to the next
  layout the attached displays allow; Up/Down walk the layouts, Keep/Revert
  and the wireless receivers; Enter applies. A new layout applies at once and
  restores itself after 15 seconds unless Keep is chosen. 1–4 choose a layout
  directly.
- Workspace rename and Wi-Fi password entry consume editing keys before global
  chords. Password characters can never accidentally open Search.
- Escape follows visual stacking order and closes only the top transient
  surface. It does not leak through to the app underneath.

## Acceptance rules

- The active tab is the sole recipient of ordinary keyboard input.
- Opening modal shell chrome removes keyboard focus from hosted clients.
- Every pointer-only global route has a keyboard route, except direct spatial
  manipulation such as dragging tabs or strips.
- Focus never changes merely because Overview selection is previewed.
- Shortcut labels shown in UI must use this table rather than declaring a
  second binding locally.
