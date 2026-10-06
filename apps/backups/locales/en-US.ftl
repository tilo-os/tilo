# Backups: counted strings.
#
# Every string that carries a count lives here, so each language words its
# own singular and plural forms. `$files` and `$snapshots` are the counts
# already formatted for display; `$count` selects the plural form.

## Health, as the status bubble reports it
backups-health-ok = All backups healthy
backups-health-attention = { $count ->
    [one] One backup needs attention
   *[other] { $snapshots } backups need attention
}

## A snapshot's change, in a timeline row and the snapshot details
backups-changed-files = { $count ->
    [one] { $size } changed in one file
   *[other] { $size } changed in { $files } files
}
backups-size-in-files = { $count ->
    [one] { $size } in one file
   *[other] { $size } in { $files } files
}
backups-restored-files = { $count ->
    [one] Restored and checked one file
   *[other] Restored and checked { $files } files
}
backups-verified-files = { $count ->
    [one] Checked one file, { $size }
   *[other] Checked { $files } files, { $size }
}
backups-backed-up-files = { $count ->
    [one] Backed up one file, { $size }
   *[other] Backed up { $files } files, { $size }
}
backups-progress-files = { $count ->
    [one] One file copied, { $size }
   *[other] { $files } files copied, { $size }
}

## Summary tiles: the unit after a number
backups-unit-minutes = { $count ->
    [one] minute
   *[other] min
}
backups-unit-minutes-ago = { $count ->
    [one] minute ago
   *[other] min ago
}
backups-unit-hours-ago = { $count ->
    [one] hour ago
   *[other] hours ago
}
backups-unit-days-ago = { $count ->
    [one] day ago
   *[other] days ago
}

## Retention
backups-retention-total = { $count ->
    [one] Each source keeps at most one snapshot, plus the ones kept until removed.
   *[other] Each source keeps at most { $snapshots } snapshots, plus the ones kept until removed.
}
