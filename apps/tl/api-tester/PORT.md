# API Tester in tilolang

Every feature the Rust app (`apps/api-tester`) had, and where it lives in
`apps/tl/api-tester`. A test name is a test of
`crates/lang/tilolang-runtime/tests/e2e_api_tester.rs` unless it names a crate.

## Resources

| The Rust app | The tilolang package |
|---|---|
| `api-tester.workspace` resource, one per workspace | `Workspace`, the app's root `tester` |
| `collections` of saved collections, each with its requests | `Workspace.collections`, `SavedCollection.requests` |
| `environments` with variables, `active_environment` | `Workspace.environments`, `Workspace.active_environment` |
| the builder's request, kept as one nested value | `Workspace.builder`, a `Request` |
| the saved request the builder holds a copy of | `Workspace.opened: Option<Request>` |
| `history` of sent requests, newest first, 50 kept | `Workspace.history: Collection<Sent>` |
| the response shown in the window | `Workspace.answer: Answer` (with `pretty`), `answered`, `failure`, `notice` |
| the credential held only on the device (secret store) | `Credentials.reference`, a name in the device's own store (engine `keep_credential`) |
| secret variable values held on each device | `Variable.reference`, the same store |
| rows of parameters, headers and form fields | `Row` in `Request.params`, `.headers`, `.form` |

## Actions

| The Rust app | The tilolang package | Test |
|---|---|---|
| `new-request` | `new_request` | `a_request_is_built_saved_opened_and_deleted` |
| `new-collection` | `new_collection` | same |
| `delete-collection` | `delete_collection` | same |
| `rename-collection` (effect) | `rename_collection`, and the sheet | same |
| `add-request` | `add_request` | same |
| `save-request` | `save_request`, `fill_from` | same |
| `open-request` | `open_request` | same |
| `delete-request` | `delete_request`, `delete_opened_request` | same |
| `rename request` (form) | `rename_draft` | same test view path |
| `new-environment` | `new_environment` | `environments_fill_variables_and_keep_their_secrets_on_the_device` |
| `delete-environment` | `delete_environment`, `delete_active_environment` | same |
| `use-environment` | `use_environment` | same |
| `pick-environment` (offer) | the environment menu, `use_environment`, `no_environment` | same |
| `add-variable` | `add_variable`, the Variables sheet | same |
| `delete variable` (form) | `delete_variable` | same |
| `set-secret-value` (effect) | `keep_variable` | same |
| `set-credential` (effect) | `keep_credential`, `forget_credential`, `set_scheme` | `a_send_hands_the_device_its_variables_and_its_sign_in` |
| `send` (effect) | `send`, the `watch_send` trigger, `poll_send` | `the_answer_reaches_the_window_and_the_history` |
| `send-saved` (effect) | open the saved request (`open_request`) and `send` | `a_sent_request_opens_again_and_can_be_saved` |
| `cancel-send` (effect) | `cancel_send` | `a_send_can_be_stopped_and_a_failed_one_says_why` |
| `record-sent` (verb) | `record_sent` inside `poll_send` | `the_answer_reaches_the_window_and_the_history` |
| `open-sent` | `open_sent` | `a_sent_request_opens_again_and_can_be_saved` |
| `save-sent` | `save_sent` | same |
| `delete-sent` | `delete_sent` | same |
| `copy-response` (body, headers, request; effect) | `copy_body`, `copy_headers`, `copy_request` | `the_answer_and_the_request_leave_the_app_as_a_copy` |
| `request-menu`, `environment-menu` (views) | `request_menu`, `environment_menu` | `the_window_shows_the_request_tabs_the_answer_and_the_sheets` |
| add/delete a parameter, header, form field | `add_param`, `delete_param`, `add_header`, `delete_header`, `add_form_field`, `delete_form_field` | `a_request_is_built_saved_opened_and_deleted` |
| `set body mode` (toggle) | `set_body_mode` | `a_sent_request_opens_again_and_can_be_saved` |

## What the window shows

| The Rust app | The tilolang package | Test |
|---|---|---|
| collections and their requests in the sidebar, method first, collapsible, add per collection | `NavShell` sidebar: `ui.NavList`, `ui.NavSection(collapsible, add:)`, `ui.NavItem` | `the_window_shows_the_request_tabs_the_answer_and_the_sheets` |
| history rows with method, short path and status | the History section's `ui.NavItem(count: status)` | same |
| environment picker with "No environment" and the managing commands | `environment_menu` `ui.MenuButton` | same (drawn closed) |
| method chooser and URL field | `ui.EnumField(bind: builder.method)`, `ui.TextInput(bind: builder.url)` | `a_send_hands_the_device_its_variables_and_its_sign_in` |
| variable hint (`1 variable resolved`, `Unresolved variable: x`) | `variable_hint` | not driven by a walk step (a `fn`, pure) |
| request tabs Params / Headers / Auth / Body | `ui.ViewTabs` in `request_panel` | `the_window_shows_...` |
| key/value tables with a switch, key, value and remove | `parameter_row`, `header_row`, `form_row` | same |
| the trailing blank row that becomes a pair | an explicit "Add parameter/header/field" button | same |
| auth schemes and their fields, stored credential shown | `auth_panel`, `credential_label`, the sign-in sheet | `a_send_hands_the_device_...` |
| body modes and their editors | `body_panel` (`ui.CodeEditor`, form rows) | same |
| response status badge in its tone, duration, size, "More than", "First N shown (as hex)" | `response_pane`, `size_caption`, `shown_caption` | `the_answer_reaches_the_window_and_the_history` |
| Pretty / Raw / Headers views; JSON laid out and coloured; binary as a hex dump | `response_pane`, `pretty_json`, `response_headers` | `the_answer_reaches_the_window_and_the_history` |
| empty, waiting and failed panes; "Try again" | `ui.EmptyState`, `ui.Spinner`, `failure` | `a_send_can_be_stopped_and_a_failed_one_says_why` |
| copy body, copy headers, copy request, and the copy notice | `copy_body`, `copy_headers`, `copy_request`, `dismiss_notice` | `the_answer_and_..._as_a_copy` |
| the sheets that collect a name, a key or a value; delete confirmations | `sheets` (`ui.ModalDialog` per sheet) | `the_window_shows_...` |
| request and response split | the `split` layout (structural, not a draggable divider) | — |

## The engine

| The Rust app | The tilolang package |
|---|---|
| `http.rs`: `build_outgoing`, any method, encoded query and form, auth headers, content type | `crates/engines/tilo-api-client`: `build_outgoing`, unit-tested there |
| 30 s timeout, cancellable send, 2 MiB body cap, 64 KiB hex dump, text/binary decision | the same crate: `REQUEST_TIMEOUT`, jobs with `cancel`, `MAX_SHOWN_BODY_BYTES`, `MAX_HEX_BYTES`, `ShownBody` |
| credentials sealed into the device's secret store (`credentials.rs`) | the same crate: `keep_credential`, `forget_credential`, `credential` over `tilo-secrets` for this app |
| `send` ran in the window and reported status; `record-sent` wrote history | `send` starts a job on the device; the `watch_send` trigger polls it once a second and writes the answer and the history entry |

## Differences a person would look for

- The request/response divider is not draggable here: the `split` layout gives both panes their
  share and stacks them in a narrow window.
- JSON in the response is laid out and coloured (Pretty) but not foldable: the toolkit's code
  editor has no folds.
- A saved request is sent by opening it and pressing Send; the Rust app's separate
  "Send saved request" offer had no window of its own for it.
- This device's browser page runs no engine for the app yet (the same gap Books' online search
  has), so the web build builds and opens but sends nothing.
