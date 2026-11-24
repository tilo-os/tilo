# The App Store, ported to tilolang

The Rust app in `apps/store` is replaced by `apps/tl/store` (the package `so.tilo.store`, the same
app id) and its device work by `crates/engines/store-engine` (`tilo-store-engine`), which answers
`engine.call`, `engine.start`, `engine.status` and `engine.cancel` for the app by name. The Rust
app and its crate are deleted in the same change.

Every change the app makes is still an action of the workspace's tree, carried out by this
computer's daemon (`workspace/apps#install`, `#update`, `#pin`, `#unpin`, `#uninstall` through
`app_cli::device`). The engine only reads the sources and starts those changes.

## Inventory: every feature of the Rust app, its home here, and its test

Tests are phases of `crates/lang/tilolang-runtime/tests/e2e_store.rs` unless named otherwise.

| Rust app feature | Home in the port | Test |
|---|---|---|
| Read the enabled Tilo registries (signatures verified, unreachable/expired said) | engine `catalogue` → `app_cli::all_offers`; `refresh`, `take_catalogue`, `absorb`, `Store.unanswered`/`expired` | `the_sources_are_read_as_a_job_and_what_they_found_reaches_the_catalogue` |
| Flathub's cached catalogue, in the person's language | engine `catalogue` → `app_catalogue::appstream::load_cached` | same |
| A Tilo app wins over a Flathub app of the same name | engine `catalogue` (`tilo_names`) | same |
| Installed state, pinned state, offered update | engine `catalogue` → `package_store::list_installed`; `App.installed`/`pinned`/`available`/`updatable` | same |
| Catalogue places: Discover, Updates, On this device, Bots, categories | `App`'s declared filters + `fn places_of`/`categories_of` + `std.places` | `the_window_lists_the_catalogue_and_an_app_s_page_shows_its_facts` |
| The window's search | the window bar's search (`on search`), and the type's own search fields | same |
| Show more | `view app_list`'s `shown` state and its button | same |
| A card/row: origin, summary, action, progress, update mark | `view app_row`, `view app_card`, `fn device_words`/`origin_short`/`verb_of` | same |
| An app's page: description, screenshots, what's new, facts, technical details, trust, publisher key, digest, runtime | `view app_page`, `view facts_section`, `view permissions_section`; `show_details` → engine `details` | `an_app_s_page_reads_what_its_source_says_once`, `the_window_lists_...` |
| Get (a Tilo app not run yet) | `action get` → engine job `install`, exact root/surface/publisher | `a_change_is_the_exact_signed_offer_and_runs_as_a_job` |
| Update | `action update` → engine job `update` | same |
| Uninstall | `action remove` → engine job `uninstall` | `an_uninstall_and_a_pin_are_the_workspace_s_own_actions` |
| Keep this version / allow updates (pin/unpin) | `action pin`/`unpin` → engine jobs `pin`/`unpin` | same |
| Open a Tilo app (fetch when absent) | `action open_app` → `session.open_app`; the workspace install already fetches | `the_store_reads_its_sources_...` (action surface) |
| Flatpak install/update | `action get`/`update` with `kind: "flatpak"`, whose reference the engine builds and installs through `workspace/apps#install` | `a_change_is_the_exact_signed_offer_and_runs_as_a_job` (shape) |
| Flatpak open | `action open_app` → `session.open_app`, which launches a Flatpak by its desktop id | as above |
| "Install Flatpak first" | `fn verb_of`/`refusal` when the catalogue says this computer has none; the page's banner and the button's label | `the_sources_are_read_as_a_job_...` |
| A change that cannot run, with its reason | `App.trouble` from the engine, `refusal`, `require` on every change | `an_app_this_computer_cannot_run_is_refused_before_anything_starts` |
| A change's progress and Stop | `App.job`, `poll`, `follow`, `cancel_change`; `trigger` once a second (`poll`) | `a_change_is_the_exact_signed_offer_and_runs_as_a_job` |
| Update all | `action update_all` → engine job `update-all` | — (the job takes the same `Change` list the single change does) |
| Flatpak repositories: list, add, enable, disable, remove, refresh | `view remotes_page`, `actions refresh_remotes`/`add_remote`/`enable_remote`/`disable_remote`/`remove_remote`; engine `remotes` and `remote-*` | `a_repository_is_browsed_and_its_apps_join_the_catalogue` |
| Browse a repository's apps | `action browse` → engine `browse` (Flatpak's remote-ls), absorbed into the catalogue | same |
| A repository's page | `default view remote_page` | same |
| Block / unblock an app (the person's own list, shared with the system's search) | `type Block (readers: author)`, `action block_app`/`unblock_app`, engine `block` → `registry_types::BlockList` | `blocking_a_person_s_choice_hides_the_app_and_reaches_the_shared_list` |
| A blocked app appears nowhere and cannot be opened | engine `catalogue`/`one` drop blocked apps; `fn blocked` filters the lists | same |
| Review a handed `.tapp`, then install it | `action import_package` (`opens:`) → engine `review`; `action install_reviewed` → engine job `install` with the reviewed root and file | `a_handed_package_is_reviewed_and_installed_only_after_it` |
| Open a `.flatpakref` | `action open_reference` (`opens:`) → engine `reference` | `a_handed_package_...` (same review path) |
| Review an app's runtime, install it | `action review_runtime` → engine `runtime`; `action install_runtime` → engine job `install-runtime` | — (the same offer path as Get) |
| Report drafts: start, note (typed, shared), status, case | `type Report`, `primary action draft_report`, `action record_moderation_case`, the note's field edit | `a_report_is_a_draft_the_registry_answers_and_evidence_is_a_file` |
| Prepare and submit a report; appeal a decision | `action prepare_report` → engine `prepare-report`; `submit_report`/`appeal` refuse today: signing needs the workspace identity, which this computer has no path for (see "Gaps") | same (prepare) |
| Refresh a case's verified status | `action refresh_status` → engine `report-status` (owner-signed transparency log) | same |
| Export a report's evidence | `action report_file` + `file.save` on the page's control | same |
| The trust page: how reports work, what a signature means, publishing guidelines, reports list | `view trust_page`, `view report_row`, `default view report_page` | `a_report_has_a_page_its_person_fills_in` |
| A repository that did not answer / whose catalogue expired | `Store.unanswered`/`expired`, the trust page's Sources section | `the_sources_are_read_as_a_job_...` |
| Directory bots | `App.bot`, `bot_section`, `action add_bot` refuses with "Add a bot from inside the workspace it runs in": the current listing API no longer publishes bot entries | — |
| Store links (`tilo-store:app/<id>`, catalogue, bots, runtime) | the app's `App` records have a default view, so the system's open by link opens the page; no app-local link parser is needed | — |
| Opening the app on a `.tapp`/`.flatpakref` | the two `opens:` actions above | — |
| Being found by the system's search (name, id, categories, source) | the `App` type's search block and the engine's records | — |
| The person's language | the engine reads `settings_store::appearance::preferred_language` | — |
| Screenshot scenarios | none: the app starts from the workspace, as every app does; a scenario can be added when the shots lane needs one | — |

## Gaps reported, not dropped silently

- **Signing a report**: the old app handed `submit-report`/`appeal` to the admitted action
  transport, which signed with the workspace identity. A device's `engine.call` has no such
  path. The port prepares the exact operation (kept on the report) and refuses the send, saying
  why. A platform action that signs a moderation report with the workspace identity closes it.
- **Opening a web address from an action**: the page's `ui.Link` controls open a website and
  source code (the toolkit does it). There is no platform action to open a URI, so the old
  `open-link` verb has no action form for agents.
- **German translation**: `locales/en-US.ftl` is written by the compiler; `locales/de-DE.ftl` is
  not translated yet, so a German window reads this package in English.
- **Bot entries**: `app_cli`'s current listing publishes no directory bots, so the Bots place is
  empty until a listing API offers them again.
- **The device surface is stale on this branch**: `crates/desktop/session/device-surface.json`
  lacks the types `tilolang-tilo`'s `platform_apps` names (`ServerSettings`, `Speakers`, …), so
  `tilolang check` fails for every package until the shell lane regenerates it
  (`UPDATE_SNAPSHOTS=1`). The check of this app was run with a local, uncommitted stub of those
  types.
