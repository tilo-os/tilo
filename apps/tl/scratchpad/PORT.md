# Scratchpad: what came over

One note, kept for you. The Rust app was a resource, an editor and a window bar; the
tilolang package is the same three things over the system's own history, sync and search.

| What the Rust app had | Where it is now | Test |
| --- | --- | --- |
| `scratchpad.note` with a `body` text field, one per workspace | `type Note { body: RichText }`, the package's `root note` | `e2e_scratchpad::…a_new_note_opens_empty_and_its_editor_takes_what_is_written` |
| A multi-line editor with the placeholder "Start writing…", at the readable measure, scrolling | `ui.BlockEditor(bind: n.body)` on `std.document`'s readable page | same case; `the_note_is_named_by_the_first_line_it_holds` |
| The window bar names the note by its opening line, elided to 60 characters | the type's title rule: the first line of its first text field, and "Untitled note" while it is empty | `…the_note_is_named_by_the_first_line_it_holds` |
| Sample note and a long note, for captures | scenarios `a note` and `a long note` | `…the_scenarios_stage_the_sample_note_and_one_long_enough_to_scroll` |
| Undo and history, one step per edit, attributed | the system keeps each edit as one change of the note | the versions check in the first case |
| A viewer's edit refused; agents and scripts invoke the same action | the app's field edit, checked by the host's grants | the runtime's `authority` cases |
| German strings, the German name, search words, the tag line, the category | `locales/de-DE.ftl` and the `app` header | the package's catalogue check |
| Runs on every platform, in the app window and on the page | a tilolang package runs in the app window wherever the session runs | `app-walk/scratchpad` |
| A native input probe that reported the note's hash and control rects to performance captures | not carried: it measured the Rust toolkit's input path and the note's text; a tilolang app's input and layout are measured by `tilolang profile` and `tilolang layout-check` over the package | — |
