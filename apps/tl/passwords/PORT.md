# Passwords, in tilolang

The Rust app at `apps/passwords` is replaced by `apps/tl/passwords`. This lists what the Rust
app did and where each thing is now, so nothing went with it.

## Where things stand

- **The vault** is the workspace's: every member the workspace lets read Passwords reads the same
  items, and a change to one is everyone's. The items are records of the app's own type, kept in
  the workspace's store, sealed to the readers the store places them with.
- **A device's own vault** (the secret service a machine keeps for its person) is reached through
  the app's engine (`crates/engines/passwords-engine`): the entries a workspace may share come
  into it once, and the machine's authentication material and its Wi-Fi networks stay on it.
- **What search holds.** The password, the notes and the custom fields are `conceal`ed: search
  never holds or finds them. The name, the username and the belonging are searchable.

## The feature rows

| Rust app | tl home | Test |
| --- | --- | --- |
| Workspace vault document (`passwords.workspace-vault`) sealed under its keys | `type Vault` and `type Item`, kept in the workspace's store, sealed to its readers | `a_saved_item_lists_and_reads_back` |
| Root resource `passwords.vault` | `root vault: Vault` | `a_saved_item_lists_and_reads_back` |
| Item kinds: login, API key, secure note, Wi-Fi, other | `enum Kind`, `type Item.kind` | `every_kind_saves_and_names_its_secret` |
| Name, username, password, notes, addresses, tags, custom fields, favorite | `type Item` fields | `a_saved_item_lists_and_reads_back` |
| New item, with what is typed first as its name | `primary action new_item(v, title)` | `new_items_are_made_from_the_entry` |
| Save the editor's draft, its checks (a name, a note's text, a secret's value, a web address) | `action save_editor`, `action add_origin`, `fn origin_of` | `the_editor_refuses_what_cannot_be_saved` |
| Save a login in one step for agents and scripts (`save-login`) | `action save_login` | `a_login_saves_in_one_step_and_its_address_is_checked` |
| List logins by id and name (`list-logins`) | `action list_logins` | `logins_list_by_name_and_id_and_never_their_password` |
| Set a typed password, keeping the old one when empty | `action set_password` | `a_typed_password_lands_and_an_empty_one_keeps_it` |
| Generate a strong password | `action generate_password`, `fn generated` | `a_generated_password_is_strong` |
| Edit notes | `action set_notes` | `notes_are_written_and_read` |
| Favorite toggle and the favorites filter | `action favorite`, sidebar `Favorites` | `favorites_are_one_press_and_a_place` |
| Tags: add and remove | `action add_tag`, `action remove_tag` | `tags_are_added_and_taken_off` |
| Addresses: add and remove | `action add_origin`, `action remove_origin` | `addresses_are_added_and_taken_off` |
| Copy the secret, the identity and the addresses | `action copy_secret`, `copy_identity`, `copy_origins` | `the_secret_and_what_identifies_a_person_copy` |
| Reveal a secret, then hide it | `item_page`'s reveal dialog and `showing` state | `a_revealed_secret_shows_and_hides` |
| Password review: reused, weak, not changed in a year | `view review_page`, `fn is_reused`, `is_weak`, `is_old` | `the_review_finds_reused_weak_and_old_passwords` |
| App access: permissions and requests, allow, decline, revoke | the system's grants: the vault is a workspace document, and an app reads an entry only through a grant the system asks for and revokes | `crates/core/tilo-workspace-service` vault tests |
| This device's vault taken into the workspace once | `action absorb_device` | `the_devices_vault_comes_in_once_and_leaves_the_device` |
| Browser CSV import with a preview and skipped logins | `preview_csv`, `apply_import`, `view import_preview` | `a_csv_import_previews_and_skips_what_the_vault_holds` |
| Browser CSV export | `action export_csv` | `a_csv_export_hands_the_browser_format` |
| Encrypted backup with a chosen password | `action export_backup`, `export_encrypted_backup` | `a_backup_takes_a_long_enough_password_and_encrypts` |
| Restore from a backup | `action restore_backup`, `import_encrypted_backup` | `a_backup_restores_its_items` |
| The vault searches by name, username and tags, never by a secret | `Item.search` with `conceal` | `a_password_never_enters_search` (compiler) and `search` in `crates/lang/tilolang-tilo` |
| Window: sidebar places with counts, the item list, the content | `view vault_window`, `item_page`, `item_editor` | `the_window_lists_places_and_the_open_item` |
| The tools' pages: import and export, review | `view transfer_page`, `review_page` | the rows' own tests |
| Toasts, the empty states and the unreachable notice | banners and empty states in the views | `an_empty_vault_says_so_and_offers_a_new_item` |
| Captures' fixture vault | `scenario "a vault"` | staged by `tilolang layout-check` |

## What changed, and why

- The editor writes what a person typed once, on Save, for the password, the notes and the custom
  fields: writing them as they are typed would keep every prefix of a password in the history.
  The name and the username are written as they are typed, as every document's are.
- Revealing a secret is a question over the item, and the reveal lasts as long as the page shows
  it, hidden when another item opens: the ten-second timer is the window's, not a state.
- The clipboard copy is the platform's own. Its prompt clear after 30 seconds is not yet in the
  clipboard the engine now uses; noted as a shared gap.
- App access is no longer a page of Passwords: the vault is the workspace's, and who reads an entry
  is decided by the system's grants, asked and revoked where every other grant is.
