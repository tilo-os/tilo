# Tilo shell navigation and keyboard map

Status: implemented contract, 2026-09-01. This is the compact product map for
shell navigation. The executable chord ownership table remains in
`crates/session/shell/src/compositor/client_input.rs`; overlay-specific behavior lives in
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
| Search all Tilo | Super+K, Super+Space; Alt+Space when nested | Search |
| Open/close Overview | Super+Tab | Overview button |
| Preview workspace in Overview | Up/Down | Workspace rail |
| Commit preview | Enter | Select workspace |
| Previous/next workspace | Super+Page Up/Page Down | Workspace arrows/wheel |
| Previous/next strip | Super+Shift+Up/Down | Strip navigation |
| Cycle previous/next window | Alt+Shift+Tab / Alt+Tab | Select group/tab |
| Spatial previous/next window | Super+Left/Right | Select group/tab |
| Summon/dismiss workspace wheel | Super+S | — |
| Dismiss top transient surface | Escape | Scrim/close action |

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
| Duplicate focused tab | Super+D | Tab action |
| Maximize/restore focused window group | Super+Up | Frame maximize/restore |
| Move active tab into a new group | Super+Shift+Enter | Drag tab into a gap |
| Move active tab into adjacent group | Super+[/] | Drag tab onto adjacent group |
| Previous/next tab in current group | Ctrl+Shift+Tab / Ctrl+Tab | Tab strip |
| Open wireless display picker | Super+P | Quick Settings / display route |

Application Ctrl shortcuts stay application-owned; shell-level window actions
use Super so a browser, terminal or editor does not lose familiar bindings.

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
