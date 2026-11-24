# System Monitor, ported from the Rust app

Every feature the Rust app had, where it lives now, and the test that holds it. The process table
and the machine's readings come from the session's system information service as the device's
`processes` reading; the window and its arrangement are this package.

| Feature | Where it lives now | Test |
| --- | --- | --- |
| The process table from `/proc`, and the machine's readings | `processes` reading, `crates/os/system-service/src/processes.rs` over the system information service's snapshots | `the_process_list_shows_the_processes_and_what_the_machine_uses` |
| Name, user, CPU, memory, threads and PID, busiest first | `process_row` | `the_process_list_shows_the_processes_and_what_the_machine_uses` |
| Sort by processor, memory, name, PID, user or threads, rising or falling | the "Sort by" menu, `sorted`, `reversed` | `sorting_by_name_orders_the_list_rising` |
| A tree under each process, or a flat list | `tree_rows`, `flat_rows`, the "Flat list" toggle | `the_tree_indents_children_under_their_parent_and_the_flat_list_does_not` |
| Kernel threads hidden until asked for | the "Kernel threads" toggle and the filter in `tree_rows`/`flat_rows` | `kernel_threads_show_when_they_are_asked_for` |
| The filter over the list | `matches`, the filter field in `processes_pane` | `the_filter_narrows_the_list` |
| CPU, memory, network and disk tiles with their history | `processes_pane`, `ui.StatTile` with the reading's histories | `the_process_list_shows_the_processes_and_what_the_machine_uses` |
| The process inspector: load, PID, user, status, app, parent, threads, virtual memory, running for | `process_detail` | `a_chosen_process_shows_its_facts_and_the_verbs_that_end_it` |
| End, kill, and ending every process of an app | `process_detail` → `device.end`/`device.kill`/`end_app` | `an_app_from_the_reading_is_offered_its_end`, `ending_a_process_asks_the_device_for_its_lifetime` |
| The update interval, including pausing | the "Update frequency" menu → `device.set_interval` | — (the verbs are the reading's; the menu is the test above's window) |
| The resources dashboard: processor and cores, memory and swap, network, disk, graphics, battery, temperature, pressure, the machine's facts | `resources_pane` | `the_resources_tab_shows_the_machine_readings` |
| A service that does not answer is explained, with a way to ask again | `engine_missing` | `a_computer_with_no_system_information_service_says_so_and_offers_to_try_again` |
| The first hour's walk: open, sort, filter, resources, dark | `scripts/headless-examples/app-walks/monitor.json` | `t test app-walk/monitor` |

## Known missing rows

- **A process's command line** was shown when the person granted command lines to the app. The
  device reading is one value for the whole session, and the system information service grants
  command lines to one app at a time; a reading cannot ask each reader. This is reported for the
  shared framework (a per-reader field or grant on a device reading), not dropped silently.
- **A process's open file count** was read from `/proc/<pid>/fd` under the app's file grant. The
  system information service's process row does not carry it; adding it there is the fix.
