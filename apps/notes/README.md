# Notes

Notes is Tilo's native collaborative note-taking app. Notes live in a nested
side tree, and each note contains editable paragraphs, headings, lists,
checklists, quotes, code, formulas, tables, images and dividers. Inline marks
include bold, italic, underline, strikethrough, code, external links and
`[[Note]]` links. A standalone `![[Note]]` becomes a live, one-level embed; its
stable target survives renames and bounded previews keep cyclic embeds safe.

Created and last-edited dates sit below the title, with the note's tags and the
notes that link to it as buttons. The transient status bubble reports save
state and word count without taking permanent editor space, and a note that
cannot be saved says so under its title until it can. The
layout fills the available view, keeps the note header visible while the body
scrolls, and collapses the note tree into a drawer at narrow widths.

The editor supports multiline blocks, mixed nested lists and cross-block
selection. A block's drag handle appears while its text has the caret or when
the pointer hovers it. Dragging previews the destination; `Alt-Up` and
`Alt-Down` provide the same move from the keyboard. Right-click a note, press
the Menu key, or press `Shift-F10` for note actions. Hover or focus a sidebar
row to reveal its quick child-note action.

Fenced code uses grammar-backed, theme-derived syntax colours and a copy
control. Type three backticks followed by a language, such as `python`, then
press Enter to create a code block immediately. `Ctrl-Enter` exits to a
paragraph after the fence, while `Ctrl-Shift-Enter` inserts one before it.
Tables retain editable GFM source and also offer a native grid. Formula fences
offer source and visual editing for TeX and Typst.

Workspace-relative Markdown images decode off-thread. **Insert image** copies a
portal-selected PNG, JPEG or WebP into managed assets. Markdown import and
export preserve managed images with portable references and refuse unsafe
paths. These operations, note navigation, editing, tags and block operations
are all published as resources and actions for the platform palette,
accessibility tools, scripts and agents.

## Running it

```bash
cargo run -p notes --release
```

The binary opens the host-owned default notebook. A new notebook starts empty
and presents **New note**. The shared record host supplies persistence,
history, undo and collaboration; the status bubble shows **Saving...** while a
write is queued and **Saved** once it lands.

On a standalone personal launch, Notes imports an existing legacy notebook
from `~/notes/` once when no canonical notebook exists. Set `TILO_NOTES_ROOT`
to an absolute directory to select another legacy import source. Empty and
relative values are rejected. Passing one Markdown file on the command line
imports it into the opened notebook.

For headless visual review, render the production component offscreen:

```bash
cargo run -p notes --release -- --shots out.png --viewport 800x600
# add --scenario drawer for the narrow note drawer
# add --text-scale 1.5 to inspect the large-text layout
# add --scenario shortcuts for the keyboard reference
```

## Canonical and legacy storage

The current app stores one host-owned `notes.notebook` Automerge record. Notes,
blocks, tags, note memberships and managed assets share that document, so
platform history, undo and collaboration observe the same operations.

Older standalone builds used this recoverable layout:

```text
~/notes/
|-- workspace.automerge
|-- pages/
|   `-- {uuid}.md
`-- assets/
    `-- {uuid}.{png,jpg,webp}
```

The app can migrate that layout into the canonical record. It leaves the
legacy files untouched after import.

## Block-type reference

| Block | Markdown syntax |
| --- | --- |
| Paragraph | anything else |
| Heading 1 / 2 / 3 | `# heading` / `## heading` / `### heading` |
| Quote | `> quotation` |
| Bullet | `- item` (two leading spaces per nesting level) |
| Numbered | `1. item` (ordinals render automatically) |
| Checklist | `- [ ] open` / `- [x] done` |
| Code block | three backticks plus an optional language |
| TeX formula | fenced `tex`, `latex` or `math` source |
| Typst formula | fenced `typst` or `typst-math` source |
| Table | GFM pipe table |
| Image | `![alternative text](relative/path.png)` |
| Divider | a line containing exactly `---` |

Formula cards expose a **Source / Visual** switch. Visual edits serialize back
to canonical TeX or Typst; unsupported source remains visible and produces a
diagnostic instead of being discarded.

Inline marks inside non-code blocks:

| Mark | Markdown |
| --- | --- |
| Bold | `**bold**` |
| Italic | `*italic*` |
| Inline code | `` `code` `` |
| Underline | `++underline++` |
| Strike | `~~strikethrough~~` |
| Web link | `[label](https://example.com)` |
| Note link | `[[Note Title]]` |
| Note embed | `![[Note Title]]` |

## Keyboard shortcuts

| Area | Action | Shortcut |
| --- | --- | --- |
| Platform | Search notes and actions | `Cmd-K` / `Ctrl-K` |
| Navigation | Back / forward note | `Alt-Left` / `Alt-Right` |
| Navigation | Move sidebar selection | `Up` / `Down` |
| Navigation | Collapse / expand branch | `Left` / `Right` |
| Navigation | Open shortcut reference | `?` |
| Notes | New child of selected note | `Cmd-N` / `Ctrl-N` |
| Notes | Rename selected note | double-click / `F2` |
| Notes | Open note actions | Menu / `Shift-F10` |
| Notes | Indent / outdent note | `Tab` / `Shift-Tab` |
| Notes | Reorder among siblings | `Alt-Up` / `Alt-Down` |
| Notes | Delete selected note | `Delete` |
| Editing | Find in note | `Cmd-F` / `Ctrl-F` |
| Editing | Continue or split a block | `Enter` |
| Editing | Indent / outdent list item | `Tab` / `Shift-Tab` |
| Editing | Move focused block | `Alt-Up` / `Alt-Down` |
| Editing | Open block menu | `/` |
| Editing | Link / embed another note | `[[` / `![[` |
| Code | Exit after the code block | `Ctrl-Enter` |
| Code | Insert a paragraph before | `Ctrl-Shift-Enter` |
| Code | Insert a blank code line | `Shift-Enter` |
| Code | Indent / outdent code | `Tab` / `Shift-Tab` |
| Code | Copy the code block | `Ctrl-Shift-C` |
| Formatting | Bold / italic / inline code | `Ctrl-B` / `Ctrl-I` / `Ctrl-E` |

## Current boundaries

- One person can select across blocks, but the editor does not expose multiple
  independent local carets. Remote collaborator carets are still visible.
- Comments are not implemented yet. Notes, folders, tags, blocks, links and
  inline formatting are durable collaborative resources and actions.

## Architecture

- `notebook.rs`: canonical Automerge schema and validation.
- `actions.rs` / `doc.rs`: canonical note, block, tag and inline-mark actions.
- `main_panel.rs`: title, metadata, scrolling and the shared `BlockEditor`.
- `sidebar.rs`: responsive note tree, hover actions, context menus and
  drag/reparent behavior.
- `app.rs` / `hosting.rs` / `component.rs`: component lifecycle, published
  resources/actions, record binding and local presentation state.
- `markdown.rs`: block-list and Markdown conversion.
- `model.rs` / `storage.rs` / `legacy_import.rs`: the legacy standalone format
  and its import, which now only seed test and capture fixtures; a launch
  never reads it.
- `capture.rs`: semantic production-component fixtures for visual review.
