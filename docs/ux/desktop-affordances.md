# Desktop affordances: focused-tab operations

Audit base: `b5b1ae23c4` (after the shell redesign and keyboard overview fix).
This is a source-and-production-test audit, not a claim that every input device
or every app has been exercised.

| Purpose / journey | Evidence at the audit base | First-slice decision |
|---|---|---|
| Find and focus a window, including an already-selected tab | Shell strip navigation and native accessibility activation exist; spatial opt-in and focus reconciliation are owned by PR #2913 | Preserve that work |
| Select several files, open their menu without a pointer, reveal a search hit, undo a file operation | Files has selection-aware menu construction, Context Menu keyboard handling, Reveal and framework-history journeys (`files_app/tests.rs`, `framework_history_tests.rs`, `search_provider.rs`) | These are not missing wholesale; no duplicate implementation |
| Discover what can be done with the focused shell tab without knowing individual chords or positioning a pointer | `TabBar` only queues a menu on right-click; shell harvests those requests. Menu arrows, accessible rows, Close / Split Right / Move to New Group already exist | Implement a keyboard and published-action entrance |
| Carry resources between apps and share them with collaborators | Apps publish typed children and effects; Files has scoped grant/revoke menus. Chat #2912 and Contacts #2910 own their resource changes | Cross-app journey audit is a follow-up, not permission to invent a second sharing protocol |
| Touch text entry | Existing OSK work has a separate owner | No OSK edits |

## Before and after

Before: with a tab focused, discover its shell operations by moving to its pill
and right-clicking, or find each operation separately in search. The menu's
keyboard support starts only after that pointer gesture.

After: press Super+F10 (rebindable in Settings and listed in the shortcut guide)
or invoke `actions` / `tab.menu`. The same shell menu opens beside the focused
tab, independently of the pointer, clamped to the keyboard-focused display.
A collapsed pill uses its real tab bar until the next layout expands it.
The first row is already focused. Down and
Enter split a multi-tab group through the existing dispatcher; Escape changes
no windows. A held opening chord keeps the current row. Single-tab groups only
offer Close. Locked or removed tabs cannot open a stale menu. App Context Menu
and Shift+F10 remain available for the app's own resource operations.

The menu uses the shared widget and live theme, including high contrast. No new
assets, borrowed reference implementation, dependency or licence are introduced:
this is original repository code under its existing AGPL-3.0 licence. Work per
opening is bounded by the focused strip/group/tab lookup; no preview renderer,
background polling or continuous frame demand is added.

## Reproduction and evidence

All Cargo compilation and execution use the approved remote rcargo runner:

```sh
rcargo test --locked -p shell --lib tab_context_menu -j 4
rcargo test --locked -p shell --lib tab_menu_ -j 4
rcargo test --locked -p settings-store --lib shortcuts -j 4
rcargo check --locked -p shell --tests -j 4
rcargo --on-builder --fetch ux-tab-menu-dark run --locked -p shell --bin shots -j 4 -- --mode offscreen --scenario tab_menu_keyboard --out target/ux-tab-menu-dark
```

Use `RCARGO_ENV_TILO_SHOTS_THEME=light|dark|high-contrast` for the three material/contrast
variants. The scenario emits desktop, focused-menu, next-row and cancelled
frames. These are production offscreen frames, never the user's desktop.
Actual results and any unrun checks belong in the PR description. Native screen
reader, real touch/keyboard hardware and older-machine latency remain device
validation, not inferred passes from these tests.
