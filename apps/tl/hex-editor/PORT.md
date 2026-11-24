# Hex Editor: every feature of the Rust app and where it lives now

The Rust app (`apps/hex-editor`, `apps/hex-editor-model`, `apps/hex-editor-model-wasm`) is
gone. This package (`apps/tl/hex-editor`) does what a person did with it, and the device's work
it needed — reading a file's bytes, applying the edits typed into them, finding bytes and reading
them as numbers — is `crates/engines/hex-engine`, which registers as the engine of
`so.tilo.hex-editor`. The grid itself is `ui.HexGrid` in `ui-core`, a described component like
the rest of the toolkit.

Each row names where the capability lives and the test that goes through it
(`crates/lang/tilolang-runtime/tests/e2e_hex_editor.rs`, whose device is the real engine and a
real content store).

## A file and its bytes

| Feature | Now | Test |
|---|---|---|
| Open a file named on this device | `open_path` → `engine.call("open")` (128 MiB at most) | `a_file_named_on_this_device_opens_as_its_own_buffer` |
| Open a file the shell hands over, of any kind | `open_file`, `opens: ["*/*"]` | `a_file_another_app_hands_over_opens_from_its_address` |
| Open a file the person picks | the window's `file.choose(open_chosen)` | `the_library_lists_the_buffers_a_person_opened` |
| A new empty buffer | `new_buffer` | `a_new_empty_buffer_is_a_buffer_with_no_bytes` |
| The bytes as a document every member reads | `Document.base`, the address in the workspace's content store; the edits are `Document.changes`, appended `at:remove:insert` | `a_file_another_app_hands_over_opens_from_its_address` |
| Two people's edits of different bytes both kept | the edits list is spliced, each append its own record | (the store's; the app appends, never rewrites) |
| A file too large to hold | `MAX_FILE_BYTES` in the engine, refused before it is read | (engine) |
| Save where it came from | `save` (`Ctrl+S`) → `engine.call("save")`, atomic write | `saving_writes_the_file_and_what_was_saved_is_the_base` |
| Save as, placed by the person | `save_bytes` + the window's `file.save` | `a_file_with_no_path_on_this_device_is_saved_where_the_person_places_it` |

## Reading and editing

| Feature | Now | Test |
|---|---|---|
| The grid: offset, hexadecimal pairs, text, changed bytes lifted | `ui.HexGrid` (`rows`, `changed`, `first`, `columns`) | `the_window_shows_the_grid_the_bytes_and_the_status` |
| A cursor in either column, a selection and its mirror | `ui.HexGrid` (`cursor`, `anchor`, `column`), `move_cursor`, `pressed` | `shift_makes_a_selection_and_delete_takes_it` |
| Typing a hex digit: high nibble, then the byte, then on | `press` → `type_hex` → `edit` | `typed_hex_digits_overwrite_a_byte_and_advance` |
| Insert mode splices, overwrite replaces | `set_mode`, `desk.mode`, the engine's `apply` | `insert_mode_splices_bytes_in` |
| Typing into the text column | `type_ascii` | `the_ascii_column_types_characters` |
| Arrows, Home, End, Page Up and Down, with Shift | `press` → `moved` | `the_cursor_moves_and_the_window_follows` |
| Tab switches columns, Insert the mode, Escape clears | `press` | `the_ascii_column_types_characters` |
| Backspace and Delete | `backspace`, `delete_at_cursor` | `shift_makes_a_selection_and_delete_takes_it` |
| The window scrolls to keep the cursor, the wheel and bar move it | `move_cursor`, `scrolled` | `the_cursor_moves_and_the_window_follows` |
| Offsets past the end are refused | `hex-engine`'s `checked` | `the_published_verbs_edit_the_bytes_and_refuse_offsets_past_the_end` |

## Finding, clipboard, inspector

| Feature | Now | Test |
|---|---|---|
| Find a hex pattern or text, case and whole words | `search` → `engine.call("find")` | `a_pattern_is_found_in_hex_and_in_text_and_revealed` |
| Matches drawn in the grid, the current one standing out | `ui.HexGrid` (`matches`, `at_match`) | `a_pattern_is_found_in_hex_and_in_text_and_revealed` |
| Reveal a match | `reveal` | `a_pattern_is_found_in_hex_and_in_text_and_revealed` |
| Replace every match, in one step | `replace_all` → `engine.call("replace")` | `every_match_is_replaced_at_once` |
| Copy the bytes as hex or as text | `copy` → `engine.call("range")` → `clipboard.copy` | `ctrl_copy_carries_the_bytes_then_paste_puts_them_back` |
| Cut and paste, overwrite mode refusing a longer paste | `cut`, `paste` | `ctrl_copy_carries_the_bytes_then_paste_puts_them_back` |
| Select all | `select_all` | (the grid's menu) |
| Go to an offset: `0x1F`, bare hex or decimal | `go_to`, `offset_of` | `go_to_takes_hexadecimal_and_decimal_offsets` |
| The inspector: i8…u64, f32, f64, LEB128, bin, oct, char | `engine.call("rows")` → `desk.findings` → `ui.PropertyList` | `the_inspector_reads_the_bytes_at_the_cursor_and_its_byte_order` |
| Either byte order | `set_endian` | `the_inspector_reads_the_bytes_at_the_cursor_and_its_byte_order` |
| The published verbs an agent calls | `insert_bytes`, `overwrite_bytes`, `delete_bytes` | `the_published_verbs_edit_the_bytes_and_refuse_offsets_past_the_end` |

## Left for the shared framework

- **A grid a person types into from a walk.** `ui.HexGrid` takes the keyboard from a press and
  reports keys; the headless walk presses controls by accessible name, and the grid is one
  table whose cells are not separate targets. A walk that types bytes needs either a component
  whose cells are addressable or a walk op for a focused component's keys.
- **Save as does not move the document.** The system's save chooser reports nothing back about
  where it wrote, so the document keeps its own path and name; a second save asks again. The
  Rust app moved the buffer to the chosen file.
- **One undo step per key.** Each key is one action, so undo steps byte by byte; the Rust app
  grouped a run of typing into one step.
- **Cursor and window per person.** They are the reader's own record (`Desk`, `readers:
  author`), not view state, because the device answers their rows: an app's view cannot ask the
  engine, and view state cannot hold what an action answered.
