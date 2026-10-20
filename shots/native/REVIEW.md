# Native review of the tilolang apps

Every shot comes from the framework's offscreen `--shots` path: no window was opened.

    TILO_PACKAGE_ID=<app> ./target/debug/tilolang --shots OUT --scenario "<scenario>" --viewport WxH --text-scale S
    ./target/debug/{notes,snippets,contacts,totp} --shots OUT --viewport WxH --text-scale S

Viewports are 420x860, 1280x800 and 2560x1440, each at text scale 1 and 1.5. `tl-<app>-*` is a
tilolang app; `native-<app>-*` is the native app it is compared with; `tl-<app>-empty-*` is the
app with no scenario.

| tilolang app | scenario | compared with |
|---|---|---|
| notes | a week of notes | native-notes, native-snippets |
| todo | a saturday list | native-snippets, native-totp |
| letters | a desk of letters | native-notes, native-contacts |
| chat | a busy workspace | native-contacts |
| counter | a busy morning | native-totp |
| board | a morning board | native-notes (canvas page) |

## Checklist

- Header row: one row, title, count badge, controls, icon actions, More, primary last, history.
- Sidebar: platform width, toggle in the header, collapses before the content is cramped.
- Rows: native ListRow heights, separators, selection lift, one menu with every action.
- One content edge: the page column, nothing app-drawn around it.
- Empty state: one sentence, at most one action.
- No app-drawn title, no hand-made chrome.

## Shots and findings

### notes
- `tl-notes-420x860-x1.png`, `tl-notes-420x860-x1.5.png`: the scenario's note opens pushed, with
  Back, the pin and delete icons and history in the header. The title is edited in place as a
  large title and wraps; the header shows no second title. Pass.
- `tl-notes-1280x800-x1.png`, `tl-notes-1280x800-x1.5.png`: filter sidebar (All notes, Pinned with
  counts), list with sort, search and New note last, detail column. At 1.5 the filter sidebar
  collapses behind its toggle; native Notes keeps its tree. Pass, see differences.
- `tl-notes-2560x1440-x1.png`, `tl-notes-2560x1440-x1.5.png`: three columns, the document on the
  centred page column as in native Notes. Pass.
- `tl-notes-empty-1280x800-x1.png`: "No notes yet" with one New note action; detail reads "No note
  selected". Pass.

### todo
- `tl-todo-420x860-x1.png`, `tl-todo-420x860-x1.5.png`: the returned todo pushed; the long title
  wraps; Done is a switch; mark-done is an icon action in the header. Pass.
- `tl-todo-1280x800-x1.png`, `tl-todo-1280x800-x1.5.png`, `tl-todo-2560x1440-x1.png`,
  `tl-todo-2560x1440-x1.5.png`: list and detail columns, rows a separator apart, the selected row
  lifted. Pass.
- `tl-todo-empty-1280x800-x1.png`: one sentence, one action. Pass.

### letters
- `tl-letters-420x860-x1.png`, `tl-letters-420x860-x1.5.png`: the boiler letter pushed; recipient,
  title in place, body, Send in the content (so not repeated in the header); delete and history in
  the header. Pass.
- `tl-letters-1280x800-x1.png`, `tl-letters-1280x800-x1.5.png`, `tl-letters-2560x1440-x1.png`,
  `tl-letters-2560x1440-x1.5.png`: Drafts/Sent sidebar with counts, two-line rows (subject,
  recipient), New letter last. At 1280x1.5 the sidebar collapses and the list shows its scrollbar.
  Pass.
- `tl-letters-empty-1280x800-x1.png`: one sentence, one action. Pass.

### chat
- `tl-chat-420x860-x1.png`, `tl-chat-420x860-x1.5.png`: the conversation pushed with its title in
  the header (the title is not edited in place, so the header carries it), messages with avatar and
  author, the composer as an input and Send. Pass.
- `tl-chat-1280x800-x1.png`, `tl-chat-1280x800-x1.5.png`, `tl-chat-2560x1440-x1.png`,
  `tl-chat-2560x1440-x1.5.png`: conversation list with message counts as trailing detail. Every
  message reads "You" and the other conversations read "0 messages": the scenario can only post as
  its invoker and fills one conversation. Pass for the host; scenario limit noted below.
- `tl-chat-empty-1280x800-x1.png`: one sentence, one action. Pass.

### counter
- `tl-counter-420x860-x1.png`, `tl-counter-420x860-x1.5.png`, `tl-counter-1280x800-x1.png`,
  `tl-counter-1280x800-x1.5.png`, `tl-counter-2560x1440-x1.png`, `tl-counter-2560x1440-x1.5.png`:
  title-only header with search and history, the page column holding the count and Decrement /
  Increment (primary) buttons with icons. Pass.

### board
- `tl-board-420x860-x1.png`, `tl-board-420x860-x1.5.png`, `tl-board-1280x800-x1.png`,
  `tl-board-1280x800-x1.5.png`, `tl-board-2560x1440-x1.png`, `tl-board-2560x1440-x1.5.png`: a
  canvas page: the Morning cleanup flow above three cards (Saturday todos with checkboxes, Notes,
  Chats as chips) that stack at 420. Pass; Morning cleanup has no icon (see differences).

### native comparisons
- `native-notes-{420x860,1280x800,2560x1440}-x{1,1.5}.png`
- `native-snippets-{420x860,1280x800,2560x1440}-x{1,1.5}.png`
- `native-contacts-{420x860,1280x800,2560x1440}-x{1,1.5}.png`
- `native-totp-{420x860,1280x800,2560x1440}-x{1,1.5}.png`

Header height, sidebar width, toggle, row heights, selection lift, separators, the count badge and
the history button match these. Contacts and Totp place the primary last; the tilolang header now
does the same.

## Fixed during the review (in the host or the vocabulary, never in an app)

- Rows lost separators and selection: a context menu per row hid the list; now one menu sits over
  the list and asks for the row under the pointer (`ContextMenuArea::over_content`,
  `VirtualList::key_at_point`).
- A 12px strip at the bottom: absent children of a stack kept their gap (`arranged.rs`).
- Header order, a More menu for icon-less actions, primary last.
- An action shown as a content button is not repeated in the header.
- Page parts a section apart, one content edge.
- The board scenario was blank: scenarios now reach the roots of the packages they use.
- Detail shots showed nothing selected: a scenario returns the resource the window opens.
- A document's title and body are edited in place (large title, plain chrome) and the header title
  stays blank; a long title wraps at 420.

## Remaining differences

- Filter sidebar items carry no icons (Snippets and Contacts have them): enum variants declare no
  icon.
- The narrow page gutter is 12px; native pages use 24 to 46px at 420.
- The body is a plain multi-line input, not the block editor native Notes uses.
- "Toggle pin" is a static label; native apps flip Pin / Unpin with the state.
- At 1280x1.5 a list frame collapses its sidebar where native Notes keeps its tree.
- After closing a detail the column reads "No note selected" rather than staying blank.
- Flows take no options, so Morning cleanup has no icon.
- Chat scenarios post only as the invoker.
