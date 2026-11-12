# Backups: counted strings.
#
# Every string that carries a count lives here, so each language words its
# own singular and plural forms. `$files` and `$snapshots` are the counts
# already formatted for display; `$count` selects the plural form.

## Health, as the status bubble reports it
backups-health-ok = No failed backups
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

## Retention: what the counts on screen would remove
backups-retention-removes-none = Nothing would be removed: the next backup keeps every snapshot.
backups-retention-removes-one = The next backup would remove one snapshot, taken { $when }.
backups-retention-removes-many = { $count ->
    [one] The next backup would remove { $snapshots } snapshot: { $when }
   *[other] The next backup would remove { $snapshots } snapshots: { $when }
}

## History

backups-retention-policy-hourly = { $new ->
    [0] Keeping no hourly snapshots
    [one] Keeping one hourly snapshot
   *[other] Keeping { $new } hourly snapshots
}
backups-retention-policy-daily = { $new ->
    [0] Keeping no daily snapshots
    [one] Keeping one daily snapshot
   *[other] Keeping { $new } daily snapshots
}
backups-retention-policy-weekly = { $new ->
    [0] Keeping no weekly snapshots
    [one] Keeping one weekly snapshot
   *[other] Keeping { $new } weekly snapshots
}
backups-retention-policy-monthly = { $new ->
    [0] Keeping no monthly snapshots
    [one] Keeping one monthly snapshot
   *[other] Keeping { $new } monthly snapshots
}

## Window and accessibility copy
backups-file-list-pending = The file list for this snapshot isn’t available yet.
backups-restore-note = The whole snapshot is restored into a new or empty folder you choose, and every file is checked. Nothing that already exists is replaced.
backups-heading-snapshots = Snapshots
backups-a11y-order = Order of snapshots
backups-a11y-window = Backups

## Facts, property labels and section titles
backups-section-contents = Contents
backups-stored-on = Stored on
backups-size = Size
backups-taken = Taken
backups-encryption = Encryption
backups-folder = Folder
backups-source = Source
backups-failure = Failure
backups-changed = Changed
backups-retention = Retention
backups-type = Type
backups-status = Status
backups-last-backup = Last backup
backups-capacity = Capacity
backups-free = Free
backups-used = Used
backups-kind = Kind
backups-schedule = Schedule

## Retention policy fields
backups-retention-heading = Snapshots to keep
backups-keep-hourly = Hourly snapshots to keep
backups-keep-daily = Daily snapshots to keep
backups-keep-weekly = Weekly snapshots to keep
backups-keep-monthly = Monthly snapshots to keep

## What the window shows
backups-verifying-on = Verifying on { $destination }
backups-copy-failed = Couldn’t copy to { $destination }: { $reason }
backups-copy-failed-unrecorded = Couldn’t copy to { $destination }; no reason was recorded
backups-restore = Restore…
backups-verify = Verify files
backups-restore-labelled = Restore “{ $snapshot }”
backups-restore-from = Restore { $snapshot } from { $when }

## What a job is doing and when the next one runs
backups-job-backing-up = Backing up
backups-job-verifying = Verifying
backups-job-restoring = Restoring
backups-snapshot = Snapshot
backups-schedule-on-demand = When you back up
backups-schedule-hourly = Every hour, on the hour
backups-schedule-hourly-at = Every hour at { $minute } past
backups-schedule-daily = Every day at { $time }
backups-schedule-weekly = Every { $weekday } at { $time }
backups-empty-snapshots-hint = Back up now to create the first one.
backups-setup-start-hint = Choose a folder to keep a copy of every file in it, checked so you know it is intact.
backups-setup-destination-hint = Pick a drive or a folder outside the one you are backing up.
