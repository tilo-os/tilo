# Code Editor: every feature of the Rust app and where it lives now

The Rust app (`apps/code-editor`) is gone. This package (`apps/tl/code-editor`) does what a
person did with it, and the device's work it needed — reading and writing files, searching a
tree, running commands and answering language servers — is `crates/engines/code-engine`, which
registers as the engine of `so.tilo.code-editor`.

Each row names where the capability lives and the test that goes through it
(`crates/lang/tilolang-runtime/tests/e2e_code_editor.rs`).

## The project

| Feature | Now | Test |
|---|---|---|
| A project of files and folders, shared and synced | `Project.files: Collection<Entry>`, `Entry.parent`, `Entry.kind` | `a_project_starts_with_a_file_in_the_folder_it_names` |
| A file's text, spliced between people | `Entry.text: Text`, edited through `ui.CodeEditor(bind:)` | `a_text_edit_splices_the_file` |
| New file, with a name, a folder and its first text | `new_file` | `a_new_file_is_an_entry_with_its_language` |
| New folder | `new_folder` | `a_folder_holds_entries_and_never_holds_itself` |
| Names unique in their folder, no slash, not empty | `named`, `free_in`, each verb's `require` | `a_new_file_is_an_entry_with_its_language` |
| Rename | `rename` | `a_new_file_is_an_entry_with_its_language` |
| Move into a folder, before an entry, after a drop | `move_entry`, `move_after`, `std.tree`'s `place`/`place_after` | `a_folder_holds_entries_and_never_holds_itself` |
| Delete, and a folder with what it holds | `delete` | `deleting_a_folder_takes_its_entries` |
| The language a file is, by its extension, for 17 languages | `language_of`, `indent_of`, `comment_of` | `every_language_the_editor_knows_reads_by_its_extension` |
| Choosing a file's language | `set_language`, the file head's language menu | `a_file_is_kept_as_the_language_a_person_chose` |
| The path an entry stands at | `path_of`, `depth_of` | `a_folder_holds_entries_and_never_holds_itself` |
| Files and folders found from the palette | the store's own search over `Project.files` (no app copy) | `the_window_shows_the_tree_the_code_and_the_files_state` |
| Explorer rows, folders first, with their icons | `file_row`, `std.tree` | `the_window_shows_the_tree_the_code_and_the_files_state` |

## The device's files

| Feature | Now | Test |
|---|---|---|
| Open a folder as the project | `open_folder` (`engine.call("list")`, recursively) | `opening_a_folder_reads_its_files_through_the_engine` |
| Open a file a person chose, or one handed over | `open_file`, with the Rust app's mime and extension providers | `a_file_handed_over_opens_with_its_words` |
| Read a file's text and its stamp | `read_text` (`engine.call("load")`) | `opening_a_folder_reads_its_files_through_the_engine` |
| Refuse a file that is not text, or is too large | `disk::load` in the engine (24 MiB, a NUL in the first 8 KiB) | `the_engine_refuses_what_is_not_a_text_file` |
| Save, atomically, checked against the stamp read | `save` → `engine.call("save")` → `disk::save` | `saving_checks_the_file_did_not_change_under_it` |
| Save over a change on disk, after asking | `overwrite` | `saving_checks_the_file_did_not_change_under_it` |
| Reload from disk, dropping what changed here | `reload` | `saving_checks_the_file_did_not_change_under_it` |
| Save all | `save_all` | `saving_checks_the_file_did_not_change_under_it` |
| Save as, into a folder a person picks | `save_as`, `save_into` | `saving_checks_the_file_did_not_change_under_it` |
| Notice a file that changed on disk | `Entry.stale`, `ui.Badge("Changed on disk")` | `saving_checks_the_file_did_not_change_under_it` |
| Build output and VCS internals never read | `disk::IGNORED_DIRS`, `listing` in the engine | `the_engine_lists_a_folder_without_its_build_output` |
| A path the app cannot reach says what to do | `Project.trouble`, the window's footer and status line | `a_file_that_does_not_read_says_why` |

## The code

