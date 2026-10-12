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
| Cancel an accidental tab drag before releasing the pointer | A real pointer/key journey at `90add8ad64` starts a drag, then Escape is unhandled; releasing can still commit the drop | Next slice: cancel the shared pending gesture and expose the same cancellation action |
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

Use plain Cargo or any Cargo drop-in appropriate to the build environment:

```sh
cargo test --locked -p shell --lib tab_context_menu -j 4
cargo test --locked -p shell --lib tab_menu_ -j 4
cargo test --locked -p settings-store --lib shortcuts -j 4
cargo check --locked -p shell --tests -j 4
cargo run --locked -p shell --bin shots -j 4 -- --mode offscreen --scenario tab_menu_keyboard --out target/ux-tab-menu-dark
```

Use `TILO_SHOTS_THEME=light|dark|high-contrast` for the three material/contrast
variants; a remote Cargo drop-in must forward that environment variable to its
runner. The scenario emits desktop, focused-menu, next-row and cancelled
frames. These are production offscreen frames, never the user's desktop.
Actual results and any unrun checks belong in the PR description. Native screen
reader, real touch/keyboard hardware and older-machine latency remain device
validation, not inferred passes from these tests.


## Cancelling a tab drag

With a tab lifted, press Escape to abandon its pending drop immediately. The
later pointer release cannot move or detach it. `actions` / `tab.drag.cancel`
requests the same host cancellation for automation and assistive technology;
locked sessions refuse that request. With no pending tab gesture, Escape keeps
its existing menu/dialog/application meaning.

Cancellation clears the existing shared tab bar gesture and the shell's
in-strip/cross-strip feedback. It preserves every live tab host and does not
write the replicated window topology. A carried handle remains visible even
when an inactive app has not been laid out: the shared tab ghost is painted
without forcing the app to render a cold preview. The carried handle uses the
shared themed surface, and forced-colour themes keep its contrast opaque. This slice does not change group
reordering, resize gestures or application-owned drags.

The regression journey uses actual pointer press/move, Escape and release,
asserts that a drag started, then checks the live widget and durable tree.
Run `cargo test --locked -p shell --lib tab_drag_ -j 4` and the existing
`cargo test --locked -p shell --test it cross_strip_drag:: -j 4` regression.
The `tab_drag_cancel` offscreen scenario records before, lifted, cancelled and
released frames. Physical touch, keyboard and screen-reader qualification
remain separate device checks.

## Published file selection

Files now publishes `select-entries`, `select-all`, `clear-selection`,
`copy-selection` and `cut-selection` on its folder resource. A caller can select
the current row IDs, sort, copy the batch, clear its selection, navigate and
paste through the same handlers as the keyboard and pointer. Explicit batches
are limited to 256 IDs and validated whole before selection changes; names,
paths, missing rows and oversized batches are refused. Answers carry a bounded
list of stable row IDs, count and primary row, never row numbers.

Selection remains local to the addressed window and changes no shared document.
View/Copy retain Read permission and Cut retains Write. Empty Copy/Cut are
unavailable and refuse without overwriting the clipboard. Production journeys
verify exact copied bytes, sorting, atomic refusals, two-window selection
isolation, Viewer refusal of Cut and one undo removing the whole copied batch
while retaining its originals. Generated evidence belongs on the PR.

Cross-window file transfer is not implemented by these selection actions. The
[local file clipboard boundary](../architecture/file-clipboard.md) records the
observed raw-path replacement problem and the proposed host-owned extension
that requires architectural review before implementation.

## Recent files through the resource door

At baseline `12f4dfb2d5`, Recent's pointer menu could open a file's containing
folder or forget its recent-use entry, but its published file rows exposed
neither action. The resource journey failed at the missing `reveal` action.
This made the same everyday task unavailable to automation and assistive
technology using the app's public surface.

Recent rows now publish localized **Open containing folder** (`reveal`) and
**Remove from Recent** (`forget-recent`). Invoke the first to open the parent
folder with the exact file selected, then Back to return to Recent. Invoke the
second to remove only that device's recent-use entry; the file and other Recent
items remain intact, including after reopening the window. Both use the same
commands as the existing pointer menu. They are view operations: a Viewer can
use them, while Denied access is refused through the framework's existing door.

The effects resolve an opaque published row in the current Recent view. They
refuse retained requests in an ordinary folder, deleted targets, withdrawn
rows and caller attempts to substitute a path. The existing collaborative
record reveal outside Recent keeps its host and permission checks. No new
clipboard transport, permission grant, shared history mutation, asset or
dependency is introduced. The code is original work under AGPL-3.0-only.

```sh
cargo test --locked -p file-explorer --lib recent_resource_tests -j 4
cargo test --locked -p file-explorer --lib open_with_tests -j 4
cargo check --locked -p file-explorer --all-targets -j 4
cargo check --locked -p shell --tests -j 4
cargo run --locked -p file-explorer -j 4 -- --shots target/recent.png --scenario recent --theme dark
```

Capture `recent`, `recent-menu`, `recent-revealed` and `recent-forgotten` in
dark, light and high-contrast themes. Each uses a fresh scratch home, actual
Recent settings and production command handlers. Generated frames and logs
belong in PR/CI evidence, never source control. These captures and simulated
roles do not qualify physical touch, a native screen reader or older-hardware
latency.

## Keeping file selection through sorting

At baseline `1ab90c8c0f`, sorting Files left the selected row numbers in place.
Selecting `a.txt` and `c.txt`, then invoking `sort-size`, silently selected
`b.txt` and `a.txt` instead. A subsequent Copy used those wrong files.

Sort columns, the view menu and published sort actions now preserve the same
files, the keyboard cursor and the Shift range anchor. The cursor still follows
an item deselected by Ctrl-click. The next Shift gesture extends from that
item's new row. A click remembered before sorting cannot turn a click on a
different file into a double-click and open it.
An active inline rename follows its original file as well, keeping the draft
name and editor focus. At the intermediate candidate `f32dfec47a`, the rename
regression still renamed `b.txt` while editing `a.txt` after sorting by size;
the final regression checks the renamed file's exact bytes and leaves `b.txt`
untouched.

The implementation uses the shared selection model and remaps only its tracked
items against the existing directory snapshot. It adds linear listing work and
storage proportional to the selection, without reading file contents or changing
shared state, permissions, or the clipboard transport. Sorting does not rename
or replace entries; metadata refresh continues using its existing record-aware
identity handling.

The production regression copies a multiselection into a scratch destination
after invoking the published sort action and checks the exact file bytes and
absence of the unselected file. Additional journeys exercise every published
sort, Shift extension, a deselected cursor, an empty selection, active rename and click/open
continuity. Run:

```sh
cargo test --locked -p file-explorer --lib sort_selection_tests -j 4
cargo test --locked -p tilo-ui --lib remapping_selection -j 4
cargo check --locked -p shell --tests -j 4
```

The `selection-sort`, `selection-sorted` and `selection-reversed` Files capture
scenarios show the same Notes and Recipes files selected before sorting, after
sorting by size, and after reversing that order. Capture each with the existing
`--shots` runner in dark, light and high-contrast themes. They use the prepared
scratch home. Physical assistive technology and older-hardware performance
remain separate device checks.
