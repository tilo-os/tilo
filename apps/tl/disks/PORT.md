# Disks, ported from the Rust app

| Feature | Where it lives now | Test |
| --- | --- | --- |
| The drives and their partitions | the `disks` reading, `crates/os/system-service/src/disks.rs` over the system information service's `lsblk` tree, published as `device.disks` | `the_drive_list_names_the_drives_and_their_health` |
| Each drive's name, size, transport and whether it is failing | `drive_row`, `drive_name` | `the_drive_list_names_the_drives_and_their_health` |
| The names a person gives a drive, following it by serial everywhere | the app root's `names` map and `name_drive` | `a_drive_is_named_in_the_workspace` |
| The proportional partition map and the unallocated tail | `overview_pane`, `shape`, `allocated` | `a_chosen_drive_shows_its_partitions_and_unallocated_space` |
| The drive's own health: verdict, warnings, temperature, hours, cycles, life, spare, bytes, errors, self-test | `health_pane` | `the_health_tab_shows_what_the_drive_reports` |
| The partition list with filesystem, label, size, use and mount point | `partitions_pane`, `partition_words` | `the_partitions_tab_offers_each_partition_its_verbs` |
| Open and close a partition, or a whole drive | `device.mount`/`device.unmount`; the service asks UDisks2 | `the_partitions_tab_offers_each_partition_its_verbs` |
| Erase a partition or drive, choosing the filesystem, behind a confirmation that says what is lost | `format_dialog`, `format_device`; the service writes it through UDisks2 | `erasing_asks_for_the_filesystem_and_then_asks_the_device` |
| Measure the drive, with the reading and random reads a second shown | `device.benchmark`; the service reads the device and keeps the result in the reading's benchmark | `a_failing_drive_says_so_and_offers_its_eject` |
| Eject a removable drive | `device.eject`; the service powers it off through UDisks2 | `a_failing_drive_says_so_and_offers_its_eject` |

## Known missing rows

- **Opening a mounted partition in Files** is not ported: the Rust app asked the shell to show
  the mount point in Files, and a tilolang app has no binding that opens a folder in another
  app. Reported for the shared framework.
- **Opening the Disk Usage app** for a partition is not ported, for the same reason: no binding
  opens another app at a path.
