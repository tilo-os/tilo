# qOS shell navigation and keyboard map

Status: implemented contract, 2026-09-01. This is the compact product map for
shell navigation. The executable chord ownership table remains in
`shell/src/compositor/client_input.rs`; overlay-specific behavior lives in
`shell/src/shell/overlay_host.rs`.

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
| Search all qOS | Super+K, Super+Space; Alt+Space when nested | Search |
| Open/close Overview | Ctrl+Tab or Super+Tab | Overview button |
| Preview workspace in Overview | Up/Down | Workspace rail |
| Commit preview | Enter | Select workspace |
| Previous/next workspace | Ctrl+Up/Down or Super+Up/Down | Workspace arrows/wheel |
| Previous/next strip | Ctrl+Shift+Up/Down or Super+Shift+Up/Down | Strip navigation |
| Previous/next window group | Super+Left/Right | Select group |
| Summon/dismiss workspace wheel | Super+S | — |
| Dismiss top transient surface | Escape | Scrim/close action |

Ctrl+Left/Right is deliberately never captured: text fields and editors retain
word-wise movement. Escape is captured only while shell chrome is open.

## Window and tab actions

| Intent | Keyboard | Pointer route |
|---|---|---|
| Open app as tab in current group | Super+T | Tab-strip `+` |
| Open app as adjacent window | Super+N | Global search/open |
| Open recovery terminal | Super+Enter | Global search/open Terminal |
| Close focused tab | Super+W | Tab close |
| Duplicate focused tab | Super+D | Tab action |
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
