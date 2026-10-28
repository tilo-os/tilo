# Native review of the tilolang apps

Every shot comes from the framework's offscreen `--shots` path, rendered on lavapipe (software
Vulkan). No window was opened and the GPU was not used: the Vulkan loader was given only the
lavapipe ICD, and every shot's loader log names `lvp_icd` as the only driver it found.

    export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.aarch64.json
    export WGPU_BACKEND=vulkan WGPU_ADAPTER_NAME=llvmpipe

The tilolang apps run through the installed-package path, the way the launcher starts them:

    tilolang build apps/tl/<app> --out OUT --identity ID
    TILO_APPS_DATA_DIR=STORE tilo-install install --force --allow-self-signed --ignore-missing-runtime OUT/<app>.tapp
    TILO_PACKAGE_ID=so.tilo.lang.<app> TILO_APP_MODULE=<payload-root> tilolang-app --shots PNG [--scenario S] --viewport WxH --text-scale T

    ./target/debug/{notes,contacts,snippets,totp,chat} --shots PNG --viewport WxH --text-scale T

Viewports are 420x860, 1280x800 and 2560x1440, each at text scale 1 and 1.5. `tl-<app>-*` is a
tilolang app with its first scenario, `tl-<app>-empty-*` the same app with no scenario, and
`native-<app>-*` a native app.

| tilolang app | scenario | compared with |
|---|---|---|
| notes | a week of notes | native-notes, native-snippets |
| todo | a saturday list | native-snippets, native-totp |
| letters | a desk of letters | native-notes, native-contacts |
| chat | a busy workspace | native-chat, native-contacts |
| counter | a busy morning | native-totp |
| board | a morning board | (no native dashboard) |

## Findings per app

### notes
- 1280 and 2560: filter sidebar with icons (All notes, Pinned) and counts, the list, and the note.
  The pin is a toggled icon action, filled on a pinned note; its label reads Unpin. The body is the
  block editor native Notes uses. Its text starts under the title and wraps at the column.
- 420: the note is pushed, with Back, pin, delete and history in the header. Title and body wrap.
- 1280x1.5: the filter sidebar collapses behind its toggle. NavShell does that for three columns,
  and native Contacts does the same at this size. Native Notes keeps its tree only because it has
  two columns.
- Empty: "No notes yet" with New note; the detail column shows the app icon and "No note
  selected", as native Contacts and Mail do.

### todo
- Rows show the due date briefly as meta (Oct 24, Nov 2). The filter cases have icons (All, Open,
  Done). Check is a stateful icon action.
- The detail shows the title in place, Done as a switch, and Due as the date field: the calendar
  icon and the date, which opens a month calendar in a popover, with Clear date when the date may
  be empty. The notes field is the block editor.
- 420 and 1.5: the title wraps and the fields stack.

### letters
- Unchanged from the last round: Drafts/Sent sidebar with counts, two-line rows, recipient,
  title, body and Send in the content.

### chat
- Rows show the conversation, the last message as the secondary line, its time as meta, and its
  author's avatar. The scenario fills twelve conversations from Mia, Leo, Ada and you. A
  conversation with no messages yet reads "No messages yet" and has no avatar.
- The conversation is a transcript across the content column, opened at its end: avatar, author
  in the label role, the time, then the message. The composer is pinned to the bottom of the
  column, with an icon-only Send.
- Times read as 10:42 AM, the capture's pinned clock, because `now` reads the toolkit's clock.
- 420: the conversation is pushed, with the composer at the bottom.

### counter
- Title-only header with search and history; the count, then Decrement and Increment (primary)
  with icons. Matches Totp's page column.

### board
- `tl-board-*` (with the scenario) is empty, and so is `tl-board-empty-*`. Both show the app icon
  and "Nothing here yet" instead of a blank canvas. With the scenario it is empty because of a
  platform gap: in installed mode the runtime holds only the schemas of the packages the board
  uses, so the scenario's `root todo.TodoList` traps ("type is not declared by any loaded
  package"). A board cannot reach another app's records either. In the test host, which loads
  every package, the board shows three hugging cards: open todos, pinned notes and conversations.
  Each card holds rows with the resource's icon and title that open the resource, under a section
  header with a count pill. `crates/lang/tilolang-runtime/tests/e2e_apps.rs` covers it.

## Remaining differences

- Board in installed mode: see above. It needs a platform answer for reading and opening
  another package's resources.
- Native Chat's composer is one framed field with the send icon inside it; ours is an input
  beside an icon button. At 2560 the composer stretches across the whole column, as native Chat's
  does, but the transcript's short lines leave it looking very wide.
- Avatars and row icons stay at their 1x size at text scale 1.5; native Chat's avatars do too.
- Native Chat has a channels sidebar as its third column; our chat has two columns.
- The block editor keeps its own end inset, so at 420 a body wraps about 24px before the
  column's edge (native Notes does the same).
- The toggled pin draws a filled lift behind the icon; native Notes has no pin action to compare
  with.
- Letters and counter are unchanged since the last review.
