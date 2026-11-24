# Containers, ported from the Rust app

Every feature the Rust app had, where it lives now, and the test that holds it. The reading and
the verbs come from the session's system service, which runs rootless podman; the window and the
pull job live here.

| Feature | Where it lives now | Test |
| --- | --- | --- |
| Containers, images, volumes and distroboxes read from podman | `containers` reading, `crates/os/system-service/src/containers.rs` (JSON of `podman ps/images/volume ls/inspect/stats/logs`), published as `device.containers` | `the_window_lists_the_places_and_what_the_engine_holds` |
| Running first, then by name; each container's name, image and state | `window` / `containers_page` rows, `state_words` | `the_window_lists_the_places_and_what_the_engine_holds` |
| Container detail with Logs, Stats, Inspect and Terminal tabs | `container_detail`, `logs_pane`, `stats_pane`, `inspect_pane`, `terminal_pane` | `a_chosen_container_shows_its_logs_with_their_severity`, `the_usage_tab_shows_what_the_container_used`, `the_details_tab_shows_ports_mounts_environment_and_labels` |
| Log lines marked by severity, an unreadable log told from an empty one | `logs_pane`, `trouble_line`, `warn_line`; `logs_error` in the reading | `a_chosen_container_shows_its_logs_with_their_severity` |
| Processor, memory and disk use, received and sent bytes, process count | `stats_pane`, `mem_fraction`; `stats` in the reading | `the_usage_tab_shows_what_the_container_used` |
| Ports, mounts, environment and labels | `inspect_pane` | `the_details_tab_shows_ports_mounts_environment_and_labels` |
| Start, stop, restart, pause and resume the verbs the state allows | `lifecycle_buttons` → `device.start/stop/restart/pause/unpause` | `a_running_container_offers_stop_restart_and_pause`, `a_stopped_container_offers_start` |
| Remove a container, behind a confirmation | `confirm_dialog`, `confirm_run` → `device.remove` | `a_container_is_removed_only_after_the_confirmation` |
| Attach a shell to a running container | `terminal_pane` → `tool.container_shell(container, "podman")` | `a_shell_opens_inside_the_container_and_a_distrobox` |
| Images: pull, tag, remove, in-use count, size and total | `images_page`, `tag_image`, `prompt_run`, `image_bytes`, `used_by` | `the_images_place_shows_the_images_and_their_verbs`, `an_image_is_tagged_and_removed_by_its_verbs` |
| Volumes: create, remove, driver, use and mountpoint | `volumes_page`, `make_volume`, `prompt_run` | `the_volumes_place_shows_the_volumes`, `a_volume_is_made_and_removed_by_its_verbs` |
| Distroboxes: list each box and enter it | `distrobox_page`, `enter_box` → `tool.container_shell(box, "distrobox")` | `the_distrobox_place_offers_each_box_its_shell` |
| New-container form: name, image, ports, environment, volumes and command | `create_form`, `create_container`; the draft is a collaborative record edited by `bind` | `the_draft_is_filled_a_field_at_a_time_and_creates_the_container` |
| The installed images suggested in the image field | `ui.Autocomplete` over `device.containers.images` in `create_form` | `the_draft_is_filled_a_field_at_a_time_and_creates_the_container` |
| Agents and scripts fill the draft and clear it | `fill`, `clear_create` | `the_draft_is_filled_a_field_at_a_time_and_creates_the_container` |
| A pull with its output, its stop and its close | `begin_pull`, `poll_pull`, `cancel_pull`, `dismiss_pull`, `pull_banner`; the engine job is `crates/lang/tilolang-process/src/containers_engine.rs`, `trigger watch_pull` polls it | `a_pull_runs_in_the_apps_engine_and_its_output_shows` |
| A sample engine for captures | removed: captures are the tests above and the app starts empty, as the owner's rule says | — |
| A Podman that does not answer is explained, with a way to ask again | `engine_missing`, the reading's `connection` | `a_computer_without_podman_says_so_and_how_to_try_again` |
| The window follows the engine as it changes | the system service follows `podman events` and reads again | — (service side) |
| Search over containers, images, volumes and logs | **open gap**: the Rust app published live rows and search entries; a device reading is not indexed by the system's search yet, and an app may not keep a search of its own. Reported for the shared framework. | — |

The Rust app's sample engine and its `captures` fixtures are not ported: a test stages a reading
instead, and the app starts from what the device holds.
