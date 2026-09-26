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
| Search all Tilo | Super+K, Super+Space | Search |
| Open/close Overview | Super+Tab | Overview button |
| Preview workspace in Overview | Up/Down | Workspace rail |
| Commit preview | Enter | Select workspace |
| Previous/next workspace | Super+Page Up/Page Down | Workspace arrows/wheel |
| Previous/next strip | Super+Shift+Up/Down | Strip navigation |
| Cycle previous/next window | Alt+Shift+Tab / Alt+Tab | Select group/tab |
| Spatial previous/next window | Super+Left/Right | Select group/tab |
| Summon/dismiss workspace wheel | Super+S | — |
| Capture Studio (choose what to capture) | Print | Capture action |
| Screenshot the whole screen now | Shift+Print | — |
| Screenshot the focused window now | Alt+Print | — |
| Dismiss top transient surface | Escape | Scrim/close action |

Shift+Print and Alt+Print skip the chooser: the capture is saved to
`Pictures/Screenshots`, copied to the clipboard and announced by the same
receipt a chooser capture leaves. Alt+Print with no focused window opens the
chooser.

Ctrl+Left/Right is deliberately never captured: text fields and editors retain
word-wise movement. Ctrl+Up/Down likewise remains application-owned. Escape is
captured only while shell chrome is open. Alt+Tab wraps at the strip edges;
Super+Left/Right clamps because it describes a spatial direction.

## Window and tab actions

| Intent | Keyboard | Pointer route |
|---|---|---|
| Open app as tab in current group | Super+T | Tab-strip `+` |
| Open app as adjacent window | Super+N | Global search/open |
| Open recovery terminal | Super+Enter | Global search/open Terminal |
| Close focused tab | Super+W | Tab close |
| Reopen last closed tab (this workspace) | Super+Shift+T | Close toast's Undo |
| Duplicate focused tab | Super+D | Tab action |
| Maximize/restore focused window group | Super+Up | Frame maximize/restore |
| Move active tab into a new group | Super+Shift+Enter | Drag tab into a gap |
| Move active tab into adjacent group | Super+[/] | Drag tab onto adjacent group |
| Previous/next tab in current group | Ctrl+Shift+Tab / Ctrl+Tab | Tab strip |
| Open wireless display picker | Super+P | Quick Settings / display route |

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
| Close tab | Alt+F4  /  Super+W |
| Change workspace | Super+Page Up / Down |
| New window | Super+N |
| Add tab here | Super+T |
| Duplicate tab | Super+D |
| Open terminal | Super+Enter |
| Global search | Super+Space |
| Lock | Super+L |
| Workspace wheel | Super+S |
| Wireless display | Super+P |
| Capture screenshot | Super+Shift+S |
| Picture in picture | Super+Shift+P |
| Save recent replay | Super+Shift+R |
| Workspace history | Super+H |
| Explore workspace history | Super+Shift+H |
| Keyboard guide | F1 |
| Stop all remote input | Super+Esc |
<!-- fixed-shell-chords:end -->

## Overlay behavior

- Quick Settings root: Tab/Shift+Tab moves through every enabled control,
  Enter/Space activates it, and Left/Right adjusts a focused slider.
- Overview: Up/Down changes only the preview; Enter commits and closes.
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