| Feature | Now | Test |
|---|---|---|
| Syntax colouring, line numbers, current line | `ui.CodeEditor` (`gutter`, the language's grammar) | `the_window_shows_the_tree_the_code_and_the_files_state` |
| Caret, selection, word and line movement, multi-cursor, clipboard, IME, undo | `ui.CodeEditor`/`ui.TextInput`'s shared core | `the_editor_keeps_the_shared_core_caret_and_selection` |
| Text zoom | the window's zoom (`ContentZoom` in the shared core) | — (shared component) |
| Duplicate, move and delete a line | `duplicate_line`, `move_line`, `delete_line` | `line_operations_duplicate_move_delete_and_comment` |
| Comment and uncomment a line | `toggle_comment`, per language | `line_operations_duplicate_move_delete_and_comment` |
| Indent and outdent a line | `indent_line`, `outdent_line`, `indent_of` | `line_operations_duplicate_move_delete_and_comment` |
| Write a whole file, or a range of one, from an agent | `replace_text`, `replace_range` | `a_text_edit_splices_the_file` |
| Find in the file, case and whole word aside | `find_text` | `finding_and_replacing_in_a_file` |
| Replace every match in the file | `replace_in_file` | `finding_and_replacing_in_a_file` |
| Go to a line | `Project.answer_line`/`answer_column`, `seen_line` from a result or a problem | `a_project_search_runs_and_its_result_opens_the_file` |
| The declarations a file writes (go to symbol) | `symbols_in`, `declared` | `the_declarations_of_a_file_are_its_symbols` |

## Code help

| Feature | Now | Test |
|---|---|---|
| A language server per language and project folder | `ensure_server` → `engine.start("server")` (rust-analyzer, pylsp, typescript-language-server, gopls, clangd) | `a_language_server_answers_with_its_problems` |
| Problems from the server, by file | `poll_server` → `Entry.problems` | `a_language_server_answers_with_its_problems` |
| The editor's own checks until a server answers | `lint` (brackets, trailing whitespace, line length, TODOs, mixed indentation), `check` | `the_editors_own_checks_stand_until_a_server_answers` |
| Suggestions at a place | `complete` → `engine.call("ask")`, `accept_completion` | `a_language_server_answers_with_its_problems` |
| What stands at a place | `hover` | `a_language_server_answers_with_its_problems` |
| Go to definition | `definition` | `a_language_server_answers_with_its_problems` |
| Format a file, and format on save | `format_file`, `toggle_format_on_save`, `save` | `formatting_on_save_formats_before_it_writes` |
| The server's progress ("Loading project 42%") | `Project.server_progress`, the status line | `a_language_server_answers_with_its_problems` |
| A server that is missing or stopped, and trying again | `server_trouble`, `restart_server` | `a_missing_language_server_says_so` |
| Language servers on or off | `toggle_language_servers`, `has_server` | `language_servers_can_be_turned_off` |

## Search, terminal, window

| Feature | Now | Test |
|---|---|---|
| Search every file of the project | `search_project` → `engine.start("search")` → `disk` walk + `search` | `a_project_search_runs_and_its_result_opens_the_file` |
| Results with file, line and preview; running, scanned, truncated | `search_hits`, `search_said`, `search_panel` | `a_project_search_runs_and_its_result_opens_the_file` |
| Open a result where it stands | `open_hit` | `a_project_search_runs_and_its_result_opens_the_file` |
| Stop a search | `stop_search` | `a_project_search_runs_and_its_result_opens_the_file` |
| Run a command in the project | `run` → `engine.start("run")` | `a_command_runs_and_its_output_names_the_places` |
| Its output, its exit status, a truncated output | `poll`, `Command.output`/`exit`/`truncated` | `a_command_runs_and_its_output_names_the_places` |
| The places a command's output names | `items_of`, `item_of`, `reveal_item` | `a_command_runs_and_its_output_names_the_places` |
| Stop a running command | `stop` → `engine.cancel` | `a_command_runs_and_its_output_names_the_places` |
| The commands run, with where and how they ended | `Command`, `commands` view, `command_view` | `a_command_runs_and_its_output_names_the_places` |
| Run one again, and one test of one again | `rerun`, `rerun_test` | `a_command_runs_and_its_output_names_the_places` |
| Clear the terminal's history | `clear_commands` | `a_command_runs_and_its_output_names_the_places` |
| A terminal at the project's folder | `open_terminal` → `tool.terminal` | `a_command_runs_and_its_output_names_the_places` |
| The window: tree, code, panel, status line | `project`, `file_page`, `file_panel`, `status_line` | `the_window_shows_the_tree_the_code_and_the_files_state` |
| Problems, Terminal and Search results in one panel | `state panel` | `the_window_shows_the_tree_the_code_and_the_files_state` |
| Language, unsaved changes, problem count, caret, progress | `status_line` | `the_window_shows_the_tree_the_code_and_the_files_state` |
| A file dropped on the window is imported | `import_dropped`, `std.tree`'s `drop_files` | `a_file_handed_over_opens_with_its_words` |

## Known gaps in the shared framework (not this app's to close)

These are reported, not worked around:

- `ui.CodeEditor` exposes six languages (`plain_text`, `rust`, `typst`, `sql`, `json`,
  `tilolang`); the app colours the rest as plain text. The grader grammar list in ui-core has
  all of them, so the fix is the component's `Language` cases.
- `ui.CodeEditor` exposes no caret or reveal prop, so a problem, a search result or a symbol
  moves `Project.answer_line` but cannot put the caret where it names, and shows no
  diagnostics under the words. The shared `TextInput` core has both; the component does not
  describe them.
- The window's own find bar (the Rust app's `Findable`) is Rust-only; the app offers a find
  row and `find_text` on the page until the window finds in a `CodeEditor`.
- A terminal as a component (the Rust app embedded one) does not exist in the shared library;
  commands run through the engine and their output shows in the window, and the real terminal
  opens from `tool.terminal`.
