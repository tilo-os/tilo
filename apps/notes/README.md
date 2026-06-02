# notes

A native, CRDT-ready note-taking app for qOS. Pages live in a side-tree;
each page is a stack of editable blocks (paragraphs, headings, bullets,
checklists, code, dividers) with inline marks for bold, italic, code,
and `[[Page]]` links. Cmd-K jumps to any page. The bottom status bar
tells you whether your edits are saved and how many words live on the
current page.

This is the v0.1 release: feature-complete for solo daily-driver use,
with the collaboration / sync layer deferred behind the same UI surface.

## Running it

```bash
cargo run -p notes --release
```

The binary opens a window, restores the workspace from
`~/notes/` (creating a seed "Welcome" page on first launch), and runs
until you close it. The background saver flushes every change with a
500 ms debounce; the status bar shows "Saving…" while a write is
queued and "Saved" once it lands. There is no `--shots` mode yet —
visual review goes through the offscreen smoke test, which writes a
PNG when `NOTES_SNAPSHOT=path` is set.

## Storage layout

Everything lives under one workspace root (default `~/notes/`):

```
~/notes/
├── workspace.automerge        # automerge save of WorkspaceData (page tree, selection)
└── pages/
    └── {uuid}.md              # one markdown body per page
```

The workspace file holds the JSON-encoded `WorkspaceData` inside an
automerge `Text` field — picked for forward-compat with a per-field
automerge migration in a future phase. Page bodies are plain markdown:
crack one open with any editor and you'll get your work back even if
the notes binary disappears.

## Block-type reference

| Block       | Markdown syntax                                |
| ----------- | ---------------------------------------------- |
| Paragraph   | anything else                                  |
| Heading 1   | `# heading`                                    |
| Heading 2   | `## heading`                                   |
| Heading 3   | `### heading`                                  |
| Bullet      | `- item`  (indent with two leading spaces)     |
| Checklist   | `- [ ] open` / `- [x] done`                    |
| Code block  | ` ```lang ` … ` ``` ` (single-line body, v0.1) |
| Divider     | a line of exactly `---`                        |

Inline marks inside any non-code block:

| Mark        | Markdown                |
| ----------- | ----------------------- |
| Bold        | `**bold**`              |
| Italic      | `*italic*`              |
| Inline code | `` `code` ``            |
| Page link   | `[[Page Title]]`        |

## Keyboard shortcuts

### Navigation
| Action                       | Shortcut             |
| ---------------------------- | -------------------- |
| Jump to page (palette)       | `Cmd-K` / `Ctrl-K`   |
| Move sidebar selection       | `↑` / `↓`            |
| Open shortcuts overlay       | `?`                  |
| Close overlay / popover      | `Esc`                |

### Pages
| Action                       | Shortcut             |
| ---------------------------- | -------------------- |
| New child of selected page   | `Cmd-N` / `Ctrl-N`   |
| Rename selected page         | double-click / `F2`  |
| Delete selected page         | `Delete`             |

### Editing
| Action                       | Shortcut             |
| ---------------------------- | -------------------- |
| New paragraph                | `Enter`              |
| Indent / outdent list item   | `Tab` / `Shift-Tab`  |
| Open block menu              | `/`                  |
| Insert page link             | `[[`                 |
| Toggle checklist item        | click the checkbox   |

### Formatting (inline marks)
| Action                       | Shortcut             |
| ---------------------------- | -------------------- |
| Bold                         | `**bold**` / `Ctrl-B`|
| Italic                       | `*italic*` / `Ctrl-I`|
| Inline code                  | `` `code` `` / `Ctrl-E`|
| Heading 1 / 2 / 3            | `# ` / `## ` / `### ` at line start |

## Known limitations (v0.1)

The shape of the app is right; these are sharp edges that will get
filed down in follow-up phases.

- **Code blocks are single-line.** The fenced code block parses and
  serialises one body line per block. Newlines inside the block are
  rendered as literal `\n` characters.
- **Code-block text uses the proportional UI font.** Inline `code`
  chips render in the canonical monospace family; full-block code is
  still on the proportional font until a per-block font override
  lands.
- **Link text doesn't track renames.** A `[[Welcome]]` link points at
  the page id resolved at the time the block was parsed. If you
  rename the target page, the link still navigates correctly but the
  visible text in the source page stays the old title until you
  manually re-type it.
- **Inline-mark toggles aren't on the CRDT undo path.** `Ctrl-B` /
  `Ctrl-I` / `Ctrl-E` toggle marks immediately; undo only walks the
  text edits, not the mark transitions.
- **Popover anchoring is line-bottom, not caret-pixel.** The link
  popover and slash menu open at the bottom-left of the current line
  rather than directly under the caret. Reads as "close enough" for
  v0.1 but a polish target.
- **Link click hit-test uses a hardcoded theme.** The byte→pixel
  reverse lookup for click-on-link reads `Theme::dark()` to resolve
  text padding. Harmless in practice (the editor draws in
  plain-chrome mode where padding is zero) but a TODO when light
  theme lands.
- **No collaboration yet.** The app is single-user. The UI toolkit
  has the CRDT-native plumbing wired all the way through; a peer
  layer is the next phase.
- **No multi-cursor / multi-selection inside blocks.** One caret per
  line; the editor selects by paragraph, not by a free-form range.
- **No drag-and-drop reordering of pages or blocks.** Pages get
  reordered by parent + creation order; blocks reorder by editing
  the markdown.

## Architecture

- `model.rs` — pure data: `WorkspaceData`, `PageMeta`, `PageId`.
- `storage.rs` — disk IO + the two debounced savers (workspace +
  per-page body). Each saver publishes a `SaveStatus` atomic the
  status bar reads.
- `markdown.rs` — block-list ⇄ markdown round-trip; the byte-stable
  format the storage layer relies on.
- `sidebar.rs` — `NoteTreeView`; flattens the workspace into a
  scrollable row list with inline rename + keyboard shortcuts.
- `main_panel.rs` — `MainPanel`; hosts the `BlockEditor` from `ui`
  plus the page title.
- `status_bar.rs` — `StatusBar`; bottom strip with save indicator,
  page path, page count + word count.
- `shortcuts_overlay.rs` — `ShortcutsOverlay`; fullscreen modal
  listing every shortcut.
- `app.rs` — `NotesApp`; wires it all together, owns the workspace
  and the Cmd-K palette, drives the savers.
